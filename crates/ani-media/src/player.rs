use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use ani_contracts::{
    PlayerCapabilities, PlayerCommand, PlayerCommandAction, PlayerCommandResult, PlayerError,
    PlayerErrorCode, PlayerRecoveryAction, PlayerSnapshot,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const SUBTITLE_SCALES: &[u16] = &[100, 125, 150, 175, 200];

/// 终版增强链路中可独立替换的处理阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancementStage {
    Shader,
    ModelSuperResolution,
    FrameInterpolation,
}

/// 单帧预算，调度器据此拒绝超预算的模型组合。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnhancementBudget {
    pub target_frame_time_ms: f64,
    pub estimated_frame_time_ms: f64,
    pub available_vram_bytes: u64,
    pub required_vram_bytes: u64,
}

/// 模型权重的受控清单；播放器只接受摘要、尺寸和资源预算均有效的模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementModelManifest {
    pub model_id: String,
    pub backend: String,
    pub weight_sha256: String,
    pub input_width: u32,
    pub input_height: u32,
    pub required_vram_bytes: u64,
    pub estimated_frame_time_ms: u32,
}

/// 校验模型清单和当前会话预算，失败时保持能力关闭。
pub fn validate_model_manifest(
    manifest: &EnhancementModelManifest,
    available_vram_bytes: u64,
    target_frame_time_ms: f64,
) -> Result<(), String> {
    if manifest.model_id.trim().is_empty() || manifest.backend.trim().is_empty() {
        return Err("模型标识或推理后端为空".to_owned());
    }
    if manifest.input_width == 0 || manifest.input_height == 0 {
        return Err("模型输入尺寸无效".to_owned());
    }
    if manifest.weight_sha256.len() != 64
        || !manifest
            .weight_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("模型权重 SHA-256 摘要无效".to_owned());
    }
    let budget = EnhancementBudget {
        target_frame_time_ms,
        estimated_frame_time_ms: f64::from(manifest.estimated_frame_time_ms),
        available_vram_bytes,
        required_vram_bytes: manifest.required_vram_bytes,
    };
    if !budget.fits() {
        return Err("模型超出当前帧时间或显存预算".to_owned());
    }
    Ok(())
}

/// 读取权重文件并验证其 SHA-256；文件校验失败时模型不得进入可用状态。
pub async fn validate_model_weight(
    weight_path: &Path,
    manifest: &EnhancementModelManifest,
    available_vram_bytes: u64,
    target_frame_time_ms: f64,
) -> Result<(), String> {
    validate_model_manifest(manifest, available_vram_bytes, target_frame_time_ms)?;
    let mut file = tokio::fs::File::open(weight_path)
        .await
        .map_err(|error| format!("读取模型权重失败：{error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("读取模型权重失败：{error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest.finalize();
    let actual = format!("{digest:x}");
    if !actual.eq_ignore_ascii_case(&manifest.weight_sha256) {
        return Err("模型权重 SHA-256 与清单不一致".to_owned());
    }
    Ok(())
}

/// RIFE 等插帧后端共用的有界帧队列，满载时丢弃最旧帧并可观测计数。
#[derive(Debug)]
pub struct BoundedFrameQueue<T> {
    capacity: usize,
    frames: VecDeque<T>,
    dropped_frames: u64,
}

impl<T> BoundedFrameQueue<T> {
    /// 创建至少容纳一帧的队列。
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::new(),
            dropped_frames: 0,
        }
    }

    /// 推入一帧，满载时丢弃最旧帧。
    pub fn push(&mut self, frame: T) {
        if self.frames.len() == self.capacity {
            let _ = self.frames.pop_front();
            self.dropped_frames = self.dropped_frames.saturating_add(1);
        }
        self.frames.push_back(frame);
    }

    /// 取出最早的一帧。
    pub fn pop(&mut self) -> Option<T> {
        self.frames.pop_front()
    }

    /// 返回当前队列长度。
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 返回队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 返回累计丢帧数。
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
}

impl EnhancementBudget {
    /// 返回当前模型是否能在目标帧预算与显存预算内运行。
    pub fn fits(self) -> bool {
        self.target_frame_time_ms.is_finite()
            && self.estimated_frame_time_ms.is_finite()
            && self.target_frame_time_ms > 0.0
            && self.estimated_frame_time_ms <= self.target_frame_time_ms
            && self.required_vram_bytes <= self.available_vram_bytes
    }
}

/// 模型超分后端端口；真实推理 SDK 由桌面平台另行实现。
pub trait ModelEnhancer: Send + Sync {
    /// 返回后端稳定标识。
    fn backend_id(&self) -> &str;
    /// 返回模型是否已完成权重摘要和资源校验。
    fn ready(&self) -> bool;
    /// 返回本次处理的资源预算。
    fn budget(&self) -> EnhancementBudget;
}

/// 模型插帧后端端口；输入帧队列由具体实现负责保持有界。
pub trait FrameInterpolator: Send + Sync {
    /// 返回后端稳定标识。
    fn backend_id(&self) -> &str;
    /// 返回模型是否已完成权重摘要和资源校验。
    fn ready(&self) -> bool;
    /// 返回本次处理的资源预算。
    fn budget(&self) -> EnhancementBudget;
}

/// 按“插帧 -> 模型超分 -> Shader”顺序做安全降级的调度结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancementDecision {
    Disabled,
    Shader,
    ModelSuperResolution,
    FrameInterpolation,
}

