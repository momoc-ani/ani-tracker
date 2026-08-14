use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ani_contracts::{
    PlayerFrameInterpolation, PlayerVideoEnhancement, RemoteDirectEnhancementDiagnostics,
    RemoteDirectEnhancementStatus, RemoteInterpolationCapacity, RemotePlaybackDiagnostics,
    RemotePlaybackEnhancement, RemotePlaybackPath, RemotePlaybackSession, RemotePlaybackSubtitle,
};
use ani_domain::{AppSettings, DownloadTask, MediaFile, PlaybackCheckpoint};
use ani_media::model_sidecar::{ModelSidecarConfig, ModelSidecarRuntime};
use ani_media::player::{
    plan_interpolation, FrameInterpolator, InterpolationCapacityInput, InterpolationPlan,
    ModelEnhancer, RawVideoFrame,
};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use std::process::Stdio;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const TRANSCODER_START_TIMEOUT: Duration = Duration::from_secs(20);
const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const HLS_SEGMENT_SECONDS: &str = "2";
const HLS_SEEK_PREROLL_SECONDS: f64 = 3.0;
const FFMPEG_STDERR_LIMIT: usize = 32 * 1024;
const MAX_SESSIONS: usize = 2;
const RIFE_FRAME_QUEUE_CAPACITY: usize = 2;
const RIFE_HARD_MAX_MULTIPLIER: u8 = 2;
const REMOTE_OUTPUT_FRAME_RATE_CAP: f64 = 60.0;
const MODEL_PIPELINE_UTILIZATION_LIMIT: f64 = 0.8;
const MODEL_PIPELINE_SAFETY_MARGIN_MS: f64 = 5.0;
const MAX_DIRECT_DIAGNOSTIC_CODECS: usize = 16;
const MAX_DIRECT_DIAGNOSTIC_CODEC_BYTES: usize = 64;
const MAX_DIRECT_DIAGNOSTIC_REASON_BYTES: usize = 512;
#[cfg(target_os = "macos")]
const ENCODER_CANDIDATES: &[(&str, &str)] = &[
    ("h264_videotoolbox", "videotoolbox"),
    ("libx264", "libx264"),
];
#[cfg(target_os = "windows")]
const ENCODER_CANDIDATES: &[(&str, &str)] = &[
    ("h264_nvenc", "nvenc"),
    ("h264_amf", "amf"),
    ("h264_qsv", "qsv"),
    ("libx264", "libx264"),
];
#[cfg(target_os = "linux")]
const ENCODER_CANDIDATES: &[(&str, &str)] = &[
    ("h264_nvenc", "nvenc"),
    ("h264_qsv", "qsv"),
    ("libx264", "libx264"),
];
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
const ENCODER_CANDIDATES: &[(&str, &str)] = &[("libx264", "libx264")];
const MEDIA_FILE_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// 远程媒体核心读取下载、媒体、设置和续播状态的公共端口。
#[async_trait]
pub trait RemoteMediaRepository: Send + Sync {
    /// 读取指定下载任务。
    async fn get_download_task(&self, task_id: &str) -> Result<Option<DownloadTask>, String>;

    /// 读取全部已登记媒体文件。
    async fn list_media_files(&self) -> Result<Vec<MediaFile>, String>;

    /// 读取当前桌面媒体设置。
    async fn get_settings(&self) -> Result<AppSettings, String>;

    /// 读取指定下载文件的续播检查点。
    async fn get_playback_checkpoint(
        &self,
        task_id: &str,
        file_index: Option<i64>,
    ) -> Result<Option<PlaybackCheckpoint>, String>;
}

/// 桌面 FFmpeg 与 FFprobe 的受控候选路径。
#[derive(Debug, Clone)]
pub struct RemoteMediaTools {
    pub ffprobe_paths: Vec<PathBuf>,
    pub ffmpeg_path: PathBuf,
    pub timeout: Duration,
    pub rife_sidecar_root: Option<PathBuf>,
    pub realesrgan_sidecar_root: Option<PathBuf>,
    pub model_available_vram_bytes: u64,
}

/// 创建浏览器或外部播放器媒体会话的受控输入。
pub struct RemoteMediaSessionInput<'a> {
    pub task_id: &'a str,
    pub requested_mode: &'a str,
    pub file_index: Option<i64>,
    pub enhancement: RemotePlaybackEnhancement,
    pub start_position_seconds: Option<f64>,
    pub subtitle_mode: &'a str,
    pub subtitle_id: Option<&'a str>,
}

/// 网关可输出的受控媒体文件。
#[derive(Debug, Clone)]
pub struct RemoteMediaAsset {
    pub file_path: PathBuf,
    pub content_type: String,
    pub direct: bool,
    pub file_name: Option<String>,
}

/// 远程媒体会话的稳定协议错误。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RemoteMediaError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl RemoteMediaError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionAccess {
    Browser,
    External,
}

/// 汇总创建远程媒体会话所需参数，避免调用链散落位置参数。
struct SessionCreateRequest<'a> {
    task_id: &'a str,
    device_id: &'a str,
    requested_mode: &'a str,
    file_index: Option<i64>,
    enhancement: RemotePlaybackEnhancement,
    start_position_seconds: Option<f64>,
    subtitle_mode: &'a str,
    subtitle_id: Option<&'a str>,
    access: SessionAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedSubtitleMode {
    Soft,
    Burned,
    Off,
}

struct PreparedSubtitle {
    public: RemotePlaybackSubtitle,
    output_path: PathBuf,
}

struct SessionRecord {
    public: Mutex<RemotePlaybackSession>,
    access: SessionAccess,
    access_token: Option<String>,
    device_id: String,
    source_path: PathBuf,
    content_type: String,
    temporary_directory: PathBuf,
    process: Mutex<Option<MediaProcess>>,
    last_accessed_at_millis: Mutex<i64>,
}

struct MediaProcess {
    encoder: Child,
    decoder: Option<Child>,
    pipeline: Option<JoinHandle<Result<(), String>>>,
    pipeline_state: Option<Arc<Mutex<ModelPipelineState>>>,
    stderr_captures: Vec<FfmpegStderrCapture>,
}

impl Drop for MediaProcess {
    fn drop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.abort();
        }
        for capture in self.stderr_captures.drain(..) {
            capture.abort();
        }
    }
}

impl MediaProcess {
    async fn stop(mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.abort();
            let _ = pipeline.await;
        }
        if let Some(mut decoder) = self.decoder.take() {
            let _ = decoder.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), decoder.wait()).await;
        }
        let _ = self.encoder.kill().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), self.encoder.wait()).await;
        for capture in self.stderr_captures.drain(..) {
            capture.finish().await;
        }
    }
}

struct FfmpegStderrCapture {
    label: &'static str,
    buffer: Arc<StdMutex<Vec<u8>>>,
    task: JoinHandle<()>,
}

impl Drop for FfmpegStderrCapture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FfmpegStderrCapture {
    /// 返回当前已捕获的 FFmpeg 错误文本，供启动失败诊断使用。
    fn snapshot(&self) -> String {
        let bytes = self
            .buffer
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }

    /// 等待 stderr 读取完成，并记录仍有价值的运行时警告。
    async fn finish(mut self) {
        if tokio::time::timeout(Duration::from_secs(1), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
        let message = self.snapshot();
        if !message.is_empty() {
            log::debug!("FFmpeg {} stderr={message}", self.label);
        }
    }

    /// 中止随媒体进程一起销毁的 stderr 读取任务。
    fn abort(self) {
        self.task.abort();
    }
}

struct HlsStart {
    process: MediaProcess,
    encoder: &'static str,
    encoder_degraded: bool,
    actual_video_enhancement: PlayerVideoEnhancement,
    actual_interpolation: PlayerFrameInterpolation,
    interpolation_capacity: Option<RemoteInterpolationCapacity>,
    model_backend: Option<String>,
    degradation_reason: Option<String>,
}

struct ModelPipelineState {
    actual_video_enhancement: PlayerVideoEnhancement,
    actual_interpolation: PlayerFrameInterpolation,
    interpolation_capacity: Option<RemoteInterpolationCapacity>,
    rife_model_backend: Option<String>,
    realesrgan_model_backend: Option<String>,
    degradation_reasons: Vec<String>,
}

impl ModelPipelineState {
    fn model_backend(&self) -> Option<String> {
        model_backend_name(
            self.rife_model_backend.as_deref(),
            self.realesrgan_model_backend.as_deref(),
        )
    }

    fn degradation_reason(&self) -> Option<String> {
        (!self.degradation_reasons.is_empty()).then(|| self.degradation_reasons.join("；"))
    }
}

struct ResolvedMedia {
    path: PathBuf,
    file_name: String,
    file_index: Option<i64>,
    duration_seconds: Option<i64>,
    content_type: String,
}

/// 管理绑定设备的短期直传、字幕和实时 HLS 会话。
pub struct RemoteMediaSessionService {
    repository: Arc<dyn RemoteMediaRepository>,
    tools: RemoteMediaTools,
    temporary_root: PathBuf,
    sessions: Mutex<HashMap<String, Arc<SessionRecord>>>,
    rife_runtime: Mutex<Option<Arc<ModelSidecarRuntime>>>,
    realesrgan_runtime: Mutex<Option<Arc<ModelSidecarRuntime>>>,
    encoder_candidates: Mutex<Option<Vec<(&'static str, &'static str)>>>,
    subtitle_burn_support: Mutex<Option<Result<(), String>>>,
}

impl RemoteMediaSessionService {
    /// 使用数据库端口、媒体工具和受控临时目录创建服务。
    pub fn new(
        repository: Arc<dyn RemoteMediaRepository>,
        tools: RemoteMediaTools,
        temporary_root: PathBuf,
    ) -> Self {
        Self {
            repository,
            tools,
            temporary_root,
            sessions: Mutex::new(HashMap::new()),
            rife_runtime: Mutex::new(None),
            realesrgan_runtime: Mutex::new(None),
            encoder_candidates: Mutex::new(None),
            subtitle_burn_support: Mutex::new(None),
        }
    }

