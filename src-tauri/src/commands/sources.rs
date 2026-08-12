use std::sync::{Arc, Mutex};

use ani_contracts::AppCommandError;
use ani_domain::{
    AnimeReleaseQuery, AnimeSourceBinding, AnimeSourceBindingState, AnimeStatus, AppSettings,
    ConfirmAnimeSourceBindingInput, Episode, EpisodePreference, EpisodeReleasePreview, FansubGroup,
    MyAnime, Release, ReleaseMatchContext, ReleaseQuery, ReleaseSearchError, ReleaseSearchResult,
    ReleaseSourceConfig, RemoveAnimeSourceCandidateMismatchInput,
    ReportAnimeSourceCandidateMismatchInput, RssSubscriptionReleaseQuery,
    RssSubscriptionReleaseResult, SetAnimeSourceExclusionInput, SourceKind,
};
use ani_repository::{prelude::*, RepositoryError};
use ani_sources::{
    build_anime_release_search_terms, rank_releases, sort_releases_by_rules,
    AnimeSourceBindingService, ReleaseSearchService, SourceError,
    COMPLETED_ANIME_RELEASE_CACHE_TTL_MS,
};
use ani_storage::Storage;
use tauri::State;

use crate::sources::{AppSourceState, SharedReleaseSearchStore};
use crate::storage::AppStorageState;

/// 资源搜索需要的只读仓储快照。
struct SearchSnapshot {
    settings: AppSettings,
    sources: Vec<ReleaseSourceConfig>,
    fansubs: Vec<FansubGroup>,
}

/// 番剧级资源搜索需要的追番、单集和偏好快照。
struct AnimeSearchSnapshot {
    search: SearchSnapshot,
    anime: MyAnime,
    episodes: Vec<Episode>,
    preferences: Vec<EpisodePreference>,
    bindings: Vec<AnimeSourceBinding>,
}

/// 将来源与仓储错误转换为稳定 Tauri 命令错误。
fn map_source_error(action: &str, error: SourceError) -> AppCommandError {
    log::error!("Tauri 来源命令失败 action={action} error={error}");
    let code = match &error {
        SourceError::InvalidUrl(_)
        | SourceError::UnsupportedScheme(_)
        | SourceError::InvalidProxy(_)
        | SourceError::InvalidHeader(_)
        | SourceError::Parse(_) => "source_invalid_response",
        SourceError::CircuitOpen { .. } => "source_circuit_open",
        SourceError::HttpStatus { .. } | SourceError::Transport(_) => "source_network_failed",
        SourceError::ResponseTooLarge { .. } => "source_response_too_large",
        SourceError::Repository(error) => return map_repository_error(action, error.clone()),
    };
    AppCommandError {
        code: code.to_owned(),
        message: format!("{action}失败：{error}"),
    }
}

/// 将 Repository 错误转换为稳定 Tauri 命令错误。
fn map_repository_error(action: &str, error: RepositoryError) -> AppCommandError {
    let code = match &error {
        RepositoryError::InvalidInput { .. } => "invalid_input",
        RepositoryError::RecordNotFound { .. } => "record_not_found",
        RepositoryError::BackendUnavailable { .. } => "storage_unavailable",
        RepositoryError::Backend { .. } => "storage_operation_failed",
    };
    AppCommandError {
        code: code.to_owned(),
        message: format!("{action}失败：{error}"),
    }
}

/// 将线程池或 SQLite 锁错误转换为稳定命令错误。
fn map_runtime_error(action: &str, error: impl std::fmt::Display) -> AppCommandError {
    AppCommandError {
        code: "source_runtime_failed".to_owned(),
        message: format!("{action}失败：{error}"),
    }
}