/// 统一模型能力门禁，未准备好或超预算时永远不会打开能力字段。
pub struct EnhancementScheduler<'a> {
    pub model: Option<&'a dyn ModelEnhancer>,
    pub interpolator: Option<&'a dyn FrameInterpolator>,
    pub shader_available: bool,
}

impl EnhancementScheduler<'_> {
    /// 计算当前会话唯一允许的增强阶段。
    pub fn decide(&self, requested: EnhancementStage) -> EnhancementDecision {
        match requested {
            EnhancementStage::FrameInterpolation => {
                if self
                    .interpolator
                    .is_some_and(|backend| backend.ready() && backend.budget().fits())
                {
                    EnhancementDecision::FrameInterpolation
                } else if self
                    .model
                    .is_some_and(|backend| backend.ready() && backend.budget().fits())
                {
                    EnhancementDecision::ModelSuperResolution
                } else if self.shader_available {
                    EnhancementDecision::Shader
                } else {
                    EnhancementDecision::Disabled
                }
            }
            EnhancementStage::ModelSuperResolution => {
                if self
                    .model
                    .is_some_and(|backend| backend.ready() && backend.budget().fits())
                {
                    EnhancementDecision::ModelSuperResolution
                } else if self.shader_available {
                    EnhancementDecision::Shader
                } else {
                    EnhancementDecision::Disabled
                }
            }
            EnhancementStage::Shader => {
                if self.shader_available {
                    EnhancementDecision::Shader
                } else {
                    EnhancementDecision::Disabled
                }
            }
        }
    }
}

/// 平台播放器 transport 的稳定失败，不泄漏 SDK 或 FFI 类型。
#[derive(Debug, Clone, thiserror::Error)]
pub enum PlayerTransportError {
    #[error("播放器运行时不可用：{0}")]
    Unavailable(String),
    #[error("播放器原生调用失败：{0}")]
    Native(String),
    #[error("播放器媒体加载失败：{0}")]
    LoadFailed(String),
    #[error("播放器返回无效状态：{0}")]
    InvalidResponse(String),
}

/// 桌面 FFI、Android Kotlin 与 iOS Swift 共用的播放器端口。
#[async_trait]
pub trait PlayerTransport: Send + Sync {
    /// 返回当前后端稳定公开的能力。
    async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError>;
    /// 执行一条已验证的播放器命令。
    async fn dispatch(
        &self,
        command: PlayerCommand,
    ) -> Result<PlayerCommandResult, PlayerTransportError>;
    /// 返回当前完整快照；尚未创建会话时返回空。
    async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError>;
    /// 幂等释放原生媒体和运行时句柄。
    async fn shutdown(&self) -> Result<(), PlayerTransportError>;
}

/// 在平台 transport 外统一校验命令、会话和快照时序。
pub struct PlayerService {
    transport: Arc<dyn PlayerTransport>,
    state: Mutex<PlayerServiceState>,
}

#[derive(Default)]
struct PlayerServiceState {
    active_session_id: Option<String>,
    last_snapshot: Option<PlayerSnapshot>,
}

impl PlayerService {
    /// 使用单个平台 transport 创建播放器服务。
    pub fn new(transport: Arc<dyn PlayerTransport>) -> Self {
        Self {
            transport,
            state: Mutex::new(PlayerServiceState::default()),
        }
    }

    /// 返回平台后端能力，运行时缺失由 transport 明确标记。
    pub async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError> {
        self.transport.capabilities().await
    }

