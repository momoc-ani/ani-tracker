use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use ani_domain::{
    Anime, AnimeReleaseQuery, AnimeSourceBinding, FansubGroup, MyAnime, Release,
    ReleaseContentKind, ReleaseQuery, ReleaseSearchError, ReleaseSearchResult, ReleaseSourceConfig,
    ReleaseSourceSearchResult, ReleaseSourceSyncState, RssSubscriptionReleaseQuery,
    RssSubscriptionReleaseResult, SourceKind, SubtitleLanguage, SubtitlePreference,
};
use ani_repository::{
    CachedReleaseQuery, ReleaseCacheRepository, ReleaseSearchCacheEntry, ReleaseSourceRepository,
    RepositoryResult,
};
use chrono::{Duration, SecondsFormat, Utc};
use futures_util::future::join_all;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::parsers::{
    extract_info_hash, extract_torrent_url_info_hash, normalize_info_hash,
    parse_acgnx_api_response, parse_acgnx_html, parse_anibt_rss, parse_dmhy_list,
    parse_mikan_release_list, parse_rss_releases, parse_torznab_releases,
};
use crate::release::{
    build_anime_release_search_terms, classify_anime_release, enrich_release_from_title,
    matches_anime_release_title, normalize_release_search_text, release_matches_episode,
    AnimeReleaseCompatibility,
};
use crate::{
    CircuitStateStore, HttpMethod, NativeHttpRequest, NetworkRequestChannel, SourceError,
    SourceNetworkService,
};

pub const MAX_RELEASE_SOURCE_FETCH_LIMIT: usize = 50;
pub const MAX_RELEASE_SOURCE_RESULT_LIMIT: usize = 200;
pub const COMPLETED_ANIME_RELEASE_CACHE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const RELEASE_SEARCH_CACHE_VERSION: u32 = 5;
const MAX_ANIBT_BGM_FEEDS_PER_SEARCH: usize = 3;
const DESKTOP_BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/138.0 Safari/537.36";
const DESKTOP_BROWSER_ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,ja;q=0.8,en;q=0.7";

/// 单个来源增量采集返回的资源与条件请求游标。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSyncFetchResult {
    pub releases: Vec<Release>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub not_modified: bool,
}

