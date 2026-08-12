use std::collections::HashSet;
use std::sync::Arc;

use ani_domain::{
    is_restricted_anime_content, resolve_anime_download_path, AnimeReleaseQuery,
    AnimeSourceBinding, AnimeStatus, AppSettings, AutomationDownloadedItem, AutomationRunError,
    AutomationRunResult, AutomationSkippedItem, Episode, EpisodePreference, EpisodeStatus,
    FansubGroup, MyAnime, NotificationKind, NotificationRecord, NotificationSeverity, Release,
    ReleaseMatchContext, ReleaseMatchResult, ReleaseSourceConfig, RssSubscriptionReleaseQuery,
};
use ani_repository::RepositoryResult;
use ani_sources::{
    evaluate_automatic_download, normalize_fansub_name, parse_release_title, rank_releases,
    release_satisfies_subtitle_requirement, ReleaseSearchService, ReleaseSearchStore, SourceError,
    SourceNetworkService,
};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};

use crate::{EpisodeSyncService, EpisodeSyncStore};

const RELEASE_SEARCH_LIMIT: usize = 80;
const COMPLETED_CACHE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

/// 自动扫描用于判重的下载任务最小快照。
#[derive(Debug, Clone, PartialEq)]
pub struct AutomationDownloadReference {
    pub task_id: String,
    pub anime_id: Option<String>,
    pub episode_id: Option<String>,
    pub episode_no: Option<f64>,
}

/// 自动下载执行器接收的完整业务上下文。
#[derive(Debug, Clone)]
pub struct AutomaticDownloadRequest {
    pub anime: MyAnime,
    pub episode: Episode,
    pub release: Release,
    pub save_path: String,
}

/// 下载引擎成功接收任务后的稳定回执。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticDownloadReceipt {
    pub task_id: String,
}

/// 隔离自动扫描决策与具体 torrent/qBittorrent 实现。
#[async_trait]
pub trait AutomaticDownloadExecutor: Send + Sync {
    /// 将已通过可信度门禁的资源加入实际下载引擎并持久化任务。
    async fn execute(
        &self,
        request: AutomaticDownloadRequest,
    ) -> Result<AutomaticDownloadReceipt, String>;
}

/// 自动扫描依赖的数据库无关窄存储端口。
pub trait AutomationScanStore: ReleaseSearchStore {
    /// 读取全部追番。
    fn list_automation_anime(&self) -> RepositoryResult<Vec<MyAnime>>;

    /// 读取指定番剧单集。
    fn list_automation_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>>;

    /// 读取指定番剧单集偏好。
    fn list_automation_preferences(
        &self,
        anime_id: &str,
    ) -> RepositoryResult<Vec<EpisodePreference>>;

    /// 读取指定番剧已确认来源绑定。
    fn list_automation_bindings(&self, anime_id: &str)
        -> RepositoryResult<Vec<AnimeSourceBinding>>;

    /// 读取全部下载任务判重快照。
    fn list_automation_downloads(&self) -> RepositoryResult<Vec<AutomationDownloadReference>>;

    /// 保存自动扫描推进后的单集状态。
    fn save_automation_episode(&self, episode: &Episode) -> RepositoryResult<()>;

    /// 保存扫描发现的动态字幕组。
    fn observe_automation_fansubs(
        &self,
        anime_id: &str,
        releases: &[Release],
    ) -> RepositoryResult<Vec<FansubGroup>>;

    /// 写入自动扫描结果通知。
    fn add_automation_notifications(
        &self,
        records: &[NotificationRecord],
    ) -> RepositoryResult<Vec<NotificationRecord>>;
}

/// 一次自动扫描的显式运行参数。
#[derive(Debug, Clone)]
pub struct AutomationRunOptions {
    pub now: Option<DateTime<Utc>>,
    pub settings: AppSettings,
    pub sources: Vec<ReleaseSourceConfig>,
    pub fansubs: Vec<FansubGroup>,
}

/// 执行单集补齐、资源匹配、可信度判定和下载委派。
pub struct AutomationRunService {
    search: ReleaseSearchService,
    executor: Arc<dyn AutomaticDownloadExecutor>,
}

impl AutomationRunService {
    /// 创建复用来源连接池和平台下载适配器的自动扫描服务。
    pub fn new(
        network: Arc<SourceNetworkService>,
        executor: Arc<dyn AutomaticDownloadExecutor>,
    ) -> Self {
        Self {
            search: ReleaseSearchService::new(network),
            executor,
        }
    }