    /// 创建浏览器使用的 Bearer/Cookie 绑定会话。
    pub async fn create_session(
        self: &Arc<Self>,
        device_id: &str,
        input: RemoteMediaSessionInput<'_>,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        self.create_session_record(SessionCreateRequest {
            task_id: input.task_id,
            device_id,
            requested_mode: input.requested_mode,
            file_index: input.file_index,
            enhancement: input.enhancement,
            start_position_seconds: input.start_position_seconds,
            subtitle_mode: input.subtitle_mode,
            subtitle_id: input.subtitle_id,
            access: SessionAccess::Browser,
        })
        .await
    }

    /// 创建带高熵 URL 票据的外部播放器会话。
    pub async fn create_external_session(
        self: &Arc<Self>,
        device_id: &str,
        input: RemoteMediaSessionInput<'_>,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        self.create_session_record(SessionCreateRequest {
            task_id: input.task_id,
            device_id,
            requested_mode: input.requested_mode,
            file_index: input.file_index,
            enhancement: input.enhancement,
            start_position_seconds: input.start_position_seconds,
            subtitle_mode: input.subtitle_mode,
            subtitle_id: input.subtitle_id,
            access: SessionAccess::External,
        })
        .await
    }

    async fn create_session_record(
        self: &Arc<Self>,
        request: SessionCreateRequest<'_>,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        let SessionCreateRequest {
            task_id,
            device_id,
            requested_mode,
            file_index,
            enhancement,
            start_position_seconds: requested_start_position_seconds,
            subtitle_mode,
            subtitle_id,
            access,
        } = request;
        if !matches!(requested_mode, "direct" | "transcode") {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_MODE_INVALID",
                "播放模式无效",
            ));
        }
        if file_index.is_some_and(|value| value < 0) {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_FILE_INVALID",
                "媒体文件标识无效",
            ));
        }
        validate_remote_enhancement(requested_mode, enhancement)?;
        let subtitle_mode = parse_requested_subtitle_mode(subtitle_mode)?;
        if subtitle_mode == RequestedSubtitleMode::Burned && requested_mode != "transcode" {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_SUBTITLE_BURN_REQUIRES_TRANSCODE",
                "烧录字幕只能在实时转码模式使用",
            ));
        }
        if subtitle_mode == RequestedSubtitleMode::Burned && subtitle_id.is_none() {
            return Err(RemoteMediaError::new(
                400,
                "MEDIA_SUBTITLE_INVALID",
                "烧录字幕必须指定字幕轨道",
            ));
        }
        if subtitle_mode == RequestedSubtitleMode::Burned {
            self.ensure_subtitle_burn_supported().await?;
        }
        self.cleanup_expired().await;
        let task = self
            .repository
            .get_download_task(task_id)
            .await
            .map_err(internal_media_error)?
            .ok_or_else(|| RemoteMediaError::new(404, "MEDIA_TASK_NOT_FOUND", "下载任务不存在"))?;
        let media = self.resolve_media(&task, file_index).await?;
        let checkpoint = self
            .repository
            .get_playback_checkpoint(&task.id, media.file_index)
            .await
            .map_err(internal_media_error)?;
        let mode = if requested_mode == "transcode" {
            "hls"
        } else {
            "direct"
        };
        let start_position_seconds = resolve_session_start_position(
            requested_start_position_seconds,
            checkpoint.as_ref(),
            media.duration_seconds,
        )?;
        let stream_start_position_seconds = if mode == "hls" {
            start_position_seconds
                .map(|position| (position - HLS_SEEK_PREROLL_SECONDS).max(0.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        self.close_matching(device_id, task_id, access).await;
        self.reserve_slot().await;

        tokio::fs::create_dir_all(&self.temporary_root)
            .await
            .map_err(|error| internal_media_error(error.to_string()))?;
        let id = random_token(24);
        let temporary_directory = self.temporary_root.join(format!("session-{id}"));
        tokio::fs::create_dir(&temporary_directory)
            .await
            .map_err(|error| internal_media_error(error.to_string()))?;
        let access_token = (access == SessionAccess::External).then(|| random_token(32));
        let asset_base = access_token.as_ref().map_or_else(
            || format!("/api/media/sessions/{id}"),
            |token| format!("/api/media/external/{token}/sessions/{id}"),
        );
        let prepared_subtitles = if subtitle_mode == RequestedSubtitleMode::Off {
            Vec::new()
        } else {
            self.prepare_subtitles(
                &media.path,
                &temporary_directory,
                &asset_base,
                stream_start_position_seconds,
            )
            .await
        };
        let burn_subtitle = if subtitle_mode == RequestedSubtitleMode::Burned {
            match prepared_subtitles
                .iter()
                .find(|subtitle| Some(subtitle.public.id.as_str()) == subtitle_id)
                .map(|subtitle| subtitle.output_path.clone())
            {
                Some(output_path) => Some(output_path),
                None => {
                    let _ = tokio::fs::remove_dir_all(&temporary_directory).await;
                    return Err(RemoteMediaError::new(
                        409,
                        "MEDIA_SUBTITLE_UNAVAILABLE",
                        "请求的字幕轨道不可用于烧录",
                    ));
                }
            }
        } else {
            None
        };
        let subtitles = if subtitle_mode == RequestedSubtitleMode::Soft {
            prepared_subtitles
                .into_iter()
                .map(|subtitle| subtitle.public)
                .collect()
        } else {
            Vec::new()
        };
        let mut process = None;
        let mut diagnostics = RemotePlaybackDiagnostics {
            playback_path: if mode == "hls" {
                RemotePlaybackPath::Hls
            } else {
                RemotePlaybackPath::Direct
            },
            subtitle_mode: (subtitle_mode != RequestedSubtitleMode::Off).then(|| {
                if subtitle_mode == RequestedSubtitleMode::Burned {
                    "burned".to_owned()
                } else {
                    "soft".to_owned()
                }
            }),
            enhanced_frame_input: enhancement.video_enhancement != PlayerVideoEnhancement::Off
                || enhancement.frame_interpolation != PlayerFrameInterpolation::Off,
            video_enhancement: enhancement.video_enhancement,
            frame_interpolation: enhancement.frame_interpolation,
            ..Default::default()
        };
        if mode == "hls" {
            let started = self
                .start_hls(
                    &media.path,
                    &temporary_directory,
                    enhancement,
                    stream_start_position_seconds,
                    burn_subtitle.as_deref(),
                )
                .await
                .inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&temporary_directory);
                })?;
            process = Some(started.process);
            diagnostics.encoder = Some(started.encoder.to_owned());
            diagnostics.encoder_degraded = started.encoder_degraded;
            diagnostics.video_enhancement = started.actual_video_enhancement;
            diagnostics.frame_interpolation = started.actual_interpolation;
            diagnostics.interpolation_capacity = started.interpolation_capacity;
            diagnostics.model_backend = started.model_backend;
            diagnostics.enhanced_frame_input = diagnostics.video_enhancement
                != PlayerVideoEnhancement::Off
                || diagnostics.frame_interpolation != PlayerFrameInterpolation::Off;
            diagnostics.degradation_reason = started.degradation_reason;
        }
        let now = Utc::now();
        let direct_asset_name =
            utf8_percent_encode(&media.file_name, MEDIA_FILE_SEGMENT_ENCODE_SET).to_string();
        let public = RemotePlaybackSession {
            id: id.clone(),
            task_id: task.id.clone(),
            file_index: media.file_index,
            file_name: media.file_name,
            mode: mode.to_owned(),
            stream_url: if mode == "hls" {
                format!("{asset_base}/hls/index.m3u8")
            } else if access == SessionAccess::External {
                format!("{asset_base}/{direct_asset_name}")
            } else {
                format!("{asset_base}/file")
            },
            expires_at: (now
                + chrono::Duration::from_std(SESSION_TTL).unwrap_or(chrono::Duration::minutes(30)))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
            duration_seconds: media.duration_seconds,
            start_position_seconds,
            stream_start_position_seconds,
            subtitles,
            diagnostics,
        };
        let record = Arc::new(SessionRecord {
            public: Mutex::new(public.clone()),
            access,
            access_token,
            device_id: device_id.to_owned(),
            source_path: media.path,
            content_type: media.content_type,
            temporary_directory,
            process: Mutex::new(process),
            last_accessed_at_millis: Mutex::new(now.timestamp_millis()),
        });
        self.sessions.lock().await.insert(id.clone(), record);
        let service = Arc::clone(self);
        let expiration_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SESSION_TTL).await;
            service.close_if_expired(&expiration_id).await;
        });
        log::info!(
            "Rust 远程媒体会话已创建 session_id={id} task_id={} mode={mode} start_position={:?} stream_start={stream_start_position_seconds:.3}",
            task.id,
            start_position_seconds
        );
        Ok(public)
    }

    /// 接收设备拥有的直传会话所上报的 WebCodecs/WebGPU 运行快照。
    pub async fn report_direct_enhancement(
        &self,
        session_id: &str,
        device_id: &str,
        mut diagnostics: RemoteDirectEnhancementDiagnostics,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        validate_direct_enhancement_diagnostics(&diagnostics)?;
        let record = self
            .require_session(session_id, Some(device_id), None)
            .await?;
        if record.access != SessionAccess::Browser {
            return Err(session_not_found());
        }
        let mut public = record.public.lock().await;
        if public.mode != "direct" {
            return Err(RemoteMediaError::new(
                409,
                "MEDIA_DIRECT_DIAGNOSTICS_INVALID_MODE",
                "只有原文件直传会话可以上报终端增强诊断",
            ));
        }
        if public
            .diagnostics
            .direct_enhancement
            .as_ref()
            .is_some_and(|current| current.sequence >= diagnostics.sequence)
        {
            return Ok(public.clone());
        }
        diagnostics.reported_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
        public.diagnostics.playback_path =
            if diagnostics.status == RemoteDirectEnhancementStatus::Active {
                RemotePlaybackPath::DirectEnhanced
            } else {
                RemotePlaybackPath::Direct
            };
        public.diagnostics.direct_enhancement = Some(diagnostics);
        Ok(public.clone())
    }

    /// 返回设备拥有会话中的媒体资源。
    pub async fn get_asset(
        &self,
        session_id: &str,
        device_id: &str,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let record = self
            .require_session(session_id, Some(device_id), None)
            .await?;
        self.resolve_asset(&record, asset_name).await
    }

    /// 返回设备拥有会话的最新实际增强诊断。
    pub async fn get_session(
        &self,
        session_id: &str,
        device_id: &str,
    ) -> Result<RemotePlaybackSession, RemoteMediaError> {
        let record = self
            .require_session(session_id, Some(device_id), None)
            .await?;
        let mut public = record.public.lock().await.clone();
        let pipeline_state = record
            .process
            .lock()
            .await
            .as_ref()
            .and_then(|process| process.pipeline_state.clone());
        if let Some(state) = pipeline_state {
            let state = state.lock().await;
            public.diagnostics.video_enhancement = state.actual_video_enhancement;
            public.diagnostics.frame_interpolation = state.actual_interpolation;
            public.diagnostics.interpolation_capacity = state.interpolation_capacity.clone();
            public.diagnostics.model_backend = state.model_backend();
            public.diagnostics.enhanced_frame_input = state.actual_video_enhancement
                != PlayerVideoEnhancement::Off
                || state.actual_interpolation != PlayerFrameInterpolation::Off;
            public.diagnostics.degradation_reason = join_degradation_reasons(
                public.diagnostics.degradation_reason.take(),
                state.degradation_reason().into_iter().collect(),
            );
        }
        Ok(public)
    }

    /// 使用外部会话专属票据返回媒体资源。
    pub async fn get_external_asset(
        &self,
        session_id: &str,
        access_token: &str,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let record = self
            .require_session(session_id, None, Some(access_token))
            .await?;
        self.resolve_asset(&record, asset_name).await
    }

    async fn require_session(
        &self,
        session_id: &str,
        device_id: Option<&str>,
        access_token: Option<&str>,
    ) -> Result<Arc<SessionRecord>, RemoteMediaError> {
        let record = self
            .sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(session_not_found)?;
        if device_id.is_some_and(|device| record.device_id != device)
            || access_token.is_some_and(|token| {
                record.access != SessionAccess::External
                    || !secure_token_equals(record.access_token.as_deref(), token)
            })
        {
            return Err(session_not_found());
        }
        self.refresh_expiration(&record).await;
        Ok(record)
    }

    async fn resolve_asset(
        &self,
        record: &SessionRecord,
        asset_name: &str,
    ) -> Result<RemoteMediaAsset, RemoteMediaError> {
        let public = record.public.lock().await;
        if let Some(subtitle) = public
            .subtitles
            .iter()
            .find(|subtitle| subtitle.url.ends_with(&format!("/subtitles/{asset_name}")))
        {
            if !is_subtitle_asset(asset_name) {
                return Err(asset_not_found());
            }
            let path = canonical_asset(&record.temporary_directory, asset_name).await?;
            return Ok(RemoteMediaAsset {
                file_path: path,
                content_type: if subtitle.subtitle_type == "ass" {
                    "text/x-ssa; charset=utf-8"
                } else {
                    "text/vtt; charset=utf-8"
                }
                .to_owned(),
                direct: false,
                file_name: None,
            });
        }
        if public.mode == "direct" {
            if asset_name != "file" && asset_name != public.file_name {
                return Err(asset_not_found());
            }
            return Ok(RemoteMediaAsset {
                file_path: record.source_path.clone(),
                content_type: record.content_type.clone(),
                direct: true,
                file_name: Some(public.file_name.clone()),
            });
        }
        if !is_hls_asset(asset_name) {
            return Err(asset_not_found());
        }
        Ok(RemoteMediaAsset {
            file_path: canonical_asset(&record.temporary_directory, asset_name).await?,
            content_type: if asset_name.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            }
            .to_owned(),
            direct: false,
            file_name: None,
        })
    }

    /// 关闭设备拥有的会话并回收 FFmpeg 与临时资源。
    pub async fn close_session(&self, session_id: &str, device_id: &str) -> bool {
        let record = self.sessions.lock().await.get(session_id).cloned();
        if record
            .as_ref()
            .is_none_or(|record| record.device_id != device_id)
        {
            return false;
        }
        self.remove_session(session_id).await;
        true
    }

    /// 关闭全部会话，供网关停止和应用退出时回收资源。
    pub async fn stop_all(&self) {
        let ids = self
            .sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.remove_session(&id).await;
        }
    }

    /// 关闭指定设备拥有的全部会话，供吊销令牌时立即回收媒体资源。
    pub async fn close_device_sessions(&self, device_id: &str) -> usize {
        let snapshot = self.sessions.lock().await.clone();
        let ids = snapshot
            .into_iter()
            .filter_map(|(id, record)| (record.device_id == device_id).then_some(id))
            .collect::<Vec<_>>();
        for id in &ids {
            self.remove_session(id).await;
        }
        ids.len()
    }

    async fn remove_session(&self, session_id: &str) {
        let record = self.sessions.lock().await.remove(session_id);
        let Some(record) = record else {
            return;
        };
        if let Some(process) = record.process.lock().await.take() {
            process.stop().await;
        }
        if let Err(error) = tokio::fs::remove_dir_all(&record.temporary_directory).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("清理远程媒体缓存失败 session_id={session_id} error={error}");
            }
        }
        log::info!("Rust 远程媒体会话已关闭 session_id={session_id}");
    }

    async fn close_matching(&self, device_id: &str, task_id: &str, access: SessionAccess) {
        let snapshot = self.sessions.lock().await.clone();
        for (id, record) in snapshot {
            if record.device_id == device_id
                && record.access == access
                && record.public.lock().await.task_id == task_id
            {
                self.remove_session(&id).await;
            }
        }
    }

    async fn reserve_slot(&self) {
        let snapshot = self.sessions.lock().await.clone();
        if snapshot.len() < MAX_SESSIONS {
            return;
        }
        let mut oldest = None;
        for (id, record) in snapshot {
            let last_accessed = *record.last_accessed_at_millis.lock().await;
            if oldest
                .as_ref()
                .is_none_or(|(_, timestamp)| last_accessed < *timestamp)
            {
                oldest = Some((id, last_accessed));
            }
        }
        if let Some((id, _)) = oldest {
            self.remove_session(&id).await;
        }
    }

    async fn cleanup_expired(&self) {
        let now = Utc::now().timestamp_millis();
        let snapshot = self.sessions.lock().await.clone();
        for (id, record) in snapshot {
            let public = record.public.lock().await;
            let expired = chrono::DateTime::parse_from_rfc3339(&public.expires_at)
                .map(|value| value.timestamp_millis() <= now)
                .unwrap_or(true);
            drop(public);
            if expired {
                self.remove_session(&id).await;
            }
        }
    }

    async fn close_if_expired(&self, session_id: &str) {
        let record = self.sessions.lock().await.get(session_id).cloned();
        let Some(record) = record else {
            return;
        };
        let expires_at = record.public.lock().await.expires_at.clone();
        if chrono::DateTime::parse_from_rfc3339(&expires_at)
            .map(|value| value <= Utc::now())
            .unwrap_or(true)
        {
            self.remove_session(session_id).await;
        }
    }

    async fn refresh_expiration(&self, record: &SessionRecord) {
        let now = Utc::now();
        *record.last_accessed_at_millis.lock().await = now.timestamp_millis();
        record.public.lock().await.expires_at = (now
            + chrono::Duration::from_std(SESSION_TTL).unwrap_or(chrono::Duration::minutes(30)))
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    }

    async fn resolve_media(
        &self,
        task: &DownloadTask,
        requested_file_index: Option<i64>,
    ) -> Result<ResolvedMedia, RemoteMediaError> {
        let settings = self
            .repository
            .get_settings()
            .await
            .map_err(internal_media_error)?;
        let extensions = video_extensions(&settings);
        let media_files = self
            .repository
            .list_media_files()
            .await
            .map_err(internal_media_error)?;
        let mut task_files = task
            .files
            .iter()
            .filter(|file| {
                file.selected && file.progress >= 1.0 && extensions.contains(&extension(&file.name))
            })
            .collect::<Vec<_>>();
        task_files.sort_by_key(|file| Reverse(file.size));
        if requested_file_index
            .is_some_and(|index| !task_files.iter().any(|file| file.index == index))
        {
            return Err(RemoteMediaError::new(
                409,
                "MEDIA_FILE_UNAVAILABLE",
                "指定媒体文件不存在或尚未写入完成",
            ));
        }
        let mut candidates = Vec::new();
        if let Some(index) = requested_file_index {
            let file = task_files
                .iter()
                .find(|file| file.index == index)
                .expect("file index checked");
            candidates.push((
                task_file_path(task, &file.name),
                Path::new(&file.name)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                Some(file.index),
                None,
            ));
        } else {
            let mut registered = media_files
                .into_iter()
                .filter(|media| media.download_task_id.as_deref() == Some(&task.id))
                .collect::<Vec<_>>();
            registered.sort_by_key(|media| Reverse(media.size));
            candidates.extend(registered.into_iter().map(|media| {
                (
                    PathBuf::from(&media.file_path),
                    media.file_name.clone(),
                    task.files.iter().find_map(|file| {
                        (task_file_path(task, &file.name).as_path() == Path::new(&media.file_path))
                            .then_some(file.index)
                    }),
                    Some(media),
                )
            }));
            candidates.extend(task_files.into_iter().map(|file| {
                (
                    task_file_path(task, &file.name),
                    Path::new(&file.name)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    Some(file.index),
                    None,
                )
            }));
        }

        for (path, file_name, file_index, media) in candidates {
            let Some(path) = validate_media_path(Path::new(&task.save_path), &path).await else {
                continue;
            };
            if !extensions.contains(&extension(&path.to_string_lossy())) {
                continue;
            }
            let duration_seconds = self.probe_duration(&path).await.or_else(|| {
                let fallback = media.as_ref().and_then(|media| media.duration_seconds);
                if fallback.is_some() {
                    log::warn!("FFprobe 未返回媒体总时长，回退数据库媒体时长");
                }
                fallback
            });
            return Ok(ResolvedMedia {
                content_type: direct_content_type(&path),
                path,
                file_name,
                file_index,
                duration_seconds,
            });
        }
        Err(RemoteMediaError::new(
            409,
            "MEDIA_FILE_UNAVAILABLE",
            "已完成的媒体文件不存在或尚未写入完成",
        ))
    }

    async fn probe_duration(&self, path: &Path) -> Option<i64> {
        for command_path in &self.tools.ffprobe_paths {
            let output = run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=noprint_wrappers=1:nokey=1",
                ],
                Some(path),
                self.tools.timeout,
            )
            .await;
            if let Ok(output) = output {
                if let Some(duration) = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                {
                    return Some(duration.round() as i64);
                }
            }
        }
        None
    }

    async fn prepare_subtitles(
        &self,
        source_path: &Path,
        output_directory: &Path,
        asset_base: &str,
        stream_start_position_seconds: f64,
    ) -> Vec<PreparedSubtitle> {
        let streams = match self.probe_subtitles(source_path).await {
            Ok(streams) => streams,
            Err(error) => {
                log::warn!("远程字幕探测失败 error={error}");
                return Vec::new();
            }
        };
        let supported = streams
            .into_iter()
            .filter_map(SupportedSubtitle::from_stream)
            .collect::<Vec<_>>();
        let mut subtitles = Vec::new();
        for (order, stream) in supported.into_iter().enumerate() {
            let asset_name = format!("subtitle-{order:03}.{}", stream.output_type);
            let output_path = output_directory.join(&asset_name);
            let mut command = hidden_command(&self.tools.ffmpeg_path);
            command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
            append_input_seek_args(&mut command, stream_start_position_seconds);
            command
                .arg("-i")
                .arg(source_path)
                .args([
                    "-map",
                    &format!("0:{}", stream.index),
                    "-c:s",
                    if stream.output_type == "ass" {
                        "ass"
                    } else {
                        "webvtt"
                    },
                    "-y",
                ])
                .arg(&output_path)
                .kill_on_drop(true);
            let completed = tokio::time::timeout(self.tools.timeout, command.output()).await;
            let valid = matches!(completed, Ok(Ok(ref output)) if output.status.success())
                && tokio::fs::metadata(&output_path)
                    .await
                    .is_ok_and(|metadata| metadata.len() > 0);
            if valid {
                let language = normalize_language(stream.tags.get("language").map(String::as_str));
                let title = stream.tags.get("title").map(String::as_str);
                subtitles.push(PreparedSubtitle {
                    public: RemotePlaybackSubtitle {
                        id: format!("subtitle-{}", stream.index),
                        label: subtitle_label(title, language.as_deref(), order),
                        language,
                        subtitle_type: stream.output_type.to_owned(),
                        url: format!("{asset_base}/subtitles/{asset_name}"),
                        default: stream.disposition.default == 1,
                    },
                    output_path,
                });
            }
        }
        subtitles
    }

    async fn probe_subtitles(&self, source_path: &Path) -> Result<Vec<SubtitleStream>, String> {
        let mut last_error = "没有可用 FFprobe".to_owned();
        for command_path in &self.tools.ffprobe_paths {
            match run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-print_format",
                    "json",
                    "-show_streams",
                    "-select_streams",
                    "s",
                ],
                Some(source_path),
                self.tools.timeout,
            )
            .await
            {
                Ok(output) => {
                    let parsed: SubtitleProbeOutput = serde_json::from_slice(&output.stdout)
                        .map_err(|error| format!("FFprobe 字幕 JSON 无效：{error}"))?;
                    return Ok(parsed.streams);
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    async fn start_hls(
        &self,
        source_path: &Path,
        output_directory: &Path,
        enhancement: RemotePlaybackEnhancement,
        stream_start_position_seconds: f64,
        burn_subtitle: Option<&Path>,
    ) -> Result<HlsStart, RemoteMediaError> {
        let wants_rife = enhancement.frame_interpolation == PlayerFrameInterpolation::RifeRealtime;
        let wants_realesrgan = enhancement.video_enhancement == PlayerVideoEnhancement::Clear;
        if wants_rife || wants_realesrgan {
            let mut degradation_reasons = Vec::new();
            let mut rife = if wants_rife {
                match self.ensure_rife_runtime().await {
                    Ok(runtime) => Some(runtime),
                    Err(reason) => {
                        log::warn!("RIFE sidecar 不可用，关闭模型插帧 reason={reason}");
                        degradation_reasons.push(reason);
                        None
                    }
                }
            } else {
                None
            };
            let realesrgan = if wants_realesrgan {
                match self.ensure_realesrgan_runtime().await {
                    Ok(runtime) => Some(runtime),
                    Err(reason) => {
                        log::warn!(
                            "Real-ESRGAN sidecar 不可用，回退 FFmpeg 清晰滤镜 reason={reason}"
                        );
                        degradation_reasons.push(reason);
                        None
                    }
                }
            } else {
                None
            };
            let mut video_probe = None;
            let mut interpolation_capacity = None;
            if let Some(rife_runtime) = rife.as_ref() {
                match self.probe_video(source_path).await {
                    Ok(video) => {
                        let (plan, capacity) = model_interpolation_plan(
                            video.frame_rate,
                            rife_runtime,
                            realesrgan.as_ref(),
                            self.tools.model_available_vram_bytes,
                        )
                        .await;
                        video_probe = Some(video);
                        interpolation_capacity = Some(capacity);
                        if plan.selected_multiplier < RIFE_HARD_MAX_MULTIPLIER {
                            let reason = plan.rejection_reason.unwrap_or_else(|| {
                                "当前处理链无法满足 2x AI 插帧预算，已保持源帧率".to_owned()
                            });
                            log::warn!("RIFE 容量门禁关闭模型插帧 reason={reason}");
                            degradation_reasons.push(reason);
                            rife = None;
                        }
                    }
                    Err(reason) => {
                        let reason = format!("无法探测源帧率，已关闭 RIFE 插帧：{reason}");
                        log::warn!("{reason}");
                        degradation_reasons.push(reason);
                        rife = None;
                    }
                }
            }
            if rife.is_some() || realesrgan.is_some() {
                match self
                    .start_model_hls(
                        source_path,
                        output_directory,
                        ModelHlsConfig {
                            enhancement: RemotePlaybackEnhancement {
                                video_enhancement: enhancement.video_enhancement,
                                frame_interpolation: if rife.is_some() {
                                    PlayerFrameInterpolation::RifeRealtime
                                } else {
                                    PlayerFrameInterpolation::Off
                                },
                            },
                            rife,
                            realesrgan,
                            video_probe,
                            interpolation_capacity: interpolation_capacity.clone(),
                        },
                        stream_start_position_seconds,
                        burn_subtitle,
                    )
                    .await
                {
                    Ok(mut started) => {
                        started.degradation_reason = join_degradation_reasons(
                            started.degradation_reason.take(),
                            degradation_reasons,
                        );
                        return Ok(started);
                    }
                    Err(error) => {
                        log::warn!("模型远程管线启动失败，回退 FFmpeg error={}", error.message);
                        degradation_reasons.push(error.message);
                    }
                }
            }
            return self
                .start_hls_ffmpeg(
                    source_path,
                    output_directory,
                    RemotePlaybackEnhancement {
                        frame_interpolation: if wants_rife {
                            PlayerFrameInterpolation::Off
                        } else {
                            enhancement.frame_interpolation
                        },
                        ..enhancement
                    },
                    stream_start_position_seconds,
                    burn_subtitle,
                )
                .await
                .map(|mut started| {
                    started.interpolation_capacity = interpolation_capacity;
                    started.degradation_reason = join_degradation_reasons(
                        started.degradation_reason.take(),
                        degradation_reasons,
                    );
                    started.encoder_degraded = true;
                    started
                });
        }
        self.start_hls_ffmpeg(
            source_path,
            output_directory,
            enhancement,
            stream_start_position_seconds,
            burn_subtitle,
        )
        .await
    }

    async fn ensure_rife_runtime(&self) -> Result<Arc<ModelSidecarRuntime>, String> {
        let mut guard = self.rife_runtime.lock().await;
        if let Some(runtime) = guard.as_ref() {
            if FrameInterpolator::ready(runtime.as_ref()) {
                return Ok(Arc::clone(runtime));
            }
            *guard = None;
        }
        let root = self
            .tools
            .rife_sidecar_root
            .clone()
            .ok_or_else(|| "未找到 RIFE sidecar 资源".to_owned())?;
        let config = ModelSidecarConfig::new(root, self.tools.model_available_vram_bytes, 33.0);
        let runtime = Arc::new(ModelSidecarRuntime::launch(config).await?);
        *guard = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    async fn ensure_realesrgan_runtime(&self) -> Result<Arc<ModelSidecarRuntime>, String> {
        let mut guard = self.realesrgan_runtime.lock().await;
        if let Some(runtime) = guard.as_ref() {
            if ModelEnhancer::ready(runtime.as_ref()) {
                return Ok(Arc::clone(runtime));
            }
            *guard = None;
        }
        let root = self
            .tools
            .realesrgan_sidecar_root
            .clone()
            .ok_or_else(|| "未找到 Real-ESRGAN sidecar 资源".to_owned())?;
        let config = ModelSidecarConfig::new(root, self.tools.model_available_vram_bytes, 33.0);
        let runtime = Arc::new(ModelSidecarRuntime::launch(config).await?);
        if runtime.output_scale() != 2 || !ModelEnhancer::ready(runtime.as_ref()) {
            return Err("Real-ESRGAN sidecar 未声明可用的 2x 单帧增强".to_owned());
        }
        *guard = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    /// 探测当前 FFmpeg 真正可启动的编码器，并缓存本机稳定候选顺序。
    async fn available_encoder_candidates(&self) -> Vec<(&'static str, &'static str)> {
        if let Some(candidates) = self.encoder_candidates.lock().await.as_ref() {
            return candidates.clone();
        }
        let mut candidates = Vec::new();
        for &(codec, encoder) in ENCODER_CANDIDATES {
            match probe_video_encoder(&self.tools.ffmpeg_path, codec).await {
                Ok(()) => {
                    log::info!("远程视频编码器探测通过 encoder={encoder} codec={codec}");
                    candidates.push((codec, encoder));
                }
                Err(reason) => {
                    log::warn!(
                        "远程视频编码器探测失败 encoder={encoder} codec={codec} reason={reason}"
                    );
                }
            }
        }
        if candidates.is_empty() {
            candidates.push(("libx264", "libx264"));
        }
        *self.encoder_candidates.lock().await = Some(candidates.clone());
        candidates
    }

    /// 烧录字幕依赖 FFmpeg `subtitles`/libass 滤镜，缺失时在会话创建前明确拒绝。
    async fn ensure_subtitle_burn_supported(&self) -> Result<(), RemoteMediaError> {
        if let Some(result) = self.subtitle_burn_support.lock().await.as_ref() {
            return result.clone().map_err(|reason| {
                RemoteMediaError::new(503, "MEDIA_SUBTITLE_BURN_UNAVAILABLE", reason)
            });
        }
        let result = run_command(
            &self.tools.ffmpeg_path,
            &["-hide_banner", "-filters"],
            None,
            ENCODER_PROBE_TIMEOUT,
        )
        .await
        .and_then(|output| {
            if ffmpeg_filter_available(&output.stdout, "subtitles")
                || ffmpeg_filter_available(&output.stderr, "subtitles")
            {
                Ok(())
            } else {
                Err("当前 FFmpeg 未启用 libass subtitles 滤镜，无法烧录字幕".to_owned())
            }
        });
        *self.subtitle_burn_support.lock().await = Some(result.clone());
        result
            .map_err(|reason| RemoteMediaError::new(503, "MEDIA_SUBTITLE_BURN_UNAVAILABLE", reason))
    }

    async fn start_hls_ffmpeg(
        &self,
        source_path: &Path,
        output_directory: &Path,
        enhancement: RemotePlaybackEnhancement,
        stream_start_position_seconds: f64,
        burn_subtitle: Option<&Path>,
    ) -> Result<HlsStart, RemoteMediaError> {
        let playlist = output_directory.join("index.m3u8");
        let segments = output_directory.join("segment-%06d.ts");
        let mut last_error = None;
        let candidates = self.available_encoder_candidates().await;
        for (profile_index, profile) in hls_enhancement_profiles(enhancement)
            .into_iter()
            .enumerate()
        {
            for &(codec, encoder) in &candidates {
                let _ = tokio::fs::remove_file(&playlist).await;
                let mut command = hidden_command(&self.tools.ffmpeg_path);
                command.args(["-nostdin", "-hide_banner", "-loglevel", "warning"]);
                append_input_seek_args(&mut command, stream_start_position_seconds);
                command
                    .arg("-i")
                    .arg(source_path)
                    .args(["-map", "0:v:0", "-map", "0:a:0?", "-c:v"])
                    .args(encoder_video_args(codec))
                    .args(remote_video_filter_args(profile.enhancement, burn_subtitle))
                    .args([
                        "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k", "-ac", "2",
                    ])
                    .stderr(Stdio::piped());
                append_hls_output_args(&mut command, &segments, &playlist);
                command.kill_on_drop(true);
                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        last_error = Some(error.to_string());
                        continue;
                    }
                };
                let stderr = child
                    .stderr
                    .take()
                    .map(|stderr| capture_ffmpeg_stderr("encoder", stderr));
                let started = tokio::time::Instant::now();
                loop {
                    if hls_playlist_ready(&playlist).await {
                        return Ok(HlsStart {
                            process: MediaProcess {
                                encoder: child,
                                decoder: None,
                                pipeline: None,
                                pipeline_state: None,
                                stderr_captures: stderr.into_iter().collect(),
                            },
                            encoder,
                            encoder_degraded: profile_index > 0 || encoder_is_degraded(encoder),
                            actual_video_enhancement: profile.enhancement.video_enhancement,
                            actual_interpolation: profile.enhancement.frame_interpolation,
                            interpolation_capacity: None,
                            model_backend: None,
                            degradation_reason: profile.degradation_reason.map(str::to_owned),
                        });
                    }
                    if let Ok(Some(status)) = child.try_wait() {
                        let detail = stderr
                            .as_ref()
                            .map(FfmpegStderrCapture::snapshot)
                            .unwrap_or_default();
                        last_error = Some(encoder_failure_message(
                            encoder,
                            &format!("提前退出：{status}"),
                            &detail,
                        ));
                        break;
                    }
                    if started.elapsed() >= TRANSCODER_START_TIMEOUT {
                        let _ = child.kill().await;
                        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                        let detail = stderr
                            .as_ref()
                            .map(FfmpegStderrCapture::snapshot)
                            .unwrap_or_default();
                        last_error = Some(encoder_failure_message(encoder, "启动超时", &detail));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if let Some(stderr) = stderr {
                    stderr.finish().await;
                }
                if profile.enhancement.frame_interpolation
                    == PlayerFrameInterpolation::MotionCompensated
                    && last_error
                        .as_deref()
                        .is_some_and(|message| message.contains("启动超时"))
                {
                    break;
                }
            }
        }
        Err(RemoteMediaError::new(
            503,
            "TRANSCODER_UNAVAILABLE",
            last_error.unwrap_or_else(|| "没有可用的 HLS 编码器".to_owned()),
        ))
    }

    async fn start_model_hls(
        &self,
        source_path: &Path,
        output_directory: &Path,
        config: ModelHlsConfig,
        stream_start_position_seconds: f64,
        burn_subtitle: Option<&Path>,
    ) -> Result<HlsStart, RemoteMediaError> {
        let ModelHlsConfig {
            enhancement,
            rife,
            realesrgan,
            video_probe,
            interpolation_capacity,
        } = config;
        let video = match video_probe {
            Some(video) => video,
            None => self
                .probe_video(source_path)
                .await
                .map_err(|error| RemoteMediaError::new(503, "MODEL_VIDEO_PROBE_FAILED", error))?,
        };
        let playlist = output_directory.join("index.m3u8");
        let segments = output_directory.join("segment-%06d.ts");
        let output_fps = if rife.is_some() {
            (video.frame_rate * 2.0).clamp(1.0, 240.0)
        } else {
            video.frame_rate
        };
        let output_scale = realesrgan
            .as_ref()
            .map_or(1, |runtime| runtime.output_scale());
        let model_enhancement_active = realesrgan.is_some();
        let output_width = video.width.checked_mul(output_scale).ok_or_else(|| {
            RemoteMediaError::new(503, "MODEL_OUTPUT_SIZE_INVALID", "模型输出宽度溢出")
        })?;
        let output_height = video.height.checked_mul(output_scale).ok_or_else(|| {
            RemoteMediaError::new(503, "MODEL_OUTPUT_SIZE_INVALID", "模型输出高度溢出")
        })?;
        let dimensions = format!("{output_width}x{output_height}");
        let candidates = self.available_encoder_candidates().await;
        if candidates.is_empty() {
            return Err(RemoteMediaError::new(
                503,
                "MODEL_ENCODER_UNAVAILABLE",
                "没有可用的视频编码器",
            ));
        }
        let mut last_error = None;
        for &(encoder_codec, encoder_name) in &candidates {
            clear_hls_output(output_directory, &playlist).await;
            match self
                .start_model_hls_with_encoder(
                    source_path,
                    &playlist,
                    &segments,
                    enhancement,
                    rife.clone(),
                    realesrgan.clone(),
                    video,
                    interpolation_capacity.clone(),
                    stream_start_position_seconds,
                    output_fps,
                    model_enhancement_active,
                    &dimensions,
                    encoder_codec,
                    encoder_name,
                    burn_subtitle,
                )
                .await
            {
                Ok(started) => return Ok(started),
                Err(error) => {
                    let message =
                        model_encoder_attempt_failure_message(encoder_name, &error.message);
                    log::warn!("{message} code={}", error.code);
                    last_error = Some(message);
                }
            }
        }
        Err(RemoteMediaError::new(
            503,
            "MODEL_ENCODER_UNAVAILABLE",
            last_error.unwrap_or_else(|| "没有可用的视频编码器".to_owned()),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_model_hls_with_encoder(
        &self,
        source_path: &Path,
        playlist: &Path,
        segments: &Path,
        enhancement: RemotePlaybackEnhancement,
        rife: Option<Arc<ModelSidecarRuntime>>,
        realesrgan: Option<Arc<ModelSidecarRuntime>>,
        video: VideoProbe,
        interpolation_capacity: Option<RemoteInterpolationCapacity>,
        stream_start_position_seconds: f64,
        output_fps: f64,
        model_enhancement_active: bool,
        dimensions: &str,
        encoder_codec: &str,
        encoder_name: &'static str,
        burn_subtitle: Option<&Path>,
    ) -> Result<HlsStart, RemoteMediaError> {
        let mut decoder = hidden_command(&self.tools.ffmpeg_path);
        decoder.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
        append_input_seek_args(&mut decoder, stream_start_position_seconds);
        decoder
            .arg("-i")
            .arg(source_path)
            .args([
                "-map", "0:v:0", "-an", "-vsync", "0", "-f", "rawvideo", "-pix_fmt", "rgb24",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut decoder = decoder.spawn().map_err(|error| {
            RemoteMediaError::new(503, "MODEL_DECODER_UNAVAILABLE", error.to_string())
        })?;
        let decoder_stderr = decoder
            .stderr
            .take()
            .map(|stderr| capture_ffmpeg_stderr("model-decoder", stderr));
        let decoder_stdout = match decoder.stdout.take() {
            Some(stdout) => stdout,
            None => {
                stop_model_startup_process(
                    decoder,
                    None,
                    None,
                    decoder_stderr.into_iter().collect(),
                )
                .await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_DECODER_OUTPUT_UNAVAILABLE",
                    "FFmpeg RGB 输出不可用",
                ));
            }
        };

        let mut encoder = hidden_command(&self.tools.ffmpeg_path);
        let fps_arg = format!("{output_fps:.6}");
        encoder.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "warning",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            dimensions,
            "-r",
            &fps_arg,
            "-i",
            "pipe:0",
        ]);
        append_input_seek_args(&mut encoder, stream_start_position_seconds);
        encoder
            .arg("-i")
            .arg(source_path)
            .args(["-map", "0:v:0", "-map", "1:a:0?", "-c:v"])
            .args(encoder_video_args(encoder_codec))
            .args(remote_video_filter_args(
                RemotePlaybackEnhancement {
                    video_enhancement: if model_enhancement_active {
                        PlayerVideoEnhancement::Off
                    } else {
                        enhancement.video_enhancement
                    },
                    frame_interpolation: PlayerFrameInterpolation::Off,
                },
                burn_subtitle,
            ))
            .args([
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-ac",
                "2",
                "-shortest",
            ])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped());
        append_hls_output_args(&mut encoder, segments, playlist);
        encoder.kill_on_drop(true);
        let mut encoder = match encoder.spawn() {
            Ok(encoder) => encoder,
            Err(error) => {
                stop_model_startup_process(
                    decoder,
                    None,
                    None,
                    decoder_stderr.into_iter().collect(),
                )
                .await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_ENCODER_UNAVAILABLE",
                    error.to_string(),
                ));
            }
        };
        let encoder_stderr = encoder
            .stderr
            .take()
            .map(|stderr| capture_ffmpeg_stderr("model-encoder", stderr));
        let encoder_stdin = match encoder.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let mut stderr_captures: Vec<_> = decoder_stderr.into_iter().collect();
                stderr_captures.extend(encoder_stderr);
                stop_model_startup_process(decoder, Some(encoder), None, stderr_captures).await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_ENCODER_INPUT_UNAVAILABLE",
                    "FFmpeg rawvideo 输入不可用",
                ));
            }
        };
        let pipeline_state = Arc::new(Mutex::new(ModelPipelineState {
            actual_video_enhancement: enhancement.video_enhancement,
            actual_interpolation: enhancement.frame_interpolation,
            interpolation_capacity: interpolation_capacity.clone(),
            rife_model_backend: rife
                .as_ref()
                .map(|runtime| FrameInterpolator::backend_id(runtime.as_ref()).to_owned()),
            realesrgan_model_backend: realesrgan
                .as_ref()
                .map(|runtime| ModelEnhancer::backend_id(runtime.as_ref()).to_owned()),
            degradation_reasons: Vec::new(),
        }));
        let model_backend = pipeline_state.lock().await.model_backend();
        let mut pipeline = tokio::spawn(run_model_pipeline(
            decoder_stdout,
            encoder_stdin,
            ModelPipelineConfig {
                rife,
                realesrgan,
                state: Arc::clone(&pipeline_state),
                width: video.width,
                height: video.height,
                frame_rate: video.frame_rate,
                available_vram_bytes: self.tools.model_available_vram_bytes,
            },
        ));
        let started = tokio::time::Instant::now();
        loop {
            if hls_playlist_ready(playlist).await {
                let mut stderr_captures = Vec::new();
                stderr_captures.extend(decoder_stderr);
                stderr_captures.extend(encoder_stderr);
                return Ok(HlsStart {
                    process: MediaProcess {
                        encoder,
                        decoder: Some(decoder),
                        pipeline: Some(pipeline),
                        pipeline_state: Some(pipeline_state),
                        stderr_captures,
                    },
                    encoder: encoder_name,
                    encoder_degraded: encoder_is_degraded(encoder_name),
                    actual_video_enhancement: enhancement.video_enhancement,
                    actual_interpolation: enhancement.frame_interpolation,
                    interpolation_capacity,
                    model_backend,
                    degradation_reason: None,
                });
            }
            if pipeline.is_finished() {
                let pipeline_error = match (&mut pipeline).await {
                    Ok(Ok(())) => "模型帧管线在首个 HLS 分片前结束".to_owned(),
                    Ok(Err(reason)) => reason,
                    Err(error) => format!("模型帧任务失败：{error}"),
                };
                let mut stderr_captures: Vec<_> = decoder_stderr.into_iter().collect();
                stderr_captures.extend(encoder_stderr);
                stop_model_startup_process(decoder, Some(encoder), None, stderr_captures).await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_PIPELINE_EXITED",
                    pipeline_error,
                ));
            }
            if let Ok(Some(status)) = encoder.try_wait() {
                let detail = encoder_stderr
                    .as_ref()
                    .map(FfmpegStderrCapture::snapshot)
                    .unwrap_or_default();
                let mut stderr_captures: Vec<_> = decoder_stderr.into_iter().collect();
                stderr_captures.extend(encoder_stderr);
                stop_model_startup_process(decoder, Some(encoder), Some(pipeline), stderr_captures)
                    .await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_ENCODER_EXITED",
                    encoder_failure_message(encoder_name, &format!("提前退出：{status}"), &detail),
                ));
            }
            if started.elapsed() >= TRANSCODER_START_TIMEOUT {
                let encoder_detail = encoder_stderr
                    .as_ref()
                    .map(FfmpegStderrCapture::snapshot)
                    .unwrap_or_default();
                let decoder_detail = decoder_stderr
                    .as_ref()
                    .map(FfmpegStderrCapture::snapshot)
                    .unwrap_or_default();
                let detail = [encoder_detail, decoder_detail]
                    .into_iter()
                    .find(|value| !value.is_empty())
                    .unwrap_or_default();
                let mut stderr_captures: Vec<_> = decoder_stderr.into_iter().collect();
                stderr_captures.extend(encoder_stderr);
                stop_model_startup_process(decoder, Some(encoder), Some(pipeline), stderr_captures)
                    .await;
                return Err(RemoteMediaError::new(
                    503,
                    "MODEL_PIPELINE_START_TIMEOUT",
                    encoder_failure_message(encoder_name, "模型远程管线启动超时", &detail),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn probe_video(&self, source_path: &Path) -> Result<VideoProbe, String> {
        let mut last_error = "没有可用 FFprobe".to_owned();
        for command_path in &self.tools.ffprobe_paths {
            match run_command(
                command_path,
                &[
                    "-v",
                    "quiet",
                    "-print_format",
                    "json",
                    "-show_entries",
                    "stream=width,height,avg_frame_rate",
                    "-select_streams",
                    "v:0",
                ],
                Some(source_path),
                self.tools.timeout,
            )
            .await
            {
                Ok(output) => {
                    let parsed: VideoProbeOutput = serde_json::from_slice(&output.stdout)
                        .map_err(|error| format!("FFprobe 视频 JSON 无效：{error}"))?;
                    let stream = parsed
                        .streams
                        .into_iter()
                        .next()
                        .ok_or_else(|| "媒体没有视频流".to_owned())?;
                    let width = stream.width.ok_or_else(|| "视频宽度缺失".to_owned())?;
                    let height = stream.height.ok_or_else(|| "视频高度缺失".to_owned())?;
                    let frame_rate =
                        parse_frame_rate(stream.avg_frame_rate.as_deref().unwrap_or("0/1"))?;
                    return Ok(VideoProbe {
                        width,
                        height,
                        frame_rate,
                    });
                }
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

#[derive(Debug, Clone, Copy)]
struct VideoProbe {
    width: u32,
    height: u32,
    frame_rate: f64,
}

#[derive(Clone, Copy)]
struct HlsEnhancementProfile {
    enhancement: RemotePlaybackEnhancement,
    degradation_reason: Option<&'static str>,
}

struct ModelHlsConfig {
    enhancement: RemotePlaybackEnhancement,
    rife: Option<Arc<ModelSidecarRuntime>>,
    realesrgan: Option<Arc<ModelSidecarRuntime>>,
    video_probe: Option<VideoProbe>,
    interpolation_capacity: Option<RemoteInterpolationCapacity>,
}

struct ModelPipelineConfig {
    rife: Option<Arc<ModelSidecarRuntime>>,
    realesrgan: Option<Arc<ModelSidecarRuntime>>,
    state: Arc<Mutex<ModelPipelineState>>,
    width: u32,
    height: u32,
    frame_rate: f64,
    available_vram_bytes: u64,
}

#[derive(Default, Deserialize)]
struct VideoProbeOutput {
    #[serde(default)]
    streams: Vec<VideoProbeStream>,
}

#[derive(Default, Deserialize)]
struct VideoProbeStream {
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
}

fn parse_frame_rate(value: &str) -> Result<f64, String> {
    let (numerator, denominator) = value
        .split_once('/')
        .ok_or_else(|| "视频帧率格式无效".to_owned())?;
    let numerator = numerator
        .parse::<f64>()
        .map_err(|_| "视频帧率无效".to_owned())?;
    let denominator = denominator
        .parse::<f64>()
        .map_err(|_| "视频帧率无效".to_owned())?;
    let frame_rate = numerator / denominator;
    if !frame_rate.is_finite() || frame_rate <= 0.0 || frame_rate > 120.0 {
        return Err("视频帧率超出 RIFE 管线限制".to_owned());
    }
    Ok(frame_rate)
}

async fn run_model_pipeline(
    mut decoder_stdout: ChildStdout,
    mut encoder_stdin: ChildStdin,
    config: ModelPipelineConfig,
) -> Result<(), String> {
    let ModelPipelineConfig {
        mut rife,
        mut realesrgan,
        state: pipeline_state,
        width,
        height,
        frame_rate,
        available_vram_bytes,
    } = config;
    // 即使 RIFE 运行中降级，编码器仍保持已启动的双倍帧率，后续帧必须重复补齐时间轴。
    let maintain_doubled_cadence = rife.is_some();
    let fallback_scale = realesrgan
        .as_ref()
        .map_or(1, |runtime| runtime.output_scale());
    let frame_size = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(3))
        .and_then(|stride| {
            usize::try_from(height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or_else(|| "模型视频帧大小溢出".to_owned())?;
    let frame_interval = (1_000_000.0 / frame_rate).round() as i64;
    let (sender, mut receiver) = mpsc::channel::<RawVideoFrame>(RIFE_FRAME_QUEUE_CAPACITY);
    let reader = tokio::spawn(async move {
        let mut index = 0_i64;
        loop {
            let mut data = vec![0_u8; frame_size];
            match decoder_stdout.read_exact(&mut data).await {
                Ok(_) => {
                    let frame = RawVideoFrame {
                        width,
                        height,
                        stride: width.saturating_mul(3),
                        pts_micros: index.saturating_mul(frame_interval),
                        data,
                    };
                    if sender.send(frame).await.is_err() {
                        return Ok::<(), String>(());
                    }
                    index = index.saturating_add(1);
                }
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(format!("读取 FFmpeg RGB 帧失败：{error}")),
            }
        }
    });
    let Some(mut previous) = receiver.recv().await else {
        let _ = reader.await;
        return Err("FFmpeg 没有输出视频帧".to_owned());
    };
    while let Some(next) = receiver.recv().await {
        let original = apply_model_enhancement(
            &mut realesrgan,
            previous.clone(),
            fallback_scale,
            &pipeline_state,
        )
        .await;
        encoder_stdin
            .write_all(&original.data)
            .await
            .map_err(|error| format!("写入模型管线原始帧失败：{error}"))?;
        if maintain_doubled_cadence {
            if rife.is_some()
                && !refresh_interpolation_capacity(
                    frame_rate,
                    rife.as_ref(),
                    realesrgan.as_ref(),
                    available_vram_bytes,
                    &pipeline_state,
                )
                .await
            {
                rife = None;
            }
            let middle = if let Some(runtime) = rife.clone() {
                match runtime.interpolate(previous.clone(), next.clone()).await {
                    Ok(middle) => {
                        let middle = apply_model_enhancement(
                            &mut realesrgan,
                            middle,
                            fallback_scale,
                            &pipeline_state,
                        )
                        .await;
                        if !refresh_interpolation_capacity(
                            frame_rate,
                            rife.as_ref(),
                            realesrgan.as_ref(),
                            available_vram_bytes,
                            &pipeline_state,
                        )
                        .await
                        {
                            rife = None;
                        }
                        Some(middle)
                    }
                    Err(reason) => {
                        log::warn!("RIFE 运行中降级，继续双倍帧率原始时间轴 reason={reason}");
                        rife = None;
                        let mut state = pipeline_state.lock().await;
                        state.actual_interpolation = PlayerFrameInterpolation::Off;
                        state.rife_model_backend = None;
                        if let Some(capacity) = state.interpolation_capacity.as_mut() {
                            capacity.target_frame_rate = capacity.source_frame_rate;
                            capacity.selected_multiplier = 1;
                            capacity.max_feasible_multiplier = 1;
                        }
                        state.degradation_reasons.push(reason);
                        None
                    }
                }
            } else {
                None
            };
            encoder_stdin
                .write_all(middle.as_ref().map_or(&original.data, |frame| &frame.data))
                .await
                .map_err(|error| format!("写入模型管线补间帧失败：{error}"))?;
        }
        previous = next;
    }
    let last =
        apply_model_enhancement(&mut realesrgan, previous, fallback_scale, &pipeline_state).await;
    encoder_stdin
        .write_all(&last.data)
        .await
        .map_err(|error| format!("写入模型管线最后一帧失败：{error}"))?;
    if maintain_doubled_cadence {
        encoder_stdin
            .write_all(&last.data)
            .await
            .map_err(|error| format!("写入模型管线尾部重复帧失败：{error}"))?;
    }
    encoder_stdin
        .shutdown()
        .await
        .map_err(|error| format!("关闭模型管线编码输入失败：{error}"))?;
    reader
        .await
        .map_err(|error| format!("读取模型帧任务失败：{error}"))??;
    Ok(())
}

fn model_backend_name(
    rife_backend: Option<&str>,
    realesrgan_backend: Option<&str>,
) -> Option<String> {
    match (rife_backend, realesrgan_backend) {
        (Some(rife), Some(realesrgan)) => Some(format!("rife:{rife}+realesrgan:{realesrgan}")),
        (Some(rife), None) => Some(format!("rife:{rife}")),
        (None, Some(realesrgan)) => Some(format!("realesrgan:{realesrgan}")),
        (None, None) => None,
    }
}

async fn apply_model_enhancement(
    runtime: &mut Option<Arc<ModelSidecarRuntime>>,
    frame: RawVideoFrame,
    fallback_scale: u32,
    pipeline_state: &Arc<Mutex<ModelPipelineState>>,
) -> RawVideoFrame {
    let Some(enhancer) = runtime.as_ref() else {
        return if fallback_scale == 1 {
            frame
        } else {
            upscale_rgb24_nearest(frame, fallback_scale).unwrap_or_else(|(frame, error)| {
                log::error!("Real-ESRGAN 降级帧缩放失败 error={error}");
                frame
            })
        };
    };
    match enhancer.enhance(frame.clone()).await {
        Ok(enhanced) => enhanced,
        Err(reason) => {
            log::warn!("Real-ESRGAN 运行中降级，继续固定输出尺寸 reason={reason}");
            *runtime = None;
            let mut state = pipeline_state.lock().await;
            state.actual_video_enhancement = PlayerVideoEnhancement::Off;
            state.realesrgan_model_backend = None;
            state.degradation_reasons.push(reason);
            drop(state);
            upscale_rgb24_nearest(frame, fallback_scale).unwrap_or_else(|(frame, error)| {
                log::error!("Real-ESRGAN 降级帧缩放失败 error={error}");
                frame
            })
        }
    }
}

fn upscale_rgb24_nearest(
    frame: RawVideoFrame,
    scale: u32,
) -> Result<RawVideoFrame, (RawVideoFrame, String)> {
    let Some(output_width) = frame.width.checked_mul(scale) else {
        return Err((frame, "降级输出宽度溢出".to_owned()));
    };
    let Some(output_height) = frame.height.checked_mul(scale) else {
        return Err((frame, "降级输出高度溢出".to_owned()));
    };
    let Some(output_stride) = output_width.checked_mul(3) else {
        return Err((frame, "降级输出步长溢出".to_owned()));
    };
    let Some(output_len) = usize::try_from(output_stride).ok().and_then(|stride| {
        usize::try_from(output_height)
            .ok()
            .and_then(|height| stride.checked_mul(height))
    }) else {
        return Err((frame, "降级输出帧大小溢出".to_owned()));
    };
    let scale_usize = scale as usize;
    let input_stride = frame.stride as usize;
    let output_stride_usize = output_stride as usize;
    let mut data = vec![0_u8; output_len];
    for output_y in 0..output_height as usize {
        let input_y = output_y / scale_usize;
        for output_x in 0..output_width as usize {
            let input_x = output_x / scale_usize;
            let input_offset = input_y * input_stride + input_x * 3;
            let output_offset = output_y * output_stride_usize + output_x * 3;
            data[output_offset..output_offset + 3]
                .copy_from_slice(&frame.data[input_offset..input_offset + 3]);
        }
    }
    Ok(RawVideoFrame {
        width: output_width,
        height: output_height,
        stride: output_stride,
        pts_micros: frame.pts_micros,
        data,
    })
}

async fn model_interpolation_plan(
    source_frame_rate: f64,
    rife: &Arc<ModelSidecarRuntime>,
    realesrgan: Option<&Arc<ModelSidecarRuntime>>,
    available_vram_bytes: u64,
) -> (InterpolationPlan, RemoteInterpolationCapacity) {
    let rife_budget = FrameInterpolator::budget(rife.as_ref());
    let rife_diagnostics = rife.diagnostics().await;
    let interpolation_p95_ms = rife_diagnostics
        .p95_frame_time_ms
        .unwrap_or(rife_diagnostics.warmup_frame_time_ms)
        .max(rife_budget.estimated_frame_time_ms);
    let (enhancement_p95_ms, enhancement_sample_count, required_vram_bytes) =
        if let Some(realesrgan) = realesrgan {
            let budget = ModelEnhancer::budget(realesrgan.as_ref());
            let diagnostics = realesrgan.diagnostics().await;
            (
                diagnostics
                    .p95_frame_time_ms
                    .unwrap_or(diagnostics.warmup_frame_time_ms)
                    .max(budget.estimated_frame_time_ms),
                Some(diagnostics.frame_time_sample_count),
                rife_budget
                    .required_vram_bytes
                    .saturating_add(budget.required_vram_bytes),
            )
        } else {
            (0.0, None, rife_budget.required_vram_bytes)
        };
    let plan = plan_interpolation(InterpolationCapacityInput {
        source_frame_rate,
        output_frame_rate_cap: REMOTE_OUTPUT_FRAME_RATE_CAP,
        interpolation_p95_ms,
        enhancement_p95_ms,
        decode_p95_ms: 0.0,
        encode_p95_ms: 0.0,
        safety_margin_ms: MODEL_PIPELINE_SAFETY_MARGIN_MS,
        utilization_limit: MODEL_PIPELINE_UTILIZATION_LIMIT,
        available_vram_bytes,
        required_vram_bytes,
        hard_max_multiplier: RIFE_HARD_MAX_MULTIPLIER,
    });
    let latency_sample_count = enhancement_sample_count
        .map_or(rife_diagnostics.frame_time_sample_count, |count| {
            count.min(rife_diagnostics.frame_time_sample_count)
        });
    let capacity = RemoteInterpolationCapacity {
        source_frame_rate: plan.source_frame_rate,
        target_frame_rate: plan.target_frame_rate,
        selected_multiplier: plan.selected_multiplier,
        max_feasible_multiplier: plan.max_feasible_multiplier,
        output_frame_rate_cap: REMOTE_OUTPUT_FRAME_RATE_CAP,
        interval_budget_ms: plan.interval_budget_ms,
        estimated_interval_cost_ms: plan.estimated_interval_cost_ms,
        interpolation_p95_ms,
        enhancement_p95_ms: realesrgan.map(|_| enhancement_p95_ms),
        latency_sample_count,
    };
    (plan, capacity)
}

async fn refresh_interpolation_capacity(
    source_frame_rate: f64,
    rife: Option<&Arc<ModelSidecarRuntime>>,
    realesrgan: Option<&Arc<ModelSidecarRuntime>>,
    available_vram_bytes: u64,
    pipeline_state: &Arc<Mutex<ModelPipelineState>>,
) -> bool {
    let Some(rife) = rife else {
        return false;
    };
    let (plan, capacity) =
        model_interpolation_plan(source_frame_rate, rife, realesrgan, available_vram_bytes).await;
    let mut state = pipeline_state.lock().await;
    state.interpolation_capacity = Some(capacity);
    if plan.selected_multiplier >= RIFE_HARD_MAX_MULTIPLIER {
        return true;
    }
    let reason = plan
        .rejection_reason
        .unwrap_or_else(|| "RIFE 运行时 P95 超出插帧预算，已保持源帧率".to_owned());
    log::warn!("RIFE 运行时容量降级 reason={reason}");
    state.actual_interpolation = PlayerFrameInterpolation::Off;
    state.rife_model_backend = None;
    if !state.degradation_reasons.contains(&reason) {
        state.degradation_reasons.push(reason);
    }
    false
}

fn join_degradation_reasons(current: Option<String>, additional: Vec<String>) -> Option<String> {
    let mut reasons = Vec::new();
    let mut seen = HashSet::new();
    for group in current.into_iter().chain(additional) {
        for reason in group
            .split('；')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if seen.insert(reason.to_owned()) {
                reasons.push(reason.to_owned());
            }
        }
    }
    (!reasons.is_empty()).then(|| reasons.join("；"))
}

fn validate_direct_enhancement_diagnostics(
    diagnostics: &RemoteDirectEnhancementDiagnostics,
) -> Result<(), RemoteMediaError> {
    if diagnostics.sequence == 0 {
        return Err(invalid_direct_diagnostics("诊断序号必须大于 0"));
    }
    let has_smooth_codec = diagnostics
        .smooth_codecs
        .iter()
        .any(|codec| diagnostics.supported_codecs.contains(codec));
    let capability_supported = diagnostics.web_codecs
        && diagnostics.audio_web_codecs
        && diagnostics.audio_context
        && diagnostics.shader
        && diagnostics.web_gpu
        && diagnostics.offscreen_canvas
        && diagnostics.media_capabilities
        && has_smooth_codec;
    if diagnostics.capability_supported != capability_supported {
        return Err(invalid_direct_diagnostics("终端能力汇总结论与明细不一致"));
    }
    if diagnostics.status == RemoteDirectEnhancementStatus::Active {
        if !diagnostics.capability_supported {
            return Err(invalid_direct_diagnostics(
                "增强激活状态必须已通过终端能力探测",
            ));
        }
        if !matches!(
            diagnostics.effective_preset,
            Some(PlayerVideoEnhancement::Balanced | PlayerVideoEnhancement::Clear)
        ) {
            return Err(invalid_direct_diagnostics(
                "增强激活状态必须报告实际画质预设",
            ));
        }
        if diagnostics.audio_clock.as_deref() != Some("audio-context") {
            return Err(invalid_direct_diagnostics(
                "增强激活状态必须使用 AudioContext 主时钟",
            ));
        }
    }
    for preset in [diagnostics.requested_preset, diagnostics.effective_preset]
        .into_iter()
        .flatten()
    {
        if preset == PlayerVideoEnhancement::Off {
            return Err(invalid_direct_diagnostics(
                "终端增强预设只能是 balanced 或 clear",
            ));
        }
    }
    if diagnostics
        .audio_clock
        .as_deref()
        .is_some_and(|clock| clock != "audio-context")
    {
        return Err(invalid_direct_diagnostics("音频时钟类型无效"));
    }
    validate_direct_metric(diagnostics.dropped_frame_ratio, 0.0, 1.0, "丢帧比例")?;
    for (value, maximum, label) in [
        (diagnostics.frame_budget_ms, 1_000.0, "帧预算"),
        (diagnostics.gpu_queue_p95_ms, 60_000.0, "GPU P95"),
        (
            diagnostics.current_av_drift_ms.map(f64::abs),
            60_000.0,
            "当前音画漂移",
        ),
        (
            diagnostics.maximum_av_drift_ms.map(f64::abs),
            60_000.0,
            "最大音画漂移",
        ),
    ] {
        if let Some(value) = value {
            validate_direct_metric(value, 0.0, maximum, label)?;
        }
    }
    if diagnostics.recovered_range_count > diagnostics.range_retry_count
        || diagnostics.range_retry_count > diagnostics.range_request_count
    {
        return Err(invalid_direct_diagnostics("Range 恢复计数关系无效"));
    }
    for codecs in [
        &diagnostics.supported_codecs,
        &diagnostics.smooth_codecs,
        &diagnostics.power_efficient_codecs,
    ] {
        if codecs.len() > MAX_DIRECT_DIAGNOSTIC_CODECS
            || codecs.iter().any(|codec| {
                codec.trim().is_empty() || codec.len() > MAX_DIRECT_DIAGNOSTIC_CODEC_BYTES
            })
        {
            return Err(invalid_direct_diagnostics("编解码器能力列表无效"));
        }
    }
    if diagnostics
        .smooth_codecs
        .iter()
        .chain(&diagnostics.power_efficient_codecs)
        .any(|codec| !diagnostics.supported_codecs.contains(codec))
    {
        return Err(invalid_direct_diagnostics(
            "流畅或节能编解码器不在支持列表中",
        ));
    }
    if diagnostics
        .degradation_reason
        .as_ref()
        .is_some_and(|reason| reason.len() > MAX_DIRECT_DIAGNOSTIC_REASON_BYTES)
    {
        return Err(invalid_direct_diagnostics("降级原因超过长度上限"));
    }
    Ok(())
}

fn validate_direct_metric(
    value: f64,
    minimum: f64,
    maximum: f64,
    label: &str,
) -> Result<(), RemoteMediaError> {
    if !value.is_finite() || value < minimum || value > maximum {
        return Err(invalid_direct_diagnostics(format!("{label}超出有效范围")));
    }
    Ok(())
}

fn invalid_direct_diagnostics(message: impl Into<String>) -> RemoteMediaError {
    RemoteMediaError::new(400, "MEDIA_DIRECT_DIAGNOSTICS_INVALID", message)
}

fn validate_remote_enhancement(
    requested_mode: &str,
    enhancement: RemotePlaybackEnhancement,
) -> Result<(), RemoteMediaError> {
    let enabled = enhancement.video_enhancement != PlayerVideoEnhancement::Off
        || enhancement.frame_interpolation != PlayerFrameInterpolation::Off;
    if requested_mode != "transcode" && enabled {
        return Err(RemoteMediaError::new(
            400,
            "MEDIA_ENHANCEMENT_REQUIRES_TRANSCODE",
            "远程画质增强和插帧只能在实时转码模式使用",
        ));
    }
    if enhancement.frame_interpolation == PlayerFrameInterpolation::DisplayResample {
        return Err(RemoteMediaError::new(
            400,
            "MEDIA_INTERPOLATION_UNAVAILABLE",
            "远程转码不使用播放器显示刷新率重采样",
        ));
    }
    Ok(())
}

fn parse_requested_subtitle_mode(value: &str) -> Result<RequestedSubtitleMode, RemoteMediaError> {
    match value {
        "soft" => Ok(RequestedSubtitleMode::Soft),
        "burned" => Ok(RequestedSubtitleMode::Burned),
        "off" => Ok(RequestedSubtitleMode::Off),
        _ => Err(RemoteMediaError::new(
            400,
            "MEDIA_SUBTITLE_MODE_INVALID",
            "字幕输出模式无效",
        )),
    }
}

fn remote_video_filter_args(
    enhancement: RemotePlaybackEnhancement,
    burn_subtitle: Option<&Path>,
) -> Vec<String> {
    let mut filters = Vec::<String>::new();
    match enhancement.video_enhancement {
        PlayerVideoEnhancement::Off => {}
        PlayerVideoEnhancement::Balanced => {
            filters.push("hqdn3d=1.2:1.2:4:4".to_owned());
            filters.push("unsharp=5:5:0.45:5:5:0".to_owned());
        }
        PlayerVideoEnhancement::Clear => {
            filters.push("hqdn3d=0.8:0.8:3:3".to_owned());
            filters.push("unsharp=7:7:0.75:5:5:0".to_owned());
        }
    }
    if enhancement.frame_interpolation == PlayerFrameInterpolation::MotionCompensated {
        filters
            .push("minterpolate=fps=60:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1".to_owned());
    }
    if let Some(path) = burn_subtitle {
        filters.push(format!(
            "subtitles=filename='{}'",
            escape_ffmpeg_filter_path(path)
        ));
    }
    if filters.is_empty() {
        Vec::new()
    } else {
        vec!["-vf".to_owned(), filters.join(",")]
    }
}

/// 转义 libavfilter 的字幕路径；字幕文件位于受控会话目录，不拼接任意滤镜表达式。
fn escape_ffmpeg_filter_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized
        .chars()
        .fold(String::new(), |mut output, character| {
            if matches!(character, ':' | '\'' | ',' | '[' | ']' | ';') {
                output.push('\\');
            }
            output.push(character);
            output
        })
}

/// 为高成本运动补偿追加无插帧重试，保证远程播放优先可用。
fn hls_enhancement_profiles(enhancement: RemotePlaybackEnhancement) -> Vec<HlsEnhancementProfile> {
    let mut profiles = vec![HlsEnhancementProfile {
        enhancement,
        degradation_reason: None,
    }];
    if enhancement.frame_interpolation == PlayerFrameInterpolation::MotionCompensated {
        profiles.push(HlsEnhancementProfile {
            enhancement: RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::Off,
                ..enhancement
            },
            degradation_reason: Some("运动补偿未能在实时预算内启动，已关闭远程插帧"),
        });
    }
    profiles
}

/// 返回各编码器合法的视频参数，避免把 libx264 参数误传给硬件编码器。
fn encoder_video_args(codec: &str) -> &'static [&'static str] {
    match codec {
        "h264_videotoolbox" => &[
            "h264_videotoolbox",
            "-realtime",
            "1",
            "-allow_sw",
            "1",
            "-prio_speed",
            "1",
            "-b:v",
            "6M",
            "-maxrate",
            "8M",
            "-bufsize",
            "12M",
        ],
        "h264_nvenc" => &["h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "23"],
        "h264_amf" => &[
            "h264_amf", "-quality", "balanced", "-qp_i", "23", "-qp_p", "23",
        ],
        "h264_qsv" => &["h264_qsv", "-global_quality", "23"],
        _ => &[
            "libx264",
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-crf",
            "23",
        ],
    }
}

/// 判断当前编码器是否偏离平台首选项，供远程会话准确上报降级状态。
fn encoder_is_degraded(encoder: &str) -> bool {
    ENCODER_CANDIDATES
        .first()
        .is_some_and(|(_, preferred)| *preferred != encoder)
}

/// 追加低延迟 HLS 输出参数，确保首段在固定关键帧处及时落盘。
fn append_hls_output_args(command: &mut Command, segments: &Path, playlist: &Path) {
    command
        .args([
            "-force_key_frames",
            "expr:gte(t,n_forced*2)",
            "-f",
            "hls",
            "-hls_time",
            HLS_SEGMENT_SECONDS,
            "-hls_init_time",
            "1",
            "-hls_list_size",
            "0",
            "-hls_playlist_type",
            "event",
            "-hls_flags",
            "independent_segments+temp_file",
            "-hls_segment_filename",
        ])
        .arg(segments)
        .arg(playlist);
}

/// 为视频、音频或字幕输入追加统一的快速定位参数。
fn append_input_seek_args(command: &mut Command, position_seconds: f64) {
    if position_seconds <= 0.0 {
        return;
    }
    command.arg("-ss").arg(format!("{position_seconds:.3}"));
}

/// 确认播放列表已包含可播放分片，而不只判断空文件存在。
async fn hls_playlist_ready(playlist: &Path) -> bool {
    tokio::fs::read(playlist).await.ok().is_some_and(|content| {
        content
            .windows(b"#EXTINF".len())
            .any(|item| item == b"#EXTINF")
    })
}

/// 每次模型链重试前移除上次候选留下的输出，避免把旧分片当作本轮启动成功。
async fn clear_hls_output(output_directory: &Path, playlist: &Path) {
    let _ = tokio::fs::remove_file(playlist).await;
    let Ok(mut entries) = tokio::fs::read_dir(output_directory).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let is_hls_segment = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("segment-"));
        if is_hls_segment {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// 等待启动失败的模型子进程彻底结束，防止下一编码器尝试复用失效管道。
async fn stop_model_startup_process(
    mut decoder: Child,
    encoder: Option<Child>,
    pipeline: Option<JoinHandle<Result<(), String>>>,
    mut stderr_captures: Vec<FfmpegStderrCapture>,
) {
    if let Some(pipeline) = pipeline {
        pipeline.abort();
        let _ = pipeline.await;
    }
    let _ = decoder.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), decoder.wait()).await;
    if let Some(mut encoder) = encoder {
        let _ = encoder.kill().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), encoder.wait()).await;
    }
    for capture in stderr_captures.drain(..) {
        capture.finish().await;
    }
}

/// 有界读取 FFmpeg stderr，避免子进程日志填满管道并保留失败证据。
fn capture_ffmpeg_stderr(label: &'static str, mut stderr: ChildStderr) -> FfmpegStderrCapture {
    let buffer = Arc::new(StdMutex::new(Vec::new()));
    let task_buffer = Arc::clone(&buffer);
    let task = tokio::spawn(async move {
        let mut chunk = [0_u8; 4 * 1024];
        loop {
            let count = match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let Ok(mut output) = task_buffer.lock() else {
                break;
            };
            let overflow = output
                .len()
                .saturating_add(count)
                .saturating_sub(FFMPEG_STDERR_LIMIT);
            if overflow > 0 {
                let drain = overflow.min(output.len());
                output.drain(..drain);
            }
            output.extend_from_slice(&chunk[..count]);
        }
    });
    FfmpegStderrCapture {
        label,
        buffer,
        task,
    }
}

/// 使用最小测试帧验证编码器不仅存在于清单中，而且能实际创建会话。
async fn probe_video_encoder(ffmpeg_path: &Path, codec: &str) -> Result<(), String> {
    let mut command = hidden_command(ffmpeg_path);
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=1",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
        ])
        .args(encoder_video_args(codec))
        .args(["-pix_fmt", "yuv420p", "-f", "null", "-"])
        .kill_on_drop(true);
    match tokio::time::timeout(ENCODER_PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => Err(encoder_failure_message(
            codec,
            &format!("探测退出：{}", output.status),
            &String::from_utf8_lossy(&output.stderr),
        )),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("编码器探测超时".to_owned()),
    }
}

