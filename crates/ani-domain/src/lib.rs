use serde::{Deserialize, Serialize};
use serde_json::Value;

mod download_path;

pub use download_path::resolve_anime_download_path;

/// 追番状态，与 TypeScript `AnimeStatus` 契约保持一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimeStatus {
    Watching,
    Planned,
    Completed,
    Paused,
    Dropped,
}

/// 单集生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeStatus {
    Upcoming,
    Aired,
    Matched,
    Downloading,
    Downloaded,
    Watched,
}

/// 下载任务生命周期状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    FetchingMetadata,
    Downloading,
    Stalled,
    WaitingNetwork,
    Paused,
    Checking,
    Moving,
    Completed,
    Seeding,
    Error,
    MissingFiles,
}

impl DownloadStatus {
    /// 判断状态是否仍处于下载活动生命周期。
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::FetchingMetadata
                | Self::Downloading
                | Self::Stalled
                | Self::WaitingNetwork
                | Self::Paused
                | Self::Checking
                | Self::Moving
        )
    }

    /// 判断状态是否明确表示下载数据已完成。
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed | Self::Seeding)
    }
}

/// 下载任务使用的引擎类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TorrentEngineKind {
    Embedded,
    Qbittorrent,
}

impl TorrentEngineKind {
    /// 返回跨存储与协议稳定使用的下载引擎标识。
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Qbittorrent => "qbittorrent",
        }
    }

    /// 将引擎内任务标识转换为应用内唯一任务标识。
    pub fn scope_task_id(&self, engine_task_id: &str) -> String {
        let prefix = format!("{}:", self.as_key());
        if engine_task_id.starts_with(&prefix) {
            engine_task_id.to_owned()
        } else {
            format!("{prefix}{engine_task_id}")
        }
    }

    /// 从应用任务标识中取回下载引擎使用的原始标识。
    pub fn unscoped_task_id<'a>(&self, task_id: &'a str) -> &'a str {
        task_id
            .strip_prefix(self.as_key())
            .and_then(|value| value.strip_prefix(':'))
            .unwrap_or(task_id)
    }
}

/// 番剧别名语言，与现有目录数据保持一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimeAliasLanguage {
    Zh,
    Ja,
    En,
    Romaji,
    Custom,
}

/// 番剧目录别名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeAlias {
    pub id: String,
    pub anime_id: String,
    pub alias: String,
    pub language: AnimeAliasLanguage,
    pub priority: i64,
}

/// 番剧评分摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeRating {
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    pub source: String,
}

/// 本地识别到的字幕组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FansubGroup {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub source_ids: Vec<String>,
}

/// 下载源类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Rss,
    Torznab,
    SiteAdapter,
    Manual,
}

/// 下载源配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSourceConfig {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub enabled: bool,
    #[serde(default)]
    pub use_proxy: bool,
    #[serde(default = "default_source_request_interval_ms")]
    pub request_interval_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// 返回下载源默认请求间隔。
fn default_source_request_interval_ms() -> i64 {
    1_500
}

/// 单个下载源的请求和同步游标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSourceSyncState {
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request_at: Option<String>,
    #[serde(default)]
    pub request_failure_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// 单个下载源增量同步失败信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncError {
    pub source_id: String,
    pub message: String,
}

/// 一次下载源增量同步的稳定结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncRunResult {
    pub started_at: String,
    pub finished_at: String,
    pub synced_source_ids: Vec<String>,
    pub skipped_source_ids: Vec<String>,
    pub added_release_count: usize,
    pub errors: Vec<SourceSyncError>,
}

/// 下载源每日同步调度器的当前状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSyncSchedulerStatus {
    pub enabled: bool,
    pub running: bool,
    pub in_flight: bool,
    pub daily_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<SourceSyncRunResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 自动扫描已加入下载队列的单集摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDownloadedItem {
    pub anime_id: String,
    pub anime_title: String,
    pub episode_id: String,
    pub episode_no: f64,
    pub release_id: String,
    pub release_title: String,
    pub download_task_id: String,
}

/// 自动扫描跳过的番剧或单集摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSkippedItem {
    pub anime_id: String,
    pub anime_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    pub reason: String,
}

/// 自动扫描失败摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    pub message: String,
}

