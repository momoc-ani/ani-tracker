use serde::{Deserialize, Serialize};

use ani_domain::Anime;

/// 跨语言契约金样的版本化外层结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractFixture<T> {
    pub schema_version: u32,
    pub kind: String,
    pub payload: T,
}

/// 无边框窗口控制区需要的最小窗口状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppWindowState {
    pub maximized: bool,
}

/// Tauri 命令返回给 Renderer 的稳定错误结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCommandError {
    pub code: String,
    pub message: String,
}

/// 本地媒体后台任务当前阶段。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalMediaImportPhase {
    #[default]
    Idle,
    Scanning,
    Matching,
    Importing,
    AwaitingReview,
    Verifying,
    Completed,
    Cancelled,
    Failed,
}

/// 扫描目录中按番剧聚合的一组本地媒体候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMediaImportCandidate {
    pub id: String,
    pub title_hint: String,
    pub relative_directory: String,
    pub file_count: usize,
    pub episode_numbers: Vec<f64>,
    pub confidence: u8,
    pub file_title_consensus: u8,
    pub suggested_anime_id: Option<String>,
    pub alternatives: Vec<Anime>,
    #[serde(default)]
    pub current_associations: Vec<LocalMediaImportAssociation>,
}

/// 本地媒体候选在扫描前已经存在的番剧关联汇总。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMediaImportAssociation {
    pub anime_id: String,
    pub anime_title: String,
    pub file_count: usize,
}

/// Renderer 确认低置信度候选时提交的番剧选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMediaImportSelection {
    pub candidate_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default)]
    pub create_local: bool,
}

/// 本地媒体扫描、导入或校验任务的共享状态。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMediaImportJobStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub phase: LocalMediaImportPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    pub discovered_files: usize,
    pub processed_files: usize,
    pub total_files: usize,
    pub imported_anime_count: usize,
    pub imported_media_count: usize,
    pub available_files: usize,
    pub changed_files: usize,
    pub missing_files: usize,
    pub unavailable_files: usize,
    #[serde(default)]
    pub candidates: Vec<LocalMediaImportCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// 设置页展示的已管理本地媒体目录汇总。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMediaSourceSummary {
    pub root_path: String,
    pub media_count: usize,
    pub available_count: usize,
    pub problem_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scanned_at: Option<String>,
}

/// 当前默认下载服务的实现模式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadServiceMode {
    Embedded,
    Managed,
    External,
}

/// 当前默认下载服务的健康状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadServiceState {
    Online,
    Idle,
    Error,
}

/// 应用壳展示的统一下载服务状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadServiceStatus {
    pub mode: DownloadServiceMode,
    pub state: DownloadServiceState,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_count: Option<usize>,
}

/// 外部 qBittorrent 连接测试结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentConnectionTestResult {
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_count: Option<usize>,
}

/// 托管 qBittorrent-nox 的进程状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QbittorrentManagedStatus {
    pub enabled: bool,
    pub auto_start: bool,
    pub running: bool,
    pub web_ui_url: String,
    pub platform: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stopped_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 内置 torrent-core 的进程和协议状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedTorrentCoreStatus {
    pub enabled: bool,
    pub running: bool,
    pub platform: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_service: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy_blocked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stopped_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 单个桌面媒体工具的解析和版本状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaToolStatus {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 桌面 FFprobe 与 FFmpeg 的统一可用状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopMediaToolsStatus {
    pub ffprobe: MediaToolStatus,
    pub ffmpeg: MediaToolStatus,
}

/// 桌面播放器探测使用的稳定平台枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerRuntimePlatform {
    Windows,
    Macos,
    Linux,
    Other,
}

/// 一条外部播放器配置的实际可用状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerDetectionCandidate {
    pub profile_id: String,
    pub name: String,
    pub configured_path: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
}

/// 当前桌面平台的全部外部播放器探测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerDetectionResult {
    pub platform: PlayerRuntimePlatform,
    pub candidates: Vec<PlayerDetectionCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_executable_path: Option<String>,
}

/// 原生文件选择器选择播放器程序时使用的最小输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectPlayerExecutableInput {
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
}

/// 播放器后端类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerBackend {
    Artplayer,
    Libvlc,
    Mpv,
}

/// GPU 视频增强预设；字幕和 OSD 在该阶段之后由播放器合成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerVideoEnhancement {
    #[default]
    Off,
    Balanced,
    Clear,
}