/// 合并稳定错误和 FFmpeg 最后一条有效诊断，避免只返回笼统超时。
fn encoder_failure_message(encoder: &str, summary: &str, stderr: &str) -> String {
    let detail = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(512).collect::<String>());
    detail.map_or_else(
        || format!("编码器 {encoder} {summary}"),
        |detail| format!("编码器 {encoder} {summary}：{detail}"),
    )
}

/// 每轮失败保留实际候选名，使最终的结构化错误能定位最后一次启动原因。
fn model_encoder_attempt_failure_message(encoder: &str, reason: &str) -> String {
    format!("模型 HLS 编码器 {encoder} 启动失败：{reason}")
}

#[cfg(test)]
mod encoder_tests {
    use super::{
        encoder_failure_message, encoder_is_degraded, encoder_video_args, ffmpeg_filter_available,
        hls_enhancement_profiles, model_encoder_attempt_failure_message,
        parse_requested_subtitle_mode, remote_video_filter_args, validate_remote_enhancement,
        RequestedSubtitleMode, ENCODER_CANDIDATES, MODEL_PIPELINE_SAFETY_MARGIN_MS,
        MODEL_PIPELINE_UTILIZATION_LIMIT, REMOTE_OUTPUT_FRAME_RATE_CAP, RIFE_HARD_MAX_MULTIPLIER,
    };
    use ani_contracts::{
        PlayerFrameInterpolation, PlayerVideoEnhancement, RemotePlaybackEnhancement,
    };
    use ani_media::player::{plan_interpolation, InterpolationCapacityInput, RawVideoFrame};