/// 资源搜索需要的最小缓存与熔断状态端口。
pub trait ReleaseSearchStore: CircuitStateStore {
    /// 读取尚未过期的搜索结果缓存。
    fn get_search_cache(
        &self,
        cache_key: &str,
        current_time: &str,
    ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>>;

    /// 保存搜索结果缓存。
    fn save_search_cache(
        &self,
        cache_key: &str,
        entry: &ReleaseSearchCacheEntry,
    ) -> RepositoryResult<()>;

    /// 按来源和番剧读取跨重启原始资源缓存。
    fn list_release_cache(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>>;

    /// 增量保存网络返回的原始资源。
    fn save_release_cache(&self, releases: &[Release]) -> RepositoryResult<usize>;
}

impl<T> ReleaseSearchStore for T
where
    T: ReleaseSourceRepository + ReleaseCacheRepository,
{
    /// 将完整来源 Repository 适配为搜索缓存端口。
    fn get_search_cache(
        &self,
        cache_key: &str,
        current_time: &str,
    ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>> {
        ReleaseSourceRepository::get_release_search_cache(self, cache_key, current_time)
    }

    /// 将完整来源 Repository 适配为搜索缓存端口。
    fn save_search_cache(
        &self,
        cache_key: &str,
        entry: &ReleaseSearchCacheEntry,
    ) -> RepositoryResult<()> {
        ReleaseSourceRepository::upsert_release_search_cache(self, cache_key, entry)
    }

    /// 将完整缓存 Repository 适配为搜索原始资源读取端口。
    fn list_release_cache(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>> {
        ReleaseCacheRepository::list_cached_releases(self, query)
    }

    /// 将完整缓存 Repository 适配为搜索原始资源写入端口。
    fn save_release_cache(&self, releases: &[Release]) -> RepositoryResult<usize> {
        ReleaseCacheRepository::upsert_cached_releases(self, releases)
    }
}

/// 组合来源网络、站点适配器、标题匹配和持久化缓存的搜索服务。
pub struct ReleaseSearchService {
    network: Arc<SourceNetworkService>,
    channel: NetworkRequestChannel,
}

impl ReleaseSearchService {
    /// 创建复用同一连接池、限流和熔断策略的搜索服务。
    pub fn new(network: Arc<SourceNetworkService>) -> Self {
        Self {
            network,
            channel: NetworkRequestChannel::Interactive,
        }
    }

    /// 创建使用独立后台限流与熔断状态的采集服务。
    pub fn new_background(network: Arc<SourceNetworkService>) -> Self {
        Self {
            network,
            channel: NetworkRequestChannel::Background,
        }
    }

    /// 按任意关键词并行搜索全部启用来源，单源失败不会清空成功结果。
    pub async fn search<S>(
        &self,
        store: &S,
        configs: &[ReleaseSourceConfig],
        fansubs: &[FansubGroup],
        query: ReleaseQuery,
    ) -> Result<ReleaseSearchResult, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let cache_key = build_search_cache_key(&query, configs, fansubs, None);
        if !query.force_refresh {
            if let Some(result) = load_cached_result(store, cache_key.as_deref(), &query)? {
                log::info!(
                    "Rust 资源搜索命中持久化缓存 anime_id={:?} keyword={} count={}",
                    query.anime_id,
                    query.keyword,
                    result.releases.len()
                );
                return Ok(result);
            }
        }

        let sources = enabled_supported_sources(configs);
        let fetched = join_all(
            sources
                .iter()
                .map(|config| self.fetch_source(store, config, &query)),
        )
        .await;
        let mut source_results = Vec::with_capacity(sources.len());
        let mut errors = Vec::new();
        for (config, fetched) in sources.iter().zip(fetched) {
            match fetched {
                Ok(releases) => source_results.push(SourceFetchResult {
                    source: config.clone(),
                    releases: releases
                        .into_iter()
                        .map(|release| enrich_release_from_title(release, fansubs))
                        .collect(),
                }),
                Err(error) => {
                    log::warn!(
                        "Rust 下载源搜索失败 source_id={} error={}",
                        config.id,
                        error
                    );
                    errors.push(ReleaseSearchError {
                        source_id: config.id.clone(),
                        message: format_source_error(&error),
                    });
                    source_results.push(SourceFetchResult {
                        source: config.clone(),
                        releases: Vec::new(),
                    });
                }
            }
        }

        let matches_query = |release: &Release| {
            release_matches_episode(release, query.episode_no)
                && matches_keyword(release, &query.keyword)
        };
        let limit = normalize_result_limit(query.limit);
        let live_releases = dedupe_releases(
            source_results
                .iter()
                .flat_map(|result| result.releases.iter().cloned())
                .collect(),
        );
        store.save_release_cache(&live_releases)?;
        let cached_releases = store
            .list_release_cache(&CachedReleaseQuery {
                source_ids: Some(sources.iter().map(|source| source.id.clone()).collect()),
                anime_id: None,
                limit: Some(2_000),
            })?
            .into_iter()
            .map(|release| enrich_release_from_title(release, fansubs))
            .collect::<Vec<_>>();
        let releases = sort_releases(dedupe_releases(
            live_releases
                .iter()
                .chain(cached_releases.iter())
                .filter(|release| matches_query(release))
                .cloned()
                .collect(),
        ))
        .into_iter()
        .take(limit)
        .collect::<Vec<_>>();
        let result = ReleaseSearchResult {
            query: query.clone(),
            releases,
            source_results: source_results
                .into_iter()
                .map(|result| {
                    let source_id = result.source.id;
                    let source_name = result.source.name;
                    let releases = sort_releases(dedupe_releases(
                        result
                            .releases
                            .into_iter()
                            .chain(
                                cached_releases
                                    .iter()
                                    .filter(|release| release.source_id == source_id)
                                    .cloned(),
                            )
                            .filter(matches_query)
                            .collect(),
                    ))
                    .into_iter()
                    .take(limit)
                    .collect();
                    ReleaseSourceSearchResult {
                        source_id,
                        source_name,
                        releases,
                    }
                })
                .collect(),
            searched_source_ids: sources.iter().map(|source| source.id.clone()).collect(),
            errors,
        };
        save_cached_result(store, cache_key.as_deref(), query.cache_ttl_ms, &result);
        log::info!(
            "Rust 资源搜索完成 keyword={} source_count={} release_count={} error_count={}",
            query.keyword,
            result.searched_source_ids.len(),
            result.releases.len(),
            result.errors.len()
        );
        Ok(result)
    }

    /// 按本地番剧标题、原名和别名搜索并过滤季度与目标集数。
    pub async fn search_anime<S>(
        &self,
        store: &S,
        configs: &[ReleaseSourceConfig],
        fansubs: &[FansubGroup],
        anime: &Anime,
        bindings: &[AnimeSourceBinding],
        query: AnimeReleaseQuery,
    ) -> Result<ReleaseSearchResult, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let release_query = ReleaseQuery {
            keyword: anime.title.clone(),
            anime_id: Some(query.anime_id.clone()),
            episode_no: query.episode_no,
            fansub_group_id: query.fansub_group_id.clone(),
            preferred_resolution: query.preferred_resolution.clone(),
            limit: query.limit,
            cache_ttl_ms: query.cache_ttl_ms,
            force_refresh: query.force_refresh,
        };
        let terms = build_anime_release_search_terms(anime, &[], 8);
        let cache_key = build_search_cache_key(
            &release_query,
            configs,
            fansubs,
            Some(json!({
                "kind": "anime",
                "anime": anime,
                "terms": terms,
                "bindings": bindings.iter().filter(|binding| binding.confirmed).collect::<Vec<_>>(),
            })),
        );
        if !query.force_refresh {
            if let Some(result) = load_cached_result(store, cache_key.as_deref(), &release_query)? {
                return Ok(result);
            }
        }

        let sources = enabled_supported_sources(configs)
            .into_iter()
            .filter(|source| !is_mikan_site_config(source))
            .collect::<Vec<_>>();
        let fetched = join_all(sources.iter().map(|config| async {
            let binding = bindings.iter().find(|binding| {
                binding.anime_id == anime.id && binding.source_id == config.id && binding.confirmed
            });
            let (result, anime_scoped) = if is_bindable_anime_source(config) {
                (
                    match binding {
                        Some(binding) => {
                            self.fetch_bound_anime_source(store, config, binding, &release_query)
                                .await
                        }
                        None => Err(SourceError::Parse(format!(
                            "下载源无法生成当前番剧的精确 RSS，请先确认{}番剧匹配",
                            config.name
                        ))),
                    },
                    binding.is_some(),
                )
            } else if config.kind == SourceKind::Rss {
                (
                    self.fetch_source(
                        store,
                        config,
                        &ReleaseQuery {
                            keyword: String::new(),
                            ..release_query.clone()
                        },
                    )
                    .await,
                    false,
                )
            } else {
                let mut releases = Vec::new();
                for term in &terms {
                    releases.extend(
                        self.fetch_source(
                            store,
                            config,
                            &ReleaseQuery {
                                keyword: term.clone(),
                                ..release_query.clone()
                            },
                        )
                        .await?,
                    );
                }
                (Ok(dedupe_releases(releases)), false)
            };
            result.map(|releases| (config.clone(), releases, anime_scoped))
        }))
        .await;

        let mut errors = Vec::new();
        let mut per_source = Vec::with_capacity(sources.len());
        for (config, fetched) in sources.iter().zip(fetched) {
            match fetched {
                Ok((source, releases, anime_scoped)) => per_source.push(SourceFetchResult {
                    source,
                    releases: releases
                        .into_iter()
                        .map(|release| enrich_release_from_title(release, fansubs))
                        .filter(|release| {
                            (anime_scoped || matches_anime_release_title(&release.title, &terms))
                                && classify_anime_release(release, anime)
                                    != AnimeReleaseCompatibility::Mismatch
                                && release_matches_episode(release, query.episode_no)
                        })
                        .map(|mut release| {
                            release.anime_id = Some(anime.id.clone());
                            release
                        })
                        .collect(),
                }),
                Err(error) => {
                    errors.push(ReleaseSearchError {
                        source_id: config.id.clone(),
                        message: format_source_error(&error),
                    });
                    per_source.push(SourceFetchResult {
                        source: config.clone(),
                        releases: Vec::new(),
                    });
                }
            }
        }
        let limit = normalize_result_limit(query.limit);
        let live_releases = dedupe_releases(
            per_source
                .iter()
                .flat_map(|result| result.releases.iter().cloned())
                .collect(),
        );
        store.save_release_cache(&live_releases)?;
        let cached_releases = store
            .list_release_cache(&CachedReleaseQuery {
                source_ids: Some(sources.iter().map(|source| source.id.clone()).collect()),
                anime_id: Some(anime.id.clone()),
                limit: Some(2_000),
            })?
            .into_iter()
            .map(|release| enrich_release_from_title(release, fansubs))
            .filter(|release| {
                classify_anime_release(release, anime) != AnimeReleaseCompatibility::Mismatch
                    && release_matches_episode(release, query.episode_no)
            })
            .collect::<Vec<_>>();
        let releases = sort_releases(dedupe_releases(
            live_releases
                .iter()
                .chain(cached_releases.iter())
                .cloned()
                .collect(),
        ))
        .into_iter()
        .take(limit)
        .collect();
        let result = ReleaseSearchResult {
            query: release_query,
            releases,
            source_results: per_source
                .into_iter()
                .map(|result| {
                    let source_id = result.source.id;
                    let source_name = result.source.name;
                    let releases = sort_releases(dedupe_releases(
                        result
                            .releases
                            .into_iter()
                            .chain(
                                cached_releases
                                    .iter()
                                    .filter(|release| release.source_id == source_id)
                                    .cloned(),
                            )
                            .collect(),
                    ))
                    .into_iter()
                    .take(limit)
                    .collect();
                    ReleaseSourceSearchResult {
                        source_id,
                        source_name,
                        releases,
                    }
                })
                .collect(),
            searched_source_ids: sources.iter().map(|source| source.id.clone()).collect(),
            errors,
        };
        save_cached_result(store, cache_key.as_deref(), query.cache_ttl_ms, &result);
        Ok(result)
    }

    /// 搜索单条追番 RSS，独立于全局来源开关并按番剧上下文过滤。
    pub async fn search_rss_subscription<S>(
        &self,
        store: &S,
        source: &ReleaseSourceConfig,
        fansubs: &[FansubGroup],
        anime: &MyAnime,
        query: RssSubscriptionReleaseQuery,
        preferred_subtitle_languages: &[String],
    ) -> RssSubscriptionReleaseResult
    where
        S: ReleaseSearchStore + Sync,
    {
        let source_query = ReleaseQuery {
            keyword: String::new(),
            anime_id: Some(query.anime_id.clone()),
            episode_no: None,
            fansub_group_id: None,
            preferred_resolution: query.preferred_resolution.clone(),
            limit: query.limit,
            cache_ttl_ms: None,
            force_refresh: true,
        };
        match self.fetch_source(store, source, &source_query).await {
            Ok(releases) => {
                let terms = build_anime_release_search_terms(&anime.anime, &[], 12);
                let exact_mikan = source
                    .rss_url
                    .as_deref()
                    .is_some_and(is_exact_mikan_rss_url);
                let mut releases = releases
                    .into_iter()
                    .map(|release| enrich_release_from_title(release, fansubs))
                    .filter(|release| {
                        (exact_mikan || matches_anime_release_title(&release.title, &terms))
                            && classify_anime_release(release, &anime.anime)
                                != AnimeReleaseCompatibility::Mismatch
                    })
                    .map(|mut release| {
                        release.anime_id = Some(anime.anime.id.clone());
                        release
                    })
                    .collect::<Vec<_>>();
                let preferred = preferred_subtitle_languages
                    .iter()
                    .filter_map(|value| parse_subtitle_language(value))
                    .collect::<Vec<_>>();
                releases.sort_by(|left, right| {
                    subtitle_sort_rank(left, &preferred)
                        .cmp(&subtitle_sort_rank(right, &preferred))
                        .then_with(|| right.published_at.cmp(&left.published_at))
                });
                RssSubscriptionReleaseResult {
                    query,
                    releases,
                    errors: Vec::new(),
                }
            }
            Err(error) => RssSubscriptionReleaseResult {
                errors: vec![ReleaseSearchError {
                    source_id: query.subscription_id.clone(),
                    message: format_source_error(&error),
                }],
                query,
                releases: Vec::new(),
            },
        }
    }

    /// 为每日同步采集一个来源，优先使用追番的已确认精确 RSS。
    pub async fn collect_source_for_sync<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        tracked_anime: &[MyAnime],
        bindings: &[AnimeSourceBinding],
        state: &ReleaseSourceSyncState,
    ) -> Result<SourceSyncFetchResult, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        if is_bindable_anime_source(config) {
            let mut releases = Vec::new();
            for item in tracked_anime {
                let Some(binding) = bindings.iter().find(|binding| {
                    binding.anime_id == item.anime.id
                        && binding.source_id == config.id
                        && binding.confirmed
                }) else {
                    continue;
                };
                releases.extend(
                    self.fetch_bound_anime_source(
                        store,
                        config,
                        binding,
                        &ReleaseQuery {
                            keyword: String::new(),
                            anime_id: Some(item.anime.id.clone()),
                            episode_no: None,
                            fansub_group_id: None,
                            preferred_resolution: None,
                            limit: Some(MAX_RELEASE_SOURCE_FETCH_LIMIT),
                            cache_ttl_ms: None,
                            force_refresh: true,
                        },
                    )
                    .await?
                    .into_iter()
                    .map(|mut release| {
                        release.anime_id = Some(item.anime.id.clone());
                        release
                    }),
                );
            }
            if !releases.is_empty() || is_mikan_site_config(config) {
                return Ok(SourceSyncFetchResult {
                    releases: dedupe_releases(releases),
                    etag: None,
                    last_modified: None,
                    not_modified: false,
                });
            }
        }

        let query = ReleaseQuery {
            keyword: String::new(),
            anime_id: None,
            episode_no: None,
            fansub_group_id: None,
            preferred_resolution: None,
            limit: Some(MAX_RELEASE_SOURCE_FETCH_LIMIT),
            cache_ttl_ms: None,
            force_refresh: true,
        };
        if config.kind == SourceKind::Rss {
            return self
                .fetch_generic_rss_for_sync(store, config, &query, state)
                .await;
        }
        Ok(SourceSyncFetchResult {
            releases: self.fetch_source(store, config, &query).await?,
            etag: None,
            last_modified: None,
            not_modified: false,
        })
    }