/// 基于模型的实时补帧模式；只有完成模型运行时接入后才可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerFrameInterpolation {
    #[default]
    Off,
    DisplayResample,
    MotionCompensated,
    RifeRealtime,
}

/// HDR 输出模式；Auto 只有在源、渲染器和显示器能力齐全时才允许开启。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerHdrMode {
    #[default]
    Off,
    Auto,
}

/// HDR 自动输出所需的三项独立能力；三者同时满足前不得声明 HDR 可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerHdrCapabilities {
    #[serde(default)]
    pub source_hdr: bool,
    #[serde(default)]
    pub renderer_hdr: bool,
    #[serde(default)]
    pub display_hdr: bool,
}

impl PlayerHdrCapabilities {
    /// 返回源、渲染器和显示器是否形成完整 HDR 输出链路。
    pub fn available(self) -> bool {
        self.source_hdr && self.renderer_hdr && self.display_hdr
    }
}

/// 当前增强链路的可观测信息，不代表模型一定已加载。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerEnhancementDiagnostics {
    #[serde(default)]
    pub pipeline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_time_ms: Option<f64>,
    #[serde(default)]
    pub dropped_frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(default)]
    pub hdr_capabilities: PlayerHdrCapabilities,
}

/// 播放器所在的平台宿主。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerHostPlatform {
    RemoteWeb,
    TauriDesktop,
    Android,
    Ios,
}

/// 播放器运行时可用状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerAvailability {
    Unknown,
    Available,
    Unavailable,
}

/// 播放器生命周期与播放状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerStatus {
    Idle,
    Loading,
    Ready,
    Buffering,
    Playing,
    Paused,
    Ended,
    Error,
    Closed,
}

/// 播放器错误的稳定分类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerErrorCode {
    ResourceUnavailable,
    Network,
    Decoder,
    Permission,
    Transcode,
    RuntimeMissing,
    Unsupported,
    Unknown,
}

/// 播放失败后可展示的恢复动作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayerRecoveryAction {
    Retry,
    Transcode,
    Close,
}

/// 跨平台播放器的结构化错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerError {
    pub code: PlayerErrorCode,
    pub message: String,
    pub recoverable: bool,
    pub recovery_actions: Vec<PlayerRecoveryAction>,
}

/// 画面比例选项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerAspectRatio {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "16:9")]
    Ratio16x9,
    #[serde(rename = "4:3")]
    Ratio4x3,
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "fit")]
    Fit,
    #[serde(rename = "custom")]
    Custom,
}

/// 播放器稳定公开的能力集合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCapabilities {
    pub backend: PlayerBackend,
    pub platform: PlayerHostPlatform,
    pub availability: PlayerAvailability,
    pub can_seek: bool,
    pub can_set_volume: bool,
    pub can_mute: bool,
    pub playback_rates: Vec<f64>,
    pub supports_audio_tracks: bool,
    pub supports_subtitle_tracks: bool,
    #[serde(default)]
    pub supports_subtitle_scale: bool,
    #[serde(default)]
    pub supports_video_enhancement: bool,
    #[serde(default)]
    pub supports_frame_interpolation: bool,
    #[serde(default)]
    pub supports_model_enhancement: bool,
    pub supports_aspect_ratio: bool,
    pub supports_fullscreen: bool,
    pub supports_picture_in_picture: bool,
    pub supports_playlist_navigation: bool,
    pub supports_direct_playback: bool,
    pub supports_transcoding_fallback: bool,
    pub supports_hdr: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// 外挂字幕来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSubtitleSource {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "type")]
    pub subtitle_type: PlayerSubtitleType,
    pub uri: String,
    pub default: bool,
}

/// 播放器支持的字幕格式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerSubtitleType {
    Ass,
    Vtt,
}

/// 播放器加载的受控媒体来源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerMediaSource {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u32>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_uri: Option<String>,
    pub uri: String,
    pub mode: PlayerMediaMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    pub subtitles: Vec<PlayerSubtitleSource>,
}

/// 媒体交付模式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerMediaMode {
    Direct,
    Hls,
}

/// 音频或字幕轨道类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerTrackKind {
    Audio,
    Subtitle,
}

/// 当前可选择的音频或字幕轨道。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerTrack {
    pub id: String,
    pub kind: PlayerTrackKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub selected: bool,
}

/// 播放列表中的单集。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPlaylistItem {
    pub id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u32>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

/// 当前播放列表和活动项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPlaylist {
    pub items: Vec<PlayerPlaylistItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_item_id: Option<String>,
}