    /// 执行一次完整扫描；单集失败隔离后继续处理其余追番。
    pub async fn run<S>(
        &self,
        store: &S,
        options: AutomationRunOptions,
    ) -> Result<AutomationRunResult, SourceError>
    where
        S: AutomationScanStore + EpisodeSyncStore + Sync,
    {
        let started = options.now.unwrap_or_else(Utc::now);
        let mut result = empty_result(started);
        let policy = ScanPolicy::from_settings(&options.settings);
        let anime_items = store.list_automation_anime()?;
        if !policy.global_enabled {
            result.skipped.push(AutomationSkippedItem {
                anime_id: String::new(),
                anime_title: "全局自动下载".to_owned(),
                episode_id: None,
                episode_no: None,
                reason: "全局自动下载未开启".to_owned(),
            });
            finish_result(&mut result);
            return Ok(result);
        }

        let mut downloads = store.list_automation_downloads()?;
        log::info!(
            "Rust 自动扫描开始：anime_count={}, fallback={}, candidate_count={}",
            anime_items.len(),
            policy.fallback,
            policy.candidate_names.len()
        );

        for anime in anime_items {
            if is_restricted_anime_content(&anime.anime) {
                push_anime_skip(&mut result, &anime, "番剧标记为成人内容");
                continue;
            }
            if matches!(anime.status, AnimeStatus::Completed | AnimeStatus::Dropped) {
                push_anime_skip(
                    &mut result,
                    &anime,
                    if anime.status == AnimeStatus::Completed {
                        "追番已完成"
                    } else {
                        "追番已弃"
                    },
                );
                continue;
            }
            if !anime.auto_download {
                push_anime_skip(&mut result, &anime, "番剧未开启自动下载");
                continue;
            }

            if let Err(error) = EpisodeSyncService::sync(store, &anime, &[], started) {
                push_anime_error(&mut result, &anime, format!("同步单集失败：{error}"));
                continue;
            }
            let episodes = match store.list_automation_episodes(&anime.anime.id) {
                Ok(items) => items,
                Err(error) => {
                    push_anime_error(&mut result, &anime, format!("读取单集失败：{error}"));
                    continue;
                }
            };
            let preferences = match store.list_automation_preferences(&anime.anime.id) {
                Ok(items) => items,
                Err(error) => {
                    push_anime_error(&mut result, &anime, format!("读取单集偏好失败：{error}"));
                    continue;
                }
            };
            let bindings = match store.list_automation_bindings(&anime.anime.id) {
                Ok(items) => items,
                Err(error) => {
                    push_anime_error(&mut result, &anime, format!("读取来源绑定失败：{error}"));
                    continue;
                }
            };
            let actionable = episodes
                .into_iter()
                .filter(|episode| is_actionable_episode(episode, started))
                .collect::<Vec<_>>();
            if actionable.is_empty() {
                push_anime_skip(&mut result, &anime, "没有需要自动处理的单集");
                continue;
            }

            let rss_releases = self
                .search_rss_subscriptions(store, &anime, &options.fansubs)
                .await;
            if !rss_releases.is_empty() {
                if let Err(error) = store.observe_automation_fansubs(&anime.anime.id, &rss_releases)
                {
                    log::warn!(
                        "Rust 自动扫描字幕组观察失败：anime_id={}, error={}",
                        anime.anime.id,
                        error
                    );
                }
            }

            for episode in actionable {
                result.checked_episodes += 1;
                if downloads
                    .iter()
                    .any(|task| automation_download_matches(task, &anime.anime.id, &episode))
                {
                    push_episode_skip(&mut result, &anime, &episode, "已有下载任务");
                    continue;
                }
                match self
                    .scan_episode(
                        store,
                        &anime,
                        &episode,
                        &preferences,
                        &bindings,
                        &rss_releases,
                        &options,
                        &policy,
                    )
                    .await
                {
                    Ok(EpisodeScanOutcome::Downloaded(item)) => {
                        downloads.push(AutomationDownloadReference {
                            task_id: item.download_task_id.clone(),
                            anime_id: Some(anime.anime.id.clone()),
                            episode_id: Some(episode.id.clone()),
                            episode_no: Some(episode.episode_no),
                        });
                        result.downloaded.push(item);
                    }
                    Ok(EpisodeScanOutcome::Skipped(reason)) => {
                        push_episode_skip(&mut result, &anime, &episode, &reason);
                    }
                    Err(message) => result.errors.push(AutomationRunError {
                        anime_id: Some(anime.anime.id.clone()),
                        anime_title: Some(anime.anime.title.clone()),
                        episode_id: Some(episode.id.clone()),
                        episode_no: Some(episode.episode_no),
                        message,
                    }),
                }
            }
        }

        finish_result(&mut result);
        log::info!(
            "Rust 自动扫描完成：checked={}, downloaded={}, skipped={}, errors={}",
            result.checked_episodes,
            result.downloaded.len(),
            result.skipped.len(),
            result.errors.len()
        );
        Ok(result)
    }

    /// 搜索单部追番全部启用 RSS；单订阅失败不会清空成功结果。
    async fn search_rss_subscriptions<S>(
        &self,
        store: &S,
        anime: &MyAnime,
        fansubs: &[FansubGroup],
    ) -> Vec<Release>
    where
        S: AutomationScanStore + Sync,
    {
        let mut releases = Vec::new();
        for subscription in anime
            .rss_subscriptions
            .iter()
            .filter(|item| item.enabled && !item.url.trim().is_empty())
        {
            let source = ReleaseSourceConfig {
                id: format!("rss-subscription:{}", subscription.id),
                name: subscription.name.clone(),
                kind: ani_domain::SourceKind::Rss,
                enabled: true,
                use_proxy: true,
                request_interval_ms: 1_500,
                base_url: None,
                api_key: None,
                rss_url: Some(subscription.url.clone()),
                tags: vec!["anime".to_owned(), "rss".to_owned()],
            };
            let (preferred_languages, legacy_preference) =
                if subscription.preferred_subtitle_languages.is_empty() {
                    (
                        anime.preferred_subtitle_languages.as_slice(),
                        anime.preferred_subtitle.as_deref(),
                    )
                } else {
                    (subscription.preferred_subtitle_languages.as_slice(), None)
                };
            let found = self
                .search
                .search_rss_subscription(
                    store,
                    &source,
                    fansubs,
                    anime,
                    RssSubscriptionReleaseQuery {
                        anime_id: anime.anime.id.clone(),
                        subscription_id: subscription.id.clone(),
                        preferred_resolution: anime.preferred_resolution.clone(),
                        limit: Some(RELEASE_SEARCH_LIMIT),
                    },
                    preferred_languages,
                )
                .await;
            for error in found.errors {
                log::warn!(
                    "Rust 自动扫描 RSS 失败：anime_id={}, subscription_id={}, error={}",
                    anime.anime.id,
                    subscription.id,
                    error.message
                );
            }
            let (eligible, rejected) = filter_automatic_releases_by_subtitle(
                found.releases,
                preferred_languages,
                legacy_preference,
            );
            if rejected > 0 {
                log::info!(
                    "Rust 自动扫描 RSS 字幕门禁：anime_id={}, subscription_id={}, rejected={}",
                    anime.anime.id,
                    subscription.id,
                    rejected
                );
            }
            releases.extend(eligible);
        }
        dedupe_releases(releases)
    }