    async fn fetch_source<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        match config.kind {
            SourceKind::Rss => self.fetch_generic_rss(store, config, query).await,
            SourceKind::Torznab => self.fetch_torznab(store, config, query).await,
            SourceKind::SiteAdapter if is_dmhy_config(config) => {
                self.fetch_dmhy(store, config, query).await
            }
            SourceKind::SiteAdapter if is_mikan_config(config) => {
                self.fetch_mikan(store, config, query).await
            }
            SourceKind::SiteAdapter if is_anibt_config(config) => {
                self.fetch_anibt(store, config, query).await
            }
            SourceKind::SiteAdapter if is_acgnx_config(config) => {
                self.fetch_acgnx(store, config, query).await
            }
            SourceKind::SiteAdapter if is_nyaa_config(config) => {
                self.fetch_rss_pages(store, config, query, build_nyaa_rss_url, "Nyaa")
                    .await
            }
            SourceKind::SiteAdapter if is_acgrip_config(config) => {
                self.fetch_rss_pages(store, config, query, build_acgrip_rss_url, "ACG.RIP")
                    .await
            }
            SourceKind::SiteAdapter | SourceKind::Manual => Ok(Vec::new()),
        }
    }

    /// 根据已确认绑定读取 AniBT 或 Mikan 单番精确 RSS。
    async fn fetch_bound_anime_source<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        binding: &AnimeSourceBinding,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let limit = normalize_fetch_limit(query.limit);
        let url = if is_anibt_config(config) {
            build_anibt_anime_rss_url(config, &binding.source_anime_id, limit)?
        } else {
            build_mikan_anime_rss_url(config, &binding.source_anime_id)?
        };
        let headers = if is_anibt_config(config) {
            create_anibt_headers(config, "application/rss+xml,application/xml,text/xml")
        } else {
            BTreeMap::from([(
                "Accept".to_owned(),
                "application/rss+xml,application/xml,text/xml".to_owned(),
            )])
        };
        let response = self.get_with_headers(store, config, &url, headers).await?;
        let mut releases = if is_anibt_config(config) {
            parse_anibt_rss(&response.text(), config)?
        } else {
            parse_rss_releases(&response.text(), config, Some(&url))?
        };
        releases.truncate(limit);
        Ok(releases)
    }

    async fn fetch_generic_rss<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        Ok(self
            .fetch_generic_rss_for_sync(
                store,
                config,
                query,
                &ReleaseSourceSyncState {
                    source_id: config.id.clone(),
                    request_host: None,
                    last_request_at: None,
                    request_failure_count: 0,
                    backoff_until: None,
                    last_sync_attempt_at: None,
                    last_successful_sync_at: None,
                    last_sync_error: None,
                    etag: None,
                    last_modified: None,
                },
            )
            .await?
            .releases)
    }

    /// 读取通用 RSS，并为同步任务维护 ETag 与 Last-Modified。
    async fn fetch_generic_rss_for_sync<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
        state: &ReleaseSourceSyncState,
    ) -> Result<SourceSyncFetchResult, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let Some(url) = config.rss_url.as_deref() else {
            return Ok(SourceSyncFetchResult {
                releases: Vec::new(),
                etag: None,
                last_modified: None,
                not_modified: false,
            });
        };
        let mut headers = default_request_headers();
        headers.insert(
            "Accept".to_owned(),
            "application/rss+xml,application/atom+xml,application/xml,text/xml;q=0.9,*/*;q=0.8"
                .to_owned(),
        );
        if let Some(etag) = state.etag.as_deref() {
            headers.insert("If-None-Match".to_owned(), etag.to_owned());
        }
        if let Some(last_modified) = state.last_modified.as_deref() {
            headers.insert("If-Modified-Since".to_owned(), last_modified.to_owned());
        }
        let response = self.get_with_headers(store, config, url, headers).await?;
        let etag = response.header("etag").map(str::to_owned);
        let last_modified = response.header("last-modified").map(str::to_owned);
        if response.status == 304 {
            return Ok(SourceSyncFetchResult {
                releases: Vec::new(),
                etag,
                last_modified,
                not_modified: true,
            });
        }
        let keyword = normalize_release_search_text(&query.keyword);
        let releases = parse_rss_releases(&response.text(), config, Some(url))?
            .into_iter()
            .filter(|release| {
                keyword.is_empty()
                    || normalize_release_search_text(&release.title).contains(&keyword)
            })
            .take(normalize_fetch_limit(query.limit))
            .collect();
        Ok(SourceSyncFetchResult {
            releases,
            etag,
            last_modified,
            not_modified: false,
        })
    }

    async fn fetch_torznab<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let Some(base_url) = config.base_url.as_deref() else {
            return Ok(Vec::new());
        };
        let target = normalize_result_limit(query.limit);
        let mut releases = Vec::new();
        let mut offset = 0;
        while releases.len() < target {
            let page_limit = MAX_RELEASE_SOURCE_FETCH_LIMIT.min(target - releases.len());
            let mut url = Url::parse(base_url)
                .and_then(|base| base.join("/api"))
                .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
            url.query_pairs_mut()
                .append_pair("t", "search")
                .append_pair("q", &query.keyword)
                .append_pair("limit", &page_limit.to_string())
                .append_pair("offset", &offset.to_string());
            if let Some(api_key) = config.api_key.as_deref() {
                url.query_pairs_mut().append_pair("apikey", api_key);
            }
            let response = self.get(store, config, url.as_str(), &[]).await?;
            let page = parse_torznab_releases(&response.text(), config)?;
            let count = page.releases.len();
            releases.extend(page.releases);
            let reported_offset = page.offset.unwrap_or(offset);
            offset = reported_offset + count;
            if count == 0 || page.total.is_some_and(|total| offset >= total) || count < page_limit {
                break;
            }
        }
        releases.truncate(target);
        Ok(releases)
    }

    async fn fetch_dmhy<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or("https://share.dmhy.org/");
        self.fetch_html_pages(
            store,
            config,
            query,
            |page| build_dmhy_list_url(base_url, &query.keyword, page),
            parse_dmhy_list,
        )
        .await
    }

    async fn fetch_mikan<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        if query.keyword.trim().is_empty() {
            return Ok(Vec::new());
        }
        let base_url = config.base_url.as_deref().unwrap_or("https://mikanani.me/");
        self.fetch_html_pages(
            store,
            config,
            query,
            |page| build_mikan_search_url(base_url, &query.keyword, page),
            parse_mikan_release_list,
        )
        .await
    }

    async fn fetch_anibt<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let target = normalize_fetch_limit(query.limit);
        let mut releases = Vec::new();
        if !query.keyword.trim().is_empty() {
            match self
                .search_anibt_bgm_ids(store, config, &query.keyword)
                .await
            {
                Ok(ids) => {
                    for bgm_id in ids.into_iter().take(MAX_ANIBT_BGM_FEEDS_PER_SEARCH) {
                        let url = build_anibt_anime_rss_url(config, &bgm_id, target)?;
                        let response = self
                            .get_with_headers(
                                store,
                                config,
                                &url,
                                create_anibt_headers(
                                    config,
                                    "application/rss+xml,application/xml,text/xml",
                                ),
                            )
                            .await?;
                        releases.extend(parse_anibt_rss(&response.text(), config)?);
                    }
                }
                Err(error) => log::warn!(
                    "AniBT 番剧匹配失败，回退最新 RSS source_id={} error={}",
                    config.id,
                    error
                ),
            }
        }
        if query.keyword.trim().is_empty() || releases.len() < target {
            let mut url = Url::parse(config.base_url.as_deref().unwrap_or("https://anibt.net/"))
                .and_then(|base| base.join("/rss/magnets.xml"))
                .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
            url.query_pairs_mut()
                .append_pair("limit", &target.to_string());
            let response = self
                .get_with_headers(
                    store,
                    config,
                    url.as_str(),
                    create_anibt_headers(config, "application/rss+xml,application/xml,text/xml"),
                )
                .await?;
            let latest = parse_anibt_rss(&response.text(), config)?;
            releases.extend(latest.into_iter().filter(|release| {
                query.keyword.trim().is_empty() || matches_keyword(release, &query.keyword)
            }));
        }
        let mut releases = dedupe_releases(releases);
        releases.truncate(target);
        Ok(releases)
    }

    async fn search_anibt_bgm_ids<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        keyword: &str,
    ) -> Result<Vec<String>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let mut url = Url::parse(config.base_url.as_deref().unwrap_or("https://anibt.net/"))
            .and_then(|base| base.join("/api/bgm/search"))
            .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
        url.query_pairs_mut().append_pair("q", keyword);
        let response = self
            .get_with_headers(
                store,
                config,
                url.as_str(),
                create_anibt_headers(config, "application/json"),
            )
            .await?;
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::Parse(error.to_string()))?;
        if payload.get("ok") == Some(&Value::Bool(false)) {
            return Err(SourceError::Parse("AniBT BGM 查询返回错误".to_owned()));
        }
        Ok(payload
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("bgmId"))
            .filter_map(value_to_string)
            .collect())
    }

    async fn fetch_acgnx<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let target = normalize_result_limit(query.limit);
        let mut releases = Vec::new();
        let max_pages = target.div_ceil(MAX_RELEASE_SOURCE_FETCH_LIMIT);
        for page in 1..=max_pages {
            let mut page_releases = Vec::new();
            let candidates = build_acgnx_search_urls(
                config
                    .base_url
                    .as_deref()
                    .unwrap_or("https://share.acgnx.se/"),
                &query.keyword,
                page,
                MAX_RELEASE_SOURCE_FETCH_LIMIT,
            )?;
            let mut last_error = None;
            for url in candidates {
                match self
                    .get(
                        store,
                        config,
                        &url,
                        &[("Accept", "application/json,text/html,application/xhtml+xml")],
                    )
                    .await
                {
                    Ok(response) => {
                        let text = response.text();
                        let trimmed = text.trim();
                        page_releases = if trimmed.starts_with('{') || trimmed.starts_with('[') {
                            let payload = serde_json::from_str(trimmed)
                                .map_err(|error| SourceError::Parse(error.to_string()))?;
                            parse_acgnx_api_response(&payload, config)
                        } else {
                            parse_acgnx_html(&text, config)
                        };
                        if !page_releases.is_empty() {
                            break;
                        }
                    }
                    Err(SourceError::HttpStatus {
                        status: 404 | 405, ..
                    }) => continue,
                    Err(error) => last_error = Some(error),
                }
            }
            if page_releases.is_empty() {
                if releases.is_empty() {
                    if let Some(error) = last_error {
                        return Err(error);
                    }
                }
                break;
            }
            let count = page_releases.len();
            releases.extend(page_releases);
            if releases.len() >= target || count < MAX_RELEASE_SOURCE_FETCH_LIMIT {
                break;
            }
        }
        let mut releases = dedupe_releases(releases);
        releases.truncate(target);
        Ok(releases)
    }

    async fn fetch_rss_pages<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
        build_url: fn(&str, &str, usize) -> Result<String, SourceError>,
        source_name: &str,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let Some(base_url) = config.base_url.as_deref() else {
            return Ok(Vec::new());
        };
        let target = normalize_result_limit(query.limit);
        let max_pages = target.div_ceil(MAX_RELEASE_SOURCE_FETCH_LIMIT);
        let mut releases = Vec::new();
        for page in 1..=max_pages {
            let url = build_url(base_url, &query.keyword, page)?;
            let response = self
                .get(
                    store,
                    config,
                    &url,
                    &[("Accept", "application/rss+xml,application/xml,text/xml")],
                )
                .await?;
            let page_releases = parse_rss_releases(&response.text(), config, Some(&url))?;
            let count = page_releases.len();
            releases.extend(page_releases);
            if releases.len() >= target || count < MAX_RELEASE_SOURCE_FETCH_LIMIT {
                break;
            }
        }
        releases.truncate(target);
        log::info!("Rust {source_name} 搜索完成 count={}", releases.len());
        Ok(releases)
    }

    async fn fetch_html_pages<S, B, P>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        query: &ReleaseQuery,
        build_url: B,
        parse: P,
    ) -> Result<Vec<Release>, SourceError>
    where
        S: ReleaseSearchStore + Sync,
        B: Fn(usize) -> Result<String, SourceError>,
        P: Fn(&str, &ReleaseSourceConfig) -> Vec<Release>,
    {
        let target = normalize_result_limit(query.limit);
        let max_pages = target.div_ceil(MAX_RELEASE_SOURCE_FETCH_LIMIT);
        let mut releases = Vec::new();
        for page in 1..=max_pages {
            let url = build_url(page)?;
            let response = self
                .get(
                    store,
                    config,
                    &url,
                    &[("Accept", "text/html,application/xhtml+xml")],
                )
                .await?;
            let page_releases = parse(&response.text(), config);
            let count = page_releases.len();
            releases.extend(page_releases);
            if releases.len() >= target || count < MAX_RELEASE_SOURCE_FETCH_LIMIT {
                break;
            }
        }
        releases.truncate(target);
        Ok(releases)
    }

    async fn get<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<crate::NativeHttpResponse, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let mut values = default_request_headers();
        values.extend(
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        self.get_with_headers(store, config, url, values).await
    }

    async fn get_with_headers<S>(
        &self,
        store: &S,
        config: &ReleaseSourceConfig,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<crate::NativeHttpResponse, SourceError>
    where
        S: ReleaseSearchStore + Sync,
    {
        let request = NativeHttpRequest {
            source_id: config.id.clone(),
            method: HttpMethod::Get,
            url: url.to_owned(),
            headers,
            body: None,
            request_interval_ms: config.request_interval_ms.max(0) as u64,
        };
        match self.channel {
            NetworkRequestChannel::Interactive => {
                self.network.execute(store, config, request).await
            }
            NetworkRequestChannel::Background => {
                self.network
                    .execute_background(store, config, request)
                    .await
            }
        }
    }
}

