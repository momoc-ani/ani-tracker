use std::sync::Arc;

use ani_contracts::{
    PlayerCapabilities, PlayerCommand, PlayerCommandAction, PlayerCommandResult, PlayerError,
    PlayerErrorCode, PlayerRecoveryAction, PlayerSnapshot,
};
use async_trait::async_trait;
use tokio::sync::Mutex;

const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const SUBTITLE_SCALES: &[u16] = &[100, 125, 150, 175, 200];

/// 平台播放器 transport 的稳定失败，不泄漏 SDK 或 FFI 类型。
#[derive(Debug, Clone, thiserror::Error)]
pub enum PlayerTransportError {
    #[error("播放器运行时不可用：{0}")]
    Unavailable(String),
    #[error("播放器原生调用失败：{0}")]
    Native(String),
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
        PlayerTransportError::Native(_) | PlayerTransportError::InvalidResponse(_) => {
            (PlayerErrorCode::Unknown, true)
        }
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
            supports_aspect_ratio: true,
            supports_fullscreen: true,
            supports_picture_in_picture: false,
            supports_playlist_navigation: false,
            supports_direct_playback: true,
            supports_transcoding_fallback: false,
            supports_hdr: true,
            unavailable_reason: None,
        }
    }
}