    /// 为单集执行 RSS 优先、全局来源回退和下载委派。
    #[allow(clippy::too_many_arguments)]
    async fn scan_episode<S>(
        &self,
        store: &S,
        anime: &MyAnime,
        episode: &Episode,
        preferences: &[EpisodePreference],
        bindings: &[AnimeSourceBinding],
        rss_releases: &[Release],
        options: &AutomationRunOptions,
        policy: &ScanPolicy,
    ) -> Result<EpisodeScanOutcome, String>
    where
        S: AutomationScanStore + Sync,
    {
        let preference = preferences
            .iter()
            .find(|item| item.episode_id == episode.id);
        let preferred_fansub = preference
            .and_then(|item| item.fansub_group_id.as_ref())
            .or(anime.default_fansub_group_id.as_ref());
        let context = ReleaseMatchContext {
            anime: anime.clone(),
            episode_no: Some(episode.episode_no),
            episode_fansub_override_id: preference.and_then(|item| item.fansub_group_id.clone()),
            candidate_fansub_group_ids: candidate_group_ids(
                &policy.candidate_names,
                &options.fansubs,
            ),
            candidate_fansub_names: policy.candidate_names.clone(),
        };
        // RSS 订阅可能各自配置了不同字幕偏好，扫描单集时必须再次以番剧规则为准。
        let (eligible_rss_releases, rss_rejected) = filter_automatic_releases_by_subtitle(
            rss_releases.to_vec(),
            &anime.preferred_subtitle_languages,
            anime.preferred_subtitle.as_deref(),
        );
        if rss_rejected > 0 {
            log::info!(
                "Rust 自动扫描单集字幕门禁：anime_id={}, episode_id={}, rejected={}, required={:?}",
                anime.anime.id,
                episode.id,
                rss_rejected,
                anime.preferred_subtitle_languages
            );
        }
        let rss_ranked = rank_releases(&eligible_rss_releases, &context, &options.fansubs);
        let rss_candidates = apply_fansub_policy(
            &rss_ranked,
            preferred_fansub.map(String::as_str),
            policy,
            &options.fansubs,
        );
        let mut ranked = rss_ranked;
        let mut candidates = rss_candidates;

        if candidates.is_empty() {
            let mut found = self
                .search
                .search_anime(
                    store,
                    &options.sources,
                    &options.fansubs,
                    &anime.anime,
                    bindings,
                    AnimeReleaseQuery {
                        anime_id: anime.anime.id.clone(),
                        episode_no: Some(episode.episode_no),
                        fansub_group_id: if policy.fallback == "candidate" {
                            None
                        } else {
                            preferred_fansub.cloned()
                        },
                        preferred_resolution: anime.preferred_resolution.clone(),
                        limit: Some(RELEASE_SEARCH_LIMIT),
                        cache_ttl_ms: if anime.status == AnimeStatus::Completed {
                            Some(COMPLETED_CACHE_TTL_MS)
                        } else {
                            None
                        },
                        force_refresh: false,
                    },
                )
                .await
                .map_err(|error| format!("搜索资源失败：{error}"))?;
            let original_releases = std::mem::take(&mut found.releases);
            let (eligible, rejected) = filter_automatic_releases_by_subtitle(
                original_releases,
                &anime.preferred_subtitle_languages,
                anime.preferred_subtitle.as_deref(),
            );
            found.releases = eligible;
            if rejected > 0 {
                log::info!(
                    "Rust 自动扫描全局来源字幕门禁：anime_id={}, episode_id={}, rejected={}",
                    anime.anime.id,
                    episode.id,
                    rejected
                );
            }
            if let Err(error) = store.observe_automation_fansubs(&anime.anime.id, &found.releases) {
                log::warn!(
                    "Rust 自动扫描字幕组观察失败：anime_id={}, error={}",
                    anime.anime.id,
                    error
                );
            }
            ranked = rank_releases(&found.releases, &context, &options.fansubs);
            candidates = apply_fansub_policy(
                &ranked,
                preferred_fansub.map(String::as_str),
                policy,
                &options.fansubs,
            );
        }

        if candidates.is_empty() {
            save_episode_status(store, episode, EpisodeStatus::Aired)?;
            return Ok(EpisodeScanOutcome::Skipped("未找到匹配资源".to_owned()));
        }
        log_candidate_summary(anime, episode, &ranked, &candidates);
        let decision = evaluate_automatic_download(&candidates);
        if !decision.accepted {
            save_episode_status(store, episode, EpisodeStatus::Matched)?;
            return Ok(EpisodeScanOutcome::Skipped(decision.reason));
        }
        let release = candidates[0].release.clone();
        if let Some(reason) = automatic_subtitle_rejection_reason(
            &release,
            &anime.preferred_subtitle_languages,
            anime.preferred_subtitle.as_deref(),
        ) {
            log::info!(
                "Rust 自动扫描提交前字幕硬门禁拒绝：anime_id={}, episode_id={}, reason={}, title={:?}, actual={:?}, required={:?}",
                anime.anime.id,
                episode.id,
                reason,
                release.title,
                release.subtitle_languages,
                anime.preferred_subtitle_languages
            );
            save_episode_status(store, episode, EpisodeStatus::Matched)?;
            return Ok(EpisodeScanOutcome::Skipped("字幕规则不满足".to_owned()));
        }
        if release.magnet_url.is_none() && release.torrent_url.is_none() {
            return Ok(EpisodeScanOutcome::Skipped(
                "最佳资源没有下载地址".to_owned(),
            ));
        }
        let receipt = self
            .executor
            .execute(AutomaticDownloadRequest {
                anime: anime.clone(),
                episode: episode.clone(),
                release: release.clone(),
                save_path: resolve_anime_download_path(&options.settings, Some(anime)),
            })
            .await?;
        save_episode_status(store, episode, EpisodeStatus::Downloading)?;
        Ok(EpisodeScanOutcome::Downloaded(AutomationDownloadedItem {
            anime_id: anime.anime.id.clone(),
            anime_title: anime.anime.title.clone(),
            episode_id: episode.id.clone(),
            episode_no: episode.episode_no,
            release_id: release.id,
            release_title: release.title,
            download_task_id: receipt.task_id,
        }))
    }
}

/// 同时按稳定单集标识和番剧集数识别已有下载任务。
fn automation_download_matches(
    task: &AutomationDownloadReference,
    anime_id: &str,
    episode: &Episode,
) -> bool {
    task.anime_id.as_deref() == Some(anime_id)
        && (task.episode_id.as_deref() == Some(episode.id.as_str())
            || task
                .episode_no
                .is_some_and(|number| (number - episode.episode_no).abs() < 1e-9))
}

enum EpisodeScanOutcome {
    Downloaded(AutomationDownloadedItem),
    Skipped(String),
}

struct ScanPolicy {
    global_enabled: bool,
    fallback: String,
    candidate_names: Vec<String>,
}