/// 播放器命令的公共信封，动作字段会平铺到 JSON 顶层。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCommand {
    pub command_id: String,
    pub session_id: String,
    #[serde(flatten)]
    pub action: PlayerCommandAction,
}

/// 所有原生播放器后端必须识别的动作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum PlayerCommandAction {
    Load {
        source: PlayerMediaSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_position_seconds: Option<f64>,
    },
    Play,
    Pause,
    Seek {
        position_seconds: f64,
    },
    SetVolume {
        volume: f64,
    },
    SetMuted {
        muted: bool,
    },
    SetRate {
        rate: f64,
    },
    SelectAudioTrack {
        track_id: String,
    },
    SelectSubtitleTrack {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track_id: Option<String>,
    },
    SetSubtitleScale {
        subtitle_scale: u16,
    },
    SetVideoEnhancement {
        video_enhancement: PlayerVideoEnhancement,
    },
    SetFrameInterpolation {
        frame_interpolation: PlayerFrameInterpolation,
    },
    SetHdr {
        hdr: PlayerHdrMode,
    },
    SetAspectRatio {
        aspect_ratio: PlayerAspectRatio,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    SetFullscreen {
        fullscreen: bool,
    },
    SetPictureInPicture {
        enabled: bool,
    },
    PreviousItem,
    NextItem,
    Retry,
    Close,
}

impl PlayerCommand {
    /// 返回动作的稳定短名，供日志和能力检查使用。
    pub fn action_name(&self) -> &'static str {
        match &self.action {
            PlayerCommandAction::Load { .. } => "load",
            PlayerCommandAction::Play => "play",
            PlayerCommandAction::Pause => "pause",
            PlayerCommandAction::Seek { .. } => "seek",
            PlayerCommandAction::SetVolume { .. } => "set-volume",
            PlayerCommandAction::SetMuted { .. } => "set-muted",
            PlayerCommandAction::SetRate { .. } => "set-rate",
            PlayerCommandAction::SelectAudioTrack { .. } => "select-audio-track",
            PlayerCommandAction::SelectSubtitleTrack { .. } => "select-subtitle-track",
            PlayerCommandAction::SetSubtitleScale { .. } => "set-subtitle-scale",
            PlayerCommandAction::SetVideoEnhancement { .. } => "set-video-enhancement",
            PlayerCommandAction::SetFrameInterpolation { .. } => "set-frame-interpolation",
            PlayerCommandAction::SetHdr { .. } => "set-hdr",
            PlayerCommandAction::SetAspectRatio { .. } => "set-aspect-ratio",
            PlayerCommandAction::SetFullscreen { .. } => "set-fullscreen",
            PlayerCommandAction::SetPictureInPicture { .. } => "set-picture-in-picture",
            PlayerCommandAction::PreviousItem => "previous-item",
            PlayerCommandAction::NextItem => "next-item",
            PlayerCommandAction::Retry => "retry",
            PlayerCommandAction::Close => "close",
        }
    }
}

/// 原生播放器对单条命令的结构化响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCommandResult {
    pub command_id: String,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PlayerError>,
}

/// 跨平台播放器的完整状态快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub session_id: String,
    pub sequence: u64,
    pub backend: PlayerBackend,
    pub platform: PlayerHostPlatform,
    pub status: PlayerStatus,
    pub capabilities: PlayerCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlayerMediaSource>,
    pub playlist: PlayerPlaylist,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub buffered_seconds: f64,
    pub volume: f64,
    pub muted: bool,
    pub playback_rate: f64,
    pub audio_tracks: Vec<PlayerTrack>,
    pub subtitle_tracks: Vec<PlayerTrack>,
    #[serde(default = "default_subtitle_scale")]
    pub subtitle_scale: u16,
    #[serde(default)]
    pub video_enhancement: PlayerVideoEnhancement,
    #[serde(default)]
    pub video_enhancement_degraded: bool,
    #[serde(default)]
    pub frame_interpolation: PlayerFrameInterpolation,
    #[serde(default)]
    pub hdr: PlayerHdrMode,
    #[serde(default)]
    pub enhancement_diagnostics: PlayerEnhancementDiagnostics,
    pub aspect_ratio: PlayerAspectRatio,
    pub fullscreen: bool,
    pub picture_in_picture: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PlayerError>,
}

/// 兼容旧播放器快照缺少字幕缩放字段的情况。
const fn default_subtitle_scale() -> u16 {
    100
}