/// 在线程池读取来源、字幕组和设置快照。
async fn load_search_snapshot(
    storage: Arc<Mutex<Storage>>,
    defaults: AppSettings,
    anime_id: Option<String>,
) -> Result<SearchSnapshot, AppCommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|error| map_runtime_error("读取资源搜索上下文", error))?;
        let repository = storage.repository();
        Ok(SearchSnapshot {
            settings: repository
                .get_settings(&defaults)
                .map_err(|error| map_repository_error("读取设置", error))?,
            sources: repository
                .list_sources()
                .map_err(|error| map_repository_error("读取下载源", error))?,
            fansubs: repository
                .list_fansubs(anime_id.as_deref())
                .map_err(|error| map_repository_error("读取字幕组", error))?,
        })
    })
    .await
    .map_err(|error| map_runtime_error("读取资源搜索上下文", error))?
}

/// 在线程池读取追番及其单集偏好快照。
async fn load_anime_search_snapshot(
    storage: Arc<Mutex<Storage>>,
    defaults: AppSettings,
    anime_id: String,
) -> Result<AnimeSearchSnapshot, AppCommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|error| map_runtime_error("读取番剧资源上下文", error))?;
        let repository = storage.repository();
        let anime = repository
            .list_my_anime()
            .map_err(|error| map_repository_error("读取我的追番", error))?
            .into_iter()
            .find(|item| item.anime.id == anime_id)
            .ok_or_else(|| AppCommandError {
                code: "record_not_found".to_owned(),
                message: format!("追番不存在：{anime_id}"),
            })?;
        Ok(AnimeSearchSnapshot {
            search: SearchSnapshot {
                settings: repository
                    .get_settings(&defaults)
                    .map_err(|error| map_repository_error("读取设置", error))?,
                sources: repository
                    .list_sources()
                    .map_err(|error| map_repository_error("读取下载源", error))?,
                fansubs: repository
                    .list_fansubs(Some(&anime_id))
                    .map_err(|error| map_repository_error("读取字幕组", error))?,
            },
            episodes: repository
                .list_episodes(&anime_id)
                .map_err(|error| map_repository_error("读取单集", error))?,
            preferences: repository
                .list_episode_preferences(&anime_id)
                .map_err(|error| map_repository_error("读取单集偏好", error))?,
            bindings: repository
                .list_anime_source_bindings(&anime_id)
                .map_err(|error| map_repository_error("读取番剧来源绑定", error))?,
            anime,
        })
    })
    .await
    .map_err(|error| map_runtime_error("读取番剧资源上下文", error))?
}

/// 在线程池持久化搜索中观察到的番剧字幕组。
async fn observe_search_fansubs(
    storage: Arc<Mutex<Storage>>,
    anime_id: String,
    releases: Vec<Release>,
) -> Result<(), AppCommandError> {
    if releases.is_empty() {
        return Ok(());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|error| map_runtime_error("保存搜索字幕组", error))?;
        let repository = storage.repository();
        let followed = repository
            .list_my_anime()
            .map_err(|error| map_repository_error("读取我的追番", error))?
            .iter()
            .any(|item| item.anime.id == anime_id);
        if followed {
            repository
                .observe_anime_fansubs(&anime_id, &releases)
                .map_err(|error| map_repository_error("保存搜索字幕组", error))?;
        }
        Ok(())
    })
    .await
    .map_err(|error| map_runtime_error("保存搜索字幕组", error))?
}

/// 按任意关键词搜索全部启用下载源。
#[tauri::command]
pub(crate) async fn search_releases(
    query: ReleaseQuery,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<ReleaseSearchResult, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let anime_id = query.anime_id.clone();
    let snapshot = load_search_snapshot(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
        query.anime_id.clone(),
    )
    .await?;
    let network = source_state
        .network_service(&snapshot.settings)
        .await
        .map_err(|error| map_source_error("初始化来源网络", error))?;
    let store = SharedReleaseSearchStore::new(Arc::clone(&storage));
    let result = ReleaseSearchService::new(network)
        .search(&store, &snapshot.sources, &snapshot.fansubs, query)
        .await
        .map_err(|error| map_source_error("搜索资源", error))?;
    if let Some(anime_id) = anime_id {
        observe_search_fansubs(storage, anime_id, result.releases.clone()).await?;
    }
    Ok(result)
}