#[derive(Debug)]
struct SourceFetchResult {
    source: ReleaseSourceConfig,
    releases: Vec<Release>,
}

/// 生成动漫花园关键词分页 URL。
pub fn build_dmhy_list_url(
    base_url: &str,
    keyword: &str,
    page: usize,
) -> Result<String, SourceError> {
    let path = if page > 1 {
        format!("/topics/list/page/{page}")
    } else {
        "/topics/list".to_owned()
    };
    let mut url = Url::parse(base_url)
        .and_then(|base| base.join(&path))
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    if !keyword.trim().is_empty() {
        url.query_pairs_mut().append_pair("keyword", keyword.trim());
    }
    Ok(url.to_string())
}

/// 生成 Mikan 关键词分页 URL。
pub fn build_mikan_search_url(
    base_url: &str,
    keyword: &str,
    page: usize,
) -> Result<String, SourceError> {
    let mut url = Url::parse(base_url)
        .and_then(|base| base.join("/Home/Search"))
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut().append_pair("searchstr", keyword);
    if page > 1 {
        url.query_pairs_mut().append_pair("page", &page.to_string());
    }
    Ok(url.to_string())
}

/// 生成 Nyaa Anime RSS 查询 URL。
pub fn build_nyaa_rss_url(
    base_url: &str,
    keyword: &str,
    page: usize,
) -> Result<String, SourceError> {
    let mut url = Url::parse(base_url)
        .and_then(|base| base.join("/"))
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut().append_pair("page", "rss");
    if !keyword.trim().is_empty() {
        url.query_pairs_mut().append_pair("q", keyword.trim());
    }
    url.query_pairs_mut()
        .append_pair("c", "1_0")
        .append_pair("f", "0");
    if page > 1 {
        url.query_pairs_mut().append_pair("p", &page.to_string());
    }
    Ok(url.to_string())
}