/// 创建桌面播放器窗口时使用的受限目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPlayerWindowInput {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u32>,
}

/// 桌面透明控制层发送的受限窗口拖动阶段。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum DesktopPlayerWindowDragInput {
    Start { screen_x: f64, screen_y: f64 },
    Move { screen_x: f64, screen_y: f64 },
    End,
}

/// 创建受控播放会话时使用的下载文件目标。
pub type DesktopPlaybackSessionInput = DesktopPlayerWindowInput;

/// 播放器可加载的一条受控字幕资源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSubtitle {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "type")]
    pub subtitle_type: PlayerSubtitleType,
    pub url: String,
    pub default: bool,
}

/// Renderer 获取的受控播放会话，不泄漏真实本地路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSession {
    pub id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<u32>,
    pub file_name: String,
    pub mode: PlayerMediaMode,
    pub stream_url: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_position_seconds: Option<f64>,
    pub subtitles: Vec<PlaybackSubtitle>,
}

/// 已配对远程设备的公开信息，不包含令牌摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDeviceInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_accessed_at: Option<String>,
}

/// 桌面远程 HTTPS 网关和证书的当前状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteGatewayStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub protocol: String,
    pub lan_enabled: bool,
    pub base_url: String,
    pub addresses: Vec<String>,
    pub devices: Vec<RemoteDeviceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<RemoteCertificateInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 远程 HTTPS 服务端证书的可公开元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCertificateInfo {
    pub fingerprint: String,
    pub expires_at: String,
    pub authority_certificate_path: String,
}

/// 桌面端生成的短期一次性远程配对挑战。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingChallenge {
    pub code: String,
    pub expires_at: String,
}

/// 本地 Renderer 获取的签名图片缓存地址。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCacheResolveResult {
    pub url: String,
}

/// 远程浏览器可加载的文本字幕。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaybackSubtitle {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(rename = "type")]
    pub subtitle_type: String,
    pub url: String,
    pub default: bool,
}

/// 远程 AI 插帧在当前源帧率、模型性能、显存和输出上限下的容量结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInterpolationCapacity {
    pub source_frame_rate: f64,
    pub target_frame_rate: f64,
    pub selected_multiplier: u8,
    pub max_feasible_multiplier: u8,
    pub output_frame_rate_cap: f64,
    pub interval_budget_ms: f64,
    pub estimated_interval_cost_ms: f64,
    pub interpolation_p95_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhancement_p95_ms: Option<f64>,
    pub latency_sample_count: u64,
}

/// 远程媒体会话实际采用的传输与增强路径。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemotePlaybackPath {
    #[default]
    Direct,
    DirectEnhanced,
    Hls,
}

/// 远程浏览器中 WebCodecs/WebGPU 直传增强的生命周期状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteDirectEnhancementStatus {
    #[default]
    Idle,
    Probing,
    Starting,
    Active,
    Degraded,
}

/// 远程浏览器提交的直传增强能力和有界运行指标。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteDirectEnhancementDiagnostics {
    pub sequence: u64,
    pub status: RemoteDirectEnhancementStatus,
    #[serde(default)]
    pub capability_supported: bool,
    #[serde(default)]
    pub web_codecs: bool,
    #[serde(default)]
    pub audio_web_codecs: bool,
    #[serde(default)]
    pub audio_context: bool,
    #[serde(default)]
    pub shader: bool,
    #[serde(default)]
    pub web_gpu: bool,
    #[serde(default)]
    pub offscreen_canvas: bool,
    #[serde(default)]
    pub media_capabilities: bool,
    #[serde(default)]
    pub supported_codecs: Vec<String>,
    #[serde(default)]
    pub smooth_codecs: Vec<String>,
    #[serde(default)]
    pub power_efficient_codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_preset: Option<PlayerVideoEnhancement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_preset: Option<PlayerVideoEnhancement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_clock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_audio_track: Option<bool>,
    #[serde(default)]
    pub rendered_frames: u64,
    #[serde(default)]
    pub dropped_frames: u64,
    #[serde(default)]
    pub dropped_frame_ratio: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_budget_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_queue_p95_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_av_drift_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_av_drift_ms: Option<f64>,
    #[serde(default)]
    pub range_request_count: u64,
    #[serde(default)]
    pub received_range_bytes: u64,
    #[serde(default)]
    pub range_retry_count: u64,
    #[serde(default)]
    pub recovered_range_count: u64,
    #[serde(default)]
    pub network_failure_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_estimated_working_set_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_resource_budget_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
}