    /// 校验命令信封并阻止旧会话控制当前媒体。
    pub async fn dispatch(&self, command: PlayerCommand) -> PlayerCommandResult {
        if let Some(error) = validate_command(&command) {
            return rejected(&command.command_id, error);
        }
        {
            let state = self.state.lock().await;
            if !matches!(&command.action, PlayerCommandAction::Load { .. })
                && state.active_session_id.as_deref() != Some(command.session_id.as_str())
            {
                return rejected(
                    &command.command_id,
                    invalid_command("播放器会话已切换，请重试"),
                );
            }
        }

        let command_id = command.command_id.clone();
        let session_id = command.session_id.clone();
        let is_load = matches!(&command.action, PlayerCommandAction::Load { .. });
        match self.transport.dispatch(command).await {
            Ok(result) => {
                if result.accepted && is_load {
                    let mut state = self.state.lock().await;
                    state.active_session_id = Some(session_id);
                    state.last_snapshot = None;
                }
                result
            }
            Err(error) => rejected(&command_id, transport_error(error)),
        }
    }

    /// 过滤旧会话和乱序快照，防止换集后状态倒退。
    pub async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        let Some(snapshot) = self.transport.snapshot().await? else {
            return Ok(None);
        };
        let mut state = self.state.lock().await;
        if state.active_session_id.as_deref() != Some(snapshot.session_id.as_str()) {
            return Ok(state.last_snapshot.clone());
        }
        if state
            .last_snapshot
            .as_ref()
            .is_some_and(|current| current.sequence >= snapshot.sequence)
        {
            return Ok(state.last_snapshot.clone());
        }
        state.last_snapshot = Some(snapshot.clone());
        Ok(Some(snapshot))
    }

    /// 幂等关闭 transport 并清除活动会话。
    pub async fn shutdown(&self) -> Result<(), PlayerTransportError> {
        self.transport.shutdown().await?;
        let mut state = self.state.lock().await;
        state.active_session_id = None;
        state.last_snapshot = None;
        Ok(())
    }
}

/// 校验命令标识、会话标识和数值范围。
pub fn validate_command(command: &PlayerCommand) -> Option<PlayerError> {
    if !valid_identifier(&command.command_id, true) || !valid_identifier(&command.session_id, false)
    {
        return Some(invalid_command("播放器命令或会话标识无效"));
    }
    match &command.action {
        PlayerCommandAction::Load {
            source,
            start_position_seconds,
        } => {
            if source.task_id.trim().is_empty()
                || source.title.trim().is_empty()
                || source.uri.trim().is_empty()
                || start_position_seconds.is_some_and(|value| !finite_range(value, 0.0, f64::MAX))
            {
                return Some(invalid_command("媒体资源参数无效"));
            }
        }
        PlayerCommandAction::Seek { position_seconds } => {
            if !finite_range(*position_seconds, 0.0, f64::MAX) {
                return Some(invalid_command("跳转时间无效"));
            }
        }
        PlayerCommandAction::SetVolume { volume } => {
            if !finite_range(*volume, 0.0, 1.0) {
                return Some(invalid_command("音量参数无效"));
            }
        }
        PlayerCommandAction::SetRate { rate } => {
            if !PLAYBACK_RATES.contains(rate) {
                return Some(invalid_command("播放倍速无效"));
            }
        }
        PlayerCommandAction::SelectAudioTrack { track_id } => {
            if track_id.parse::<i32>().is_err() {
                return Some(invalid_command("音轨标识无效"));
            }
        }
        PlayerCommandAction::SelectSubtitleTrack {
            track_id: Some(track_id),
        } => {
            if track_id.parse::<i32>().is_err() && !valid_identifier(track_id, true) {
                return Some(invalid_command("字幕轨标识无效"));
            }
        }
        PlayerCommandAction::SetSubtitleScale { subtitle_scale } => {
            if !SUBTITLE_SCALES.contains(subtitle_scale) {
                return Some(invalid_command("字幕缩放比例无效"));
            }
        }
        PlayerCommandAction::SetVideoEnhancement { .. } => {}
        PlayerCommandAction::SetFrameInterpolation { .. } => {}
        PlayerCommandAction::SetAspectRatio {
            aspect_ratio: ani_contracts::PlayerAspectRatio::Custom,
            value,
        } if value.as_deref().is_none_or(str::is_empty) => {
            return Some(invalid_command("自定义画面比例无效"));
        }
        _ => {}
    }
    None
}

/// 构造当前平台不支持某项能力时的稳定拒绝结果。
pub fn unsupported(command_id: &str, message: impl Into<String>) -> PlayerCommandResult {
    rejected(
        command_id,
        PlayerError {
            code: PlayerErrorCode::Unsupported,
            message: message.into(),
            recoverable: false,
            recovery_actions: Vec::new(),
        },
    )
}