/// 生成 ACG.RIP RSS 查询 URL。
pub fn build_acgrip_rss_url(
    base_url: &str,
    keyword: &str,
    page: usize,
) -> Result<String, SourceError> {
    let mut url = Url::parse(base_url)
        .and_then(|base| base.join("/.xml"))
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    if !keyword.trim().is_empty() {
        url.query_pairs_mut().append_pair("term", keyword.trim());
    }
    if page > 1 {
        url.query_pairs_mut().append_pair("page", &page.to_string());
    }
    Ok(url.to_string())
}

/// 生成 AniBT 精确番剧 RSS URL。
pub fn build_anibt_anime_rss_url(
    config: &ReleaseSourceConfig,
    source_anime_id: &str,
    limit: usize,
) -> Result<String, SourceError> {
    let mut url = Url::parse(config.base_url.as_deref().unwrap_or("https://anibt.net/"))
        .and_then(|base| base.join("/rss/anime.xml"))
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("bgmId", source_anime_id)
        .append_pair("limit", &normalize_fetch_limit(Some(limit)).to_string());
    Ok(url.to_string())
}

/// 生成 Mikan 精确番剧 RSS URL，结果数量由客户端统一截断。
pub fn build_mikan_anime_rss_url(
    config: &ReleaseSourceConfig,
    source_anime_id: &str,
) -> Result<String, SourceError> {
    let mut url = Url::parse(
        config
            .base_url
            .as_deref()
            .or(config.rss_url.as_deref())
            .unwrap_or("https://mikanani.me/"),
    )
    .and_then(|base| base.join("/RSS/Bangumi"))
    .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("bangumiId", source_anime_id);
    Ok(url.to_string())
}

/// 根据 AniBT 凭据格式生成 Cookie、Authorization 或 X-API-Key 请求头。
pub fn create_anibt_headers(
    config: &ReleaseSourceConfig,
    accept: &str,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::from([
        ("Accept".to_owned(), accept.to_owned()),
        (
            "Accept-Language".to_owned(),
            DESKTOP_BROWSER_ACCEPT_LANGUAGE.to_owned(),
        ),
        (
            "User-Agent".to_owned(),
            DESKTOP_BROWSER_USER_AGENT.to_owned(),
        ),
    ]);
    let Some(credential) = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return headers;
    };
    let lower = credential.to_lowercase();
    if lower.starts_with("cookie:") {
        headers.insert("Cookie".to_owned(), credential[7..].trim().to_owned());
    } else if lower.starts_with("authorization:") {
        headers.insert(
            "Authorization".to_owned(),
            credential[14..].trim().to_owned(),
        );
    } else if lower.starts_with("x-api-key:") {
        headers.insert("X-API-Key".to_owned(), credential[10..].trim().to_owned());
    } else if looks_like_cookie(credential) {
        headers.insert("Cookie".to_owned(), credential.to_owned());
    } else {
        let token = credential
            .strip_prefix("Bearer ")
            .or_else(|| credential.strip_prefix("bearer "))
            .unwrap_or(credential);
        headers.insert(
            "Authorization".to_owned(),
            if lower.starts_with("bearer ") {
                credential.to_owned()
            } else {
                format!("Bearer {credential}")
            },
        );
        headers.insert("X-API-Key".to_owned(), token.to_owned());
    }
    headers
}

/// 创建站点适配器共用的浏览器兼容请求头。
fn default_request_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "Accept-Language".to_owned(),
            DESKTOP_BROWSER_ACCEPT_LANGUAGE.to_owned(),
        ),
        (
            "User-Agent".to_owned(),
            DESKTOP_BROWSER_USER_AGENT.to_owned(),
        ),
    ])
}

fn enabled_supported_sources(configs: &[ReleaseSourceConfig]) -> Vec<ReleaseSourceConfig> {
    configs
        .iter()
        .filter(|config| config.enabled && is_supported_source(config))
        .cloned()
        .collect()
}

/// 判断来源是否已有 Rust 采集适配器。
pub fn is_supported_source(config: &ReleaseSourceConfig) -> bool {
    matches!(config.kind, SourceKind::Rss | SourceKind::Torznab)
        || (config.kind == SourceKind::SiteAdapter
            && (is_dmhy_config(config)
                || is_mikan_config(config)
                || is_anibt_config(config)
                || is_acgnx_config(config)
                || is_nyaa_config(config)
                || is_acgrip_config(config)))
}

fn config_identity(config: &ReleaseSourceConfig) -> String {
    format!(
        "{} {} {}",
        config.id,
        config.name,
        config.base_url.as_deref().unwrap_or_default()
    )
    .to_lowercase()
}

fn is_dmhy_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("dmhy") || value.contains("动漫花园") || value.contains("share.dmhy.org")
}

pub(crate) fn is_mikan_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("mikan") || value.contains("蜜柑") || value.contains("mikanani.me")
}

pub(crate) fn is_mikan_site_config(config: &ReleaseSourceConfig) -> bool {
    config.kind == SourceKind::SiteAdapter && is_mikan_config(config)
}

fn is_bindable_anime_source(config: &ReleaseSourceConfig) -> bool {
    is_anibt_config(config) || is_mikan_config(config)
}

pub(crate) fn is_anibt_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("anibt") || value.contains("anibt.net")
}

fn is_acgnx_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("acgnx") || value.contains("share.acgnx")
}

fn is_nyaa_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("nyaa") || value.contains("nyaa.si")
}

fn is_acgrip_config(config: &ReleaseSourceConfig) -> bool {
    let value = config_identity(config);
    value.contains("acg-rip") || value.contains("acgrip") || value.contains("acg.rip")
}

fn build_acgnx_search_urls(
    base_url: &str,
    keyword: &str,
    page: usize,
    limit: usize,
) -> Result<Vec<String>, SourceError> {
    let base = Url::parse(base_url).map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    let paths = if base.path() == "/" || base.path().is_empty() {
        vec!["/api.php", "/api/search", "/search.php"]
    } else {
        vec![base.path()]
    };
    paths
        .into_iter()
        .map(|path| {
            let mut url = if path == base.path() && path != "/" {
                base.clone()
            } else {
                base.join(path)
                    .map_err(|error| SourceError::InvalidUrl(error.to_string()))?
            };
            if !url.query_pairs().any(|(key, _)| key == "keyword") {
                url.query_pairs_mut().append_pair("keyword", keyword);
            }
            if url.path().contains("api") && !url.query_pairs().any(|(key, _)| key == "q") {
                url.query_pairs_mut().append_pair("q", keyword);
            }
            url.query_pairs_mut()
                .append_pair("page", &page.to_string())
                .append_pair("limit", &limit.to_string());
            Ok(url.to_string())
        })
        .collect()
}