/// 一次自动扫描的稳定结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunResult {
    pub started_at: String,
    pub finished_at: String,
    pub checked_episodes: usize,
    pub downloaded: Vec<AutomationDownloadedItem>,
    pub skipped: Vec<AutomationSkippedItem>,
    pub errors: Vec<AutomationRunError>,
}

/// 自动扫描调度器的当前状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationSchedulerStatus {
    pub enabled: bool,
    pub running: bool,
    pub in_flight: bool,
    pub interval_minutes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_cooldown_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<AutomationRunResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 通用网络请求熔断状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCircuitState {
    pub key: String,
    pub group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request_at: Option<String>,
    #[serde(default)]
    pub failure_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_until: Option<String>,
}

/// 番剧来源绑定的建立方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimeSourceBindingMatchMethod {
    Manual,
    ExternalId,
    Scored,
}

/// 本地番剧与下载源番剧标识之间的稳定绑定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSourceBinding {
    pub id: String,
    pub anime_id: String,
    pub source_id: String,
    pub source_anime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_anime_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub match_method: AnimeSourceBindingMatchMethod,
    pub confidence: f64,
    pub confirmed: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 来源候选排除的作用域。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimeSourceExclusionScope {
    Candidate,
    Source,
}

/// 用户确认的单候选或整来源排除记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSourceExclusion {
    pub id: String,
    pub anime_id: String,
    pub source_id: String,
    pub scope: AnimeSourceExclusionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_anime_title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 下载源返回的待确认番剧候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSourceCandidate {
    pub source_id: String,
    pub source_name: String,
    pub source_anime_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premiere_year: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premiere_month: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub score: i64,
    pub reasons: Vec<String>,
}

/// 来源绑定页展示的已排除来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedAnimeSource {
    pub source_id: String,
    pub source_name: String,
}

/// 来源绑定页的完整聚合状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSourceBindingState {
    pub anime_id: String,
    pub bindings: Vec<AnimeSourceBinding>,
    pub candidates: Vec<AnimeSourceCandidate>,
    pub excluded_sources: Vec<ExcludedAnimeSource>,
    pub errors: Vec<ReleaseSearchError>,
}

/// 用户确认来源绑定的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmAnimeSourceBindingInput {
    pub anime_id: String,
    pub source_id: String,
    pub source_anime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_anime_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// 用户确认来源候选不匹配的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportAnimeSourceCandidateMismatchInput {
    pub anime_id: String,
    pub source_id: String,
    pub source_anime_id: String,
    pub source_anime_title: String,
    pub score: f64,
    pub reasons: Vec<String>,
}

/// 设置整来源排除状态的输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAnimeSourceExclusionInput {
    pub anime_id: String,
    pub source_id: String,
    pub excluded: bool,
}

/// 撤销单个错误候选记录的输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveAnimeSourceCandidateMismatchInput {
    pub anime_id: String,
    pub source_id: String,
    pub source_anime_id: String,
}

/// 资源包含的内容形态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseContentKind {
    Episode,
    Range,
    Batch,
    Unknown,
}

/// 资源标题声明的标准视频编码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizedVideoCodec {
    #[serde(rename = "H.264/AVC")]
    H264Avc,
    #[serde(rename = "H.265/HEVC")]
    H265Hevc,
    #[serde(rename = "AV1")]
    Av1,
    #[serde(rename = "VP9")]
    Vp9,
    #[serde(rename = "Unknown")]
    Unknown,
}

impl NormalizedVideoCodec {
    /// 返回与 TypeScript 偏好字段一致的稳定文本。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::H264Avc => "H.264/AVC",
            Self::H265Hevc => "H.265/HEVC",
            Self::Av1 => "AV1",
            Self::Vp9 => "VP9",
            Self::Unknown => "Unknown",
        }
    }
}

/// 资源声明的标准分辨率。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseResolution {
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "2160p")]
    P2160,
}

impl ReleaseResolution {
    /// 返回前端契约使用的分辨率文本。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P2160 => "2160p",
        }
    }
}

/// 可明确识别的字幕语言。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleLanguage {
    Chs,
    Cht,
    Jpn,
    Eng,
}

/// 兼容旧数据的单值字幕描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitlePreference {
    Chs,
    Cht,
    Jpn,
    Eng,
    Multi,
}

/// 连集或合集资源覆盖的集数范围。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseEpisodeRange {
    pub start: f64,
    pub end: f64,
}