    #[test]
    fn selects_vendor_specific_video_options() {
        assert_eq!(
            encoder_video_args("h264_videotoolbox")[0],
            "h264_videotoolbox"
        );
        assert!(encoder_video_args("h264_videotoolbox").contains(&"-realtime"));
        assert!(encoder_video_args("h264_videotoolbox").contains(&"-allow_sw"));
        assert_eq!(encoder_video_args("h264_nvenc")[0], "h264_nvenc");
        assert!(encoder_video_args("h264_nvenc").contains(&"-cq"));
        assert!(encoder_video_args("h264_amf").contains(&"-qp_i"));
        assert!(encoder_video_args("h264_qsv").contains(&"-global_quality"));
        assert!(encoder_video_args("libx264").contains(&"-crf"));
        assert!(encoder_video_args("libx264").contains(&"zerolatency"));
    }

    #[test]
    fn keeps_software_encoder_as_the_last_fallback() {
        assert_eq!(ENCODER_CANDIDATES.last(), Some(&("libx264", "libx264")));
        assert!(!encoder_is_degraded(ENCODER_CANDIDATES[0].1));
        if ENCODER_CANDIDATES.len() > 1 {
            assert!(encoder_is_degraded("libx264"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prefers_videotoolbox_on_macos() {
        assert_eq!(
            ENCODER_CANDIDATES,
            &[
                ("h264_videotoolbox", "videotoolbox"),
                ("libx264", "libx264"),
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tries_nvidia_amd_intel_before_software_on_windows() {
        assert_eq!(
            ENCODER_CANDIDATES,
            &[
                ("h264_nvenc", "nvenc"),
                ("h264_amf", "amf"),
                ("h264_qsv", "qsv"),
                ("libx264", "libx264"),
            ]
        );
    }

    #[test]
    fn retries_motion_compensation_without_interpolation() {
        let profiles = hls_enhancement_profiles(RemotePlaybackEnhancement {
            video_enhancement: PlayerVideoEnhancement::Clear,
            frame_interpolation: PlayerFrameInterpolation::MotionCompensated,
        });
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            profiles[0].enhancement.frame_interpolation,
            PlayerFrameInterpolation::MotionCompensated
        );
        assert_eq!(
            profiles[1].enhancement.frame_interpolation,
            PlayerFrameInterpolation::Off
        );
        assert!(profiles[1]
            .degradation_reason
            .is_some_and(|reason| reason.contains("已关闭远程插帧")));
    }

    #[test]
    fn appends_the_last_ffmpeg_diagnostic_to_encoder_errors() {
        assert_eq!(
            encoder_failure_message(
                "videotoolbox",
                "提前退出",
                "first diagnostic\nfinal diagnostic\n"
            ),
            "编码器 videotoolbox 提前退出：final diagnostic"
        );
        assert_eq!(
            encoder_failure_message("libx264", "启动超时", ""),
            "编码器 libx264 启动超时"
        );
    }

    #[test]
    fn keeps_the_last_model_encoder_failure_actionable() {
        assert_eq!(
            model_encoder_attempt_failure_message("libx264", "编码器 libx264 模型远程管线启动超时"),
            "模型 HLS 编码器 libx264 启动失败：编码器 libx264 模型远程管线启动超时"
        );
    }

    #[test]
    fn builds_actual_remote_enhancement_filter_chain() {
        let args = remote_video_filter_args(
            RemotePlaybackEnhancement {
                video_enhancement: PlayerVideoEnhancement::Clear,
                frame_interpolation: PlayerFrameInterpolation::MotionCompensated,
            },
            None,
        );
        assert_eq!(args[0], "-vf");
        assert!(args[1].contains("unsharp=7:7"));
        assert!(args[1].contains("minterpolate=fps=60"));
    }

    #[test]
    fn appends_burned_subtitles_after_the_enhancement_filters() {
        let args = remote_video_filter_args(
            RemotePlaybackEnhancement {
                video_enhancement: PlayerVideoEnhancement::Clear,
                ..Default::default()
            },
            Some(std::path::Path::new("C:\\media\\subtitle:01[main].ass")),
        );
        assert_eq!(args[0], "-vf");
        let filters = &args[1];
        assert!(filters.find("unsharp=7:7").unwrap() < filters.find("subtitles=").unwrap());
        assert!(filters.contains("subtitles=filename='C\\:/media/subtitle\\:01\\[main\\].ass'"));
    }

    #[test]
    fn validates_subtitle_output_modes() {
        assert_eq!(
            parse_requested_subtitle_mode("soft").unwrap(),
            RequestedSubtitleMode::Soft
        );
        assert_eq!(
            parse_requested_subtitle_mode("burned").unwrap(),
            RequestedSubtitleMode::Burned
        );
        assert_eq!(
            parse_requested_subtitle_mode("off").unwrap(),
            RequestedSubtitleMode::Off
        );
        assert!(parse_requested_subtitle_mode("client-defined").is_err());
    }

    #[test]
    fn detects_only_the_exact_ffmpeg_subtitle_filter() {
        let filters = b" ... null V->V Pass source\n .. subtitles V->V Render text\n";
        assert!(ffmpeg_filter_available(filters, "subtitles"));
        assert!(!ffmpeg_filter_available(filters, "subtitle"));
        assert!(!ffmpeg_filter_available(
            b" ... null V->V subtitles in help text",
            "subtitles"
        ));
    }

    #[test]
    fn rejects_enhancement_for_direct_and_unready_rife() {
        assert!(validate_remote_enhancement(
            "direct",
            RemotePlaybackEnhancement {
                video_enhancement: PlayerVideoEnhancement::Balanced,
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_remote_enhancement(
            "transcode",
            RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::DisplayResample,
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_remote_enhancement(
            "transcode",
            RemotePlaybackEnhancement {
                frame_interpolation: PlayerFrameInterpolation::RifeRealtime,
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn accepts_normal_frame_rates_and_rejects_unsafe_values() {
        assert!((super::parse_frame_rate("24000/1001").unwrap() - 23.976).abs() < 0.01);
        assert!(super::parse_frame_rate("0/1").is_err());
        assert!(super::parse_frame_rate("121/1").is_err());
        assert!(super::parse_frame_rate("nan/1").is_err());
    }

    #[test]
    fn remote_rife_policy_caps_ai_at_two_and_accounts_for_combined_cost() {
        let base = InterpolationCapacityInput {
            source_frame_rate: 24.0,
            output_frame_rate_cap: REMOTE_OUTPUT_FRAME_RATE_CAP,
            interpolation_p95_ms: 20.0,
            enhancement_p95_ms: 0.0,
            decode_p95_ms: 0.0,
            encode_p95_ms: 0.0,
            safety_margin_ms: MODEL_PIPELINE_SAFETY_MARGIN_MS,
            utilization_limit: MODEL_PIPELINE_UTILIZATION_LIMIT,
            available_vram_bytes: 4_000,
            required_vram_bytes: 2_000,
            hard_max_multiplier: RIFE_HARD_MAX_MULTIPLIER,
        };
        assert_eq!(plan_interpolation(base).selected_multiplier, 2);
        assert_eq!(
            plan_interpolation(InterpolationCapacityInput {
                enhancement_p95_ms: 10.0,
                ..base
            })
            .selected_multiplier,
            1
        );
        assert_eq!(
            plan_interpolation(InterpolationCapacityInput {
                available_vram_bytes: 1_999,
                ..base
            })
            .selected_multiplier,
            1
        );
    }

    #[test]
    fn nearest_neighbor_fallback_preserves_rgb24_and_scale() {
        let frame = RawVideoFrame {
            width: 1,
            height: 1,
            stride: 3,
            pts_micros: 7,
            data: vec![10, 20, 30],
        };
        let output = super::upscale_rgb24_nearest(frame, 2).expect("upscale fallback");
        assert_eq!(
            (
                output.width,
                output.height,
                output.stride,
                output.pts_micros
            ),
            (2, 2, 6, 7)
        );
        assert_eq!(
            output.data,
            vec![10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30]
        );
    }

    #[test]
    fn model_backend_names_active_models_and_degradation_reasons_are_unique() {
        assert_eq!(
            super::model_backend_name(Some("ncnn-vulkan"), Some("ncnn-vulkan")).as_deref(),
            Some("rife:ncnn-vulkan+realesrgan:ncnn-vulkan")
        );
        assert_eq!(
            super::model_backend_name(Some("ncnn-vulkan"), None).as_deref(),
            Some("rife:ncnn-vulkan")
        );
        assert_eq!(super::model_backend_name(None, None), None);
        assert_eq!(
            super::join_degradation_reasons(
                Some("RIFE 超时；显存不足".to_owned()),
                vec!["RIFE 超时".to_owned(), "Real-ESRGAN 超时".to_owned()]
            )
            .as_deref(),
            Some("RIFE 超时；显存不足；Real-ESRGAN 超时")
        );
    }
}

#[derive(Default, Deserialize)]
struct SubtitleProbeOutput {
    #[serde(default)]
    streams: Vec<SubtitleStream>,
}

#[derive(Default, Deserialize)]
struct SubtitleStream {
    index: Option<i64>,
    codec_name: Option<String>,
    #[serde(default)]
    disposition: SubtitleDisposition,
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[derive(Default, Deserialize)]
struct SubtitleDisposition {
    default: i64,
}

struct SupportedSubtitle {
    index: i64,
    output_type: &'static str,
    disposition: SubtitleDisposition,
    tags: HashMap<String, String>,
}

impl SupportedSubtitle {
    fn from_stream(stream: SubtitleStream) -> Option<Self> {
        let index = stream.index?;
        let codec = stream.codec_name.as_deref()?.to_ascii_lowercase();
        let output_type = if matches!(codec.as_str(), "ass" | "ssa") {
            "ass"
        } else if matches!(
            codec.as_str(),
            "subrip" | "srt" | "webvtt" | "mov_text" | "text"
        ) {
            "vtt"
        } else {
            return None;
        };
        Some(Self {
            index,
            output_type,
            disposition: stream.disposition,
            tags: stream.tags,
        })
    }
}

async fn run_command(
    command_path: &Path,
    args: &[&str],
    trailing_path: Option<&Path>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut command = hidden_command(command_path);
    command.args(args);
    if let Some(path) = trailing_path {
        command.arg(path);
    }
    command.kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(output),
        Ok(Ok(output)) => Err(format!("退出状态 {}", output.status)),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("执行超时".to_owned()),
    }
}

/// 仅接受 FFmpeg `-filters` 表格中精确登记的滤镜名，避免从说明文本误判能力。
fn ffmpeg_filter_available(output: &[u8], expected_filter: &str) -> bool {
    String::from_utf8_lossy(output)
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(expected_filter))
}

fn hidden_command(path: &Path) -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new(path);
        command.as_std_mut().creation_flags(0x0800_0000);
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new(path)
    }
}

async fn validate_media_path(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let (Ok(root), Ok(candidate)) = (
        tokio::fs::canonicalize(root).await,
        tokio::fs::canonicalize(candidate).await,
    ) else {
        return None;
    };
    if !candidate.starts_with(&root) {
        log::warn!("拒绝下载目录外的远程媒体路径");
        return None;
    }
    tokio::fs::metadata(&candidate)
        .await
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|_| candidate)
}

async fn canonical_asset(root: &Path, name: &str) -> Result<PathBuf, RemoteMediaError> {
    let candidate = root.join(name);
    let path = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|_| asset_not_found())?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(asset_not_found());
    }
    Ok(path)
}

fn task_file_path(task: &DownloadTask, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_owned()
    } else {
        Path::new(&task.save_path).join(path)
    }
}

fn video_extensions(settings: &AppSettings) -> HashSet<String> {
    settings
        .pointer("/media/videoExtensions")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(normalize_extension)
                .collect()
        })
        .filter(|values: &HashSet<_>| !values.is_empty())
        .unwrap_or_else(|| {
            [".mkv", ".mp4", ".avi"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
}

fn normalize_extension(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    if value.starts_with('.') {
        value
    } else {
        format!(".{value}")
    }
}

fn extension(value: &str) -> String {
    Path::new(value)
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default()
}

fn direct_content_type(path: &Path) -> String {
    match extension(&path.to_string_lossy()).as_str() {
        ".webm" => "video/webm",
        ".mp4" | ".m4v" => "video/mp4",
        ".mkv" => "video/x-matroska",
        ".avi" => "video/x-msvideo",
        ".mov" => "video/quicktime",
        ".mpg" | ".mpeg" => "video/mpeg",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn resolve_resume_position(checkpoint: Option<&PlaybackCheckpoint>) -> Option<f64> {
    let checkpoint = checkpoint?;
    if checkpoint.completed
        || checkpoint.position_seconds < 5.0
        || (checkpoint.duration_seconds > 0.0
            && checkpoint.duration_seconds - checkpoint.position_seconds <= 30.0)
    {
        return None;
    }
    Some(checkpoint.position_seconds)
}

/// 优先采用显式跳转位置，否则读取续播点，并限制在媒体总时长内。
fn resolve_session_start_position(
    requested_position_seconds: Option<f64>,
    checkpoint: Option<&PlaybackCheckpoint>,
    duration_seconds: Option<i64>,
) -> Result<Option<f64>, RemoteMediaError> {
    if requested_position_seconds.is_some_and(|position| !position.is_finite() || position < 0.0) {
        return Err(RemoteMediaError::new(
            400,
            "MEDIA_START_POSITION_INVALID",
            "播放起点无效",
        ));
    }
    let Some(position) = requested_position_seconds.or_else(|| resolve_resume_position(checkpoint))
    else {
        return Ok(None);
    };
    let position = duration_seconds
        .filter(|duration| *duration >= 0)
        .map_or(position, |duration| position.min(duration as f64));
    Ok(Some(position))
}

fn random_token(size: usize) -> String {
    let mut bytes = vec![0_u8; size];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn secure_token_equals(expected: Option<&str>, actual: &str) -> bool {
    expected.is_some_and(|expected| {
        expected.len() == actual.len()
            && expected.as_bytes().ct_eq(actual.as_bytes()).unwrap_u8() == 1
    })
}

fn is_hls_asset(value: &str) -> bool {
    value == "index.m3u8"
        || value
            .strip_prefix("segment-")
            .and_then(|value| value.strip_suffix(".ts"))
            .is_some_and(|digits| {
                digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn is_subtitle_asset(value: &str) -> bool {
    value
        .strip_prefix("subtitle-")
        .and_then(|value| {
            value
                .strip_suffix(".ass")
                .or_else(|| value.strip_suffix(".vtt"))
        })
        .is_some_and(|digits| digits.len() == 3 && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn normalize_language(value: Option<&str>) -> Option<String> {
    let normalized = value?.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "und" {
        return None;
    }
    Some(match normalized.as_str() {
        "zh" | "zho" | "chi" => "中文".to_owned(),
        "chs" => "简体中文".to_owned(),
        "cht" => "繁体中文".to_owned(),
        "ja" | "jpn" => "日语".to_owned(),
        "en" | "eng" => "英语".to_owned(),
        _ => sanitize_label(&normalized).unwrap_or_else(|| "未知".to_owned()),
    })
}

fn subtitle_label(title: Option<&str>, language: Option<&str>, order: usize) -> String {
    let title = title.and_then(sanitize_label);
    let mut parts = Vec::new();
    if let Some(title) = title {
        parts.push(title);
    }
    if let Some(language) = language {
        if !parts.iter().any(|part| part == language) {
            parts.push(language.to_owned());
        }
    }
    if parts.is_empty() {
        format!("字幕 {}", order + 1)
    } else {
        parts.join(" / ")
    }
}

fn sanitize_label(value: &str) -> Option<String> {
    let value = value
        .chars()
        .filter(|character| !character.is_control() && *character != '<' && *character != '>')
        .take(120)
        .collect::<String>();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn internal_media_error(error: impl Into<String>) -> RemoteMediaError {
    log::error!("Rust 远程媒体服务失败 error={}", error.into());
    RemoteMediaError::new(500, "MEDIA_SERVICE_FAILED", "远程媒体服务内部错误")
}

fn session_not_found() -> RemoteMediaError {
    RemoteMediaError::new(404, "MEDIA_SESSION_NOT_FOUND", "播放会话不存在或已过期")
}

fn asset_not_found() -> RemoteMediaError {
    RemoteMediaError::new(404, "MEDIA_ASSET_NOT_FOUND", "媒体资源不存在")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证媒体资源路由只接受固定 HLS 与字幕文件名。
    #[test]
    fn validates_asset_names() {
        assert!(is_hls_asset("index.m3u8"));
        assert!(is_hls_asset("segment-000001.ts"));
        assert!(!is_hls_asset("../segment-000001.ts"));
        assert!(is_subtitle_asset("subtitle-001.ass"));
        assert!(!is_subtitle_asset("subtitle-1.ass"));
    }

    /// 验证已完成或临近片尾的检查点不会恢复。
    #[test]
    fn guards_resume_position() {
        let checkpoint = PlaybackCheckpoint {
            task_id: "task-1".to_owned(),
            file_index: Some(0),
            position_seconds: 90.0,
            duration_seconds: 100.0,
            completed: false,
            watched_reported: true,
            updated_at: "2026-07-25T00:00:00.000Z".to_owned(),
        };
        assert_eq!(resolve_resume_position(Some(&checkpoint)), None);
    }

    /// 验证显式跳转覆盖续播点，并按总时长裁剪。
    #[test]
    fn resolves_explicit_session_start_position() {
        let checkpoint = PlaybackCheckpoint {
            task_id: "task-1".to_owned(),
            file_index: Some(0),
            position_seconds: 90.0,
            duration_seconds: 200.0,
            completed: false,
            watched_reported: false,
            updated_at: "2026-08-14T00:00:00.000Z".to_owned(),
        };
        assert_eq!(
            resolve_session_start_position(Some(240.0), Some(&checkpoint), Some(200))
                .expect("显式播放起点应有效"),
            Some(200.0)
        );
        assert!(resolve_session_start_position(Some(-1.0), None, Some(200)).is_err());
    }
}