impl ScanPolicy {
    /// 从版本化 JSON 设置读取自动化策略并提供稳定默认值。
    fn from_settings(settings: &AppSettings) -> Self {
        let automation = settings.get("automation");
        let fallback = automation
            .and_then(|value| value.get("fallbackWhenDefaultFansubMissing"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| matches!(*value, "wait" | "candidate" | "notify_only"))
            .unwrap_or("wait")
            .to_owned();
        let mut seen = HashSet::new();
        let candidate_names = automation
            .and_then(|value| value.get("candidateFansubNames"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| seen.insert(normalize_fansub_name(value)))
            .map(str::to_owned)
            .collect();
        Self {
            global_enabled: automation
                .and_then(|value| value.get("autoDownloadEnabledGlobally"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            fallback,
            candidate_names,
        }
    }
}

/// 根据自动扫描结果生成提醒中心记录。
pub fn build_automation_notifications(result: &AutomationRunResult) -> Vec<NotificationRecord> {
    let mut records = Vec::new();
    for item in &result.downloaded {
        records.push(NotificationRecord {
            id: format!(
                "notification-{}-{}",
                result.finished_at, item.download_task_id
            ),
            kind: NotificationKind::Automation,
            title: format!("已添加下载：{}", item.anime_title),
            body: format!(
                "第 {} 集已匹配资源「{}」。",
                display_episode_no(item.episode_no),
                item.release_title
            ),
            severity: NotificationSeverity::Success,
            anime_id: Some(item.anime_id.clone()),
            episode_id: Some(item.episode_id.clone()),
            download_task_id: Some(item.download_task_id.clone()),
            created_at: result.finished_at.clone(),
            read_at: None,
        });
    }
    for (index, item) in result.errors.iter().enumerate() {
        records.push(NotificationRecord {
            id: format!(
                "notification-{}-error-{}",
                result.finished_at,
                item.episode_id
                    .as_deref()
                    .or(item.anime_id.as_deref())
                    .map(str::to_owned)
                    .unwrap_or_else(|| index.to_string())
            ),
            kind: NotificationKind::Automation,
            title: item
                .anime_title
                .as_ref()
                .map(|title| format!("扫描失败：{title}"))
                .unwrap_or_else(|| "自动扫描失败".to_owned()),
            body: item
                .episode_no
                .map(|number| format!("第 {} 集：{}", display_episode_no(number), item.message))
                .unwrap_or_else(|| item.message.clone()),
            severity: NotificationSeverity::Error,
            anime_id: item.anime_id.clone(),
            episode_id: item.episode_id.clone(),
            download_task_id: None,
            created_at: result.finished_at.clone(),
            read_at: None,
        });
    }
    if records.is_empty() && result.checked_episodes > 0 {
        records.push(NotificationRecord {
            id: format!("notification-{}-summary", result.finished_at),
            kind: NotificationKind::Automation,
            title: "自动扫描完成".to_owned(),
            body: format!("已检查 {} 集，没有新增下载任务。", result.checked_episodes),
            severity: NotificationSeverity::Info,
            anime_id: None,
            episode_id: None,
            download_task_id: None,
            created_at: result.finished_at.clone(),
            read_at: None,
        });
    }
    records
}

fn apply_fansub_policy(
    ranked: &[ReleaseMatchResult],
    preferred_id: Option<&str>,
    policy: &ScanPolicy,
    groups: &[FansubGroup],
) -> Vec<ReleaseMatchResult> {
    let Some(preferred_id) = preferred_id else {
        return ranked.to_vec();
    };
    let preferred = ranked
        .iter()
        .filter(|item| item.release.fansub_group_id.as_deref() == Some(preferred_id))
        .cloned()
        .collect::<Vec<_>>();
    if !preferred.is_empty() {
        return preferred;
    }
    if policy.fallback != "candidate" || policy.candidate_names.is_empty() {
        return Vec::new();
    }
    ranked
        .iter()
        .filter(|item| release_matches_candidate(&item.release, &policy.candidate_names, groups))
        .cloned()
        .collect()
}

fn release_matches_candidate(
    release: &Release,
    candidate_names: &[String],
    groups: &[FansubGroup],
) -> bool {
    let candidate_keys = candidate_names
        .iter()
        .map(|name| normalize_fansub_name(name))
        .collect::<HashSet<_>>();
    let mut values = Vec::new();
    if let Some(name) = release.fansub_name.as_ref() {
        values.push(name.as_str());
    }
    if let Some(group) = release
        .fansub_group_id
        .as_ref()
        .and_then(|id| groups.iter().find(|group| &group.id == id))
    {
        values.push(group.name.as_str());
        values.extend(group.aliases.iter().map(String::as_str));
    }
    values
        .into_iter()
        .any(|value| candidate_keys.contains(&normalize_fansub_name(value)))
}

fn candidate_group_ids(candidate_names: &[String], groups: &[FansubGroup]) -> Vec<String> {
    groups
        .iter()
        .filter(|group| {
            candidate_names.iter().any(|candidate| {
                let key = normalize_fansub_name(candidate);
                normalize_fansub_name(&group.name) == key
                    || group
                        .aliases
                        .iter()
                        .any(|alias| normalize_fansub_name(alias) == key)
            })
        })
        .map(|group| group.id.clone())
        .collect()
}

fn save_episode_status<S>(store: &S, episode: &Episode, status: EpisodeStatus) -> Result<(), String>
where
    S: AutomationScanStore,
{
    if episode.status == status {
        return Ok(());
    }
    let mut updated = episode.clone();
    updated.status = status;
    store
        .save_automation_episode(&updated)
        .map_err(|error| format!("保存单集状态失败：{error}"))
}

fn is_actionable_episode(episode: &Episode, now: DateTime<Utc>) -> bool {
    if matches!(
        episode.status,
        EpisodeStatus::Downloading
            | EpisodeStatus::Downloaded
            | EpisodeStatus::Watched
            | EpisodeStatus::Upcoming
    ) {
        return episode.status == EpisodeStatus::Upcoming
            && episode
                .air_time
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|value| value.with_timezone(&Utc) <= now);
    }
    matches!(
        episode.status,
        EpisodeStatus::Aired | EpisodeStatus::Matched
    )
}

fn dedupe_releases(releases: Vec<Release>) -> Vec<Release> {
    let mut seen = HashSet::new();
    releases
        .into_iter()
        .filter(|release| {
            let key = release
                .info_hash
                .as_ref()
                .or(release.magnet_url.as_ref())
                .or(release.torrent_url.as_ref())
                .unwrap_or(&release.id)
                .clone();
            seen.insert(key)
        })
        .collect()
}

/// 仅保留完整覆盖字幕要求的自动下载候选，并返回被拒绝数量。
fn filter_automatic_releases_by_subtitle(
    releases: Vec<Release>,
    preferred_languages: &[String],
    legacy_preference: Option<&str>,
) -> (Vec<Release>, usize) {
    let total = releases.len();
    let eligible = releases
        .into_iter()
        .filter_map(|release| {
            let rejection = automatic_subtitle_rejection_reason(
                &release,
                preferred_languages,
                legacy_preference,
            );
            let Some(reason) = rejection else {
                return Some(release);
            };
            let source_claimed_match = release_satisfies_subtitle_requirement(
                &release,
                preferred_languages,
                legacy_preference,
            );
            if source_claimed_match {
                log::info!(
                    "Rust 自动扫描拒绝来源字幕声明：source_id={}, release_id={}, reason={}, title={:?}, actual={:?}, required={:?}",
                    release.source_id,
                    release.id,
                    reason,
                    release.title,
                    release.subtitle_languages,
                    preferred_languages
                );
            } else {
                log::debug!(
                    "Rust 自动扫描字幕候选拒绝：source_id={}, release_id={}, reason={}, title={:?}",
                    release.source_id,
                    release.id,
                    reason,
                    release.title
                );
            }
            None
        })
        .collect::<Vec<_>>();
    let rejected = total.saturating_sub(eligible.len());
    (eligible, rejected)
}

/// 返回自动扫描的字幕拒绝原因；用户未配置字幕规则时不启用标题证据门禁。
fn automatic_subtitle_rejection_reason(
    release: &Release,
    preferred_languages: &[String],
    legacy_preference: Option<&str>,
) -> Option<&'static str> {
    if !has_subtitle_requirement(preferred_languages, legacy_preference) {
        return None;
    }
    if title_declares_no_subtitles(&release.title) {
        return Some("标题明确标记无字幕");
    }

    let parsed = parse_release_title(&release.title, &[]);
    if !parsed.subtitle_languages.is_empty() {
        let mut title_evidence = release.clone();
        title_evidence.subtitle_languages = parsed.subtitle_languages;
        title_evidence.subtitle = parsed.subtitle;
        return (!release_satisfies_subtitle_requirement(
            &title_evidence,
            preferred_languages,
            legacy_preference,
        ))
        .then_some("标题字幕语言不满足");
    }
    if title_has_subtitle_track_evidence(&release.title) {
        return (!release_satisfies_subtitle_requirement(
            release,
            preferred_languages,
            legacy_preference,
        ))
        .then_some("字幕轨存在但语言不满足");
    }
    Some("标题缺少字幕证据")
}

/// 判断当前追番是否配置了有效字幕语言要求。
fn has_subtitle_requirement(
    preferred_languages: &[String],
    legacy_preference: Option<&str>,
) -> bool {
    preferred_languages
        .iter()
        .any(|value| matches!(value.as_str(), "chs" | "cht" | "jpn" | "eng"))
        || legacy_preference
            .is_some_and(|value| matches!(value, "chs" | "cht" | "jpn" | "eng" | "multi"))
}

/// 识别标题中明确声明无字幕或无中文的否定标记。
fn title_declares_no_subtitles(title: &str) -> bool {
    let lower = title.to_lowercase();
    ["无中字", "無中字", "无字幕", "無字幕", "无中文", "無中文"]
        .iter()
        .any(|marker| title.contains(marker))
        || ["no sub", "no subs", "no subtitle", "no subtitles"]
            .iter()
            .any(|marker| lower.contains(marker))
}

/// 识别字幕轨格式或内封标记，语言仍以来源字段进行二次确认。
fn title_has_subtitle_track_evidence(title: &str) -> bool {
    title.contains("字幕")
        || title.contains("内封")
        || title.contains("內封")
        || title.contains("内嵌")
        || title.contains("內嵌")
        || [
            "ass",
            "ssa",
            "srt",
            "vtt",
            "pgs",
            "sub",
            "subs",
            "subtitle",
            "subtitles",
        ]
        .iter()
        .any(|marker| contains_ascii_title_token(title, marker))
}

/// 按标题分隔符匹配 ASCII 标记，避免把字幕组名称中的子串当成字幕轨。
fn contains_ascii_title_token(title: &str, expected: &str) -> bool {
    title
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| token.eq_ignore_ascii_case(expected))
}

fn empty_result(now: DateTime<Utc>) -> AutomationRunResult {
    let started_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    AutomationRunResult {
        started_at: started_at.clone(),
        finished_at: started_at,
        checked_episodes: 0,
        downloaded: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    }
}

fn finish_result(result: &mut AutomationRunResult) {
    result.finished_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
}

fn push_anime_skip(result: &mut AutomationRunResult, anime: &MyAnime, reason: &str) {
    result.skipped.push(AutomationSkippedItem {
        anime_id: anime.anime.id.clone(),
        anime_title: anime.anime.title.clone(),
        episode_id: None,
        episode_no: None,
        reason: reason.to_owned(),
    });
}

fn push_episode_skip(
    result: &mut AutomationRunResult,
    anime: &MyAnime,
    episode: &Episode,
    reason: &str,
) {
    result.skipped.push(AutomationSkippedItem {
        anime_id: anime.anime.id.clone(),
        anime_title: anime.anime.title.clone(),
        episode_id: Some(episode.id.clone()),
        episode_no: Some(episode.episode_no),
        reason: reason.to_owned(),
    });
}

fn push_anime_error(result: &mut AutomationRunResult, anime: &MyAnime, message: String) {
    result.errors.push(AutomationRunError {
        anime_id: Some(anime.anime.id.clone()),
        anime_title: Some(anime.anime.title.clone()),
        episode_id: None,
        episode_no: None,
        message,
    });
}

fn log_candidate_summary(
    anime: &MyAnime,
    episode: &Episode,
    ranked: &[ReleaseMatchResult],
    candidates: &[ReleaseMatchResult],
) {
    let eligible = candidates
        .iter()
        .map(|item| item.release.id.as_str())
        .collect::<HashSet<_>>();
    let summary = ranked
        .iter()
        .take(5)
        .map(|item| {
            serde_json::json!({
                "releaseId": item.release.id,
                "sourceId": item.release.source_id,
                "fansubName": item.release.fansub_name,
                "score": item.score,
                "matchScore": item.match_score,
                "preferenceScore": item.preference_score,
                "availabilityScore": item.availability_score,
                "eligible": eligible.contains(item.release.id.as_str()),
            })
        })
        .collect::<Vec<_>>();
    log::info!(
        "Rust 自动扫描候选评分：anime_id={}, episode_id={}, episode_no={}, candidates={}",
        anime.anime.id,
        episode.id,
        display_episode_no(episode.episode_no),
        serde_json::Value::Array(summary)
    );
}

fn display_episode_no(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        (value as i64).to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ani_domain::{
        AnimeSourceBinding, EpisodePreference, FansubGroup, NotificationRecord, Release,
        SubtitleLanguage, SubtitlePreference,
    };
    use ani_repository::{CachedReleaseQuery, ReleaseSearchCacheEntry};
    use ani_sources::{CircuitStateStore, NativeHttpConfig, ProxyMode};
    use chrono::{TimeZone, Utc};

    use super::*;

    struct MemoryStore {
        anime: Vec<MyAnime>,
        episodes: Mutex<Vec<Episode>>,
        preferences: Vec<EpisodePreference>,
        bindings: Vec<AnimeSourceBinding>,
        releases: Vec<Release>,
        downloads: Vec<AutomationDownloadReference>,
        notifications: Mutex<Vec<NotificationRecord>>,
    }

    impl CircuitStateStore for MemoryStore {
        fn get_circuit_state(
            &self,
            _key: &str,
        ) -> RepositoryResult<Option<ani_domain::RequestCircuitState>> {
            Ok(None)
        }

        fn save_circuit_state(
            &self,
            _state: &ani_domain::RequestCircuitState,
        ) -> RepositoryResult<()> {
            Ok(())
        }
    }

    impl ReleaseSearchStore for MemoryStore {
        fn get_search_cache(
            &self,
            _cache_key: &str,
            _current_time: &str,
        ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>> {
            Ok(None)
        }

        fn save_search_cache(
            &self,
            _cache_key: &str,
            _entry: &ReleaseSearchCacheEntry,
        ) -> RepositoryResult<()> {
            Ok(())
        }

        fn list_release_cache(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>> {
            Ok(self
                .releases
                .iter()
                .filter(|release| {
                    query
                        .anime_id
                        .as_deref()
                        .is_none_or(|id| release.anime_id.as_deref() == Some(id))
                })
                .cloned()
                .collect())
        }

        fn save_release_cache(&self, _releases: &[Release]) -> RepositoryResult<usize> {
            Ok(0)
        }
    }

    impl EpisodeSyncStore for MemoryStore {
        fn list_sync_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
            Ok(self
                .episodes
                .lock()
                .expect("lock episodes")
                .iter()
                .filter(|item| item.anime_id == anime_id)
                .cloned()
                .collect())
        }

        fn save_sync_episode(&self, episode: &Episode) -> RepositoryResult<Vec<Episode>> {
            self.save_automation_episode(episode)?;
            self.list_sync_episodes(&episode.anime_id)
        }

        fn list_sync_cached_releases(&self, anime_id: &str) -> RepositoryResult<Vec<Release>> {
            self.list_release_cache(&CachedReleaseQuery {
                anime_id: Some(anime_id.to_owned()),
                ..CachedReleaseQuery::default()
            })
        }
    }

    impl AutomationScanStore for MemoryStore {
        fn list_automation_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
            Ok(self.anime.clone())
        }

        fn list_automation_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
            self.list_sync_episodes(anime_id)
        }

        fn list_automation_preferences(
            &self,
            _anime_id: &str,
        ) -> RepositoryResult<Vec<EpisodePreference>> {
            Ok(self.preferences.clone())
        }

        fn list_automation_bindings(
            &self,
            _anime_id: &str,
        ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
            Ok(self.bindings.clone())
        }

        fn list_automation_downloads(&self) -> RepositoryResult<Vec<AutomationDownloadReference>> {
            Ok(self.downloads.clone())
        }

        fn save_automation_episode(&self, episode: &Episode) -> RepositoryResult<()> {
            let mut episodes = self.episodes.lock().expect("lock episodes");
            episodes.retain(|item| item.id != episode.id);
            episodes.push(episode.clone());
            Ok(())
        }

        fn observe_automation_fansubs(
            &self,
            _anime_id: &str,
            _releases: &[Release],
        ) -> RepositoryResult<Vec<FansubGroup>> {
            Ok(Vec::new())
        }

        fn add_automation_notifications(
            &self,
            records: &[NotificationRecord],
        ) -> RepositoryResult<Vec<NotificationRecord>> {
            let mut notifications = self.notifications.lock().expect("lock notifications");
            notifications.extend_from_slice(records);
            Ok(notifications.clone())
        }
    }