/// 下载源附带的可复用来源元数据。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSourceMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mikan_bangumi_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mikan_subgroup_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mikan_subgroup_name: Option<String>,
}

/// 跨来源归一化后的下载资源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_range: Option<ReleaseEpisodeRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_season_no: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_kind: Option<ReleaseContentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_name: Option<String>,
    pub source_id: String,
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magnet_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ReleaseResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_video_codec: Option<NormalizedVideoCodec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub subtitle_languages: Vec<SubtitleLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<SubtitlePreference>,
    pub published_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_meta: Option<ReleaseSourceMeta>,
}

/// 任意关键词资源搜索输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseQuery {
    pub keyword: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl_ms: Option<u64>,
    #[serde(default)]
    pub force_refresh: bool,
}

/// 按本地番剧上下文搜索资源的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeReleaseQuery {
    pub anime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl_ms: Option<u64>,
    #[serde(default)]
    pub force_refresh: bool,
}

/// 单个下载源的资源搜索结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSourceSearchResult {
    pub source_id: String,
    pub source_name: String,
    pub releases: Vec<Release>,
}

/// 单个下载源的可展示搜索错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSearchError {
    pub source_id: String,
    pub message: String,
}

/// 多下载源聚合后的资源搜索结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSearchResult {
    pub query: ReleaseQuery,
    pub releases: Vec<Release>,
    pub source_results: Vec<ReleaseSourceSearchResult>,
    pub searched_source_ids: Vec<String>,
    pub errors: Vec<ReleaseSearchError>,
}

/// 单条追番 RSS 订阅资源搜索输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssSubscriptionReleaseQuery {
    pub anime_id: String,
    pub subscription_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// 单条追番 RSS 订阅资源搜索结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssSubscriptionReleaseResult {
    pub query: RssSubscriptionReleaseQuery,
    pub releases: Vec<Release>,
    pub errors: Vec<ReleaseSearchError>,
}

/// 资源评分所需的追番与单集上下文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMatchContext {
    pub anime: MyAnime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_fansub_override_id: Option<String>,
    #[serde(default)]
    pub candidate_fansub_group_ids: Vec<String>,
    #[serde(default)]
    pub candidate_fansub_names: Vec<String>,
}

/// 单条资源的匹配、偏好和可用性评分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMatchResult {
    pub release: Release,
    pub score: i64,
    pub match_score: i64,
    pub preference_score: i64,
    pub availability_score: i64,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
}

/// 单集规则页展示的候选资源评分预览。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeReleasePreview {
    pub anime_id: String,
    pub episode_id: String,
    pub searched_terms: Vec<String>,
    pub candidates: Vec<ReleaseMatchResult>,
    pub errors: Vec<ReleaseSearchError>,
}

/// 自动下载对最高分候选的最终判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticDownloadDecision {
    pub accepted: bool,
    pub reason: String,
}

/// 首页和追番列表需要的完整番剧目录记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anime {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    pub aliases: Vec<AnimeAlias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
    pub premiere_year: i64,
    pub premiere_month: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<AnimeRating>,
    pub external_ids: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

/// 单部追番的 RSS 订阅设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeRssSubscription {
    pub id: String,
    pub my_anime_id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    #[serde(default)]
    pub preferred_subtitle_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_interval_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetched_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 我的追番只读记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyAnime {
    pub id: String,
    pub anime: Anime,
    pub status: AnimeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_fansub_group_id: Option<String>,
    pub auto_download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_dir: Option<String>,
    #[serde(default)]
    pub rss_subscriptions: Vec<AnimeRssSubscription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_bit_depth: Option<i64>,
    #[serde(default)]
    pub preferred_subtitle_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_subtitle: Option<String>,
    pub added_at: String,
    pub updated_at: String,
}

/// 月度新番采集输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoveryQuery {
    pub year: i64,
    pub month: i64,
    #[serde(default)]
    pub force_refresh: bool,
}

/// 季度新番采集输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoverySeasonQuery {
    pub year: i64,
    pub season: String,
    #[serde(default)]
    pub force_refresh: bool,
}

/// 月度新番采集及持久化结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoveryResult {
    pub query: AnimeDiscoveryQuery,
    pub items: Vec<Anime>,
    pub added_count: usize,
    pub existing_count: usize,
    pub source: String,
    pub errors: Vec<String>,
}