fn rejected(command_id: &str, error: PlayerError) -> PlayerCommandResult {
    PlayerCommandResult {
        command_id: command_id.to_owned(),
        accepted: false,
        error: Some(error),
    }
}

fn invalid_command(message: impl Into<String>) -> PlayerError {
    PlayerError {
        code: PlayerErrorCode::Unknown,
        message: message.into(),
        recoverable: true,
        recovery_actions: vec![PlayerRecoveryAction::Retry],
    }
}

fn transport_error(error: PlayerTransportError) -> PlayerError {
    let (code, recoverable) = match &error {
        PlayerTransportError::Unavailable(_) => (PlayerErrorCode::RuntimeMissing, false),
        PlayerTransportError::Native(_)
        | PlayerTransportError::LoadFailed(_)
        | PlayerTransportError::InvalidResponse(_) => (PlayerErrorCode::Unknown, true),
    };
    PlayerError {
        code,
        message: error.to_string(),
        recoverable,
        recovery_actions: if recoverable {
            vec![PlayerRecoveryAction::Retry, PlayerRecoveryAction::Close]
        } else {
            vec![PlayerRecoveryAction::Close]
        },
    }
}

fn valid_identifier(value: &str, allow_colon: bool) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.')
                || (allow_colon && byte == b':')
        })
}

fn finite_range(value: f64, minimum: f64, maximum: f64) -> bool {
    value.is_finite() && value >= minimum && value <= maximum
}

#[cfg(test)]
mod tests {
    use ani_contracts::{
        PlayerAvailability, PlayerBackend, PlayerHostPlatform, PlayerMediaMode, PlayerMediaSource,
    };

    use super::*;

    struct TestModel {
        ready: bool,
        budget: EnhancementBudget,
    }

    impl ModelEnhancer for TestModel {
        fn backend_id(&self) -> &str {
            "test-model"
        }
        fn ready(&self) -> bool {
            self.ready
        }
        fn budget(&self) -> EnhancementBudget {
            self.budget
        }
    }

    struct TestInterpolator {
        ready: bool,
        budget: EnhancementBudget,
    }

    impl FrameInterpolator for TestInterpolator {
        fn backend_id(&self) -> &str {
            "test-interpolator"
        }
        fn ready(&self) -> bool {
            self.ready
        }
        fn budget(&self) -> EnhancementBudget {
            self.budget
        }
    }

    struct StubTransport {
        snapshot: Mutex<Option<PlayerSnapshot>>,
    }

    #[async_trait]
    impl PlayerTransport for StubTransport {
        async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError> {
            Ok(capabilities())
        }

        async fn dispatch(
            &self,
            command: PlayerCommand,
        ) -> Result<PlayerCommandResult, PlayerTransportError> {
            Ok(PlayerCommandResult {
                command_id: command.command_id,
                accepted: true,
                error: None,
            })
        }

        async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
            Ok(self.snapshot.lock().await.clone())
        }