/// 按追番上下文搜索资源并应用字幕组、清晰度和编码偏好排序。
#[tauri::command]
pub(crate) async fn search_anime_releases(
    mut query: AnimeReleaseQuery,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<ReleaseSearchResult, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let snapshot = load_anime_search_snapshot(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
        query.anime_id.clone(),
    )
    .await?;
    if snapshot.anime.status == AnimeStatus::Completed {
        query.cache_ttl_ms = Some(COMPLETED_ANIME_RELEASE_CACHE_TTL_MS);
    }
    let network = source_state
        .network_service(&snapshot.search.settings)
        .await
        .map_err(|error| map_source_error("初始化来源网络", error))?;
    let store = SharedReleaseSearchStore::new(Arc::clone(&storage));
    let mut result = ReleaseSearchService::new(network)
        .search_anime(
            &store,
            &snapshot.search.sources,
            &snapshot.search.fansubs,
            &snapshot.anime.anime,
            &snapshot.bindings,
            query,
        )
        .await
        .map_err(|error| map_source_error("搜索番剧资源", error))?;
    let episode_overrides = snapshot
        .preferences
        .iter()
        .filter_map(|preference| {
            let episode = snapshot
                .episodes
                .iter()
                .find(|episode| episode.id == preference.episode_id)?;
            preference
                .fansub_group_id
                .as_ref()
                .map(|fansub_id| (episode.episode_no, fansub_id.clone()))
        })
        .collect::<Vec<_>>();
    result.releases = sort_releases_by_rules(
        result.releases,
        |release| ReleaseMatchContext {
            anime: snapshot.anime.clone(),
            episode_no: release.episode_no,
            episode_fansub_override_id: release.episode_no.and_then(|episode_no| {
                episode_overrides
                    .iter()
                    .find(|(candidate, _)| *candidate == episode_no)
                    .map(|(_, fansub_id)| fansub_id.clone())
            }),
            candidate_fansub_group_ids: Vec::new(),
            candidate_fansub_names: Vec::new(),
        },
        &snapshot.search.fansubs,
    );
    observe_search_fansubs(
        Arc::clone(&storage),
        snapshot.anime.anime.id.clone(),
        result.releases.clone(),
    )
    .await?;
    Ok(result)
}