    struct RecordingExecutor {
        requests: Mutex<Vec<AutomaticDownloadRequest>>,
    }

    #[async_trait]
    impl AutomaticDownloadExecutor for RecordingExecutor {
        async fn execute(
            &self,
            request: AutomaticDownloadRequest,
        ) -> Result<AutomaticDownloadReceipt, String> {
            self.requests.lock().expect("lock requests").push(request);
            Ok(AutomaticDownloadReceipt {
                task_id: "task-auto-1".to_owned(),
            })
        }
    }

    /// 验证自动扫描复用缓存资源、单集覆盖偏好并委派真实下载端口。
    #[tokio::test]
    async fn scans_cached_release_and_delegates_download() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode following fixture");
        let mut anime: MyAnime =
            serde_json::from_value(fixture["payload"]["myAnime"].clone()).expect("decode my anime");
        anime.auto_download = true;
        anime.anime.detail = Some(serde_json::json!({ "episodeCount": 3 }));
        let release_fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode release fixture");
        let mut release: Release = serde_json::from_value(
            release_fixture["payload"]["searchResult"]["releases"][0].clone(),
        )
        .expect("decode release");
        release.anime_id = Some(anime.anime.id.clone());
        release.title = "[测试字幕组] P3 契约番剧 - 03 [1080p][简繁内封]".to_owned();
        release.fansub_group_id = None;
        release.subtitle_languages = vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht];
        let store = MemoryStore {
            anime: vec![anime.clone()],
            episodes: Mutex::new(vec![Episode {
                id: "episode-auto-3".to_owned(),
                anime_id: anime.anime.id.clone(),
                episode_no: 3.0,
                title: None,
                air_time: None,
                status: EpisodeStatus::Aired,
            }]),
            preferences: Vec::new(),
            bindings: Vec::new(),
            releases: vec![release],
            downloads: Vec::new(),
            notifications: Mutex::new(Vec::new()),
        };
        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let service = AutomationRunService::new(test_network(), executor.clone());
        let settings = serde_json::json!({
            "automation": {
                "autoDownloadEnabledGlobally": true,
                "fallbackWhenDefaultFansubMissing": "wait",
                "candidateFansubNames": []
            },
            "download": {
                "defaultDownloadDir": "C:/Downloads",
                "createAnimeFolder": true,
                "animeFolderPattern": "{year}-{month}/{title}"
            }
        });
        let result = service
            .run(
                &store,
                AutomationRunOptions {
                    now: Some(
                        Utc.with_ymd_and_hms(2026, 7, 25, 12, 0, 0)
                            .single()
                            .expect("fixed time"),
                    ),
                    settings,
                    sources: Vec::new(),
                    fansubs: Vec::new(),
                },
            )
            .await
            .expect("run automation");
        assert_eq!(result.checked_episodes, 1);
        assert_eq!(result.downloaded.len(), 1);
        assert_eq!(result.downloaded[0].download_task_id, "task-auto-1");
        let requests = executor.requests.lock().expect("lock requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].save_path,
            std::path::PathBuf::from("C:/Downloads")
                .join("2026-07")
                .join("P3 契约番剧")
                .to_string_lossy()
        );
        assert!(store
            .episodes
            .lock()
            .expect("lock episodes")
            .iter()
            .any(|item| item.id == "episode-auto-3" && item.status == EpisodeStatus::Downloading));
    }

    /// 验证明确定义为成人内容的追番不会进入自动下载流程。
    #[tokio::test]
    async fn skips_restricted_anime_during_automatic_scan() {
        let mut store = memory_store_with_episode();
        store.anime[0].anime.detail = Some(serde_json::json!({"contentRating": "18+"}));
        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let service = AutomationRunService::new(test_network(), executor.clone());

        let result = service
            .run(
                &store,
                AutomationRunOptions {
                    now: Some(
                        Utc.with_ymd_and_hms(2026, 7, 25, 12, 0, 0)
                            .single()
                            .expect("fixed time"),
                    ),
                    settings: automation_settings(),
                    sources: Vec::new(),
                    fansubs: Vec::new(),
                },
            )
            .await
            .expect("run automation");

        assert_eq!(result.checked_episodes, 0);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].reason, "番剧标记为成人内容");
        assert!(executor.requests.lock().expect("lock requests").is_empty());
    }

    /// 验证续作不会自动下载未标季数的同名旧季度资源。
    #[tokio::test]
    async fn skips_unmarked_sequel_release_before_download() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode following fixture");
        let mut anime: MyAnime =
            serde_json::from_value(fixture["payload"]["myAnime"].clone()).expect("decode my anime");
        anime.auto_download = true;
        anime.anime.title = "地狱模式 第二季".to_owned();
        anime.anime.original_title = Some("Hell Mode 2nd Season".to_owned());
        anime.anime.detail = Some(serde_json::json!({ "episodeCount": 8 }));

        let release_fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode release fixture");
        let mut release: Release = serde_json::from_value(
            release_fixture["payload"]["searchResult"]["releases"][0].clone(),
        )
        .expect("decode release");
        release.title = "[LoliHouse] 地狱模式～喜欢速通游戏的玩家在废设定异世界无双～ / Hell Mode - 08 [WebRip 1080p HEVC-10bit AAC][简繁内封字幕]".to_owned();
        release.anime_id = Some(anime.anime.id.clone());
        release.episode_no = Some(8.0);
        release.series_season_no = None;
        release.fansub_group_id = None;
        release.subtitle_languages = vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht];

        let store = MemoryStore {
            anime: vec![anime.clone()],
            episodes: Mutex::new(vec![Episode {
                id: "episode-hell-mode-8".to_owned(),
                anime_id: anime.anime.id.clone(),
                episode_no: 8.0,
                title: None,
                air_time: None,
                status: EpisodeStatus::Aired,
            }]),
            preferences: Vec::new(),
            bindings: Vec::new(),
            releases: vec![release],
            downloads: Vec::new(),
            notifications: Mutex::new(Vec::new()),
        };
        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let result = AutomationRunService::new(test_network(), executor.clone())
            .run(
                &store,
                AutomationRunOptions {
                    now: Some(
                        Utc.with_ymd_and_hms(2026, 8, 3, 12, 0, 0)
                            .single()
                            .expect("fixed time"),
                    ),
                    settings: automation_settings(),
                    sources: Vec::new(),
                    fansubs: Vec::new(),
                },
            )
            .await
            .expect("run automation");

        assert_eq!(result.checked_episodes, 1);
        assert!(result.downloaded.is_empty());
        assert!(result.skipped.iter().any(|item| {
            item.episode_id.as_deref() == Some("episode-hell-mode-8")
                && item.reason == "未找到匹配资源"
        }));
        assert!(executor.requests.lock().expect("lock requests").is_empty());
    }

    /// 验证已存在任务时不调用下载执行器。
    #[tokio::test]
    async fn skips_duplicate_download_task() {
        let store = memory_store_with_episode();
        let episode = store.episodes.lock().expect("lock episodes")[0].clone();
        let store = MemoryStore {
            downloads: vec![AutomationDownloadReference {
                task_id: "existing".to_owned(),
                anime_id: Some(store.anime[0].anime.id.clone()),
                episode_id: Some(episode.id),
                episode_no: None,
            }],
            ..store
        };
        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let result = AutomationRunService::new(test_network(), executor.clone())
            .run(
                &store,
                AutomationRunOptions {
                    now: None,
                    settings: automation_settings(),
                    sources: Vec::new(),
                    fansubs: Vec::new(),
                },
            )
            .await
            .expect("run automation");
        assert_eq!(result.checked_episodes, 1);
        assert!(result
            .skipped
            .iter()
            .any(|item| item.reason == "已有下载任务"));
        assert!(executor.requests.lock().expect("lock requests").is_empty());
    }

    /// 验证历史任务缺少单集标识时仍可按番剧和集数阻止重复下载。
    #[tokio::test]
    async fn skips_duplicate_download_task_by_episode_number() {
        let store = memory_store_with_episode();
        let episode = store.episodes.lock().expect("lock episodes")[0].clone();
        let store = MemoryStore {
            downloads: vec![AutomationDownloadReference {
                task_id: "existing-without-episode-id".to_owned(),
                anime_id: Some(store.anime[0].anime.id.clone()),
                episode_id: None,
                episode_no: Some(episode.episode_no),
            }],
            ..store
        };
        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let result = AutomationRunService::new(test_network(), executor.clone())
            .run(
                &store,
                AutomationRunOptions {
                    now: None,
                    settings: automation_settings(),
                    sources: Vec::new(),
                    fansubs: Vec::new(),
                },
            )
            .await
            .expect("run automation");

        assert!(result
            .skipped
            .iter()
            .any(|item| item.reason == "已有下载任务"));
        assert!(executor.requests.lock().expect("lock requests").is_empty());
    }

    /// 验证下载和错误结果生成可关联的提醒记录。
    #[test]
    fn builds_automation_notifications() {
        let mut result = empty_result(Utc::now());
        result.downloaded.push(AutomationDownloadedItem {
            anime_id: "anime-1".to_owned(),
            anime_title: "测试番".to_owned(),
            episode_id: "episode-1".to_owned(),
            episode_no: 1.0,
            release_id: "release-1".to_owned(),
            release_title: "测试资源".to_owned(),
            download_task_id: "task-1".to_owned(),
        });
        result.errors.push(AutomationRunError {
            anime_id: Some("anime-2".to_owned()),
            anime_title: Some("失败番".to_owned()),
            episode_id: None,
            episode_no: None,
            message: "网络失败".to_owned(),
        });
        let records = build_automation_notifications(&result);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].download_task_id.as_deref(), Some("task-1"));
        assert_eq!(records[1].severity, NotificationSeverity::Error);
    }

    /// 验证自动扫描字幕门禁拒绝部分覆盖和组成未知的多语资源。
    #[test]
    fn filters_automatic_releases_by_required_subtitles() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode release fixture");
        let base: Release =
            serde_json::from_value(fixture["payload"]["searchResult"]["releases"][0].clone())
                .expect("decode release");
        let mut complete = base.clone();
        complete.id = "complete".to_owned();
        complete.title = "[NIX-RAWS] 测试番 - 01 [简繁内封]".to_owned();
        complete.subtitle_languages = vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht];
        let mut partial = base.clone();
        partial.id = "partial".to_owned();
        partial.title = "[字幕组] 测试番 - 01 [简中]".to_owned();
        partial.subtitle_languages = vec![SubtitleLanguage::Chs];
        let mut unknown_multi = base;
        unknown_multi.id = "unknown-multi".to_owned();
        unknown_multi.title = "[字幕组] 测试番 - 01 [Multi-Subs]".to_owned();
        unknown_multi.subtitle_languages.clear();
        unknown_multi.subtitle = Some(SubtitlePreference::Multi);

        let (eligible, rejected) = filter_automatic_releases_by_subtitle(
            vec![complete, partial, unknown_multi],
            &["chs".to_owned(), "cht".to_owned()],
            None,
        );
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "complete");
        assert_eq!(rejected, 2);
    }

    /// 验证自动扫描不能只依据来源声明接受无标题字幕证据的资源。
    #[test]
    fn requires_title_subtitle_evidence_for_automatic_download() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode release fixture");
        let mut source_only: Release =
            serde_json::from_value(fixture["payload"]["searchResult"]["releases"][0].clone())
                .expect("decode release");
        source_only.id = "source-only-chs".to_owned();
        source_only.title = "[LoliHouse] Kokoore - 05 [WebRip 1080p HEVC-10bit AAC].mkv".to_owned();
        source_only.subtitle_languages = vec![SubtitleLanguage::Chs];
        source_only.subtitle = Some(SubtitlePreference::Chs);

        let mut explicit_none = source_only.clone();
        explicit_none.id = "explicit-no-chs".to_owned();
        explicit_none.title = format!("{}[无中字]", explicit_none.title);
        let mut ass_with_language = source_only.clone();
        ass_with_language.id = "ass-with-chs".to_owned();
        ass_with_language.title = "[字幕组] 测试番 - 05 [ASS]".to_owned();
        let mut title_chs = source_only.clone();
        title_chs.id = "title-chs".to_owned();
        title_chs.title = "[字幕组] 测试番 - 05 [简中内封]".to_owned();
        title_chs.subtitle_languages.clear();
        title_chs.subtitle = None;

        let (eligible, rejected) = filter_automatic_releases_by_subtitle(
            vec![
                source_only.clone(),
                explicit_none,
                ass_with_language,
                title_chs,
            ],
            &["chs".to_owned()],
            None,
        );
        assert_eq!(
            eligible
                .iter()
                .map(|release| release.id.as_str())
                .collect::<Vec<_>>(),
            vec!["ass-with-chs", "title-chs"]
        );
        assert_eq!(rejected, 2);

        let (eligible_without_rule, rejected_without_rule) =
            filter_automatic_releases_by_subtitle(vec![source_only], &[], None);
        assert_eq!(eligible_without_rule.len(), 1);
        assert_eq!(rejected_without_rule, 0);
    }

    /// 验证 AniBT 错标简体的无中字资源不会进入排序或下载执行器。
    #[tokio::test]
    async fn rejects_anibt_false_chs_before_download() {
        let store = memory_store_with_episode();
        let mut anime = store.anime[0].clone();
        anime.anime.title = "Kokoore".to_owned();
        anime.anime.original_title = None;
        anime.preferred_subtitle_languages = vec!["chs".to_owned()];
        let episode = store.episodes.lock().expect("lock episodes")[0].clone();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode release fixture");
        let mut release: Release =
            serde_json::from_value(fixture["payload"]["searchResult"]["releases"][0].clone())
                .expect("decode release");
        release.title = "[LoliHouse] 『你们先走我断后』，于是10年后我成为了传说 / Kokoore - 05 [WebRip 1080p HEVC-10bit AAC][无中字]".to_owned();
        release.source_id = "anibt".to_owned();
        release.anime_id = Some(anime.anime.id.clone());
        release.episode_no = Some(episode.episode_no);
        release.subtitle_languages = vec![SubtitleLanguage::Chs];
        release.subtitle = Some(SubtitlePreference::Chs);

        let executor = Arc::new(RecordingExecutor {
            requests: Mutex::new(Vec::new()),
        });
        let service = AutomationRunService::new(test_network(), executor.clone());
        let options = AutomationRunOptions {
            now: None,
            settings: automation_settings(),
            sources: Vec::new(),
            fansubs: Vec::new(),
        };
        let policy = ScanPolicy::from_settings(&options.settings);
        let outcome = service
            .scan_episode(
                &store,
                &anime,
                &episode,
                &[],
                &[],
                &[release],
                &options,
                &policy,
            )
            .await
            .expect("scan rss candidate");

        assert!(
            matches!(outcome, EpisodeScanOutcome::Skipped(reason) if reason == "未找到匹配资源")
        );
        assert!(executor.requests.lock().expect("lock requests").is_empty());
    }

    fn memory_store_with_episode() -> MemoryStore {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode following fixture");
        let mut anime: MyAnime =
            serde_json::from_value(fixture["payload"]["myAnime"].clone()).expect("decode my anime");
        anime.auto_download = true;
        anime.anime.detail = None;
        MemoryStore {
            anime: vec![anime.clone()],
            episodes: Mutex::new(vec![Episode {
                id: "episode-1".to_owned(),
                anime_id: anime.anime.id,
                episode_no: 1.0,
                title: None,
                air_time: None,
                status: EpisodeStatus::Aired,
            }]),
            preferences: Vec::new(),
            bindings: Vec::new(),
            releases: Vec::new(),
            downloads: Vec::new(),
            notifications: Mutex::new(Vec::new()),
        }
    }

    fn automation_settings() -> AppSettings {
        serde_json::json!({
            "automation": {
                "autoDownloadEnabledGlobally": true,
                "fallbackWhenDefaultFansubMissing": "wait",
                "candidateFansubNames": []
            }
        })
    }

    fn test_network() -> Arc<SourceNetworkService> {
        Arc::new(
            SourceNetworkService::new(NativeHttpConfig {
                proxy_mode: ProxyMode::Off,
                proxy_url: None,
                timeout_ms: 1_000,
                max_response_bytes: 1024 * 1024,
                user_agent: "ani-automation-test".to_owned(),
            })
            .expect("create network"),
        )
    }
}