fn build_search_cache_key(
    query: &ReleaseQuery,
    configs: &[ReleaseSourceConfig],
    fansubs: &[FansubGroup],
    extra: Option<Value>,
) -> Option<String> {
    let ttl = normalize_cache_ttl(query.cache_ttl_ms);
    if ttl == 0 {
        return None;
    }
    let sources = configs
        .iter()
        .filter(|config| config.enabled)
        .map(|config| {
            json!({
                "id": config.id,
                "name": config.name,
                "kind": config.kind,
                "useProxy": config.use_proxy,
                "requestIntervalMs": config.request_interval_ms,
                "baseUrl": config.base_url,
                "rssUrl": config.rss_url,
                "tags": config.tags,
                "apiKeyHash": config.api_key.as_deref().map(hash_value),
            })
        })
        .collect::<Vec<_>>();
    let input = json!({
        "version": RELEASE_SEARCH_CACHE_VERSION,
        "query": {
            "keyword": query.keyword,
            "animeId": query.anime_id,
            "episodeNo": query.episode_no,
            "fansubGroupId": query.fansub_group_id,
            "preferredResolution": query.preferred_resolution,
            "limit": query.limit,
            "cacheTtlMs": ttl,
        },
        "sources": sources,
        "fansubs": fansubs,
        "extra": extra,
    });
    serde_json::to_vec(&input)
        .ok()
        .map(|value| hash_bytes(&value))
}

fn load_cached_result<S>(
    store: &S,
    cache_key: Option<&str>,
    query: &ReleaseQuery,
) -> Result<Option<ReleaseSearchResult>, SourceError>
where
    S: ReleaseSearchStore,
{
    let Some(cache_key) = cache_key else {
        return Ok(None);
    };
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let Some(entry) = store.get_search_cache(cache_key, &now)? else {
        return Ok(None);
    };
    match serde_json::from_value::<ReleaseSearchResult>(entry.result) {
        Ok(mut result) => {
            result.query = query.clone();
            Ok(Some(result))
        }
        Err(error) => {
            log::warn!(
                "Rust 资源搜索缓存解码失败 cache_key={} error={}",
                cache_key,
                error
            );
            Ok(None)
        }
    }
}

fn save_cached_result<S>(
    store: &S,
    cache_key: Option<&str>,
    requested_ttl_ms: Option<u64>,
    result: &ReleaseSearchResult,
) where
    S: ReleaseSearchStore,
{
    let ttl = normalize_cache_ttl(requested_ttl_ms);
    let Some(cache_key) = cache_key.filter(|_| ttl > 0) else {
        return;
    };
    let Ok(payload) = serde_json::to_value(result) else {
        log::error!("Rust 资源搜索缓存序列化失败 cache_key={cache_key}");
        return;
    };
    let expires_at = Utc::now() + Duration::milliseconds(i64::try_from(ttl).unwrap_or(i64::MAX));
    let entry = ReleaseSearchCacheEntry {
        result: payload,
        expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    if let Err(error) = store.save_search_cache(cache_key, &entry) {
        log::warn!(
            "Rust 资源搜索缓存保存失败 cache_key={} error={}",
            cache_key,
            error
        );
    }
}

fn normalize_fetch_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(MAX_RELEASE_SOURCE_FETCH_LIMIT)
        .clamp(1, MAX_RELEASE_SOURCE_FETCH_LIMIT)
}

fn normalize_result_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(MAX_RELEASE_SOURCE_FETCH_LIMIT)
        .clamp(1, MAX_RELEASE_SOURCE_RESULT_LIMIT)
}

fn normalize_cache_ttl(value: Option<u64>) -> u64 {
    value.unwrap_or(0).min(COMPLETED_ANIME_RELEASE_CACHE_TTL_MS)
}

fn matches_keyword(release: &Release, keyword: &str) -> bool {
    let keyword = normalize_release_search_text(keyword);
    keyword.is_empty() || normalize_release_search_text(&release.title).contains(&keyword)
}

fn dedupe_releases(releases: Vec<Release>) -> Vec<Release> {
    let input_count = releases.len();
    let mut seen = HashSet::new();
    let releases = releases
        .into_iter()
        .filter(|release| seen.insert(release_episode_content_key(release)))
        .collect::<Vec<_>>();
    if releases.len() < input_count {
        log::debug!(
            "资源搜索已按集数和种子内容合并重复项：input={}, output={}",
            input_count,
            releases.len()
        );
    }
    releases
}

fn sort_releases(mut releases: Vec<Release>) -> Vec<Release> {
    releases.sort_by(|left, right| {
        release_episode_order(right)
            .partial_cmp(&release_episode_order(left))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.published_at.cmp(&left.published_at))
    });
    releases
}

/// 生成包含集数边界的资源内容键，避免相同 BTIH 跨集误合并。
fn release_episode_content_key(release: &Release) -> String {
    let episode = if let Some(range) = &release.episode_range {
        format!(
            "range:{}-{}",
            format_episode_key(range.start),
            format_episode_key(range.end)
        )
    } else if let Some(episode_no) = release.episode_no.filter(|value| value.is_finite()) {
        format!("episode:{}", format_episode_key(episode_no))
    } else if release.content_kind == Some(ReleaseContentKind::Batch) {
        format!(
            "batch:{}",
            release
                .series_season_no
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
        )
    } else {
        "unknown".to_owned()
    };
    let content = normalize_info_hash(release.info_hash.as_deref())
        .or_else(|| extract_info_hash(release.magnet_url.as_deref()))
        .or_else(|| extract_torrent_url_info_hash(release.torrent_url.as_deref()))
        .map(|hash| format!("btih:{hash}"))
        .or_else(|| {
            release
                .magnet_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("magnet:{value}"))
        })
        .or_else(|| {
            release
                .torrent_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("torrent:{value}"))
        })
        .unwrap_or_else(|| format!("release:{}:{}", release.source_id, release.id));
    format!("{episode}|{content}")
}

/// 返回资源排序使用的集数上界，未识别资源排在普通单集之后。
fn release_episode_order(release: &Release) -> f64 {
    release
        .episode_range
        .as_ref()
        .map(|range| range.end)
        .or(release.episode_no)
        .filter(|value| value.is_finite())
        .unwrap_or(-1.0)
}

/// 规范化集数键中的负零表示。
fn format_episode_key(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

fn format_source_error(error: &SourceError) -> String {
    match error {
        SourceError::Transport(error) if error.is_timeout() => {
            "下载源请求超时，请稍后重试".to_owned()
        }
        SourceError::Transport(_) => "下载源网络请求失败，请检查网络、代理或下载源地址".to_owned(),
        _ => error.to_string(),
    }
}

fn is_exact_mikan_rss_url(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| host == "mikanani.me" || host.ends_with(".mikanani.me"))
            && url.path().to_lowercase().contains("/rss/bangumi")
            && url
                .query_pairs()
                .any(|(key, value)| key.eq_ignore_ascii_case("bangumiId") && !value.is_empty())
    })
}

fn subtitle_sort_rank(release: &Release, preferred: &[SubtitleLanguage]) -> u8 {
    if preferred.is_empty() {
        return 0;
    }
    let actual = if release.subtitle_languages.is_empty() {
        match release.subtitle {
            Some(SubtitlePreference::Chs) => vec![SubtitleLanguage::Chs],
            Some(SubtitlePreference::Cht) => vec![SubtitleLanguage::Cht],
            Some(SubtitlePreference::Jpn) => vec![SubtitleLanguage::Jpn],
            Some(SubtitlePreference::Eng) => vec![SubtitleLanguage::Eng],
            Some(SubtitlePreference::Multi) | None => Vec::new(),
        }
    } else {
        release.subtitle_languages.clone()
    };
    let matched = preferred
        .iter()
        .filter(|language| actual.contains(language))
        .count();
    if matched == preferred.len() {
        0
    } else if matched > 0 {
        1
    } else if release.subtitle == Some(SubtitlePreference::Multi) {
        2
    } else {
        3
    }
}

fn parse_subtitle_language(value: &str) -> Option<SubtitleLanguage> {
    match value {
        "chs" => Some(SubtitleLanguage::Chs),
        "cht" => Some(SubtitleLanguage::Cht),
        "jpn" => Some(SubtitleLanguage::Jpn),
        "eng" => Some(SubtitleLanguage::Eng),
        _ => None,
    }
}