/// 搜索并评分一集的候选资源，不触发自动下载。
#[tauri::command]
pub(crate) async fn preview_episode_releases(
    anime_id: String,
    episode_id: String,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<EpisodeReleasePreview, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let snapshot = load_anime_search_snapshot(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
        anime_id.clone(),
    )
    .await?;
    let episode = snapshot
        .episodes
        .iter()
        .find(|episode| episode.id == episode_id)
        .cloned()
        .ok_or_else(|| AppCommandError {
            code: "record_not_found".to_owned(),
            message: format!("单集不存在：{episode_id}"),
        })?;
    let preference = snapshot
        .preferences
        .iter()
        .find(|preference| preference.episode_id == episode_id);
    let preferred_fansub_group_id = preference
        .and_then(|preference| preference.fansub_group_id.clone())
        .or_else(|| snapshot.anime.default_fansub_group_id.clone());
    let requested_ttl_ms = snapshot
        .search
        .settings
        .pointer("/automation/checkIntervalMinutes")
        .and_then(serde_json::Value::as_u64)
        .map(|minutes| minutes.saturating_mul(60_000));
    let cache_ttl_ms = if snapshot.anime.status == AnimeStatus::Completed {
        Some(COMPLETED_ANIME_RELEASE_CACHE_TTL_MS)
    } else {
        requested_ttl_ms
    };
    let network = source_state
        .network_service(&snapshot.search.settings)
        .await
        .map_err(|error| map_source_error("初始化单集预览网络", error))?;
    let store = SharedReleaseSearchStore::new(Arc::clone(&storage));
    let result = ReleaseSearchService::new(network)
        .search_anime(
            &store,
            &snapshot.search.sources,
            &snapshot.search.fansubs,
            &snapshot.anime.anime,
            &snapshot.bindings,
            AnimeReleaseQuery {
                anime_id: anime_id.clone(),
                episode_no: Some(episode.episode_no),
                fansub_group_id: preferred_fansub_group_id,
                preferred_resolution: snapshot.anime.preferred_resolution.clone(),
                limit: Some(80),
                cache_ttl_ms,
                force_refresh: false,
            },
        )
        .await
        .map_err(|error| map_source_error("预览单集资源", error))?;
    let releases = result
        .releases
        .into_iter()
        .map(|mut release| {
            release.anime_id = Some(anime_id.clone());
            release
        })
        .collect::<Vec<_>>();
    observe_search_fansubs(storage, anime_id.clone(), releases.clone()).await?;
    let mut candidates = rank_releases(
        &releases,
        &ReleaseMatchContext {
            anime: snapshot.anime.clone(),
            episode_no: Some(episode.episode_no),
            episode_fansub_override_id: preference
                .and_then(|preference| preference.fansub_group_id.clone()),
            candidate_fansub_group_ids: Vec::new(),
            candidate_fansub_names: Vec::new(),
        },
        &snapshot.search.fansubs,
    );
    candidates.truncate(20);
    Ok(EpisodeReleasePreview {
        anime_id,
        episode_id,
        searched_terms: build_anime_release_search_terms(&snapshot.anime.anime, &[], 12),
        candidates,
        errors: result.errors,
    })
}

/// 搜索一条追番 RSS 订阅，独立于全局来源开关。
#[tauri::command]
pub(crate) async fn search_rss_subscription_releases(
    query: RssSubscriptionReleaseQuery,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<RssSubscriptionReleaseResult, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let snapshot = match load_anime_search_snapshot(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
        query.anime_id.clone(),
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(error) if error.code == "record_not_found" => {
            return Ok(RssSubscriptionReleaseResult {
                errors: vec![ReleaseSearchError {
                    source_id: query.subscription_id.clone(),
                    message: "追番不存在".to_owned(),
                }],
                query,
                releases: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let Some(subscription) = snapshot
        .anime
        .rss_subscriptions
        .iter()
        .find(|subscription| subscription.id == query.subscription_id && subscription.enabled)
        .cloned()
    else {
        return Ok(RssSubscriptionReleaseResult {
            errors: vec![ReleaseSearchError {
                source_id: query.subscription_id.clone(),
                message: "RSS 订阅不存在或未启用".to_owned(),
            }],
            query,
            releases: Vec::new(),
        });
    };
    let source = ReleaseSourceConfig {
        id: format!("rss-subscription:{}", subscription.id),
        name: subscription.name,
        kind: SourceKind::Rss,
        enabled: true,
        use_proxy: true,
        request_interval_ms: 800,
        base_url: None,
        api_key: None,
        rss_url: Some(subscription.url),
        tags: vec!["anime".to_owned(), "rss".to_owned()],
    };
    let preferred_languages = if !subscription.preferred_subtitle_languages.is_empty() {
        subscription.preferred_subtitle_languages
    } else if !snapshot.anime.preferred_subtitle_languages.is_empty() {
        snapshot.anime.preferred_subtitle_languages.clone()
    } else {
        match subscription
            .preferred_subtitle
            .as_deref()
            .or(snapshot.anime.preferred_subtitle.as_deref())
        {
            Some("multi") => vec!["chs".to_owned(), "cht".to_owned()],
            Some(value) => vec![value.to_owned()],
            None => Vec::new(),
        }
    };
    let network = source_state
        .network_service(&snapshot.search.settings)
        .await
        .map_err(|error| map_source_error("初始化来源网络", error))?;
    let store = SharedReleaseSearchStore::new(Arc::clone(&storage));
    let result = ReleaseSearchService::new(network)
        .search_rss_subscription(
            &store,
            &source,
            &snapshot.search.fansubs,
            &snapshot.anime,
            query,
            &preferred_languages,
        )
        .await;
    observe_search_fansubs(storage, snapshot.anime.anime.id, result.releases.clone()).await?;
    Ok(result)
}

/// 读取来源绑定命令需要的当前平台设置。
async fn load_settings(
    storage: Arc<Mutex<Storage>>,
    defaults: AppSettings,
) -> Result<AppSettings, AppCommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|error| map_runtime_error("读取来源绑定设置", error))?;
        storage
            .repository()
            .get_settings(&defaults)
            .map_err(|error| map_repository_error("读取设置", error))
    })
    .await
    .map_err(|error| map_runtime_error("读取来源绑定设置", error))?
}