        async fn shutdown(&self) -> Result<(), PlayerTransportError> {
            Ok(())
        }
    }

    /// 验证加载新会话后旧会话命令会被拒绝。
    #[tokio::test]
    async fn rejects_commands_from_stale_session() {
        let service = PlayerService::new(Arc::new(StubTransport {
            snapshot: Mutex::new(None),
        }));
        assert!(service.dispatch(load_command("session-1")).await.accepted);

        let result = service
            .dispatch(PlayerCommand {
                command_id: "command-2".to_owned(),
                session_id: "session-2".to_owned(),
                action: PlayerCommandAction::Pause,
            })
            .await;

        assert!(!result.accepted);
        assert!(result.error.is_some());
    }

    /// 验证播放器拒绝非有限跳转位置、未声明倍速和非法字幕大小。
    #[test]
    fn validates_numeric_player_commands() {
        let invalid_seek = PlayerCommand {
            command_id: "command-seek".to_owned(),
            session_id: "session-1".to_owned(),
            action: PlayerCommandAction::Seek {
                position_seconds: f64::NAN,
            },
        };
        let invalid_rate = PlayerCommand {
            command_id: "command-rate".to_owned(),
            session_id: "session-1".to_owned(),
            action: PlayerCommandAction::SetRate { rate: 3.0 },
        };
        let invalid_subtitle_scale = PlayerCommand {
            command_id: "command-subtitle-scale".to_owned(),
            session_id: "session-1".to_owned(),
            action: PlayerCommandAction::SetSubtitleScale {
                subtitle_scale: 130,
            },
        };

        assert!(validate_command(&invalid_seek).is_some());
        assert!(validate_command(&invalid_rate).is_some());
        assert!(validate_command(&invalid_subtitle_scale).is_some());
    }

    #[test]
    fn scheduler_requires_ready_models_and_falls_back_in_order() {
        let budget = EnhancementBudget {
            target_frame_time_ms: 16.67,
            estimated_frame_time_ms: 8.0,
            available_vram_bytes: 2,
            required_vram_bytes: 1,
        };
        let model = TestModel {
            ready: true,
            budget,
        };
        let interpolator = TestInterpolator {
            ready: false,
            budget,
        };
        let scheduler = EnhancementScheduler {
            model: Some(&model),
            interpolator: Some(&interpolator),
            shader_available: true,
        };
        assert_eq!(
            scheduler.decide(EnhancementStage::FrameInterpolation),
            EnhancementDecision::ModelSuperResolution
        );

        let over_budget = TestModel {
            ready: true,
            budget: EnhancementBudget {
                estimated_frame_time_ms: 20.0,
                ..budget
            },
        };
        let scheduler = EnhancementScheduler {
            model: Some(&over_budget),
            interpolator: None,
            shader_available: true,
        };
        assert_eq!(
            scheduler.decide(EnhancementStage::ModelSuperResolution),
            EnhancementDecision::Shader
        );
    }

    #[test]
    fn validates_model_manifest_and_bounds_frame_queue() {
        let manifest = EnhancementModelManifest {
            model_id: "rife-v4".to_owned(),
            backend: "onnx-runtime".to_owned(),
            weight_sha256: "a".repeat(64),
            input_width: 1920,
            input_height: 1080,
            required_vram_bytes: 1,
            estimated_frame_time_ms: 8,
        };
        assert!(validate_model_manifest(&manifest, 2, 16.67).is_ok());
        assert!(validate_model_manifest(&manifest, 0, 16.67).is_err());

        let mut queue = BoundedFrameQueue::new(2);
        queue.push(1);
        queue.push(2);
        queue.push(3);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.dropped_frames(), 1);
    }

    #[tokio::test]
    async fn validates_model_weight_digest_before_enabling_backend() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("model.bin");
        let bytes = b"model-weight";
        tokio::fs::write(&path, bytes).await.expect("write model");
        let digest = format!("{:x}", Sha256::digest(bytes));
        let manifest = EnhancementModelManifest {
            model_id: "rife-v4".to_owned(),
            backend: "onnx-runtime".to_owned(),
            weight_sha256: digest,
            input_width: 1920,
            input_height: 1080,
            required_vram_bytes: 1,
            estimated_frame_time_ms: 8,
        };
        assert!(validate_model_weight(&path, &manifest, 2, 16.67)
            .await
            .is_ok());

        let invalid = EnhancementModelManifest {
            weight_sha256: "b".repeat(64),
            ..manifest
        };
        assert!(validate_model_weight(&path, &invalid, 2, 16.67)
            .await
            .is_err());
    }

    fn load_command(session_id: &str) -> PlayerCommand {
        PlayerCommand {
            command_id: "command-1".to_owned(),
            session_id: session_id.to_owned(),
            action: PlayerCommandAction::Load {
                source: PlayerMediaSource {
                    task_id: "task-1".to_owned(),
                    file_index: Some(0),
                    title: "测试媒体".to_owned(),
                    anime_title: None,
                    description: None,
                    artwork_uri: None,
                    uri: "ani-player://session-1/media".to_owned(),
                    mode: PlayerMediaMode::Direct,
                    duration_seconds: None,
                    subtitles: Vec::new(),
                },
                start_position_seconds: None,
            },
        }
    }

    fn capabilities() -> PlayerCapabilities {
        PlayerCapabilities {
            backend: PlayerBackend::Libvlc,
            platform: PlayerHostPlatform::TauriDesktop,
            availability: PlayerAvailability::Available,
            can_seek: true,
            can_set_volume: true,
            can_mute: true,
            playback_rates: PLAYBACK_RATES.to_vec(),
            supports_audio_tracks: true,
            supports_subtitle_tracks: true,
            supports_subtitle_scale: true,
            supports_video_enhancement: true,
            supports_frame_interpolation: false,
            supports_model_enhancement: false,
            supports_aspect_ratio: true,
            supports_fullscreen: true,
            supports_picture_in_picture: false,
            supports_playlist_navigation: false,
            supports_direct_playback: true,
            supports_transcoding_fallback: false,
            supports_hdr: false,
            unavailable_reason: None,
        }
    }
}