/// 远程增强输出的实际传输和编码诊断，不代表请求一定使用了硬件编码。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaybackDiagnostics {
    #[serde(default)]
    pub playback_path: RemotePlaybackPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(default)]
    pub encoder_degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_mode: Option<String>,
    #[serde(default)]
    pub enhanced_frame_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_backend: Option<String>,
    #[serde(default)]
    pub video_enhancement: PlayerVideoEnhancement,
    #[serde(default)]
    pub frame_interpolation: PlayerFrameInterpolation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpolation_capacity: Option<RemoteInterpolationCapacity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_enhancement: Option<RemoteDirectEnhancementDiagnostics>,
}

/// 远程转码实际请求的像素处理链；直传模式必须保持关闭。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaybackEnhancement {
    #[serde(default)]
    pub video_enhancement: PlayerVideoEnhancement,
    #[serde(default)]
    pub frame_interpolation: PlayerFrameInterpolation,
}

/// 远程设备的短期受控播放会话，不暴露本地路径。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlaybackSession {
    pub id: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<i64>,
    pub file_name: String,
    pub mode: String,
    pub stream_url: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_position_seconds: Option<f64>,
    #[serde(default)]
    pub stream_start_position_seconds: f64,
    pub subtitles: Vec<RemotePlaybackSubtitle>,
    #[serde(default)]
    pub diagnostics: RemotePlaybackDiagnostics,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{
        ContractFixture, DesktopMediaToolsStatus, DownloadServiceMode, DownloadServiceState,
        DownloadServiceStatus, EmbeddedTorrentCoreStatus, ImageCacheResolveResult, PlaybackSession,
        PlayerCommand, PlayerCommandAction, PlayerCommandResult, PlayerDetectionResult,
        PlayerHdrCapabilities, PlayerHostPlatform, PlayerSnapshot, PlayerStatus,
        QbittorrentManagedStatus, RemoteGatewayStatus, RemotePairingChallenge, RemotePlaybackPath,
        RemotePlaybackSession, TorrentConnectionTestResult,
    };

    #[test]
    fn hdr_requires_source_renderer_and_display_capabilities() {
        assert!(!PlayerHdrCapabilities {
            source_hdr: true,
            renderer_hdr: true,
            display_hdr: false,
        }
        .available());
        assert!(PlayerHdrCapabilities {
            source_hdr: true,
            renderer_hdr: true,
            display_hdr: true,
        }
        .available());
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DownloadServiceFixture {
        service_status: DownloadServiceStatus,
        connection_test: TorrentConnectionTestResult,
        managed_status: QbittorrentManagedStatus,
        embedded_status: EmbeddedTorrentCoreStatus,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MediaFixture {
        media_tools_status: DesktopMediaToolsStatus,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MobileTorrentLifecycleFixture {
        android_status: EmbeddedTorrentCoreStatus,
        ios_status: EmbeddedTorrentCoreStatus,
        execute_request: serde_json::Value,
        execute_response: serde_json::Value,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PlayerCommandFixture {
        load_command: PlayerCommand,
        rejected_result: PlayerCommandResult,
        subtitle_scale_command: PlayerCommand,
        android_capabilities: super::PlayerCapabilities,
        ios_capabilities: super::PlayerCapabilities,
        playback_session: PlaybackSession,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RemoteGatewayFixture {
        gateway_status: RemoteGatewayStatus,
        pairing_challenge: RemotePairingChallenge,
        image_cache: ImageCacheResolveResult,
        playback_session: RemotePlaybackSession,
    }

    /// 验证 Rust 可读取 P6 外部播放器探测金样。
    #[test]
    fn decodes_external_player_fixture() {
        let fixture: ContractFixture<PlayerDetectionResult> = serde_json::from_str(include_str!(
            "../../../fixtures/contracts/p6-external-player.v1.json"
        ))
        .expect("external player fixture");
        assert_eq!(fixture.kind, "p6-external-player");
        assert_eq!(fixture.payload.candidates.len(), 2);
        assert_eq!(fixture.payload.detected_profile_id.as_deref(), Some("mpv"));
    }

    /// 验证 Rust 与 TypeScript 共用远程状态、配对、图片和媒体会话字段。
    #[test]
    fn decodes_remote_gateway_fixture() {
        let fixture: ContractFixture<RemoteGatewayFixture> = serde_json::from_str(include_str!(
            "../../../fixtures/contracts/p6-remote-gateway.v1.json"
        ))
        .expect("remote gateway fixture");
        assert_eq!(fixture.kind, "p6-remote-gateway");
        assert!(fixture.payload.gateway_status.running);
        assert_eq!(fixture.payload.gateway_status.protocol, "https");
        assert_eq!(fixture.payload.pairing_challenge.code, "123456");
        assert!(fixture.payload.image_cache.url.starts_with("https://"));
        assert_eq!(fixture.payload.playback_session.mode, "direct");
        assert_eq!(
            fixture.payload.playback_session.diagnostics.playback_path,
            RemotePlaybackPath::Direct
        );
    }

    /// 验证 Rust 能严格解码前端共用的播放器快照金样。
    #[test]
    fn decodes_player_snapshot_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/player-snapshot.v1.json"
        ));
        let decoded: ContractFixture<PlayerSnapshot> =
            serde_json::from_str(fixture).expect("player snapshot fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "player-snapshot");
        assert_eq!(decoded.payload.platform, PlayerHostPlatform::TauriDesktop);
        assert_eq!(decoded.payload.status, PlayerStatus::Playing);
        assert_eq!(decoded.payload.sequence, 7);
        assert_eq!(decoded.payload.audio_tracks.len(), 2);
        assert_eq!(decoded.payload.subtitle_scale, 150);
        assert_eq!(decoded.payload.subtitle_tracks.len(), 1);
    }

    /// 验证 Rust 能严格解码下载服务、托管进程和内置核心状态金样。
    #[test]
    fn decodes_download_service_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p4-download-service-model.v1.json"
        ));
        let decoded: ContractFixture<DownloadServiceFixture> =
            serde_json::from_str(fixture).expect("download service fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p4-download-service-model");
        assert_eq!(
            decoded.payload.service_status.mode,
            DownloadServiceMode::Managed
        );
        assert_eq!(
            decoded.payload.service_status.state,
            DownloadServiceState::Online
        );
        assert!(decoded.payload.connection_test.ok);
        assert!(decoded.payload.managed_status.running);
        assert_eq!(decoded.payload.embedded_status.listen_port, Some(6881));
    }

    /// 验证 Rust 能解码桌面 FFprobe 与 FFmpeg 状态金样。
    #[test]
    fn decodes_media_tools_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p4-media-model.v1.json"
        ));
        let decoded: ContractFixture<MediaFixture> =
            serde_json::from_str(fixture).expect("media tools fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p4-media-model");
        assert!(decoded.payload.media_tools_status.ffprobe.available);
        assert!(decoded.payload.media_tools_status.ffmpeg.available);
    }

    /// 验证 Android 前台服务与 iOS Session 使用同一生命周期契约。
    #[test]
    fn decodes_mobile_torrent_lifecycle_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p4-mobile-torrent-lifecycle.v1.json"
        ));
        let decoded: ContractFixture<MobileTorrentLifecycleFixture> =
            serde_json::from_str(fixture).expect("mobile torrent fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p4-mobile-torrent-lifecycle");
        assert_eq!(
            decoded.payload.android_status.foreground_service,
            Some(true)
        );
        assert_eq!(decoded.payload.ios_status.foreground_service, Some(false));
        assert_eq!(decoded.payload.execute_request["method"], "listTasks");
        assert_eq!(decoded.payload.execute_response["ok"], "true");
    }

    /// 验证 Rust 与 TypeScript 共用平铺播放器命令和受控会话字段。
    #[test]
    fn decodes_player_command_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p5-player-command.v1.json"
        ));
        let decoded: ContractFixture<PlayerCommandFixture> =
            serde_json::from_str(fixture).expect("player command fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p5-player-command");
        assert!(matches!(
            &decoded.payload.load_command.action,
            PlayerCommandAction::Load { .. }
        ));
        assert_eq!(decoded.payload.load_command.action_name(), "load");
        assert!(matches!(
            &decoded.payload.subtitle_scale_command.action,
            PlayerCommandAction::SetSubtitleScale {
                subtitle_scale: 150
            }
        ));
        assert!(!decoded.payload.rejected_result.accepted);
        assert_eq!(
            decoded.payload.android_capabilities.platform,
            PlayerHostPlatform::Android
        );
        assert_eq!(
            decoded.payload.ios_capabilities.platform,
            PlayerHostPlatform::Ios
        );
        assert!(
            !decoded
                .payload
                .ios_capabilities
                .supports_transcoding_fallback
        );
        assert_eq!(decoded.payload.playback_session.file_index, Some(0));
    }
}