/// 读取番剧来源绑定状态，并按需发现 AniBT/Mikan 候选。
#[tauri::command]
pub(crate) async fn get_anime_source_binding_state(
    anime_id: String,
    discover_candidates: Option<bool>,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<AnimeSourceBindingState, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .get_state(
            &SharedReleaseSearchStore::new(storage),
            &anime_id,
            discover_candidates.unwrap_or(true),
        )
        .await
        .map_err(|error| map_source_error("读取番剧来源绑定", error))
}

/// 确认一个番剧来源候选并保存稳定绑定。
#[tauri::command]
pub(crate) async fn confirm_anime_source_binding(
    input: ConfirmAnimeSourceBindingInput,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<AnimeSourceBindingState, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .confirm(&SharedReleaseSearchStore::new(storage), input)
        .await
        .map_err(|error| map_source_error("确认番剧来源绑定", error))
}

/// 保存用户确认的不匹配来源候选。
#[tauri::command]
pub(crate) async fn report_anime_source_candidate_mismatch(
    input: ReportAnimeSourceCandidateMismatchInput,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<(), AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .report_mismatch(&SharedReleaseSearchStore::new(storage), input)
        .map_err(|error| map_source_error("记录来源候选不匹配", error))
}

/// 撤销一个来源候选的不匹配记录。
#[tauri::command]
pub(crate) async fn remove_anime_source_candidate_mismatch(
    input: RemoveAnimeSourceCandidateMismatchInput,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<AnimeSourceBindingState, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .remove_candidate_mismatch(&SharedReleaseSearchStore::new(storage), input)
        .await
        .map_err(|error| map_source_error("撤销来源候选不匹配", error))
}

/// 设置或取消当前番剧对整个来源的候选排除。
#[tauri::command]
pub(crate) async fn set_anime_source_excluded(
    input: SetAnimeSourceExclusionInput,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<AnimeSourceBindingState, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .set_source_excluded(&SharedReleaseSearchStore::new(storage), input)
        .await
        .map_err(|error| map_source_error("更新番剧来源排除", error))
}

/// 取消一个已确认来源绑定并重新开放候选发现。
#[tauri::command]
pub(crate) async fn remove_anime_source_binding(
    anime_id: String,
    source_id: String,
    storage_state: State<'_, AppStorageState>,
    source_state: State<'_, AppSourceState>,
) -> Result<AnimeSourceBindingState, AppCommandError> {
    let storage = Arc::clone(storage_state.storage());
    let settings = load_settings(
        Arc::clone(&storage),
        storage_state.platform_defaults().clone(),
    )
    .await?;
    let network = source_state
        .network_service(&settings)
        .await
        .map_err(|error| map_source_error("初始化来源绑定网络", error))?;
    AnimeSourceBindingService::new(network)
        .remove(
            &SharedReleaseSearchStore::new(storage),
            &anime_id,
            &source_id,
        )
        .await
        .map_err(|error| map_source_error("取消番剧来源绑定", error))
}
