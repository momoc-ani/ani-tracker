use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::player::{
    validate_model_manifest, EnhancementBudget, EnhancementModelManifest, FrameInterpolator,
    ModelEnhancer, RawVideoFrame,
};

const SIDECAR_MANIFEST_NAME: &str = "manifest.json";
const PROTOCOL_MAGIC: [u8; 8] = *b"ANIFRM1\0";
const PROTOCOL_VERSION: u16 = 1;
const HEADER_BYTES: usize = 48;
const MAX_CONTROL_PAYLOAD_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const FRAME_TIME_SAMPLE_CAPACITY: usize = 120;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSidecarManifest {
    pub schema_version: u32,
    pub protocol_version: u16,
    pub executable: String,
    pub executable_sha256: String,
    pub model: EnhancementModelManifestFile,
    pub files: Vec<ModelSidecarFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnhancementModelManifestFile {
    pub model_id: String,
    pub backend: String,
    #[serde(default)]
    pub operation: ModelOperation,
    #[serde(default = "default_output_scale")]
    pub output_scale: u32,
    pub directory: String,
    pub input_width: u32,
    pub input_height: u32,
    pub required_vram_bytes: u64,
    pub estimated_frame_time_ms: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelOperation {
    #[default]
    Interpolate,
    Enhance,
}

const fn default_output_scale() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSidecarFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ModelSidecarConfig {
    pub root: PathBuf,
    pub available_vram_bytes: u64,
    pub target_frame_time_ms: f64,
    pub startup_timeout: Duration,
    pub frame_timeout: Duration,
    pub max_frame_bytes: usize,
}

impl ModelSidecarConfig {
    pub fn new(root: PathBuf, available_vram_bytes: u64, target_frame_time_ms: f64) -> Self {
        Self {
            root,
            available_vram_bytes,
            target_frame_time_ms,
            startup_timeout: Duration::from_secs(20),
            frame_timeout: Duration::from_millis(50),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelSidecarDiagnostics {
    pub model_id: String,
    pub backend: String,
    pub gpu_device: String,
    pub warmup_frame_time_ms: f64,
    pub last_frame_time_ms: Option<f64>,
    pub p95_frame_time_ms: Option<f64>,
    pub frame_time_sample_count: u64,
    pub processed_frames: u64,
    pub dropped_frames: u64,
    pub degradation_reason: Option<String>,
}

pub struct ModelSidecarRuntime {
    manifest: EnhancementModelManifest,
    operation: ModelOperation,
    output_scale: u32,
    budget: EnhancementBudget,
    connection: Mutex<SidecarConnection>,
    frame_timeout: Duration,
    max_frame_bytes: usize,
    request_sequence: AtomicU64,
    ready: AtomicBool,
    diagnostics: Mutex<ModelSidecarDiagnostics>,
    frame_time_samples_ms: Mutex<VecDeque<f64>>,
}

struct SidecarConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HandshakePayload {
    ready: bool,
    protocol_version: u16,
    backend: String,
    gpu_device: String,
    model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
enum MessageKind {
    HandshakeRequest = 1,
    HandshakeResponse = 2,
    WarmupRequest = 3,
    InterpolateRequest = 4,
    FrameResponse = 5,
    ErrorResponse = 6,
    ShutdownRequest = 7,
    EnhanceRequest = 8,
}

#[derive(Debug)]
struct WireMessage {
    kind: MessageKind,
    request_id: u64,
    width: u32,
    height: u32,
    stride: u32,
    pts_micros: i64,
    payload: Vec<u8>,
}

impl ModelSidecarRuntime {
    /// 返回清单声明并已完成校验的输出倍率。
    pub fn output_scale(&self) -> u32 {
        self.output_scale
    }

    /// 校验全部发布资源，启动长驻 sidecar，并以真实模型帧完成 warmup 后才返回可用运行时。
    pub async fn launch(config: ModelSidecarConfig) -> Result<Self, String> {
        let validated = validate_sidecar_bundle(&config).await?;
        let mut command = hidden_command(&validated.executable);
        command
            .arg("--stdio")
            .arg("--model-dir")
            .arg(&validated.model_directory)
            .arg("--model-id")
            .arg(&validated.manifest.model.model_id)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("启动模型 sidecar 失败：{error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "模型 sidecar 标准输入不可用".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "模型 sidecar 标准输出不可用".to_owned())?;
        let mut connection = SidecarConnection {
            child,
            stdin,
            stdout,
        };

        let handshake = tokio::time::timeout(config.startup_timeout, async {
            write_message(
                &mut connection.stdin,
                &WireMessage {
                    kind: MessageKind::HandshakeRequest,
                    request_id: 1,
                    width: 0,
                    height: 0,
                    stride: 0,
                    pts_micros: 0,
                    payload: Vec::new(),
                },
            )
            .await?;
            read_message(&mut connection.stdout, MAX_CONTROL_PAYLOAD_BYTES).await
        })
        .await
        .map_err(|_| "模型 sidecar 握手超时".to_owned())??;
        if handshake.kind != MessageKind::HandshakeResponse || handshake.request_id != 1 {
            return Err("模型 sidecar 握手响应无效".to_owned());
        }
        let handshake: HandshakePayload = serde_json::from_slice(&handshake.payload)
            .map_err(|error| format!("模型 sidecar 握手 JSON 无效：{error}"))?;
        validate_handshake(&handshake, &validated.manifest)?;

        let warmup_frame = RawVideoFrame {
            width: validated.manifest.model.input_width,
            height: validated.manifest.model.input_height,
            stride: validated.manifest.model.input_width.saturating_mul(3),
            pts_micros: 0,
            data: vec![
                0;
                frame_bytes(
                    validated.manifest.model.input_width,
                    validated.manifest.model.input_height,
                    config.max_frame_bytes,
                )?
            ],
        };
        let warmup_started = Instant::now();
        let warmup_response = tokio::time::timeout(config.startup_timeout, async {
            match validated.manifest.model.operation {
                ModelOperation::Interpolate => {
                    write_frame_pair(
                        &mut connection.stdin,
                        MessageKind::WarmupRequest,
                        2,
                        &warmup_frame,
                        &warmup_frame,
                    )
                    .await?;
                }
                ModelOperation::Enhance => {
                    write_frame(
                        &mut connection.stdin,
                        MessageKind::EnhanceRequest,
                        2,
                        &warmup_frame,
                    )
                    .await?;
                }
            }
            read_message(&mut connection.stdout, config.max_frame_bytes).await
        })
        .await
        .map_err(|_| "模型 sidecar warmup 超时".to_owned())??;
        validate_frame_response(
            &warmup_response,
            2,
            &warmup_frame,
            config.max_frame_bytes,
            validated.manifest.model.operation,
            validated.manifest.model.output_scale,
        )?;
        let warmup_frame_time_ms = warmup_started.elapsed().as_secs_f64() * 1_000.0;
        if warmup_frame_time_ms > config.target_frame_time_ms {
            return Err(format!(
                "模型 warmup 帧耗时 {warmup_frame_time_ms:.2}ms 超过预算 {:.2}ms",
                config.target_frame_time_ms
            ));
        }

        Ok(Self {
            manifest: validated.model_manifest,
            operation: validated.manifest.model.operation,
            output_scale: validated.manifest.model.output_scale,
            budget: validated.budget,
            connection: Mutex::new(connection),
            frame_timeout: config.frame_timeout,
            max_frame_bytes: config.max_frame_bytes,
            request_sequence: AtomicU64::new(2),
            ready: AtomicBool::new(true),
            diagnostics: Mutex::new(ModelSidecarDiagnostics {
                model_id: handshake.model_id,
                backend: handshake.backend,
                gpu_device: handshake.gpu_device,
                warmup_frame_time_ms,
                last_frame_time_ms: None,
                p95_frame_time_ms: Some(warmup_frame_time_ms),
                frame_time_sample_count: 1,
                processed_frames: 0,
                dropped_frames: 0,
                degradation_reason: None,
            }),
            frame_time_samples_ms: Mutex::new(VecDeque::from([warmup_frame_time_ms])),
        })
    }

    pub async fn diagnostics(&self) -> ModelSidecarDiagnostics {
        self.diagnostics.lock().await.clone()
    }

    async fn record_frame_time(&self, frame_time_ms: f64) {
        let mut samples = self.frame_time_samples_ms.lock().await;
        if samples.len() == FRAME_TIME_SAMPLE_CAPACITY {
            let _ = samples.pop_front();
        }
        samples.push_back(frame_time_ms);
        let mut sorted = samples.iter().copied().collect::<Vec<_>>();
        sorted.sort_by(f64::total_cmp);
        let p95_index = ((sorted.len() * 95).div_ceil(100)).saturating_sub(1);
        let p95_frame_time_ms = sorted.get(p95_index).copied();
        let frame_time_sample_count = samples.len() as u64;
        drop(samples);

        let mut diagnostics = self.diagnostics.lock().await;
        diagnostics.last_frame_time_ms = Some(frame_time_ms);
        diagnostics.p95_frame_time_ms = p95_frame_time_ms;
        diagnostics.frame_time_sample_count = frame_time_sample_count;
        diagnostics.processed_frames = diagnostics.processed_frames.saturating_add(1);
    }

    pub async fn shutdown(&self) {
        self.ready.store(false, Ordering::Release);
        let mut connection = self.connection.lock().await;
        let request_id = self.request_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = write_message(
            &mut connection.stdin,
            &WireMessage {
                kind: MessageKind::ShutdownRequest,
                request_id,
                width: 0,
                height: 0,
                stride: 0,
                pts_micros: 0,
                payload: Vec::new(),
            },
        )
        .await;
        let _ = connection.child.kill().await;
        let _ = connection.child.wait().await;
    }

    async fn interpolate_frame(
        &self,
        previous: RawVideoFrame,
        next: RawVideoFrame,
    ) -> Result<RawVideoFrame, String> {
        if self.operation != ModelOperation::Interpolate {
            return Err("当前模型 sidecar 不提供插帧操作".to_owned());
        }
        if !self.ready.load(Ordering::Acquire) {
            return Err("模型 sidecar 已降级关闭".to_owned());
        }
        previous.validate(self.max_frame_bytes)?;
        next.validate(self.max_frame_bytes)?;
        if previous.width != next.width
            || previous.height != next.height
            || previous.stride != next.stride
        {
            return Err("插帧输入的两帧尺寸不一致".to_owned());
        }
        let request_id = self.request_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let started = Instant::now();
        let result = tokio::time::timeout(self.frame_timeout, async {
            let mut connection = self.connection.lock().await;
            write_frame_pair(
                &mut connection.stdin,
                MessageKind::InterpolateRequest,
                request_id,
                &previous,
                &next,
            )
            .await?;
            read_message(&mut connection.stdout, self.max_frame_bytes).await
        })
        .await;
        let response = match result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return self.degrade(error).await,
            Err(_) => return self.degrade("模型单帧处理超时".to_owned()).await,
        };
        if let Err(error) = validate_frame_response(
            &response,
            request_id,
            &previous,
            self.max_frame_bytes,
            ModelOperation::Interpolate,
            1,
        ) {
            return self.degrade(error).await;
        }
        let frame_time_ms = started.elapsed().as_secs_f64() * 1_000.0;
        self.record_frame_time(frame_time_ms).await;
        Ok(RawVideoFrame {
            width: response.width,
            height: response.height,
            stride: response.stride,
            pts_micros: midpoint_pts(previous.pts_micros, next.pts_micros),
            data: response.payload,
        })
    }

    async fn enhance_frame(&self, frame: RawVideoFrame) -> Result<RawVideoFrame, String> {
        if self.operation != ModelOperation::Enhance {
            return Err("当前模型 sidecar 不提供单帧增强操作".to_owned());
        }
        if !self.ready.load(Ordering::Acquire) {
            return Err("模型 sidecar 已降级关闭".to_owned());
        }
        frame.validate(self.max_frame_bytes)?;
        let request_id = self.request_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let started = Instant::now();
        let result = tokio::time::timeout(self.frame_timeout, async {
            let mut connection = self.connection.lock().await;
            write_frame(
                &mut connection.stdin,
                MessageKind::EnhanceRequest,
                request_id,
                &frame,
            )
            .await?;
            read_message(&mut connection.stdout, self.max_frame_bytes).await
        })
        .await;
        let response = match result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return self.degrade(error).await,
            Err(_) => return self.degrade("模型单帧处理超时".to_owned()).await,
        };
        if let Err(error) = validate_frame_response(
            &response,
            request_id,
            &frame,
            self.max_frame_bytes,
            ModelOperation::Enhance,
            self.output_scale,
        ) {
            return self.degrade(error).await;
        }
        let frame_time_ms = started.elapsed().as_secs_f64() * 1_000.0;
        self.record_frame_time(frame_time_ms).await;
        Ok(RawVideoFrame {
            width: response.width,
            height: response.height,
            stride: response.stride,
            pts_micros: frame.pts_micros,
            data: response.payload,
        })
    }

    async fn degrade<T>(&self, reason: String) -> Result<T, String> {
        self.ready.store(false, Ordering::Release);
        let mut diagnostics = self.diagnostics.lock().await;
        diagnostics.dropped_frames = diagnostics.dropped_frames.saturating_add(1);
        diagnostics.degradation_reason = Some(reason.clone());
        Err(reason)
    }
}

#[async_trait]
impl FrameInterpolator for ModelSidecarRuntime {
    fn backend_id(&self) -> &str {
        &self.manifest.backend
    }
    fn ready(&self) -> bool {
        self.operation == ModelOperation::Interpolate && self.ready.load(Ordering::Acquire)
    }
    fn budget(&self) -> EnhancementBudget {
        self.budget
    }

    async fn interpolate(
        &self,
        previous: RawVideoFrame,
        next: RawVideoFrame,
    ) -> Result<RawVideoFrame, String> {
        self.interpolate_frame(previous, next).await
    }
}

#[async_trait]
impl ModelEnhancer for ModelSidecarRuntime {
    fn backend_id(&self) -> &str {
        &self.manifest.backend
    }

    fn ready(&self) -> bool {
        self.operation == ModelOperation::Enhance && self.ready.load(Ordering::Acquire)
    }

    fn budget(&self) -> EnhancementBudget {
        self.budget
    }

    async fn enhance(&self, frame: RawVideoFrame) -> Result<RawVideoFrame, String> {
        self.enhance_frame(frame).await
    }
}

struct ValidatedBundle {
    manifest: ModelSidecarManifest,
    model_manifest: EnhancementModelManifest,
    budget: EnhancementBudget,
    executable: PathBuf,
    model_directory: PathBuf,
}

async fn validate_sidecar_bundle(config: &ModelSidecarConfig) -> Result<ValidatedBundle, String> {
    let root = dunce::canonicalize(&config.root)
        .map_err(|error| format!("模型 sidecar 目录不可用：{error}"))?;
    let manifest_bytes = tokio::fs::read(root.join(SIDECAR_MANIFEST_NAME))
        .await
        .map_err(|error| format!("读取模型 sidecar 清单失败：{error}"))?;
    let manifest: ModelSidecarManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("模型 sidecar 清单无效：{error}"))?;
    if manifest.schema_version != 1 || manifest.protocol_version != PROTOCOL_VERSION {
        return Err("模型 sidecar 清单或协议版本不兼容".to_owned());
    }
    if manifest.model.output_scale == 0
        || (manifest.model.operation == ModelOperation::Interpolate
            && manifest.model.output_scale != 1)
    {
        return Err("模型 sidecar 输出倍率无效".to_owned());
    }
    let executable = resolve_bundle_file(&root, &manifest.executable)?;
    validate_sha256(&executable, &manifest.executable_sha256).await?;
    for file in &manifest.files {
        let path = resolve_bundle_file(&root, &file.path)?;
        validate_sha256(&path, &file.sha256).await?;
    }
    let model_directory = resolve_bundle_file(&root, &manifest.model.directory)?;
    if !model_directory.is_dir() {
        return Err("模型清单目录不是有效文件夹".to_owned());
    }
    let aggregate_weight_sha256 = aggregate_digest(&root, &manifest.files).await?;
    let model_manifest = EnhancementModelManifest {
        model_id: manifest.model.model_id.clone(),
        backend: manifest.model.backend.clone(),
        weight_sha256: aggregate_weight_sha256,
        input_width: manifest.model.input_width,
        input_height: manifest.model.input_height,
        required_vram_bytes: manifest.model.required_vram_bytes,
        estimated_frame_time_ms: manifest.model.estimated_frame_time_ms,
    };
    validate_model_manifest(
        &model_manifest,
        config.available_vram_bytes,
        config.target_frame_time_ms,
    )?;
    let budget = EnhancementBudget {
        target_frame_time_ms: config.target_frame_time_ms,
        estimated_frame_time_ms: f64::from(model_manifest.estimated_frame_time_ms),
        available_vram_bytes: config.available_vram_bytes,
        required_vram_bytes: model_manifest.required_vram_bytes,
    };
    Ok(ValidatedBundle {
        manifest,
        model_manifest,
        budget,
        executable,
        model_directory,
    })
}

fn resolve_bundle_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("模型 sidecar 清单包含越界路径".to_owned());
    }
    let resolved = dunce::canonicalize(root.join(path))
        .map_err(|error| format!("模型 sidecar 资源不可用：{error}"))?;
    if !resolved.starts_with(root) {
        return Err("模型 sidecar 资源越过包目录".to_owned());
    }
    Ok(resolved)
}

async fn validate_sha256(path: &Path, expected: &str) -> Result<(), String> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("模型 sidecar SHA-256 格式无效".to_owned());
    }
    let actual = file_digest(path).await?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!("模型 sidecar 资源摘要不一致：{}", path.display()));
    }
    Ok(())
}

async fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("读取模型资源失败：{error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("读取模型资源失败：{error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

async fn aggregate_digest(root: &Path, files: &[ModelSidecarFile]) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut entries = files.to_vec();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    for entry in entries {
        let _ = resolve_bundle_file(root, &entry.path)?;
        digest.update(entry.path.as_bytes());
        digest.update(entry.sha256.to_ascii_lowercase().as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_handshake(
    payload: &HandshakePayload,
    manifest: &ModelSidecarManifest,
) -> Result<(), String> {
    if !payload.ready || payload.protocol_version != PROTOCOL_VERSION {
        return Err("模型 sidecar 未完成运行时初始化".to_owned());
    }
    if payload.backend != manifest.model.backend || payload.model_id != manifest.model.model_id {
        return Err("模型 sidecar 自报后端或模型与清单不一致".to_owned());
    }
    if payload.backend != "ncnn-vulkan" || payload.gpu_device.trim().is_empty() {
        return Err("模型 sidecar 未确认 Vulkan GPU 设备".to_owned());
    }
    Ok(())
}

async fn write_frame_pair(
    writer: &mut ChildStdin,
    kind: MessageKind,
    request_id: u64,
    previous: &RawVideoFrame,
    next: &RawVideoFrame,
) -> Result<(), String> {
    let mut payload = Vec::with_capacity(previous.data.len().saturating_add(next.data.len()));
    payload.extend_from_slice(&previous.data);
    payload.extend_from_slice(&next.data);
    write_message(
        writer,
        &WireMessage {
            kind,
            request_id,
            width: previous.width,
            height: previous.height,
            stride: previous.stride,
            pts_micros: midpoint_pts(previous.pts_micros, next.pts_micros),
            payload,
        },
    )
    .await
}

async fn write_frame(
    writer: &mut ChildStdin,
    kind: MessageKind,
    request_id: u64,
    frame: &RawVideoFrame,
) -> Result<(), String> {
    write_message(
        writer,
        &WireMessage {
            kind,
            request_id,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            pts_micros: frame.pts_micros,
            payload: frame.data.clone(),
        },
    )
    .await
}

async fn write_message(writer: &mut ChildStdin, message: &WireMessage) -> Result<(), String> {
    let payload_length =
        u64::try_from(message.payload.len()).map_err(|_| "模型消息过大".to_owned())?;
    let mut header = [0_u8; HEADER_BYTES];
    header[0..8].copy_from_slice(&PROTOCOL_MAGIC);
    header[8..10].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&(message.kind as u16).to_le_bytes());
    header[16..24].copy_from_slice(&message.request_id.to_le_bytes());
    header[24..28].copy_from_slice(&message.width.to_le_bytes());
    header[28..32].copy_from_slice(&message.height.to_le_bytes());
    header[32..36].copy_from_slice(&message.stride.to_le_bytes());
    header[36..44].copy_from_slice(&message.pts_micros.to_le_bytes());
    header[44..48].copy_from_slice(
        &u32::try_from(payload_length)
            .map_err(|_| "模型消息超过协议限制".to_owned())?
            .to_le_bytes(),
    );
    writer
        .write_all(&header)
        .await
        .map_err(|error| format!("写入模型消息失败：{error}"))?;
    writer
        .write_all(&message.payload)
        .await
        .map_err(|error| format!("写入模型帧失败：{error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("刷新模型消息失败：{error}"))
}

async fn read_message(
    reader: &mut ChildStdout,
    max_payload_bytes: usize,
) -> Result<WireMessage, String> {
    let mut header = [0_u8; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|error| format!("读取模型消息头失败：{error}"))?;
    if header[0..8] != PROTOCOL_MAGIC
        || u16::from_le_bytes([header[8], header[9]]) != PROTOCOL_VERSION
    {
        return Err("模型 sidecar 返回未知协议".to_owned());
    }
    let kind = match u16::from_le_bytes([header[10], header[11]]) {
        2 => MessageKind::HandshakeResponse,
        5 => MessageKind::FrameResponse,
        6 => MessageKind::ErrorResponse,
        _ => return Err("模型 sidecar 返回未知消息类型".to_owned()),
    };
    let payload_len = u32::from_le_bytes(
        header[44..48]
            .try_into()
            .map_err(|_| "模型消息长度无效".to_owned())?,
    ) as usize;
    if payload_len > max_payload_bytes {
        return Err("模型 sidecar 返回数据超过限制".to_owned());
    }
    let mut payload = vec![0; payload_len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|error| format!("读取模型消息体失败：{error}"))?;
    if kind == MessageKind::ErrorResponse {
        return Err(format!(
            "模型 sidecar 拒绝请求：{}",
            String::from_utf8_lossy(&payload)
        ));
    }
    Ok(WireMessage {
        kind,
        request_id: u64::from_le_bytes(
            header[16..24]
                .try_into()
                .map_err(|_| "模型请求标识无效".to_owned())?,
        ),
        width: u32::from_le_bytes(
            header[24..28]
                .try_into()
                .map_err(|_| "模型帧宽度无效".to_owned())?,
        ),
        height: u32::from_le_bytes(
            header[28..32]
                .try_into()
                .map_err(|_| "模型帧高度无效".to_owned())?,
        ),
        stride: u32::from_le_bytes(
            header[32..36]
                .try_into()
                .map_err(|_| "模型帧步长无效".to_owned())?,
        ),
        pts_micros: i64::from_le_bytes(
            header[36..44]
                .try_into()
                .map_err(|_| "模型帧时间戳无效".to_owned())?,
        ),
        payload,
    })
}

fn validate_frame_response(
    response: &WireMessage,
    request_id: u64,
    input: &RawVideoFrame,
    max_frame_bytes: usize,
    operation: ModelOperation,
    output_scale: u32,
) -> Result<(), String> {
    if response.kind != MessageKind::FrameResponse || response.request_id != request_id {
        return Err("模型 sidecar 帧响应与请求不匹配".to_owned());
    }
    RawVideoFrame {
        width: response.width,
        height: response.height,
        stride: response.stride,
        pts_micros: response.pts_micros,
        data: response.payload.clone(),
    }
    .validate(max_frame_bytes)?;
    let scale = match operation {
        ModelOperation::Interpolate => 1,
        ModelOperation::Enhance => output_scale,
    };
    let expected_width = input
        .width
        .checked_mul(scale)
        .ok_or_else(|| "模型输出宽度溢出".to_owned())?;
    let expected_height = input
        .height
        .checked_mul(scale)
        .ok_or_else(|| "模型输出高度溢出".to_owned())?;
    let expected_stride = expected_width
        .checked_mul(3)
        .ok_or_else(|| "模型输出步长溢出".to_owned())?;
    if response.width != expected_width
        || response.height != expected_height
        || response.stride != expected_stride
    {
        return Err("模型 sidecar 返回了意外帧尺寸".to_owned());
    }
    Ok(())
}

fn frame_bytes(width: u32, height: u32, max_frame_bytes: usize) -> Result<usize, String> {
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(3))
        .and_then(|stride| {
            usize::try_from(height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or_else(|| "模型 warmup 帧尺寸溢出".to_owned())?;
    if bytes > max_frame_bytes {
        return Err("模型 warmup 帧超过限制".to_owned());
    }
    Ok(bytes)
}

fn midpoint_pts(previous: i64, next: i64) -> i64 {
    previous.saturating_add(next.saturating_sub(previous) / 2)
}

fn hidden_command(path: &Path) -> Command {
    let command = Command::new(path);
    #[cfg(target_os = "windows")]
    {
        let mut command = command;
        command.creation_flags(0x0800_0000);
        return command;
    }
    #[allow(unreachable_code)]
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_raw_frames_and_path_traversal() {
        let frame = RawVideoFrame {
            width: 2,
            height: 2,
            stride: 6,
            pts_micros: 0,
            data: vec![0; 11],
        };
        assert!(frame.validate(64).is_err());
        assert!(resolve_bundle_file(Path::new("/tmp"), "../model.bin").is_err());
    }

    #[test]
    fn validates_only_matching_vulkan_handshake() {
        let manifest = ModelSidecarManifest {
            schema_version: 1,
            protocol_version: 1,
            executable: "sidecar".to_owned(),
            executable_sha256: "a".repeat(64),
            model: EnhancementModelManifestFile {
                model_id: "rife-v4.6".to_owned(),
                backend: "ncnn-vulkan".to_owned(),
                operation: ModelOperation::Interpolate,
                output_scale: 1,
                directory: "models/rife-v4.6".to_owned(),
                input_width: 8,
                input_height: 8,
                required_vram_bytes: 1,
                estimated_frame_time_ms: 1,
            },
            files: Vec::new(),
        };
        let valid = HandshakePayload {
            ready: true,
            protocol_version: 1,
            backend: "ncnn-vulkan".to_owned(),
            gpu_device: "AMD Radeon".to_owned(),
            model_id: "rife-v4.6".to_owned(),
        };
        assert!(validate_handshake(&valid, &manifest).is_ok());
        assert!(validate_handshake(
            &HandshakePayload {
                gpu_device: String::new(),
                ..valid
            },
            &manifest
        )
        .is_err());
    }
}