/// 季度新番采集及持久化结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoverySeasonResult {
    pub query: AnimeDiscoverySeasonQuery,
    pub items: Vec<Anime>,
    pub added_count: usize,
    pub existing_count: usize,
    pub source: String,
    pub errors: Vec<String>,
}

/// 一次季度后台同步的紧凑结果，避免状态轮询传输完整番剧列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoverySyncTaskResult {
    pub query: AnimeDiscoverySeasonQuery,
    pub item_count: usize,
    pub added_count: usize,
    pub existing_count: usize,
    pub error_count: usize,
}

/// 新番季度后台同步当前所处阶段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnimeDiscoverySyncPhase {
    Catalog,
    Details,
}

/// 新番季度同步调度器的当前任务状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoverySyncTaskStatus {
    pub in_flight: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AnimeDiscoverySyncPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_query: Option<AnimeDiscoverySeasonQuery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_finished_at: Option<String>,
    #[serde(default)]
    pub detail_completed_count: usize,
    #[serde(default)]
    pub detail_total_count: usize,
    #[serde(default)]
    pub detail_error_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<AnimeDiscoverySyncTaskResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 单个自然季度的新番目录后台同步状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeSeasonSyncState {
    pub year: i64,
    pub season: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_anilist_error: Option<String>,
}

/// 单部番剧在单个元数据来源上的周期详情刷新状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDetailRefreshState {
    pub anime_id: String,
    pub provider: String,
    pub external_id: String,
    pub slot_day: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_cycle: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(default)]
    pub failure_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<String>,
}

/// 新番关键词搜索的本地与在线聚合结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDiscoverySearchResult {
    pub keyword: String,
    pub items: Vec<Anime>,
    pub source: String,
    pub errors: Vec<String>,
}

/// 番剧详情刷新中的单来源错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDetailPartialError {
    pub source: String,
    pub message: String,
}

/// 详情页使用的本地番剧聚合结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeDetailResult {
    pub anime: Anime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_anime: Option<MyAnime>,
    pub episodes: Vec<Episode>,
    pub fansub_groups: Vec<FansubGroup>,
    pub stale: bool,
    pub partial_errors: Vec<AnimeDetailPartialError>,
}

/// 番剧单集记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: String,
    pub anime_id: String,
    pub episode_no: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_time: Option<String>,
    pub status: EpisodeStatus,
}

/// 单集级字幕组与资源覆盖偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodePreference {
    pub id: String,
    pub anime_id: String,
    pub episode_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    pub is_manual_override: bool,
}

/// 单部追番的连续观看进度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimeWatchProgress {
    pub anime_id: String,
    pub watched_episode_count: i64,
    pub total_episode_count: i64,
}

/// 原子更新单部追番观看进度的输入。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAnimeWatchProgressInput {
    pub anime_id: String,
    pub watched_episode_count: i64,
}

/// 播放器按下载任务上报观看百分比的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportPlaybackProgressInput {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<i64>,
    pub percent: f64,
}

/// 单个下载任务文件的持久化播放位置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackCheckpoint {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<i64>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub completed: bool,
    pub watched_reported: bool,
    pub updated_at: String,
}

/// 播放器保存当前位置时使用的最小输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePlaybackCheckpointInput {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_index: Option<i64>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,
}

/// 下载任务中的单个文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFile {
    pub id: String,
    pub index: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    pub size: i64,
    pub progress: f64,
    pub priority: i64,
    pub selected: bool,
}

/// 首页使用的下载任务快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub subtitle_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_tag: Option<String>,
    pub engine: TorrentEngineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_hash: Option<String>,
    pub name: String,
    pub status: DownloadStatus,
    pub progress: f64,
    pub download_speed: i64,
    pub upload_speed: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<i64>,
    pub save_path: String,
    pub files: Vec<TorrentFile>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

impl DownloadTask {
    /// 判断任务是否已完成，并兼容引擎状态延迟和文件级进度。
    pub fn is_completed(&self) -> bool {
        if matches!(
            &self.status,
            DownloadStatus::Error | DownloadStatus::MissingFiles
        ) {
            return false;
        }
        if self.status.is_completed() {
            return true;
        }

        let selected_files = self
            .files
            .iter()
            .filter(|file| file.selected)
            .collect::<Vec<_>>();
        if !selected_files.is_empty() {
            return selected_files.iter().all(|file| file.progress >= 1.0);
        }
        self.progress >= 1.0
    }