fn looks_like_cookie(value: &str) -> bool {
    value.split(';').all(|part| {
        part.trim()
            .split_once('=')
            .is_some_and(|(name, content)| !name.trim().is_empty() && !content.is_empty())
    })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn hash_value(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use ani_domain::{
        Anime, AnimeReleaseQuery, AnimeSourceBinding, AnimeSourceBindingMatchMethod, Release,
        ReleaseQuery, ReleaseSourceConfig, RequestCircuitState, SourceKind,
    };
    use ani_repository::{CachedReleaseQuery, ReleaseSearchCacheEntry, RepositoryResult};
    use data_encoding::BASE32_NOPAD;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{
        build_acgrip_rss_url, build_anibt_anime_rss_url, build_dmhy_list_url,
        build_mikan_anime_rss_url, build_mikan_search_url, build_nyaa_rss_url,
        create_anibt_headers, dedupe_releases, sort_releases, ReleaseSearchService,
        ReleaseSearchStore,
    };
    use crate::{CircuitStateStore, NativeHttpConfig, ProxyMode, SourceNetworkService};

    #[derive(Default)]
    struct MemoryReleaseSearchStore {
        circuits: Mutex<BTreeMap<String, RequestCircuitState>>,
        cache: Mutex<BTreeMap<String, ReleaseSearchCacheEntry>>,
        releases: Mutex<BTreeMap<String, Release>>,
    }

    impl CircuitStateStore for MemoryReleaseSearchStore {
        /// 读取测试内存中的指定熔断状态。
        fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
            Ok(self
                .circuits
                .lock()
                .expect("lock circuit states")
                .get(key)
                .cloned())
        }

        /// 保存测试内存中的指定熔断状态。
        fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
            self.circuits
                .lock()
                .expect("lock circuit states")
                .insert(state.key.clone(), state.clone());
            Ok(())
        }
    }

    impl ReleaseSearchStore for MemoryReleaseSearchStore {
        /// 读取仍在有效期内的测试搜索缓存。
        fn get_search_cache(
            &self,
            cache_key: &str,
            current_time: &str,
        ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>> {
            Ok(self
                .cache
                .lock()
                .expect("lock search cache")
                .get(cache_key)
                .filter(|entry| entry.expires_at.as_str() > current_time)
                .cloned())
        }

        /// 保存测试搜索缓存，模拟跨服务实例持久化。
        fn save_search_cache(
            &self,
            cache_key: &str,
            entry: &ReleaseSearchCacheEntry,
        ) -> RepositoryResult<()> {
            self.cache
                .lock()
                .expect("lock search cache")
                .insert(cache_key.to_owned(), entry.clone());
            Ok(())
        }

        /// 按测试查询条件读取内存原始资源缓存。
        fn list_release_cache(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>> {
            let mut releases = self
                .releases
                .lock()
                .expect("lock raw release cache")
                .values()
                .filter(|release| {
                    query.source_ids.as_ref().is_none_or(|source_ids| {
                        source_ids
                            .iter()
                            .any(|source_id| source_id == &release.source_id)
                    }) && query
                        .anime_id
                        .as_ref()
                        .is_none_or(|anime_id| release.anime_id.as_ref() == Some(anime_id))
                })
                .cloned()
                .collect::<Vec<_>>();
            releases.sort_by(|left, right| right.published_at.cmp(&left.published_at));
            releases.truncate(query.limit.unwrap_or(2_000));
            Ok(releases)
        }

        /// 增量保存测试内存中的原始资源。
        fn save_release_cache(&self, releases: &[Release]) -> RepositoryResult<usize> {
            let mut cache = self.releases.lock().expect("lock raw release cache");
            let mut added = 0;
            for release in releases {
                added += usize::from(!cache.contains_key(&release.id));
                cache.insert(release.id.clone(), release.clone());
            }
            Ok(added)
        }
    }

    /// 验证各站点查询地址使用对应关键词和分页协议。
    #[test]
    fn builds_site_search_urls() {
        assert_eq!(
            build_mikan_search_url("https://mikanani.me/", "测试番", 1).expect("Mikan URL"),
            "https://mikanani.me/Home/Search?searchstr=%E6%B5%8B%E8%AF%95%E7%95%AA"
        );
        assert_eq!(
            build_dmhy_list_url("https://share.dmhy.org/", "测试番", 2).expect("DMHY URL"),
            "https://share.dmhy.org/topics/list/page/2?keyword=%E6%B5%8B%E8%AF%95%E7%95%AA"
        );
        assert_eq!(
            build_nyaa_rss_url("https://nyaa.si/", "测试番", 1).expect("Nyaa URL"),
            "https://nyaa.si/?page=rss&q=%E6%B5%8B%E8%AF%95%E7%95%AA&c=1_0&f=0"
        );
        assert_eq!(
            build_acgrip_rss_url("https://acg.rip/", "测试番", 1).expect("ACG.RIP URL"),
            "https://acg.rip/.xml?term=%E6%B5%8B%E8%AF%95%E7%95%AA"
        );
    }

    /// 验证同集同 BTIH 跨来源合并，并保留复用同一 Hash 的其他集数。
    #[test]
    fn dedupes_releases_by_episode_and_normalized_btih() {
        let bytes = [
            0x54, 0x48, 0xae, 0x0e, 0xd3, 0x69, 0x12, 0xeb, 0x0d, 0xfb, 0xa5, 0x3c, 0x3e, 0x49,
            0x5b, 0x99, 0x88, 0x84, 0x1e, 0x68,
        ];
        let hex_hash = "5448ae0ed36912eb0dfba53c3e495b9988841e68";
        let mut mikan_release = test_release("mikan", 8.0, None, None);
        mikan_release.torrent_url = Some(format!(
            "https://mikanani.me/Download/20260730/{hex_hash}.torrent"
        ));
        let releases = dedupe_releases(vec![
            test_release("source-a", 8.0, Some(&hex_hash.to_ascii_uppercase()), None),
            test_release(
                "source-b",
                8.0,
                None,
                Some(&format!(
                    "magnet:?xt=urn:btih:{}&tr=udp%3A%2F%2Ftracker",
                    BASE32_NOPAD.encode(&bytes)
                )),
            ),
            mikan_release,
            test_release("source-c", 9.0, Some(hex_hash), None),
        ]);

        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].source_id, "source-a");
        assert_eq!(releases[1].episode_no, Some(9.0));
    }

    /// 验证聚合结果以集数倒序为主、发布时间倒序为同集次序。
    #[test]
    fn sorts_releases_by_episode_then_published_at() {
        let mut episode_8_old = test_release("episode-8-old", 8.0, None, None);
        episode_8_old.published_at = "2026-08-01T00:00:00.000Z".to_owned();
        let mut episode_8_new = test_release("episode-8-new", 8.0, None, None);
        episode_8_new.published_at = "2026-08-03T00:00:00.000Z".to_owned();
        let episode_12 = test_release("episode-12", 12.0, None, None);

        let releases = sort_releases(vec![episode_8_old, episode_12, episode_8_new]);
        assert_eq!(
            releases
                .iter()
                .map(|release| release.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["episode-12", "episode-8-new", "episode-8-old"]
        );
    }

    /// 验证 AniBT 精确 RSS 上限和凭据请求头。
    #[test]
    fn builds_anibt_urls_and_headers() {
        let mut config = source("anibt", "AniBT", "https://anibt.net/");
        config.api_key = Some("test-token".to_owned());
        assert_eq!(
            build_anibt_anime_rss_url(&config, "528828", 200).expect("AniBT URL"),
            "https://anibt.net/rss/anime.xml?bgmId=528828&limit=50"
        );
        let headers = create_anibt_headers(&config, "application/json");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("Bearer test-token")
        );
        assert_eq!(
            headers.get("X-API-Key").map(String::as_str),
            Some("test-token")
        );
        config.api_key = Some("Cookie: anibt.sid=session".to_owned());
        let headers = create_anibt_headers(&config, "application/json");
        assert_eq!(
            headers.get("Cookie").map(String::as_str),
            Some("anibt.sid=session")
        );

        let mikan = ReleaseSourceConfig {
            id: "mikan-rss".to_owned(),
            name: "蜜柑计划".to_owned(),
            kind: SourceKind::Rss,
            enabled: true,
            use_proxy: false,
            request_interval_ms: 250,
            base_url: None,
            api_key: None,
            rss_url: Some("https://mikanani.me/RSS/Bangumi?legacy=1".to_owned()),
            tags: Vec::new(),
        };
        assert_eq!(
            build_mikan_anime_rss_url(&mikan, "3941").expect("Mikan exact RSS URL"),
            "https://mikanani.me/RSS/Bangumi?bangumiId=3941"
        );
    }

    /// 验证单个来源失败不会清空成功结果，缓存可被新服务实例复用。
    #[tokio::test]
    async fn preserves_partial_results_and_reuses_persisted_cache() {
        let successful_url = serve_once(
            "200 OK",
            r#"<rss><channel><item><title>[测试组] 测试番 - 03 [1080p][CHS]</title><guid>release-3</guid><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link></item></channel></rss>"#,
        )
        .await;
        let failing_url = serve_once("503 Service Unavailable", "busy").await;
        let configs = vec![
            rss_source("working", "可用来源", successful_url),
            rss_source("broken", "失败来源", failing_url),
        ];
        let store = MemoryReleaseSearchStore::default();
        let query = ReleaseQuery {
            keyword: "测试番".to_owned(),
            anime_id: Some("anime-test".to_owned()),
            episode_no: Some(3.0),
            fansub_group_id: None,
            preferred_resolution: Some("1080p".to_owned()),
            limit: Some(10),
            cache_ttl_ms: Some(60_000),
            force_refresh: false,
        };

        let result = ReleaseSearchService::new(test_network())
            .search(&store, &configs, &[], query.clone())
            .await
            .expect("aggregate release search");

        assert_eq!(result.releases.len(), 1);
        assert_eq!(result.releases[0].source_id, "working");
        assert_eq!(result.source_results.len(), 2);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].source_id, "broken");
        assert_eq!(store.cache.lock().expect("lock saved cache").len(), 1);

        let cached = ReleaseSearchService::new(test_network())
            .search(&store, &configs, &[], query.clone())
            .await
            .expect("load persisted release cache");
        assert_eq!(cached, result);

        let recovered = ReleaseSearchService::new(test_network())
            .search(
                &store,
                &configs,
                &[],
                ReleaseQuery {
                    episode_no: None,
                    cache_ttl_ms: None,
                    ..query
                },
            )
            .await
            .expect("recover from raw release cache");
        assert_eq!(recovered.releases.len(), 1);
        assert_eq!(recovered.releases[0].source_id, "working");
    }

    /// 验证已确认 AniBT 绑定走精确 RSS，绑定变化后不会复用旧缓存。
    #[tokio::test]
    async fn uses_bound_anime_rss_and_scopes_cache_by_binding() {
        let body = r#"<rss xmlns:anibt="x"><channel><item><anibt:releaseId>binding-rel-3</anibt:releaseId><anibt:releaseTitle>[测试组] 来源绑定测试番 - 03 [1080p][CHS]</anibt:releaseTitle><anibt:episode>3</anibt:episode><torrent><infohash>0123456789ABCDEF0123456789ABCDEF01234567</infohash><magneturi>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</magneturi></torrent></item></channel></rss>"#;
        let (base_url, requests) = serve_requests(2, "200 OK", body).await;
        let config = source("anibt-local", "AniBT Local", &base_url);
        let store = MemoryReleaseSearchStore::default();
        let anime = binding_test_anime();
        let query = AnimeReleaseQuery {
            anime_id: anime.id.clone(),
            episode_no: Some(3.0),
            fansub_group_id: None,
            preferred_resolution: Some("1080p".to_owned()),
            limit: Some(10),
            cache_ttl_ms: Some(60_000),
            force_refresh: false,
        };
        let first_binding = binding(&anime.id, &config.id, "528828");

        let first = ReleaseSearchService::new(test_network())
            .search_anime(
                &store,
                std::slice::from_ref(&config),
                &[],
                &anime,
                std::slice::from_ref(&first_binding),
                query.clone(),
            )
            .await
            .expect("search first bound anime RSS");
        assert_eq!(first.releases.len(), 1);

        let second_binding = binding(&anime.id, &config.id, "528829");
        let second = ReleaseSearchService::new(test_network())
            .search_anime(
                &store,
                std::slice::from_ref(&config),
                &[],
                &anime,
                std::slice::from_ref(&second_binding),
                query,
            )
            .await
            .expect("search changed bound anime RSS");
        assert_eq!(second.releases.len(), 1);
        assert_eq!(store.cache.lock().expect("lock scoped cache").len(), 2);

        let requests = requests.lock().expect("lock captured requests");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("/rss/anime.xml?bgmId=528828&limit=10"));
        assert!(requests[1].contains("/rss/anime.xml?bgmId=528829&limit=10"));
    }

    fn source(id: &str, name: &str, base_url: &str) -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: SourceKind::SiteAdapter,
            enabled: true,
            use_proxy: false,
            request_interval_ms: 250,
            base_url: Some(base_url.to_owned()),
            api_key: None,
            rss_url: None,
            tags: Vec::new(),
        }
    }

    /// 创建资源聚合测试所需的最小发布数据。
    fn test_release(
        source_id: &str,
        episode_no: f64,
        info_hash: Option<&str>,
        magnet_url: Option<&str>,
    ) -> Release {
        serde_json::from_value(json!({
            "id": format!("release-{source_id}"),
            "title": format!("测试番 - {episode_no}"),
            "episodeNo": episode_no,
            "contentKind": "episode",
            "sourceId": source_id,
            "sourceName": source_id,
            "magnetUrl": magnet_url,
            "infoHash": info_hash,
            "publishedAt": "2026-08-02T00:00:00.000Z"
        }))
        .expect("create test release")
    }

    /// 创建指向本地测试服务的 RSS 来源。
    fn rss_source(id: &str, name: &str, rss_url: String) -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: SourceKind::Rss,
            enabled: true,
            use_proxy: false,
            request_interval_ms: 250,
            base_url: None,
            api_key: None,
            rss_url: Some(rss_url),
            tags: Vec::new(),
        }
    }

    /// 创建资源搜索绑定测试番剧。
    fn binding_test_anime() -> Anime {
        Anime {
            id: "anime-binding-search".to_owned(),
            title: "来源绑定测试番".to_owned(),
            original_title: None,
            aliases: Vec::new(),
            premiere_date: Some("2026-07-01".to_owned()),
            premiere_year: 2026,
            premiere_month: 7,
            season: Some("summer".to_owned()),
            summary: None,
            cover_url: None,
            rating: None,
            external_ids: json!({"bangumi": "528828"}),
            detail: None,
        }
    }

    /// 创建一条已确认来源绑定。
    fn binding(anime_id: &str, source_id: &str, source_anime_id: &str) -> AnimeSourceBinding {
        AnimeSourceBinding {
            id: format!("binding:{anime_id}:{source_id}"),
            anime_id: anime_id.to_owned(),
            source_id: source_id.to_owned(),
            source_anime_id: source_anime_id.to_owned(),
            source_anime_title: None,
            source_url: None,
            match_method: AnimeSourceBindingMatchMethod::Manual,
            confidence: 1.0,
            confirmed: true,
            created_at: "2026-07-25T00:00:00.000Z".to_owned(),
            updated_at: "2026-07-25T00:00:00.000Z".to_owned(),
        }
    }

    /// 创建关闭代理的本地测试网络服务。
    fn test_network() -> Arc<SourceNetworkService> {
        Arc::new(
            SourceNetworkService::new(NativeHttpConfig {
                proxy_mode: ProxyMode::Off,
                ..NativeHttpConfig::default()
            })
            .expect("create local source network"),
        )
    }

    /// 启动只处理一次请求的本地 HTTP 服务。
    async fn serve_once(status: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local HTTP listener");
        let address = listener.local_addr().expect("read local HTTP address");
        let status = status.to_owned();
        let body = body.as_bytes().to_vec();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept local HTTP request");
            let mut request = [0_u8; 2_048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read local HTTP request");
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write local HTTP headers");
            stream
                .write_all(&body)
                .await
                .expect("write local HTTP body");
        });
        format!("http://{address}/source")
    }

    /// 启动固定次数的本地服务并记录请求首行。
    async fn serve_requests(
        count: usize,
        status: &str,
        body: &str,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind repeated local HTTP listener");
        let address = listener.local_addr().expect("read repeated HTTP address");
        let status = status.to_owned();
        let body = body.as_bytes().to_vec();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        tokio::spawn(async move {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().await.expect("accept repeated request");
                let mut request = [0_u8; 4_096];
                let bytes = stream
                    .read(&mut request)
                    .await
                    .expect("read repeated request");
                let first_line = String::from_utf8_lossy(&request[..bytes])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                captured
                    .lock()
                    .expect("lock repeated request capture")
                    .push(first_line);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write repeated HTTP headers");
                stream
                    .write_all(&body)
                    .await
                    .expect("write repeated HTTP body");
            }
        });
        (format!("http://{address}/"), requests)
    }
}