    /// 判断任务是否应计入首页活动下载。
    pub fn is_active(&self) -> bool {
        self.status.is_active() && !self.is_completed()
    }
}

/// 媒体文件的登记来源。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOrigin {
    #[default]
    Download,
    Imported,
}

/// 已登记媒体文件当前可访问状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAvailability {
    #[default]
    Available,
    Changed,
    Missing,
    Unavailable,
}

/// 媒体文件在番剧中的内容类型。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaContentKind {
    Episode,
    Special,
    Ova,
    Oad,
    Opening,
    Ending,
    Pv,
    Cm,
    Extra,
    #[default]
    Unknown,
}

/// 首页最近完成区域使用的媒体文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    pub id: String,
    pub anime_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_task_id: Option<String>,
    #[serde(default)]
    pub content_kind: MediaContentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub special_no: Option<String>,
    pub file_path: String,
    pub file_name: String,
    pub size: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_video_codec: Option<String>,
    pub normalized_video_codec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<i64>,
    pub audio_codecs: Vec<String>,
    pub subtitle_tracks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probed_at: Option<String>,
    #[serde(default)]
    pub origin: MediaOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_modified_at: Option<String>,
    #[serde(default)]
    pub availability: MediaAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_error: Option<String>,
}

/// 应用内通知类别。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationKind {
    Automation,
    Download,
    Reminder,
    System,
}

/// 应用内通知严重程度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationSeverity {
    Info,
    Success,
    Warning,
    Error,
}

/// 提醒中心使用的通知记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationRecord {
    pub id: String,
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    pub severity: NotificationSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_task_id: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
}

/// 首页每日提醒条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyReminderItem {
    pub id: String,
    pub anime_id: String,
    pub anime_title: String,
    pub episode_id: String,
    pub episode_no: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_time: Option<String>,
    pub status: EpisodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_task_id: Option<String>,
}

/// 首页每日提醒汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyReminderSummary {
    pub date: String,
    pub total: usize,
    pub upcoming: usize,
    pub aired: usize,
    pub downloading: usize,
    pub downloaded: usize,
    pub items: Vec<DailyReminderItem>,
}

/// 首页精简单集条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeSummary {
    pub id: String,
    pub anime_title: String,
    pub episode_no: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_time: Option<String>,
    pub status: EpisodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fansub_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_task_id: Option<String>,
}

/// 首页需要人工处理的事项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    pub id: String,
    pub title: String,
    pub description: String,
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_no: Option<f64>,
}

/// 首页周播出计划中的一天。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyScheduleDay {
    pub day: String,
    pub items: Vec<EpisodeSummary>,
}

/// 首页下载源健康状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealth {
    pub source_id: String,
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
}

/// 首页完整聚合数据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardData {
    pub daily_reminder: DailyReminderSummary,
    pub today_episodes: Vec<EpisodeSummary>,
    pub pending_actions: Vec<PendingAction>,
    pub active_downloads: Vec<DownloadTask>,
    pub recent_completed: Vec<MediaFile>,
    pub weekly_schedule: Vec<WeeklyScheduleDay>,
    pub source_health: Vec<SourceHealth>,
}

/// 设置保持版本化 JSON 契约，由平台默认值补齐新增字段。
pub type AppSettings = Value;

/// 标识平台安全存储中的一项凭据，不在 SQLite 保存明文。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretReference {
    pub namespace: String,
    pub key: String,
}

/// 包装敏感字节并阻止调试日志输出明文。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// 从调用方拥有的字节创建敏感值。
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    /// 仅在调用平台安全 API 时借用敏感字节。
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    /// 调试输出始终隐藏凭据内容。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

/// 抽象 DPAPI、Keychain 与 Android Keystore 的平台安全存储端口。
pub trait SecureStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    /// 读取安全存储中的敏感值。
    fn read_secret(&self, reference: &SecretReference) -> Result<Option<SecretValue>, Self::Error>;

    /// 写入或覆盖安全存储中的敏感值。
    fn write_secret(
        &self,
        reference: &SecretReference,
        value: &SecretValue,
    ) -> Result<(), Self::Error>;

    /// 删除安全存储中的敏感值。
    fn delete_secret(&self, reference: &SecretReference) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{
        AnimeDetailResult, AnimeDiscoveryResult, AnimeDiscoverySearchResult,
        AnimeDiscoverySeasonResult, AnimeSourceBinding, AnimeSourceBindingState,
        AnimeSourceCandidate, AnimeSourceExclusion, AnimeStatus, ConfirmAnimeSourceBindingInput,
        DashboardData, Episode, EpisodePreference, EpisodeReleasePreview, MyAnime,
        NotificationKind, NotificationRecord, PlaybackCheckpoint, ReleaseSearchResult,
        ReleaseSourceConfig, ReleaseSourceSyncState, RemoveAnimeSourceCandidateMismatchInput,
        ReportAnimeSourceCandidateMismatchInput, ReportPlaybackProgressInput, RequestCircuitState,
        RssSubscriptionReleaseResult, SavePlaybackCheckpointInput, SecretValue,
        SetAnimeSourceExclusionInput, SetAnimeWatchProgressInput,
    };

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ContractFixture<T> {
        schema_version: u32,
        kind: String,
        payload: T,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P2ReadModelFixture {
        notification: NotificationRecord,
        my_anime: MyAnime,
        dashboard: DashboardData,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P3FollowingWriteModelFixture {
        my_anime: MyAnime,
        episode: Episode,
        preference: EpisodePreference,
        watch_progress_input: SetAnimeWatchProgressInput,
        report_playback_progress_input: ReportPlaybackProgressInput,
        save_playback_checkpoint_input: SavePlaybackCheckpointInput,
        checkpoint: PlaybackCheckpoint,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P3CatalogReadModelFixture {
        search_result: AnimeDiscoverySearchResult,
        detail_result: AnimeDetailResult,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P6DesktopParityFixture {
        month_result: AnimeDiscoveryResult,
        season_result: AnimeDiscoverySeasonResult,
        episode_preview: EpisodeReleasePreview,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P3SourceNetworkModelFixture {
        source: ReleaseSourceConfig,
        sync_state: ReleaseSourceSyncState,
        circuit_state: RequestCircuitState,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P3ReleaseSearchModelFixture {
        search_result: ReleaseSearchResult,
        rss_result: RssSubscriptionReleaseResult,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P3SourceBindingModelFixture {
        binding: AnimeSourceBinding,
        exclusion: AnimeSourceExclusion,
        candidate: AnimeSourceCandidate,
        state: AnimeSourceBindingState,
        confirm_input: ConfirmAnimeSourceBindingInput,
        mismatch_input: ReportAnimeSourceCandidateMismatchInput,
        set_exclusion_input: SetAnimeSourceExclusionInput,
        remove_mismatch_input: RemoveAnimeSourceCandidateMismatchInput,
    }

    /// 验证领域枚举沿用前端现有的 JSON 字面量。
    #[test]
    fn serializes_contract_enums() {
        assert_eq!(
            serde_json::to_string(&AnimeStatus::Watching).expect("serialize anime status"),
            "\"watching\""
        );
        assert_eq!(
            serde_json::to_string(&NotificationKind::System).expect("serialize notification kind"),
            "\"system\""
        );
    }

    /// 验证 Rust 能严格解码与 TypeScript 共用的 P2 只读模型金样。
    #[test]
    fn decodes_p2_read_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p2-read-model.v1.json"
        ));
        let decoded: ContractFixture<P2ReadModelFixture> =
            serde_json::from_str(fixture).expect("p2 read model fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p2-read-model");
        assert_eq!(
            decoded.payload.notification.kind,
            NotificationKind::Download
        );
        assert_eq!(decoded.payload.my_anime.anime.id, "anime-contract-1");
        assert_eq!(decoded.payload.dashboard.daily_reminder.total, 0);
    }

    /// 验证 P3 追番写模型在 Rust 与 TypeScript 间保持字段一致。
    #[test]
    fn decodes_p3_following_write_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        ));
        let decoded: ContractFixture<P3FollowingWriteModelFixture> =
            serde_json::from_str(fixture).expect("p3 following fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p3-following-write-model");
        assert_eq!(decoded.payload.my_anime.anime.id, "anime-p3-1");
        assert_eq!(decoded.payload.episode.episode_no, 1.0);
        assert!(decoded.payload.preference.is_manual_override);
        assert_eq!(
            decoded.payload.watch_progress_input.watched_episode_count,
            1
        );
        assert_eq!(decoded.payload.report_playback_progress_input.percent, 92.0);
        assert_eq!(
            decoded.payload.save_playback_checkpoint_input.file_index,
            Some(0)
        );
        assert!(decoded.payload.checkpoint.watched_reported);
    }

    /// 验证 P3 目录搜索和详情聚合契约可跨语言解码。
    #[test]
    fn decodes_p3_catalog_read_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-catalog-read-model.v1.json"
        ));
        let decoded: ContractFixture<P3CatalogReadModelFixture> =
            serde_json::from_str(fixture).expect("p3 catalog fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p3-catalog-read-model");
        assert_eq!(decoded.payload.search_result.source, "local");
        assert_eq!(decoded.payload.search_result.items.len(), 1);
        assert_eq!(
            decoded.payload.detail_result.anime.id,
            "anime-catalog-contract-1"
        );
        assert!(!decoded.payload.detail_result.stale);
    }

    /// 验证桌面功能对等补齐的采集与单集预览契约。
    #[test]
    fn decodes_p6_desktop_parity_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p6-desktop-parity.v1.json"
        ));
        let decoded: ContractFixture<P6DesktopParityFixture> =
            serde_json::from_str(fixture).expect("p6 desktop parity fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p6-desktop-parity");
        assert_eq!(decoded.payload.month_result.query.month, 7);
        assert_eq!(decoded.payload.season_result.query.season, "summer");
        assert_eq!(decoded.payload.episode_preview.candidates[0].score, 95);
    }

    /// 验证 P3 来源配置、同步游标和熔断状态契约可跨语言解码。
    #[test]
    fn decodes_p3_source_network_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-source-network-model.v1.json"
        ));
        let decoded: ContractFixture<P3SourceNetworkModelFixture> =
            serde_json::from_str(fixture).expect("p3 source network fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p3-source-network-model");
        assert_eq!(decoded.payload.source.id, "torznab-contract");
        assert_eq!(decoded.payload.sync_state.request_failure_count, 2);
        assert_eq!(
            decoded.payload.circuit_state.key,
            "release-source:torznab-contract"
        );
    }

    /// 验证 P3 来源绑定、候选、排除和命令输入契约可跨语言解码。
    #[test]
    fn decodes_p3_source_binding_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-source-binding-model.v1.json"
        ));
        let decoded: ContractFixture<P3SourceBindingModelFixture> =
            serde_json::from_str(fixture).expect("p3 source binding fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p3-source-binding-model");
        assert_eq!(decoded.payload.binding.source_anime_id, "528828");
        assert_eq!(
            decoded.payload.exclusion.source_anime_id.as_deref(),
            Some("999999")
        );
        assert_eq!(decoded.payload.candidate.score, 94);
        assert_eq!(decoded.payload.state.errors[0].source_id, "broken-contract");
        assert_eq!(decoded.payload.confirm_input.confidence, Some(0.94));
        assert_eq!(decoded.payload.mismatch_input.score, 21.0);
        assert!(decoded.payload.set_exclusion_input.excluded);
        assert_eq!(
            decoded.payload.remove_mismatch_input.source_anime_id,
            "999999"
        );
    }

    /// 验证 P3 聚合搜索、单源错误和 RSS 结果契约可跨语言解码。
    #[test]
    fn decodes_p3_release_search_model_fixture() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        ));
        let decoded: ContractFixture<P3ReleaseSearchModelFixture> =
            serde_json::from_str(fixture).expect("p3 release search fixture must decode");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.kind, "p3-release-search-model");
        assert_eq!(decoded.payload.search_result.releases.len(), 1);
        assert_eq!(decoded.payload.search_result.errors.len(), 1);
        assert_eq!(
            decoded.payload.search_result.releases[0].episode_no,
            Some(3.0)
        );
        assert_eq!(decoded.payload.rss_result.errors.len(), 1);
    }

    /// 验证敏感值不会通过 Debug 输出泄漏。
    #[test]
    fn redacts_secret_value_debug_output() {
        let secret = SecretValue::new("do-not-log");

        assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");
        assert_eq!(secret.expose(), b"do-not-log");
    }
}
