use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use ani_domain::{
    Anime, AnimeAlias, AnimeAliasLanguage, AnimeDetailPartialError, AnimeRating,
    BangumiBrowseFilters, BangumiBrowseQuery, BangumiBrowseResult, BangumiBrowseSort,
    BangumiBrowseYearRange, ReleaseSourceConfig, SourceKind,
};
use chrono::{Datelike, NaiveDate, SecondsFormat, TimeZone, Utc};
use futures_util::stream::{self, StreamExt};
use regex::Regex;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use unicode_normalization::UnicodeNormalization;
use url::Url;

use crate::{
    CircuitStateStore, HttpMethod, NativeHttpRequest, NetworkRequestChannel, SourceError,
    SourceNetworkService,
};

const BANGUMI_BASE_URL: &str = "https://api.bgm.tv/";
const ANILIST_ENDPOINT: &str = "https://graphql.anilist.co";
const MIKAN_BASE_URL: &str = "https://mikanani.me/";
const BANGUMI_PAGE_LIMIT: usize = 50;
const BANGUMI_MAX_MONTHLY_ITEMS: usize = 300;
const PROVIDER_DETAIL_CONCURRENCY: usize = 3;
const MIKAN_DETAIL_LIMIT: usize = 60;
const SEARCH_LIMIT: usize = 30;
const DETAIL_TRANSIENT_RETRY_DELAY_MS: u64 = 30_500;
const DETAIL_RATE_LIMIT_RETRY_DELAY_MS: u64 = 60_500;

static BANGUMI_ANILIST_ID_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)anilist\.co/anime/(\d+)").expect("Bangumi AniList 标识正则必须有效")
});
static BANGUMI_MAL_ID_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)myanimelist\.net/anime/(\d+)").expect("Bangumi MAL 标识正则必须有效")
});
static BANGUMI_SUBJECT_URL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:bgm\.tv|bangumi\.tv|chii\.in)/subject/(\d+)")
        .expect("Bangumi 详情地址正则必须有效")
});
static MIKAN_CANDIDATE_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/Home/Bangumi/(\d+)").expect("Mikan 番剧标识正则必须有效"));
static MIKAN_TITLE_SUFFIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*[-|].*$").expect("Mikan 标题后缀正则必须有效"));
static YEAR_FIRST_DATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(20\d{2})[年./-]\s*(\d{1,2})(?:[月./-]\s*(\d{1,2}))?").expect("年月日正则必须有效")
});
static MONTH_FIRST_DATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{1,2})[./-](\d{1,2})[./-](20\d{2})\b").expect("月日年正则必须有效")
});
static FULL_DATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(20\d{2})[年./-]\s*(\d{1,2})[月./-]\s*(\d{1,2})").expect("完整日期正则必须有效")
});
static WEEKDAY_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[周週星期]([日一二三四五六天])").expect("星期正则必须有效"));
static CLOCK_TIME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:[01]\d|2[0-3]):[0-5]\d").expect("时间正则必须有效"));
static DURATION_HOURS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(?:小时|h)").expect("小时正则必须有效"));
static DURATION_MINUTES_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+)\s*(?:分钟|min)").expect("分钟正则必须有效"));
static POSITIVE_INTEGER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+").expect("正整数正则必须有效"));
static LABELED_VALUE_REGEX_CACHE: LazyLock<Mutex<HashMap<String, Regex>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 单个元数据来源返回的一批番剧。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeMetadataBatch {
    pub source: String,
    pub items: Vec<Anime>,
}

/// 多来源采集的合并结果和单来源错误。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeMetadataCollection {
    pub items: Vec<Anime>,
    pub source: String,
    pub errors: Vec<String>,
    pub successful_sources: Vec<String>,
}

/// 单部番剧详情刷新后的候选记录和局部错误。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeMetadataRefresh {
    pub item: Anime,
    pub success_count: usize,
    pub errors: Vec<AnimeDetailPartialError>,
}

/// 一批季度目录详情补全结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeMetadataDetailCollection {
    pub items: Vec<Anime>,
    pub settled_error_count: usize,
    pub deferred_error_count: usize,
    pub retryable_requests: Vec<AnimeMetadataDetailRequest>,
    pub successful_providers: Vec<AnimeMetadataDetailProviderOutcome>,
    pub settled_failed_providers: Vec<AnimeMetadataDetailProviderOutcome>,
}

/// 季度详情补全支持的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimeMetadataDetailProvider {
    Bangumi,
    Mikan,
}

impl AnimeMetadataDetailProvider {
    /// 返回用于持久化状态和日志的稳定来源名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bangumi => "bangumi",
            Self::Mikan => "mikan",
        }
    }
}

/// 单部番剧在单个详情来源上的请求结果标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimeMetadataDetailProviderOutcome {
    pub anime_id: String,
    pub provider: AnimeMetadataDetailProvider,
}

/// 单部番剧待补全的来源及下一次重试等待时间。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeMetadataDetailRequest {
    pub item: Anime,
    pub providers: Vec<AnimeMetadataDetailProvider>,
    pub retry_after_ms: u64,
}

#[derive(Debug)]
struct DetailEnrichmentOutcome {
    item: Anime,
    persist_item: bool,
    settled_error_count: usize,
    deferred_error_count: usize,
    retryable_providers: Vec<AnimeMetadataDetailProvider>,
    successful_providers: Vec<AnimeMetadataDetailProvider>,
    settled_failed_providers: Vec<AnimeMetadataDetailProvider>,
    retry_after_ms: u64,
}

#[derive(Clone)]
struct MetadataEndpoints {
    bangumi: String,
    anilist: String,
    mikan: String,
}

impl Default for MetadataEndpoints {
    fn default() -> Self {
        Self {
            bangumi: BANGUMI_BASE_URL.to_owned(),
            anilist: ANILIST_ENDPOINT.to_owned(),
            mikan: MIKAN_BASE_URL.to_owned(),
        }
    }
}

/// 通过 Rust Native HTTP 聚合 Bangumi、AniList 和 Mikan 元数据。
pub struct AnimeMetadataService {
    network: Arc<SourceNetworkService>,
    endpoints: MetadataEndpoints,
    channel: NetworkRequestChannel,
    bangumi_catalog_cache: Mutex<HashMap<i64, BangumiSubject>>,
}

impl AnimeMetadataService {
    /// 创建使用应用共享代理、限流和熔断状态的元数据服务。
    pub fn new(network: Arc<SourceNetworkService>) -> Self {
        Self {
            network,
            endpoints: MetadataEndpoints::default(),
            channel: NetworkRequestChannel::Interactive,
            bangumi_catalog_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 创建使用独立后台限流与熔断状态的元数据采集服务。
    pub fn new_background(network: Arc<SourceNetworkService>) -> Self {
        Self {
            network,
            endpoints: MetadataEndpoints::default(),
            channel: NetworkRequestChannel::Background,
            bangumi_catalog_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 清空上一次季度任务的 Bangumi 原始目录缓存。
    fn reset_bangumi_catalog_cache(&self) {
        self.bangumi_catalog_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// 保存本次季度目录原始条目，供详情失败时回退完整映射。
    fn cache_bangumi_catalog_subjects(&self, subjects: &[BangumiSubject]) {
        let mut cache = self
            .bangumi_catalog_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.extend(
            subjects
                .iter()
                .cloned()
                .map(|subject| (subject.id, subject)),
        );
    }

    /// 按 Bangumi 标识读取本次季度任务的原始目录条目。
    fn cached_bangumi_catalog_subject(&self, external_id: &str) -> Option<BangumiSubject> {
        let id = external_id.parse::<i64>().ok()?;
        self.bangumi_catalog_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }

    /// 并发语义聚合本地结果之外的在线关键词搜索结果。
    pub async fn search<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        keyword: &str,
    ) -> AnimeMetadataCollection {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return AnimeMetadataCollection {
                items: Vec::new(),
                source: String::new(),
                errors: Vec::new(),
                successful_sources: Vec::new(),
            };
        }
        let results = tokio::join!(
            self.search_bangumi(store, keyword),
            self.search_anilist(store, keyword),
            self.search_mikan(store, keyword),
        );
        collect_search_provider_results([
            ("bangumi", results.0),
            ("anilist", results.1),
            ("mikan", results.2),
        ])
    }

    /// 直接请求 Bangumi 在线检索，不读取或写入本地季度目录。
    pub async fn browse_bangumi<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        query: BangumiBrowseQuery,
    ) -> Result<BangumiBrowseResult, SourceError> {
        let started = Instant::now();
        let offset = query.page.saturating_sub(1).saturating_mul(query.page_size);
        let mut url = endpoint_url(&self.endpoints.bangumi, "v0/search/subjects")?;
        url.query_pairs_mut()
            .append_pair("limit", &query.page_size.to_string())
            .append_pair("offset", &offset.to_string());

        let mut filter = Map::from_iter([("type".to_owned(), json!([2]))]);
        let tags = bangumi_browse_tags(&query.filters);
        if !tags.is_empty() {
            filter.insert("tag".to_owned(), json!(tags));
        }
        if let Some(air_date) = bangumi_browse_air_date(&query.filters) {
            filter.insert("air_date".to_owned(), json!(air_date));
        }
        if query.filters.min_rating > 0.0 {
            filter.insert(
                "rating".to_owned(),
                json!([format!(">={:.1}", query.filters.min_rating)]),
            );
        }
        let sort = match query.sort {
            BangumiBrowseSort::BangumiRank => "rank",
            BangumiBrowseSort::Recent => "heat",
            BangumiBrowseSort::Rating => "score",
        };
        let response: BangumiPage = self
            .post_json(
                store,
                "bangumi",
                true,
                url,
                json!({
                    "keyword": query.keyword.trim(),
                    "sort": sort,
                    "filter": Value::Object(filter)
                }),
            )
            .await?;
        let subjects = response
            .data
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.subject_type == 2)
            .collect::<Vec<_>>();
        let returned_count = subjects.len();
        let total = response
            .total
            .unwrap_or(offset.saturating_add(returned_count));
        let items = subjects
            .into_iter()
            .map(|item| {
                let (year, month) = date_or_now(item.date.as_deref());
                map_bangumi(item, year, month)
            })
            .collect::<Vec<_>>();
        log::info!(
            "Bangumi 在线浏览完成 page={} page_size={} items={} total={} duration_ms={}",
            query.page,
            query.page_size,
            items.len(),
            total,
            started.elapsed().as_millis()
        );
        Ok(BangumiBrowseResult {
            has_more: offset.saturating_add(returned_count) < total,
            query,
            items,
            total,
            source: "bangumi".to_owned(),
        })
    }

    /// 采集指定月份，并保留单来源失败信息。
    pub async fn collect_month<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        month: i64,
    ) -> Result<AnimeMetadataCollection, SourceError> {
        let season = season_for_month(month)?;
        let results = tokio::join!(
            self.collect_bangumi_month(store, year, month),
            self.collect_anilist_season(store, year, season),
            self.collect_mikan_season(store, year, season),
        );
        let mut collection = collect_provider_results([
            ("bangumi", results.0),
            ("anilist", results.1),
            ("mikan", results.2),
        ]);
        collection
            .items
            .retain(|item| item.premiere_year == year && item.premiere_month == month);
        Ok(collection)
    }

    /// 一次采集指定季度，Bangumi 按三个月补齐，其他来源使用季度入口。
    pub async fn collect_season<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
    ) -> Result<AnimeMetadataCollection, SourceError> {
        self.collect_season_inner(store, year, season, true).await
    }

    /// 只采集可浏览的季度基础目录，不在首阶段逐部请求详情。
    pub async fn collect_season_catalog<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
    ) -> Result<AnimeMetadataCollection, SourceError> {
        self.collect_season_inner(store, year, season, false).await
    }

    /// 按阶段选择是否补全单部详情，并复用多来源合并逻辑。
    async fn collect_season_inner<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
        include_details: bool,
    ) -> Result<AnimeMetadataCollection, SourceError> {
        let total_started = Instant::now();
        let months = months_for_season(season)?;
        if !include_details {
            self.reset_bangumi_catalog_cache();
        }
        let bangumi = async {
            let provider_started = Instant::now();
            let mut items = Vec::new();
            let mut errors = Vec::new();
            let month_results = stream::iter(months)
                .map(|month| async move {
                    let result = if include_details {
                        self.collect_bangumi_month(store, year, month).await
                    } else {
                        self.collect_bangumi_month_catalog(store, year, month).await
                    };
                    (month, result)
                })
                .buffered(months.len())
                .collect::<Vec<_>>()
                .await;
            for (month, result) in month_results {
                match result {
                    Ok(month_items) if !month_items.is_empty() => items.extend(month_items),
                    Ok(_) => errors.push(format!("bangumi({month}月): 未返回新番数据")),
                    Err(error) => errors.push(format!("bangumi({month}月): {error}")),
                }
            }
            log::info!(
                "Rust 新番季度来源完成 provider=bangumi year={year} season={season} items={} errors={} duration_ms={}",
                items.len(),
                errors.len(),
                provider_started.elapsed().as_millis()
            );
            (items, errors)
        };
        let anilist = async {
            let provider_started = Instant::now();
            let result = self.collect_anilist_season(store, year, season).await;
            log::info!(
                "Rust 新番季度来源完成 provider=anilist year={year} season={season} items={} success={} duration_ms={}",
                result.as_ref().map_or(0, Vec::len),
                result.is_ok(),
                provider_started.elapsed().as_millis()
            );
            result
        };
        let mikan = async {
            let provider_started = Instant::now();
            let result = if include_details {
                self.collect_mikan_season(store, year, season).await
            } else {
                self.collect_mikan_season_catalog(store, year, season).await
            };
            log::info!(
                "Rust 新番季度来源完成 provider=mikan year={year} season={season} items={} success={} duration_ms={}",
                result.as_ref().map_or(0, Vec::len),
                result.is_ok(),
                provider_started.elapsed().as_millis()
            );
            result
        };
        let results = tokio::join!(bangumi, anilist, mikan,);
        let (bangumi_items, bangumi_errors) = results.0;
        let has_bangumi_items = !bangumi_items.is_empty();
        let candidate_count = bangumi_items.len()
            + results.1.as_ref().map_or(0, Vec::len)
            + results.2.as_ref().map_or(0, Vec::len);
        let merge_started = Instant::now();
        let mut collection = collect_provider_results([
            ("bangumi", Ok(bangumi_items)),
            ("anilist", results.1),
            ("mikan", results.2),
        ]);
        log::info!(
            "Rust 新番阶段耗时 phase=deduplicate-merge candidates={candidate_count} items={} duration_ms={}",
            collection.items.len(),
            merge_started.elapsed().as_millis()
        );
        if !bangumi_errors.is_empty() {
            if !has_bangumi_items {
                collection
                    .errors
                    .retain(|error| error != "bangumi: 未返回新番数据");
            }
            let mut errors = bangumi_errors;
            errors.extend(collection.errors);
            collection.errors = errors;
        }
        log::info!(
            "Rust 新番季度多来源采集完成 year={year} season={season} items={} errors={} duration_ms={}",
            collection.items.len(),
            collection.errors.len(),
            total_started.elapsed().as_millis()
        );
        Ok(collection)
    }

    /// 仅补全 Bangumi 与 Mikan 详情，AniList 在目录阶段已返回完整字段。
    pub async fn enrich_details<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        items: &[Anime],
    ) -> AnimeMetadataDetailCollection {
        let requests = detail_requests_for_items(items);
        let mikan_candidates = items
            .iter()
            .filter(|item| external_id(item, "mikan").is_some())
            .count();
        let mikan_requests = requests
            .iter()
            .filter(|request| {
                request
                    .providers
                    .contains(&AnimeMetadataDetailProvider::Mikan)
            })
            .count();
        log::info!(
            "Rust 新番详情请求计划 items={} mikan_candidates={} mikan_requests={} mikan_skipped={}",
            items.len(),
            mikan_candidates,
            mikan_requests,
            mikan_candidates.saturating_sub(mikan_requests)
        );
        self.enrich_detail_requests(store, &requests).await
    }

    /// 仅重试上轮失败的详情来源，保留其他来源已补全的字段。
    pub async fn retry_details<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        requests: &[AnimeMetadataDetailRequest],
    ) -> AnimeMetadataDetailCollection {
        self.enrich_detail_requests(store, requests).await
    }

    /// 按请求中声明的来源并发补全详情并汇总来源级重试状态。
    async fn enrich_detail_requests<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        requests: &[AnimeMetadataDetailRequest],
    ) -> AnimeMetadataDetailCollection {
        let detail_started = Instant::now();
        let attempted_provider_count = requests
            .iter()
            .map(|request| request.providers.len())
            .sum::<usize>();
        let results = stream::iter(
            requests
                .iter()
                .cloned()
                .map(|request| async move { self.enrich_catalog_item(store, request).await }),
        )
        .buffered(PROVIDER_DETAIL_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        let settled_error_count = results
            .iter()
            .map(|result| result.settled_error_count)
            .sum();
        let deferred_error_count = results
            .iter()
            .map(|result| result.deferred_error_count)
            .sum();
        let retryable_requests = results
            .iter()
            .filter(|result| !result.retryable_providers.is_empty())
            .map(|result| AnimeMetadataDetailRequest {
                item: result.item.clone(),
                providers: result.retryable_providers.clone(),
                retry_after_ms: result.retry_after_ms,
            })
            .collect();
        let successful_providers = results
            .iter()
            .flat_map(|result| {
                result.successful_providers.iter().copied().map(|provider| {
                    AnimeMetadataDetailProviderOutcome {
                        anime_id: result.item.id.clone(),
                        provider,
                    }
                })
            })
            .collect();
        let settled_failed_providers = results
            .iter()
            .flat_map(|result| {
                result
                    .settled_failed_providers
                    .iter()
                    .copied()
                    .map(|provider| AnimeMetadataDetailProviderOutcome {
                        anime_id: result.item.id.clone(),
                        provider,
                    })
            })
            .collect();
        log::info!(
            "Rust 新番阶段耗时 phase=detail-enrichment items={} providers={} settled_errors={} deferred_errors={} duration_ms={}",
            requests.len(),
            attempted_provider_count,
            settled_error_count,
            deferred_error_count,
            detail_started.elapsed().as_millis()
        );
        AnimeMetadataDetailCollection {
            items: results
                .iter()
                .filter(|result| result.persist_item)
                .map(|result| result.item.clone())
                .collect(),
            settled_error_count,
            deferred_error_count,
            retryable_requests,
            successful_providers,
            settled_failed_providers,
        }
    }

    /// 并行补全单条目录已有的来源详情，并保持本地主记录标识。
    async fn enrich_catalog_item<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        request: AnimeMetadataDetailRequest,
    ) -> DetailEnrichmentOutcome {
        let AnimeMetadataDetailRequest {
            item: local,
            providers,
            ..
        } = request;
        let fetch_bangumi = providers.contains(&AnimeMetadataDetailProvider::Bangumi);
        let fetch_mikan = providers.contains(&AnimeMetadataDetailProvider::Mikan);
        let bangumi_id = external_id(&local, "bangumi");
        let cached_bangumi = bangumi_id
            .as_deref()
            .and_then(|id| self.cached_bangumi_catalog_subject(id));
        let bangumi = async {
            match (fetch_bangumi, bangumi_id.as_deref()) {
                (true, Some(id)) => self.fetch_bangumi_detail(store, id, &local).await.map(Some),
                _ => Ok(None),
            }
        };
        let mikan = async {
            match (fetch_mikan, external_id(&local, "mikan")) {
                (true, Some(id)) => self
                    .fetch_mikan_detail_by_id(store, &id, &local)
                    .await
                    .map(Some),
                _ => Ok(None),
            }
        };
        let (bangumi, mikan) = tokio::join!(bangumi, mikan);
        let mut settled_error_count = 0usize;
        let mut deferred_error_count = 0usize;
        let mut retryable_providers = Vec::new();
        let mut settled_failed_providers = Vec::new();
        let mut retry_after_ms = 0u64;
        let mut record_error =
            |provider: AnimeMetadataDetailProvider, source: &str, error: &SourceError| {
                if let Some(delay) = detail_retry_after_ms(error) {
                    deferred_error_count += 1;
                    retryable_providers.push(provider);
                    retry_after_ms = retry_after_ms.max(delay);
                } else {
                    settled_error_count += 1;
                    settled_failed_providers.push(provider);
                }
                log::warn!(
                    "季度新番详情补全失败 anime_id={} provider={} error={error}",
                    local.id,
                    source
                );
            };

        let mut leading_bangumi = None;
        let mut trailing_bangumi = None;
        match bangumi {
            Ok(Some(item)) => {
                if let Some(subject) = cached_bangumi.as_ref() {
                    leading_bangumi = Some(map_bangumi(
                        subject.clone(),
                        local.premiere_year,
                        local.premiere_month,
                    ));
                }
                trailing_bangumi = Some(item);
            }
            Ok(None) => {}
            Err(error) => {
                record_error(AnimeMetadataDetailProvider::Bangumi, "bangumi", &error);
                if let Some(subject) = cached_bangumi {
                    log::info!(
                        "季度新番详情使用目录回退 anime_id={} provider=bangumi",
                        local.id
                    );
                    leading_bangumi = Some(map_bangumi(
                        subject,
                        local.premiere_year,
                        local.premiere_month,
                    ));
                }
            }
        }
        let mikan = match mikan {
            Ok(item) => item,
            Err(error) => {
                record_error(AnimeMetadataDetailProvider::Mikan, "mikan", &error);
                None
            }
        };
        let authoritative_detail = trailing_bangumi.clone().or_else(|| mikan.clone());
        let mut batches = Vec::new();
        if let Some(item) = leading_bangumi {
            batches.push(AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![item],
            });
        }
        batches.push(AnimeMetadataBatch {
            source: "local".to_owned(),
            items: vec![local.clone()],
        });
        if let Some(item) = trailing_bangumi {
            batches.push(AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![item],
            });
        }
        if let Some(item) = mikan {
            batches.push(AnimeMetadataBatch {
                source: "mikan".to_owned(),
                items: vec![item],
            });
        }
        let mut item = merge_anime_metadata_batches(&batches)
            .into_iter()
            .next()
            .unwrap_or_else(|| local.clone());
        if let Some(authoritative) = authoritative_detail {
            item.summary = authoritative.summary.or(item.summary);
            item.original_title = authoritative.original_title.or(item.original_title);
            item.detail = merge_detail(authoritative.detail, item.detail);
        }
        item.id.clone_from(&local.id);
        preserve_catalog_cover(&local, &mut item);
        for (index, alias) in item.aliases.iter_mut().enumerate() {
            alias.id = format!("{}-alias-{}", item.id, index + 1);
            alias.anime_id.clone_from(&item.id);
        }
        let successful_providers = providers
            .into_iter()
            .filter(|provider| {
                !retryable_providers.contains(provider)
                    && !settled_failed_providers.contains(provider)
            })
            .collect::<Vec<_>>();
        let persist_item = item != local;
        DetailEnrichmentOutcome {
            item,
            persist_item,
            settled_error_count,
            deferred_error_count,
            retryable_providers,
            successful_providers,
            settled_failed_providers,
            retry_after_ms,
        }
    }

    /// 按已有 external id 增量刷新单部详情，单来源失败不覆盖本地字段。
    pub async fn refresh_detail<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        local: &Anime,
    ) -> AnimeMetadataRefresh {
        let mut batches = vec![AnimeMetadataBatch {
            source: "local".to_owned(),
            items: vec![local.clone()],
        }];
        let mut errors = Vec::new();
        let mut attempted = 0usize;

        for provider in ["bangumi", "anilist", "mikan"] {
            let Some(external_id) = external_id(local, provider) else {
                continue;
            };
            attempted += 1;
            let result = match provider {
                "bangumi" => self.fetch_bangumi_detail(store, &external_id, local).await,
                "anilist" => self.fetch_anilist_detail(store, &external_id, local).await,
                "mikan" => {
                    self.fetch_mikan_detail_by_id(store, &external_id, local)
                        .await
                }
                _ => unreachable!(),
            };
            match result {
                Ok(item) => batches.push(AnimeMetadataBatch {
                    source: provider.to_owned(),
                    items: vec![item],
                }),
                Err(error) => errors.push(AnimeDetailPartialError {
                    source: provider.to_owned(),
                    message: error.to_string(),
                }),
            }
        }
        if attempted == 0 {
            errors.push(AnimeDetailPartialError {
                source: "metadata".to_owned(),
                message: "没有可用的 external id".to_owned(),
            });
        }
        let success_count = batches.len().saturating_sub(1);
        let mut item = merge_anime_metadata_batches(&batches)
            .into_iter()
            .next()
            .unwrap_or_else(|| local.clone());
        item.id.clone_from(&local.id);
        preserve_catalog_cover(local, &mut item);
        for (index, alias) in item.aliases.iter_mut().enumerate() {
            alias.id = format!("{}-alias-{}", item.id, index + 1);
            alias.anime_id.clone_from(&item.id);
        }
        if success_count > 0 {
            let detail = item.detail.get_or_insert_with(|| json!({}));
            if let Some(object) = detail.as_object_mut() {
                object.insert(
                    "refreshedAt".to_owned(),
                    Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
                );
            }
        }
        AnimeMetadataRefresh {
            item,
            success_count,
            errors,
        }
    }

    async fn collect_bangumi_month<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        month: i64,
    ) -> Result<Vec<Anime>, SourceError> {
        self.collect_bangumi_month_inner(store, year, month, true)
            .await
    }

    /// 只读取 Bangumi 月度分页目录。
    async fn collect_bangumi_month_catalog<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        month: i64,
    ) -> Result<Vec<Anime>, SourceError> {
        self.collect_bangumi_month_inner(store, year, month, false)
            .await
    }

    /// 按阶段决定是否逐部请求 Bangumi 详情。
    async fn collect_bangumi_month_inner<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        month: i64,
        include_details: bool,
    ) -> Result<Vec<Anime>, SourceError> {
        let mut subjects = Vec::new();
        let mut offset = 0usize;
        while offset < BANGUMI_MAX_MONTHLY_ITEMS {
            let mut url = endpoint_url(&self.endpoints.bangumi, "v0/subjects")?;
            url.query_pairs_mut()
                .append_pair("type", "2")
                .append_pair("sort", "date")
                .append_pair("year", &year.to_string())
                .append_pair("month", &month.to_string())
                .append_pair("limit", &BANGUMI_PAGE_LIMIT.to_string())
                .append_pair("offset", &offset.to_string());
            let page: BangumiPage = self.get_json(store, "bangumi", true, url).await?;
            let page_items = page.data.unwrap_or_default();
            let page_offset = page.offset.unwrap_or(offset);
            let page_limit = page.limit.unwrap_or(BANGUMI_PAGE_LIMIT).max(1);
            let total = page.total;
            let item_count = page_items.len();
            subjects.extend(page_items.into_iter().filter(|item| {
                item.subject_type == 2
                    && item
                        .date
                        .as_deref()
                        .is_none_or(|date| date_in_month(date, year, month))
            }));
            if item_count == 0 || total.is_some_and(|total| page_offset + item_count >= total) {
                break;
            }
            let next = page_offset.saturating_add(page_limit);
            if next <= offset {
                break;
            }
            offset = next;
        }
        if !include_details {
            self.cache_bangumi_catalog_subjects(&subjects);
            let mapping_started = Instant::now();
            let item_count = subjects.len();
            let items = subjects
                .into_iter()
                .map(|item| map_bangumi_catalog(item, year, month))
                .collect();
            log::info!(
                "Rust 新番阶段耗时 phase=catalog-map provider=bangumi year={year} month={month} items={item_count} duration_ms={}",
                mapping_started.elapsed().as_millis()
            );
            return Ok(items);
        }
        let detailed = stream::iter(subjects.into_iter().map(|subject| async move {
            match self
                .get_json::<S, BangumiSubject>(
                    store,
                    "bangumi",
                    true,
                    endpoint_url(
                        &self.endpoints.bangumi,
                        &format!("v0/subjects/{}", subject.id),
                    )?,
                )
                .await
            {
                Ok(detail) => Ok(detail.merge(subject)),
                Err(error) => {
                    log::warn!("Bangumi 详情补全失败 id={} error={error}", subject.id);
                    Ok(subject)
                }
            }
        }))
        .buffered(PROVIDER_DETAIL_CONCURRENCY)
        .collect::<Vec<Result<BangumiSubject, SourceError>>>()
        .await;
        let mapping_started = Instant::now();
        let item_count = detailed.len();
        let items = detailed
            .into_iter()
            .map(|item| item.map(|item| map_bangumi(item, year, month)))
            .collect();
        log::info!(
            "Rust 新番阶段耗时 phase=detail-map provider=bangumi year={year} month={month} items={item_count} duration_ms={}",
            mapping_started.elapsed().as_millis()
        );
        items
    }

    async fn search_bangumi<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        keyword: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        let mut url = endpoint_url(&self.endpoints.bangumi, "v0/search/subjects")?;
        url.query_pairs_mut()
            .append_pair("limit", &SEARCH_LIMIT.to_string())
            .append_pair("offset", "0");
        let response: BangumiPage = self
            .post_json(
                store,
                "bangumi",
                true,
                url,
                json!({"keyword": keyword, "sort": "match", "filter": {"type": [2]}}),
            )
            .await?;
        let subjects = response
            .data
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.subject_type == 2)
            .collect::<Vec<_>>();
        let detailed = stream::iter(subjects.into_iter().map(|subject| async move {
            match self
                .get_json::<S, BangumiSubject>(
                    store,
                    "bangumi",
                    true,
                    endpoint_url(
                        &self.endpoints.bangumi,
                        &format!("v0/subjects/{}", subject.id),
                    )?,
                )
                .await
            {
                Ok(detail) => Ok(detail.merge(subject)),
                Err(_) => Ok(subject),
            }
        }))
        .buffered(PROVIDER_DETAIL_CONCURRENCY)
        .collect::<Vec<Result<BangumiSubject, SourceError>>>()
        .await;
        detailed
            .into_iter()
            .map(|item| {
                item.map(|item| {
                    let (year, month) = date_or_now(item.date.as_deref());
                    map_bangumi(item, year, month)
                })
            })
            .collect()
    }

    async fn fetch_bangumi_detail<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        external_id: &str,
        fallback: &Anime,
    ) -> Result<Anime, SourceError> {
        validate_numeric_id(external_id, "Bangumi")?;
        let item: BangumiSubject = self
            .get_json(
                store,
                "bangumi",
                true,
                endpoint_url(
                    &self.endpoints.bangumi,
                    &format!("v0/subjects/{external_id}"),
                )?,
            )
            .await?;
        let mapping_started = Instant::now();
        let mapped = map_bangumi(item, fallback.premiere_year, fallback.premiere_month);
        log::info!(
            "Rust 新番阶段耗时 phase=detail-map provider=bangumi anime_id={} duration_ms={}",
            fallback.id,
            mapping_started.elapsed().as_millis()
        );
        Ok(mapped)
    }

    async fn collect_anilist_season<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        let query = format!(
            "query SeasonalAnime($season: MediaSeason!, $seasonYear: Int!, $page: Int!, $perPage: Int!) {{ Page(page: $page, perPage: $perPage) {{ pageInfo {{ currentPage hasNextPage }} media(type: ANIME, season: $season, seasonYear: $seasonYear, sort: POPULARITY_DESC) {{ {} }} }} }}",
            anilist_fields()
        );
        let mut page = 1i64;
        let mut media = BTreeMap::<i64, AniListMedia>::new();
        loop {
            let response = self
                .anilist_request(
                    store,
                    &query,
                    json!({
                        "season": season.to_ascii_uppercase(),
                        "seasonYear": year,
                        "page": page,
                        "perPage": 50
                    }),
                )
                .await?;
            let page_data = response.data.and_then(|data| data.page).unwrap_or_default();
            let count = page_data.media.len();
            for item in page_data.media {
                media.insert(item.id, item);
            }
            if !page_data
                .page_info
                .as_ref()
                .and_then(|info| info.has_next_page)
                .unwrap_or(false)
                || count == 0
                || page >= 20
            {
                break;
            }
            page = page_data
                .page_info
                .and_then(|info| info.current_page)
                .unwrap_or(page)
                .saturating_add(1);
        }
        Ok(media
            .into_values()
            .filter(|item| item.start_date.as_ref().and_then(|date| date.year) == Some(year))
            .map(map_anilist)
            .collect())
    }

    async fn search_anilist<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        keyword: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        let query = format!(
            "query SearchAnime($search: String!, $perPage: Int!) {{ Page(page: 1, perPage: $perPage) {{ media(type: ANIME, search: $search, sort: SEARCH_MATCH) {{ {} }} }} }}",
            anilist_fields()
        );
        let response = self
            .anilist_request(
                store,
                &query,
                json!({"search": keyword, "perPage": SEARCH_LIMIT}),
            )
            .await?;
        Ok(response
            .data
            .and_then(|data| data.page)
            .unwrap_or_default()
            .media
            .into_iter()
            .map(map_anilist)
            .collect())
    }

    async fn fetch_anilist_detail<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        external_id: &str,
        _fallback: &Anime,
    ) -> Result<Anime, SourceError> {
        let id = validate_numeric_id(external_id, "AniList")?;
        let query = format!(
            "query AnimeDetail($id: Int!) {{ Media(id: $id, type: ANIME) {{ {} }} }}",
            anilist_fields()
        );
        let response = self
            .anilist_request(store, &query, json!({"id": id}))
            .await?;
        response
            .data
            .and_then(|data| data.media)
            .map(map_anilist)
            .ok_or_else(|| SourceError::Parse("AniList 未返回番剧详情".to_owned()))
    }

    async fn anilist_request<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        query: &str,
        variables: Value,
    ) -> Result<AniListResponse, SourceError> {
        let response: AniListResponse = self
            .post_json(
                store,
                "anilist",
                false,
                Url::parse(&self.endpoints.anilist)
                    .map_err(|error| SourceError::InvalidUrl(error.to_string()))?,
                json!({"query": query, "variables": variables}),
            )
            .await?;
        if !response.errors.is_empty() {
            return Err(SourceError::Parse(
                response
                    .errors
                    .into_iter()
                    .filter_map(|error| error.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        Ok(response)
    }

    async fn collect_mikan_season<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        self.collect_mikan_season_inner(store, year, season, true)
            .await
    }

    /// 只读取 Mikan 季度索引及索引页可用封面。
    async fn collect_mikan_season_catalog<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        self.collect_mikan_season_inner(store, year, season, false)
            .await
    }

    /// 按阶段决定是否逐部请求 Mikan 详情页。
    async fn collect_mikan_season_inner<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        year: i64,
        season: &str,
        include_details: bool,
    ) -> Result<Vec<Anime>, SourceError> {
        let season_text = match season {
            "winter" => "冬",
            "spring" => "春",
            "summer" => "夏",
            "fall" => "秋",
            _ => return Err(SourceError::Parse(format!("季度无效：{season}"))),
        };
        let mut errors = Vec::new();
        let mut candidates = Vec::new();
        for path in ["Home/BangumiCoverFlowByDayOfWeek", "Home/Classic"] {
            let mut url = endpoint_url(&self.endpoints.mikan, path)?;
            url.query_pairs_mut()
                .append_pair("year", &year.to_string())
                .append_pair("seasonStr", season_text);
            match self.get_text(store, "mikan", true, url).await {
                Ok(html) => {
                    candidates = parse_mikan_candidates(&html, &self.endpoints.mikan)?;
                    if !candidates.is_empty() {
                        break;
                    }
                }
                Err(error) => errors.push(error.to_string()),
            }
        }
        if candidates.is_empty() {
            return Err(SourceError::Parse(if errors.is_empty() {
                "Mikan 季度页未返回番组条目".to_owned()
            } else {
                errors.join("; ")
            }));
        }
        let fallback_month = season_start_month(season)?;
        if include_details {
            self.fetch_mikan_candidates(store, candidates, year, fallback_month)
                .await
        } else {
            Ok(candidates
                .into_iter()
                .map(|candidate| {
                    let mut item =
                        map_mikan(candidate, MikanDetail::default(), year, fallback_month);
                    item.detail = None;
                    item
                })
                .collect())
        }
    }

    async fn search_mikan<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        keyword: &str,
    ) -> Result<Vec<Anime>, SourceError> {
        let mut url = endpoint_url(&self.endpoints.mikan, "Home/Search")?;
        url.query_pairs_mut().append_pair("searchstr", keyword);
        let html = self.get_text(store, "mikan", true, url).await?;
        let candidates = parse_mikan_candidates(&html, &self.endpoints.mikan)?
            .into_iter()
            .take(SEARCH_LIMIT)
            .collect::<Vec<_>>();
        let now = Utc::now();
        self.fetch_mikan_candidates(
            store,
            candidates,
            i64::from(now.year()),
            i64::from(now.month()),
        )
        .await
    }

    async fn fetch_mikan_candidates<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        candidates: Vec<MikanCandidate>,
        fallback_year: i64,
        fallback_month: i64,
    ) -> Result<Vec<Anime>, SourceError> {
        let detailed = stream::iter(candidates.into_iter().take(MIKAN_DETAIL_LIMIT).map(
            |candidate| async move {
                let detail = match self
                    .get_text(
                        store,
                        "mikan",
                        true,
                        Url::parse(&candidate.detail_url)
                            .map_err(|error| SourceError::InvalidUrl(error.to_string()))?,
                    )
                    .await
                {
                    Ok(html) => parse_mikan_detail(&html, &candidate.detail_url),
                    Err(error) => {
                        log::warn!("Mikan 详情补全失败 id={} error={error}", candidate.id);
                        MikanDetail::default()
                    }
                };
                Ok(map_mikan(candidate, detail, fallback_year, fallback_month))
            },
        ))
        .buffered(PROVIDER_DETAIL_CONCURRENCY)
        .collect::<Vec<Result<Anime, SourceError>>>()
        .await;
        detailed.into_iter().collect()
    }

    async fn fetch_mikan_detail_by_id<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        external_id: &str,
        fallback: &Anime,
    ) -> Result<Anime, SourceError> {
        validate_numeric_id(external_id, "Mikan")?;
        let url = endpoint_url(
            &self.endpoints.mikan,
            &format!("Home/Bangumi/{external_id}"),
        )?;
        let detail_url = url.to_string();
        let html = self.get_text(store, "mikan", true, url).await?;
        Ok(map_mikan(
            MikanCandidate {
                id: external_id.to_owned(),
                title: fallback.title.clone(),
                detail_url: detail_url.clone(),
                cover_url: fallback.cover_url.clone(),
            },
            parse_mikan_detail(&html, &detail_url),
            fallback.premiere_year,
            fallback.premiere_month,
        ))
    }

    async fn get_text<S: CircuitStateStore + Sync>(
        &self,
        store: &S,
        provider: &str,
        use_proxy: bool,
        url: Url,
    ) -> Result<String, SourceError> {
        let source = metadata_source(provider, use_proxy);
        let request_target = url.path().to_owned();
        let request = NativeHttpRequest {
            source_id: provider.to_owned(),
            method: HttpMethod::Get,
            url: url.to_string(),
            headers: metadata_headers(provider),
            body: None,
            request_interval_ms: 500,
        };
        let request_started = Instant::now();
        let response = match self.channel {
            NetworkRequestChannel::Interactive => {
                self.network.execute(store, &source, request).await
            }
            NetworkRequestChannel::Background => {
                self.network
                    .execute_background(store, &source, request)
                    .await
            }
        };
        match &response {
            Ok(response) => log::info!(
                "Rust 新番阶段耗时 phase=network provider={provider} target={request_target} status={} bytes={} duration_ms={}",
                response.status,
                response.body.len(),
                request_started.elapsed().as_millis()
            ),
            Err(error) => log::warn!(
                "Rust 新番阶段失败 phase=network provider={provider} target={request_target} duration_ms={} error={error}",
                request_started.elapsed().as_millis()
            ),
        }
        let response = response?;
        Ok(response.text())
    }

    async fn get_json<S, T>(
        &self,
        store: &S,
        provider: &str,
        use_proxy: bool,
        url: Url,
    ) -> Result<T, SourceError>
    where
        S: CircuitStateStore + Sync,
        T: for<'de> Deserialize<'de>,
    {
        let text = self.get_text(store, provider, use_proxy, url).await?;
        let parse_started = Instant::now();
        let result = serde_json::from_str(&text)
            .map_err(|error| SourceError::Parse(format!("{provider} JSON 解析失败：{error}")));
        log::info!(
            "Rust 新番阶段耗时 phase=json-decode provider={provider} bytes={} success={} duration_ms={}",
            text.len(),
            result.is_ok(),
            parse_started.elapsed().as_millis()
        );
        result
    }

    async fn post_json<S, T>(
        &self,
        store: &S,
        provider: &str,
        use_proxy: bool,
        url: Url,
        body: Value,
    ) -> Result<T, SourceError>
    where
        S: CircuitStateStore + Sync,
        T: for<'de> Deserialize<'de>,
    {
        let mut headers = metadata_headers(provider);
        headers.insert("Content-Type".to_owned(), "application/json".to_owned());
        let source = metadata_source(provider, use_proxy);
        let request = NativeHttpRequest {
            source_id: provider.to_owned(),
            method: HttpMethod::Post,
            url: url.to_string(),
            headers,
            body: Some(
                serde_json::to_vec(&body).map_err(|error| SourceError::Parse(error.to_string()))?,
            ),
            request_interval_ms: 500,
        };
        let request_target = url.path().to_owned();
        let request_started = Instant::now();
        let response = match self.channel {
            NetworkRequestChannel::Interactive => {
                self.network.execute(store, &source, request).await
            }
            NetworkRequestChannel::Background => {
                self.network
                    .execute_background(store, &source, request)
                    .await
            }
        };
        match &response {
            Ok(response) => log::info!(
                "Rust 新番阶段耗时 phase=network provider={provider} target={request_target} status={} bytes={} duration_ms={}",
                response.status,
                response.body.len(),
                request_started.elapsed().as_millis()
            ),
            Err(error) => log::warn!(
                "Rust 新番阶段失败 phase=network provider={provider} target={request_target} duration_ms={} error={error}",
                request_started.elapsed().as_millis()
            ),
        }
        let response = response?;
        let parse_started = Instant::now();
        let result = serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::Parse(format!("{provider} JSON 解析失败：{error}")));
        log::info!(
            "Rust 新番阶段耗时 phase=json-decode provider={provider} bytes={} success={} duration_ms={}",
            response.body.len(),
            result.is_ok(),
            parse_started.elapsed().as_millis()
        );
        result
    }
}

/// 已有季度目录封面优先，只有目录缺图时才采用详情来源封面。
fn preserve_catalog_cover(local: &Anime, enriched: &mut Anime) {
    if local.cover_url.is_some() {
        enriched.cover_url.clone_from(&local.cover_url);
    }
}

/// 返回可补偿来源错误的等待时间，永久数据错误不进入重试队列。
fn detail_retry_after_ms(error: &SourceError) -> Option<u64> {
    match error {
        SourceError::Transport(_) => Some(DETAIL_TRANSIENT_RETRY_DELAY_MS),
        SourceError::HttpStatus { status: 429, .. } => Some(DETAIL_RATE_LIMIT_RETRY_DELAY_MS),
        SourceError::HttpStatus { status, .. } if *status >= 500 => {
            Some(DETAIL_TRANSIENT_RETRY_DELAY_MS)
        }
        SourceError::CircuitOpen { backoff_until } => {
            let backoff_until = chrono::DateTime::parse_from_rfc3339(backoff_until)
                .ok()?
                .with_timezone(&Utc);
            let remaining = backoff_until
                .signed_duration_since(Utc::now())
                .num_milliseconds()
                .max(0) as u64;
            Some(remaining.saturating_add(500))
        }
        _ => None,
    }
}

fn collect_provider_results<const N: usize>(
    results: [(&str, Result<Vec<Anime>, SourceError>); N],
) -> AnimeMetadataCollection {
    let providers = results
        .iter()
        .map(|(source, _)| *source)
        .collect::<Vec<_>>();
    let mut batches = Vec::new();
    let mut errors = Vec::new();
    let mut successful_sources = Vec::new();
    for (source, result) in results {
        match result {
            Ok(items) if !items.is_empty() => {
                successful_sources.push(source.to_owned());
                batches.push(AnimeMetadataBatch {
                    source: source.to_owned(),
                    items: unique_by_normalized_title(items),
                });
            }
            Ok(_) => errors.push(format!("{source}: 未返回新番数据")),
            Err(error) => errors.push(format!("{source}: {error}")),
        }
    }
    AnimeMetadataCollection {
        source: if batches.is_empty() {
            providers.join(",")
        } else {
            batches
                .iter()
                .map(|batch| batch.source.as_str())
                .collect::<Vec<_>>()
                .join("+")
        },
        items: merge_anime_metadata_batches(&batches),
        errors,
        successful_sources,
    }
}

/// 搜索时空结果仍视为来源成功，只隔离真正失败的来源。
fn collect_search_provider_results<const N: usize>(
    results: [(&str, Result<Vec<Anime>, SourceError>); N],
) -> AnimeMetadataCollection {
    let mut batches = Vec::new();
    let mut sources = Vec::new();
    let mut errors = Vec::new();
    for (source, result) in results {
        match result {
            Ok(items) => {
                sources.push(source);
                if !items.is_empty() {
                    batches.push(AnimeMetadataBatch {
                        source: source.to_owned(),
                        items: unique_by_normalized_title(items),
                    });
                }
            }
            Err(error) => errors.push(format!("{source}: {error}")),
        }
    }
    AnimeMetadataCollection {
        source: sources.join("+"),
        items: merge_anime_metadata_batches(&batches),
        errors,
        successful_sources: sources.into_iter().map(str::to_owned).collect(),
    }
}

/// 按 external id 和同播出窗口标题合并多来源元数据。
pub fn merge_anime_metadata_batches(batches: &[AnimeMetadataBatch]) -> Vec<Anime> {
    let candidates = batches
        .iter()
        .flat_map(|batch| batch.items.iter().cloned())
        .collect::<Vec<_>>();
    let mut parents = (0..candidates.len()).collect::<Vec<_>>();
    for (left, right) in merge_candidate_pairs(&candidates) {
        if should_merge(&candidates[left], &candidates[right]) {
            union(&mut parents, left, right);
        }
    }
    let mut groups = Vec::<(usize, Vec<Anime>)>::new();
    let mut group_indexes = HashMap::<usize, usize>::new();
    for (index, item) in candidates.into_iter().enumerate() {
        let root = find(&mut parents, index);
        if let Some(group_index) = group_indexes.get(&root).copied() {
            groups[group_index].1.push(item);
        } else {
            group_indexes.insert(root, groups.len());
            groups.push((root, vec![item]));
        }
    }
    groups
        .into_iter()
        .map(|(_, mut items)| {
            let first = items.remove(0);
            items.into_iter().fold(first, merge_anime)
        })
        .collect()
}

/// 通过外部标识和播出窗口标题建立候选桶，避免全量两两比较。
fn merge_candidate_pairs(candidates: &[Anime]) -> BTreeSet<(usize, usize)> {
    let mut external_buckets = HashMap::<(String, String), Vec<usize>>::new();
    let mut title_buckets = HashMap::<(i64, Option<String>, String), Vec<usize>>::new();
    for (index, anime) in candidates.iter().enumerate() {
        for (provider, external_id) in external_ids(anime) {
            external_buckets
                .entry((provider.to_owned(), external_id.to_owned()))
                .or_default()
                .push(index);
        }
        let mut normalized_names = normalized_anime_names(anime);
        for name in normalized_names.drain() {
            title_buckets
                .entry((anime.premiere_year, anime.season.clone(), name))
                .or_default()
                .push(index);
        }
    }

    let mut pairs = BTreeSet::new();
    for bucket in external_buckets.values().chain(title_buckets.values()) {
        for (position, left) in bucket.iter().copied().enumerate() {
            for right in bucket.iter().copied().skip(position + 1) {
                pairs.insert((left.min(right), left.max(right)));
            }
        }
    }
    pairs
}

fn unique_by_normalized_title(items: Vec<Anime>) -> Vec<Anime> {
    let mut unique = Vec::<Anime>::new();
    let mut group_names = Vec::<HashSet<String>>::new();
    let mut title_indexes = HashMap::<String, BTreeSet<usize>>::new();
    for item in items {
        let item_names = normalized_anime_names(&item);
        let existing_index = item_names
            .iter()
            .filter_map(|name| title_indexes.get(name))
            .flat_map(|indexes| indexes.iter().copied())
            .min();
        if let Some(index) = existing_index {
            for name in &group_names[index] {
                let remove_entry = title_indexes.get_mut(name).is_some_and(|indexes| {
                    indexes.remove(&index);
                    indexes.is_empty()
                });
                if remove_entry {
                    title_indexes.remove(name);
                }
            }
            let merged = merge_anime(unique[index].clone(), item);
            let merged_names = normalized_anime_names(&merged);
            for name in &merged_names {
                title_indexes.entry(name.clone()).or_default().insert(index);
            }
            unique[index] = merged;
            group_names[index] = merged_names;
        } else {
            let index = unique.len();
            for name in &item_names {
                title_indexes.entry(name.clone()).or_default().insert(index);
            }
            unique.push(item);
            group_names.push(item_names);
        }
    }
    unique
}

fn should_merge(left: &Anime, right: &Anime) -> bool {
    if conflicting_external_id(left, right) {
        return false;
    }
    shared_external_id(left, right)
        || (left.premiere_year == right.premiere_year
            && left.season == right.season
            && shared_title(left, right))
}

fn merge_anime(mut primary: Anime, secondary: Anime) -> Anime {
    let preferred_title = preferred_title(&primary, &secondary);
    let original_title = preferred_original_title(&preferred_title, &primary, &secondary);
    let premiere_date = preferred_premiere_date(
        primary.premiere_date.as_deref(),
        secondary.premiere_date.as_deref(),
    );
    let (premiere_year, premiere_month) = premiere_date.as_deref().and_then(parse_date).map_or(
        (primary.premiere_year, primary.premiere_month),
        |(year, month, _)| (year, month),
    );
    primary.title = preferred_title;
    primary.original_title = original_title;
    primary.aliases = merge_aliases(&primary, &secondary);
    primary.premiere_date = premiere_date;
    primary.premiere_year = premiere_year;
    primary.premiere_month = premiere_month;
    primary.season = primary.season.or(secondary.season);
    primary.summary = primary.summary.or(secondary.summary);
    primary.cover_url = primary.cover_url.or(secondary.cover_url);
    primary.rating = preferred_rating(primary.rating, secondary.rating);
    primary.external_ids = merge_json_objects(primary.external_ids, secondary.external_ids);
    primary.detail = merge_detail(primary.detail, secondary.detail);
    primary
}

fn merge_aliases(primary: &Anime, secondary: &Anime) -> Vec<AnimeAlias> {
    let ignored = [
        Some(primary.title.as_str()),
        primary.original_title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(normalize_title)
    .collect::<HashSet<_>>();
    let mut aliases = Vec::<AnimeAlias>::new();
    let mut candidates = primary
        .aliases
        .iter()
        .chain(secondary.aliases.iter())
        .cloned()
        .collect::<Vec<_>>();
    candidates.push(title_alias(&primary.id, &secondary.title, 90));
    if let Some(title) = secondary.original_title.as_deref() {
        candidates.push(title_alias(&primary.id, title, 85));
    }
    for mut alias in candidates {
        let normalized = normalize_title(&alias.alias);
        if normalized.is_empty()
            || ignored.contains(&normalized)
            || aliases
                .iter()
                .any(|current| normalize_title(&current.alias) == normalized)
        {
            continue;
        }
        alias.id = format!("{}-alias-{}", primary.id, aliases.len() + 1);
        alias.anime_id.clone_from(&primary.id);
        aliases.push(alias);
    }
    aliases
}

fn preferred_title(primary: &Anime, secondary: &Anime) -> String {
    [primary, secondary]
        .into_iter()
        .find_map(|anime| looks_chinese(&anime.title).then(|| anime.title.clone()))
        .or_else(|| {
            [primary, secondary].into_iter().find_map(|anime| {
                anime
                    .aliases
                    .iter()
                    .find(|alias| alias.language == AnimeAliasLanguage::Zh)
                    .map(|alias| alias.alias.clone())
            })
        })
        .unwrap_or_else(|| primary.title.clone())
}

fn preferred_original_title(title: &str, primary: &Anime, secondary: &Anime) -> Option<String> {
    let title = normalize_title(title);
    [primary, secondary]
        .into_iter()
        .flat_map(|anime| {
            anime
                .original_title
                .iter()
                .chain(std::iter::once(&anime.title))
                .chain(anime.aliases.iter().map(|alias| &alias.alias))
        })
        .find(|candidate| normalize_title(candidate) != title && looks_japanese(candidate))
        .cloned()
}

fn preferred_rating(
    primary: Option<AnimeRating>,
    secondary: Option<AnimeRating>,
) -> Option<AnimeRating> {
    match (primary, secondary) {
        (Some(left), Some(right)) => {
            let left_rank = rating_rank(&left.source);
            let right_rank = rating_rank(&right.source);
            if left_rank < right_rank || (left_rank == right_rank && left.count >= right.count) {
                Some(left)
            } else {
                Some(right)
            }
        }
        (left, right) => left.or(right),
    }
}

fn rating_rank(source: &str) -> usize {
    match source {
        "bangumi" => 0,
        "anilist" => 1,
        _ => 10,
    }
}

fn merge_detail(primary: Option<Value>, secondary: Option<Value>) -> Option<Value> {
    let (mut left, right) = match (primary, secondary) {
        (Some(Value::Object(left)), Some(Value::Object(right))) => (left, right),
        (left, right) => return left.or(right),
    };
    for (key, value) in right {
        if matches!(
            key.as_str(),
            "genres" | "studios" | "staff" | "metadataSources"
        ) {
            let merged = merge_json_arrays(left.remove(&key), Some(value));
            if let Some(merged) = merged {
                left.insert(key, merged);
            }
        } else {
            left.entry(key).or_insert(value);
        }
    }
    Some(Value::Object(left))
}

fn merge_json_arrays(left: Option<Value>, right: Option<Value>) -> Option<Value> {
    let mut items = Vec::<Value>::new();
    let mut seen = HashSet::<String>::new();
    for item in left
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .chain(
            right
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default(),
        )
    {
        let key = item.to_string().to_lowercase();
        if seen.insert(key) {
            items.push(item);
        }
    }
    (!items.is_empty()).then_some(Value::Array(items))
}

fn merge_json_objects(left: Value, right: Value) -> Value {
    let mut object = left.as_object().cloned().unwrap_or_default();
    object.extend(right.as_object().cloned().unwrap_or_default());
    Value::Object(object)
}

fn metadata_source(id: &str, use_proxy: bool) -> ReleaseSourceConfig {
    ReleaseSourceConfig {
        id: format!("metadata-{id}"),
        name: id.to_owned(),
        kind: SourceKind::SiteAdapter,
        enabled: true,
        use_proxy,
        request_interval_ms: 500,
        base_url: None,
        api_key: None,
        rss_url: None,
        tags: vec!["metadata".to_owned()],
    }
}

fn metadata_headers(provider: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::from([("Accept".to_owned(), "application/json".to_owned())]);
    if provider == "bangumi" {
        headers.insert(
            "User-Agent".to_owned(),
            "AniTracker/0.1 (https://github.com/)".to_owned(),
        );
    } else if provider == "mikan" {
        headers.insert(
            "Accept".to_owned(),
            "text/html,application/xhtml+xml".to_owned(),
        );
    }
    headers
}

fn endpoint_url(base: &str, path: &str) -> Result<Url, SourceError> {
    let base = Url::parse(base).map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    base.join(path)
        .map_err(|error| SourceError::InvalidUrl(error.to_string()))
}

fn season_for_month(month: i64) -> Result<&'static str, SourceError> {
    match month {
        1..=3 => Ok("winter"),
        4..=6 => Ok("spring"),
        7..=9 => Ok("summer"),
        10..=12 => Ok("fall"),
        _ => Err(SourceError::Parse(format!("月份无效：{month}"))),
    }
}

fn months_for_season(season: &str) -> Result<[i64; 3], SourceError> {
    match season {
        "winter" => Ok([1, 2, 3]),
        "spring" => Ok([4, 5, 6]),
        "summer" => Ok([7, 8, 9]),
        "fall" => Ok([10, 11, 12]),
        _ => Err(SourceError::Parse(format!("季度无效：{season}"))),
    }
}

fn season_start_month(season: &str) -> Result<i64, SourceError> {
    Ok(months_for_season(season)?[0])
}

fn validate_numeric_id(value: &str, provider: &str) -> Result<i64, SourceError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| SourceError::Parse(format!("{provider} 标识无效")))
}

fn external_id(anime: &Anime, provider: &str) -> Option<String> {
    anime
        .external_ids
        .get(provider)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 为一批目录记录生成实际存在外部标识的详情请求。
pub fn detail_requests_for_items(items: &[Anime]) -> Vec<AnimeMetadataDetailRequest> {
    items
        .iter()
        .filter_map(|item| {
            let providers = detail_providers_for_item(item);
            (!providers.is_empty()).then(|| AnimeMetadataDetailRequest {
                item: item.clone(),
                providers,
                retry_after_ms: 0,
            })
        })
        .collect()
}

/// 选择单部番剧需要补全的详情来源，避免 Bangumi 已覆盖时重复请求 Mikan。
fn detail_providers_for_item(anime: &Anime) -> Vec<AnimeMetadataDetailProvider> {
    let ids = external_ids(anime);
    let has_bangumi = ids.contains_key("bangumi");
    let mut providers = Vec::new();
    if has_bangumi {
        providers.push(AnimeMetadataDetailProvider::Bangumi);
    }
    if ids.contains_key("mikan") && !has_bangumi {
        providers.push(AnimeMetadataDetailProvider::Mikan);
    }
    providers
}

fn external_ids(anime: &Anime) -> HashMap<&str, &str> {
    anime
        .external_ids
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter_map(|(key, value)| value.as_str().map(|value| (key.as_str(), value)))
        .collect()
}

fn shared_external_id(left: &Anime, right: &Anime) -> bool {
    let left = external_ids(left);
    external_ids(right)
        .into_iter()
        .any(|(key, value)| left.get(key) == Some(&value))
}

fn conflicting_external_id(left: &Anime, right: &Anime) -> bool {
    let left = external_ids(left);
    external_ids(right)
        .into_iter()
        .any(|(key, value)| left.get(key).is_some_and(|left| *left != value))
}

fn shared_title(left: &Anime, right: &Anime) -> bool {
    let left = normalized_anime_names(left);
    normalized_anime_names(right)
        .into_iter()
        .any(|name| left.contains(&name))
}

/// 预计算单部番剧可参与匹配的规范化标题集合。
fn normalized_anime_names(anime: &Anime) -> HashSet<String> {
    anime_names(anime)
        .into_iter()
        .map(normalize_title)
        .filter(|name| !name.is_empty())
        .collect()
}

fn anime_names(anime: &Anime) -> Vec<&str> {
    std::iter::once(anime.title.as_str())
        .chain(anime.original_title.as_deref())
        .chain(anime.aliases.iter().map(|alias| alias.alias.as_str()))
        .collect()
}

fn normalize_title(value: &str) -> String {
    const IGNORED: &str = "()[]（）【】「」『』,，、.!！?？:：;；・／/~～_-";
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .filter(|character| !character.is_whitespace() && !IGNORED.contains(*character))
        .collect()
}

fn looks_chinese(value: &str) -> bool {
    value
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
        && !looks_japanese(value)
}

fn looks_japanese(value: &str) -> bool {
    value.chars().any(|character| {
        ('\u{3040}'..='\u{30ff}').contains(&character)
            || ('\u{31f0}'..='\u{31ff}').contains(&character)
    })
}

fn infer_alias_language(value: &str, fallback: AnimeAliasLanguage) -> AnimeAliasLanguage {
    if looks_japanese(value) {
        AnimeAliasLanguage::Ja
    } else if value
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
    {
        AnimeAliasLanguage::Zh
    } else if value.is_ascii() {
        fallback
    } else {
        AnimeAliasLanguage::Custom
    }
}

fn title_alias(anime_id: &str, value: &str, priority: i64) -> AnimeAlias {
    AnimeAlias {
        id: format!("{anime_id}-alias-preserved-{priority}"),
        anime_id: anime_id.to_owned(),
        alias: value.to_owned(),
        language: infer_alias_language(value, AnimeAliasLanguage::Custom),
        priority,
    }
}

fn preferred_premiere_date(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            let left_date = parse_date(left);
            let right_date = parse_date(right);
            if left_date.is_some_and(|(year, month, day)| {
                day == 1
                    && right_date.is_some_and(|(right_year, right_month, right_day)| {
                        year == right_year && month == right_month && right_day > 1
                    })
            }) {
                Some(right.to_owned())
            } else {
                Some(left.to_owned())
            }
        }
        (Some(value), None) | (None, Some(value)) => Some(value.to_owned()),
        (None, None) => None,
    }
}

fn parse_date(value: &str) -> Option<(i64, i64, i64)> {
    let mut parts = value.split('-');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.get(..2)?.parse().ok()?,
    ))
}

fn date_in_month(value: &str, year: i64, month: i64) -> bool {
    parse_date(value).is_some_and(|(candidate_year, candidate_month, _)| {
        candidate_year == year && candidate_month == month
    })
}

fn date_or_now(value: Option<&str>) -> (i64, i64) {
    value
        .and_then(parse_date)
        .map(|(year, month, _)| (year, month))
        .unwrap_or_else(|| {
            let now = Utc::now();
            (i64::from(now.year()), i64::from(now.month()))
        })
}

fn season_value(month: i64) -> String {
    season_for_month(month).unwrap_or("winter").to_owned()
}

fn find(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        parents[index] = find(parents, parents[index]);
    }
    parents[index]
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left = find(parents, left);
    let right = find(parents, right);
    if left != right {
        parents[right] = left;
    }
}

/// 将界面语义筛选映射为 Bangumi 搜索 API 使用的标签。
fn bangumi_browse_tags(filters: &BangumiBrowseFilters) -> Vec<String> {
    let mut tags = BTreeSet::new();
    for value in &filters.formats {
        if let Some(tag) = match value.as_str() {
            "tv" => Some("TV"),
            "movie" => Some("剧场版"),
            "ova" => Some("OVA"),
            "ona" => Some("WEB动画"),
            _ => None,
        } {
            tags.insert(tag.to_owned());
        }
    }
    for value in &filters.source_materials {
        if let Some(tag) = match value.as_str() {
            "original" => Some("原创"),
            "manga" => Some("漫画改"),
            "lightNovel" => Some("轻小说改"),
            "game" => Some("游戏改"),
            "other" => Some("小说改"),
            _ => None,
        } {
            tags.insert(tag.to_owned());
        }
    }
    for value in &filters.genres {
        if let Some(tag) = match value.as_str() {
            "reasoning" => Some("推理"),
            "harem" => Some("后宫"),
            "sciFi" => Some("科幻"),
            "girlsLove" => Some("百合"),
            "horror" => Some("恐怖"),
            "romance" => Some("恋爱"),
            "music" => Some("音乐"),
            "school" => Some("校园"),
            "timeTravel" => Some("穿越"),
            "action" => Some("战斗"),
            "sports" => Some("运动"),
            "martialArts" => Some("武侠"),
            "fantasy" => Some("奇幻"),
            "thriller" => Some("惊悚"),
            "comedy" => Some("搞笑"),
            "sliceOfLife" => Some("日常"),
            "mystery" => Some("悬疑"),
            "adventure" => Some("冒险"),
            "history" => Some("历史"),
            "otome" => Some("乙女"),
            "food" => Some("美食"),
            "workplace" => Some("职场"),
            "xuanhuan" => Some("玄幻"),
            "mecha" => Some("机战"),
            _ => None,
        } {
            tags.insert(tag.to_owned());
        }
    }
    for value in &filters.demographics {
        if let Some(tag) = match value.as_str() {
            "shounen" => Some("少年"),
            "shoujo" => Some("少女"),
            "seinen" => Some("青年"),
            "josei" => Some("女性"),
            "kids" => Some("儿童"),
            _ => None,
        } {
            tags.insert(tag.to_owned());
        }
    }
    for value in &filters.regions {
        if let Some(tag) = match value.as_str() {
            "japan" => Some("日本"),
            "china" => Some("中国"),
            "korea" => Some("韩国"),
            "western" => Some("欧美"),
            _ => None,
        } {
            tags.insert(tag.to_owned());
        }
    }
    tags.into_iter().collect()
}

/// 将单年、未来年份和更早年份转换为 Bangumi air_date 条件。
fn bangumi_browse_air_date(filters: &BangumiBrowseFilters) -> Option<Vec<String>> {
    if let Some(year) = filters.years.first() {
        return Some(vec![
            format!(">={year}-01-01"),
            format!("<{}-01-01", year + 1),
        ]);
    }
    filters.year_range.map(|range| match range {
        BangumiBrowseYearRange::Future { start_year } => {
            vec![format!(">={start_year}-01-01")]
        }
        BangumiBrowseYearRange::Earlier { end_year } => {
            vec![format!("<{end_year}-01-01")]
        }
    })
}

#[derive(Debug, Default, Deserialize)]
struct BangumiPage {
    data: Option<Vec<BangumiSubject>>,
    total: Option<usize>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BangumiSubject {
    id: i64,
    #[serde(rename = "type")]
    subject_type: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    name_cn: String,
    summary: Option<String>,
    date: Option<String>,
    images: Option<BangumiImages>,
    infobox: Option<Vec<BangumiInfoboxItem>>,
    rating: Option<BangumiRating>,
    platform: Option<String>,
    total_episodes: Option<i64>,
    tags: Option<Vec<BangumiTag>>,
}

impl BangumiSubject {
    fn merge(mut self, fallback: Self) -> Self {
        if self.name.is_empty() {
            self.name = fallback.name;
        }
        if self.name_cn.is_empty() {
            self.name_cn = fallback.name_cn;
        }
        self.summary = self.summary.or(fallback.summary);
        self.date = self.date.or(fallback.date);
        self.images = self.images.or(fallback.images);
        self.infobox = self.infobox.or(fallback.infobox);
        self.rating = self.rating.or(fallback.rating);
        self.platform = self.platform.or(fallback.platform);
        self.total_episodes = self.total_episodes.or(fallback.total_episodes);
        self.tags = self.tags.or(fallback.tags);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
struct BangumiImages {
    large: Option<String>,
    common: Option<String>,
    medium: Option<String>,
    grid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BangumiInfoboxItem {
    key: Option<String>,
    value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct BangumiRating {
    rank: Option<i64>,
    score: Option<f64>,
    total: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct BangumiTag {
    name: Option<String>,
    count: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BangumiMapStage {
    Catalog,
    Detail,
}

#[derive(Debug)]
struct IndexedInfoboxValue {
    order: usize,
    value: String,
}

#[derive(Debug, Default)]
struct BangumiInfoboxIndex {
    by_key: HashMap<String, Vec<IndexedInfoboxValue>>,
    all_values: Vec<String>,
}

impl BangumiInfoboxIndex {
    /// 单次展开 infobox，并保留原字段顺序供后续索引查询。
    fn new(items: Option<&[BangumiInfoboxItem]>) -> Self {
        let mut index = Self::default();
        let mut order = 0usize;
        for item in items.unwrap_or_default() {
            for value in collect_json_strings(item.value.as_ref()) {
                index.all_values.push(value.clone());
                if let Some(key) = item.key.as_ref() {
                    index
                        .by_key
                        .entry(key.clone())
                        .or_default()
                        .push(IndexedInfoboxValue { order, value });
                }
                order = order.saturating_add(1);
            }
        }
        index
    }

    /// 按多个字段名返回值，并保持原 infobox 顺序。
    fn values(&self, keys: &[&str]) -> Vec<String> {
        let mut values = keys
            .iter()
            .flat_map(|key| self.by_key.get(*key).into_iter().flatten())
            .collect::<Vec<_>>();
        values.sort_by_key(|value| value.order);
        values
            .into_iter()
            .map(|value| value.value.clone())
            .collect()
    }

    /// 返回多个字段名中原始顺序最靠前的值。
    fn first_value(&self, keys: &[&str]) -> Option<String> {
        keys.iter()
            .flat_map(|key| self.by_key.get(*key).into_iter().flatten())
            .min_by_key(|value| value.order)
            .map(|value| value.value.clone())
    }
}

/// 映射 Bangumi 目录顶层字段，详情派生留到后台第二阶段。
fn map_bangumi_catalog(item: BangumiSubject, fallback_year: i64, fallback_month: i64) -> Anime {
    map_bangumi_for_stage(
        item,
        fallback_year,
        fallback_month,
        BangumiMapStage::Catalog,
    )
}

/// 映射 Bangumi 顶层字段和完整详情字段。
fn map_bangumi(item: BangumiSubject, fallback_year: i64, fallback_month: i64) -> Anime {
    map_bangumi_for_stage(item, fallback_year, fallback_month, BangumiMapStage::Detail)
}

/// 根据同步阶段映射 Bangumi 数据，确保顶层字段计算完全一致。
fn map_bangumi_for_stage(
    mut item: BangumiSubject,
    fallback_year: i64,
    fallback_month: i64,
    stage: BangumiMapStage,
) -> Anime {
    let id = format!("bangumi-{}", item.id);
    let title = non_empty(&item.name_cn)
        .or_else(|| non_empty(&item.name))
        .unwrap_or_else(|| format!("Bangumi {}", item.id));
    let date = item
        .date
        .clone()
        .unwrap_or_else(|| format!("{fallback_year:04}-{fallback_month:02}-01"));
    let (year, month) = parse_date(&date)
        .map(|(year, month, _)| (year, month))
        .unwrap_or((fallback_year, fallback_month));
    let infobox = BangumiInfoboxIndex::new(item.infobox.as_deref());
    let mut external = Map::from_iter([("bangumi".to_owned(), Value::String(item.id.to_string()))]);
    for value in &infobox.all_values {
        if let Some(id) = capture_id(value, &BANGUMI_ANILIST_ID_REGEX) {
            external.insert("anilist".to_owned(), Value::String(id));
        }
        if let Some(id) = capture_id(value, &BANGUMI_MAL_ID_REGEX) {
            external.insert("mal".to_owned(), Value::String(id));
        }
    }
    let mut aliases = Vec::new();
    for (value, language, priority) in [
        (non_empty(&item.name), AnimeAliasLanguage::Ja, 95),
        (non_empty(&item.name_cn), AnimeAliasLanguage::Zh, 90),
    ] {
        if let Some(value) = value.filter(|value| normalize_title(value) != normalize_title(&title))
        {
            push_alias(&mut aliases, &id, value, language, priority);
        }
    }
    for value in infobox.values(&["中文名"]) {
        push_alias(&mut aliases, &id, value, AnimeAliasLanguage::Zh, 88);
    }
    for value in infobox.values(&["别名"]) {
        let is_english = value.is_ascii();
        let language = infer_alias_language(
            &value,
            if is_english {
                AnimeAliasLanguage::En
            } else {
                AnimeAliasLanguage::Custom
            },
        );
        push_alias(
            &mut aliases,
            &id,
            value,
            language,
            if is_english { 78 } else { 82 },
        );
    }
    let detail = (stage == BangumiMapStage::Detail)
        .then(|| build_bangumi_detail(&mut item, &infobox, &date));
    Anime {
        id,
        title,
        original_title: non_empty(&item.name),
        aliases,
        premiere_date: Some(date),
        premiere_year: year,
        premiere_month: month,
        season: Some(season_value(month)),
        summary: item.summary,
        cover_url: item.images.and_then(|images| {
            images
                .large
                .or(images.common)
                .or(images.medium)
                .or(images.grid)
        }),
        rating: item.rating.and_then(|rating| {
            rating
                .score
                .filter(|score| *score > 0.0)
                .map(|score| AnimeRating {
                    score: (score.clamp(0.0, 10.0) * 10.0).round() / 10.0,
                    count: rating.total.filter(|count| *count > 0),
                    source: "bangumi".to_owned(),
                })
        }),
        external_ids: Value::Object(external),
        detail,
    }
}

/// 从已建立的 infobox 索引构建完整 Bangumi 详情。
fn build_bangumi_detail(
    item: &mut BangumiSubject,
    infobox: &BangumiInfoboxIndex,
    premiere_date: &str,
) -> Value {
    let mut genres = item.tags.take().unwrap_or_default();
    genres.sort_by_key(|tag| std::cmp::Reverse(tag.count.unwrap_or_default()));
    let genres = genres
        .into_iter()
        .filter_map(|tag| tag.name)
        .take(8)
        .collect::<Vec<_>>();
    let end_date = infobox
        .first_value(&["放送终了", "播放结束", "上映年度"])
        .as_deref()
        .and_then(normalize_full_date);
    let format = item
        .platform
        .clone()
        .or_else(|| infobox.first_value(&["平台", "类型"]));
    let episode_count = item.total_episodes.filter(|value| *value > 0).or_else(|| {
        infobox
            .first_value(&["话数", "集数"])
            .as_deref()
            .and_then(first_positive_integer)
    });
    let duration_minutes = infobox
        .first_value(&["片长", "单集片长", "时长"])
        .as_deref()
        .and_then(parse_duration_minutes);
    let mut detail = json!({
        "format": map_bangumi_format(format.as_deref()),
        "episodeCount": episode_count,
        "airingStatus": infer_airing_status(premiere_date, end_date.as_deref()),
        "endDate": end_date,
        "broadcast": infobox.first_value(&["放送星期", "播放星期", "放送时间"]).as_deref().and_then(parse_broadcast),
        "genres": genres,
        "studios": infobox.values(&["动画制作", "制作", "製作"]),
        "staff": build_staff(infobox, &["导演", "原作", "系列构成", "脚本", "人物设定", "音乐", "总作画监督"], "bangumi"),
        "sourceMaterial": infobox.values(&["原作", "原案"]).into_iter().next(),
        "durationMinutes": duration_minutes,
        "contentRating": infobox.first_value(&["分级", "等级"]),
        "demographic": infobox.first_value(&["受众", "读者对象"]),
        "countryOfOrigin": infobox.first_value(&["国家/地区", "制片国家/地区", "国家", "地区"]),
        "ranking": item.rating.as_ref().and_then(|rating| rating.rank.filter(|rank| *rank > 0).map(|rank| json!({"rank": rank, "source": "bangumi", "category": "Bangumi 排名"}))),
        "metadataSources": ["bangumi"],
        "refreshedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    });
    remove_nulls(&mut detail);
    detail
}

fn map_bangumi_format(value: Option<&str>) -> Option<&'static str> {
    let value = value?.to_lowercase();
    if value.contains("tv") || value.contains("电视") {
        Some("tv")
    } else if value.contains("剧场") || value.contains("电影") || value.contains("movie") {
        Some("movie")
    } else if value.contains("ova") {
        Some("ova")
    } else if value.contains("web") || value.contains("ona") || value.contains("网络") {
        Some("ona")
    } else if value.contains("music") || value.contains("音乐") {
        Some("music")
    } else if value.contains("special") || value.contains("特别") {
        Some("special")
    } else {
        Some("unknown")
    }
}

/// 将 Bangumi 职员字段映射为统一职员条目。
fn build_staff(index: &BangumiInfoboxIndex, roles: &[&str], source: &str) -> Vec<Value> {
    roles
        .iter()
        .flat_map(|role| {
            index.values(&[*role]).into_iter().map(move |name| {
                json!({
                    "name": name,
                    "role": role,
                    "source": source
                })
            })
        })
        .collect()
}

fn collect_json_strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(value)) => vec![value.trim().to_owned()],
        Some(Value::Array(values)) => values
            .iter()
            .flat_map(|value| collect_json_strings(Some(value)))
            .collect(),
        Some(Value::Object(object)) => ["v", "value"]
            .into_iter()
            .flat_map(|key| collect_json_strings(object.get(key)))
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct AniListResponse {
    data: Option<AniListData>,
    #[serde(default)]
    errors: Vec<AniListError>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AniListData {
    page: Option<AniListPage>,
    media: Option<AniListMedia>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListPage {
    #[serde(default)]
    media: Vec<AniListMedia>,
    page_info: Option<AniListPageInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListPageInfo {
    current_page: Option<i64>,
    has_next_page: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AniListError {
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListMedia {
    id: i64,
    id_mal: Option<i64>,
    average_score: Option<f64>,
    banner_image: Option<String>,
    format: Option<String>,
    episodes: Option<i64>,
    status: Option<String>,
    title: Option<AniListTitle>,
    start_date: Option<AniListDate>,
    end_date: Option<AniListDate>,
    next_airing_episode: Option<AniListNextAiring>,
    season: Option<String>,
    description: Option<String>,
    #[serde(default)]
    synonyms: Vec<String>,
    cover_image: Option<AniListCover>,
    #[serde(default)]
    genres: Vec<String>,
    duration: Option<i64>,
    source: Option<String>,
    country_of_origin: Option<String>,
    is_adult: Option<bool>,
    studios: Option<AniListStudios>,
    staff: Option<AniListStaff>,
    #[serde(default)]
    rankings: Vec<AniListRanking>,
}

#[derive(Debug, Deserialize)]
struct AniListTitle {
    native: Option<String>,
    romaji: Option<String>,
    english: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AniListDate {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListNextAiring {
    airing_at: Option<i64>,
    episode: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListCover {
    large: Option<String>,
    extra_large: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AniListStudios {
    #[serde(default)]
    nodes: Vec<AniListStudio>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListStudio {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AniListStaff {
    #[serde(default)]
    edges: Vec<AniListStaffEdge>,
}

#[derive(Debug, Deserialize)]
struct AniListStaffEdge {
    role: Option<String>,
    node: Option<AniListStaffNode>,
}

#[derive(Debug, Deserialize)]
struct AniListStaffNode {
    name: Option<AniListStaffName>,
}

#[derive(Debug, Deserialize)]
struct AniListStaffName {
    full: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AniListRanking {
    rank: Option<i64>,
    #[serde(rename = "type")]
    ranking_type: Option<String>,
    context: Option<String>,
    all_time: Option<bool>,
}

fn anilist_fields() -> &'static str {
    "id idMal averageScore bannerImage format episodes status title { native romaji english } startDate { year month day } endDate { year month day } nextAiringEpisode { airingAt episode } season description(asHtml: false) synonyms coverImage { large extraLarge } genres duration source countryOfOrigin isAdult studios(isMain: true) { nodes { name isAnimationStudio } } staff(perPage: 12, sort: RELEVANCE) { edges { role node { name { full } } } } rankings { rank type context allTime }"
}

fn map_anilist(item: AniListMedia) -> Anime {
    let title = item
        .title
        .as_ref()
        .and_then(|title| {
            title
                .native
                .clone()
                .or(title.romaji.clone())
                .or(title.english.clone())
        })
        .unwrap_or_else(|| format!("AniList {}", item.id));
    let now = Utc::now();
    let year = item
        .start_date
        .as_ref()
        .and_then(|date| date.year)
        .unwrap_or(i64::from(now.year()));
    let month = item
        .start_date
        .as_ref()
        .and_then(|date| date.month)
        .unwrap_or(1);
    let day = item
        .start_date
        .as_ref()
        .and_then(|date| date.day)
        .unwrap_or(1);
    let id = format!("anilist-{}", item.id);
    let mut aliases = Vec::new();
    if let Some(titles) = item.title.as_ref() {
        if let Some(value) = titles.romaji.as_deref() {
            push_alias(&mut aliases, &id, value, AnimeAliasLanguage::Romaji, 90);
        }
        if let Some(value) = titles.english.as_deref() {
            push_alias(&mut aliases, &id, value, AnimeAliasLanguage::En, 80);
        }
    }
    for value in &item.synonyms {
        let language = infer_alias_language(value, AnimeAliasLanguage::Custom);
        push_alias(&mut aliases, &id, value, language, 70);
    }
    aliases.retain(|alias| normalize_title(&alias.alias) != normalize_title(&title));
    let next_airing_at = item
        .next_airing_episode
        .as_ref()
        .and_then(|next| next.airing_at)
        .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
        .filter(|date| *date > Utc::now())
        .map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true));
    let next_airing_episode_no = next_airing_at.as_ref().and_then(|_| {
        item.next_airing_episode
            .as_ref()
            .and_then(|next| next.episode)
            .filter(|value| *value > 0)
    });
    let ranking = item
        .rankings
        .iter()
        .find(|ranking| {
            ranking.ranking_type.as_deref() == Some("RATED") && ranking.all_time == Some(true)
        })
        .or_else(|| {
            item.rankings
                .iter()
                .find(|ranking| ranking.ranking_type.as_deref() == Some("RATED"))
        });
    let staff = item
        .staff
        .as_ref()
        .into_iter()
        .flat_map(|staff| &staff.edges)
        .filter_map(|credit| {
            Some(json!({
                "name": credit.node.as_ref()?.name.as_ref()?.full.as_deref()?,
                "role": credit.role.as_deref()?,
                "source": "anilist"
            }))
        })
        .collect::<Vec<_>>();
    let mut detail = json!({
        "bannerUrl": item.banner_image,
        "format": map_anilist_format(item.format.as_deref()),
        "episodeCount": item.episodes.filter(|value| *value > 0),
        "airingStatus": map_anilist_status(item.status.as_deref()),
        "endDate": item.end_date.as_ref().and_then(format_anilist_date),
        "nextAiringAt": next_airing_at,
        "nextAiringEpisodeNo": next_airing_episode_no,
        "genres": item.genres,
        "studios": item.studios.as_ref().into_iter().flat_map(|studios| &studios.nodes).filter_map(|studio| studio.name.clone()).collect::<Vec<_>>(),
        "staff": staff,
        "sourceMaterial": item.source,
        "countryOfOrigin": item.country_of_origin,
        "durationMinutes": item.duration.filter(|value| *value > 0),
        "contentRating": item.is_adult.filter(|value| *value).map(|_| "18+"),
        "ranking": ranking.and_then(|ranking| ranking.rank.filter(|value| *value > 0).map(|rank| json!({"rank": rank, "source": "anilist", "category": ranking.context.clone().filter(|value| !value.is_empty()).unwrap_or_else(|| "评分排行".to_owned())}))),
        "metadataSources": ["anilist"],
        "refreshedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    });
    remove_nulls(&mut detail);
    let mut external = Map::from_iter([("anilist".to_owned(), Value::String(item.id.to_string()))]);
    if let Some(mal) = item.id_mal {
        external.insert("mal".to_owned(), Value::String(mal.to_string()));
    }
    Anime {
        id,
        title,
        original_title: item.title.and_then(|title| title.native),
        aliases,
        premiere_date: Some(format!("{year:04}-{month:02}-{day:02}")),
        premiere_year: year,
        premiere_month: month,
        season: Some(
            item.season
                .as_deref()
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| season_value(month)),
        ),
        summary: item.description,
        cover_url: item
            .cover_image
            .and_then(|cover| cover.extra_large.or(cover.large)),
        rating: item
            .average_score
            .filter(|score| *score > 0.0)
            .map(|score| AnimeRating {
                score: ((score.clamp(0.0, 100.0) / 10.0) * 10.0).round() / 10.0,
                count: None,
                source: "anilist".to_owned(),
            }),
        external_ids: Value::Object(external),
        detail: Some(detail),
    }
}

fn map_anilist_format(value: Option<&str>) -> Option<&'static str> {
    match value? {
        "TV" | "TV_SHORT" => Some("tv"),
        "MOVIE" => Some("movie"),
        "OVA" => Some("ova"),
        "ONA" => Some("ona"),
        "SPECIAL" => Some("special"),
        "MUSIC" => Some("music"),
        _ => Some("unknown"),
    }
}

fn map_anilist_status(value: Option<&str>) -> Option<&'static str> {
    match value? {
        "NOT_YET_RELEASED" => Some("upcoming"),
        "RELEASING" => Some("airing"),
        "FINISHED" => Some("finished"),
        "HIATUS" => Some("hiatus"),
        "CANCELLED" => Some("cancelled"),
        _ => Some("unknown"),
    }
}

fn format_anilist_date(value: &AniListDate) -> Option<String> {
    Some(format!(
        "{:04}-{:02}-{:02}",
        value.year?, value.month?, value.day?
    ))
}

#[derive(Debug, Clone)]
struct MikanCandidate {
    id: String,
    title: String,
    detail_url: String,
    cover_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct MikanDetail {
    title: Option<String>,
    original_title: Option<String>,
    summary: Option<String>,
    cover_url: Option<String>,
    premiere_date: Option<String>,
    bangumi_id: Option<String>,
    episode_count: Option<i64>,
    broadcast: Option<Value>,
    genres: Option<Vec<String>>,
    studios: Option<Vec<String>>,
    staff: Option<Vec<Value>>,
    duration_minutes: Option<i64>,
}

fn parse_mikan_candidates(html: &str, base_url: &str) -> Result<Vec<MikanCandidate>, SourceError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href*='/Home/Bangumi/']")
        .map_err(|error| SourceError::Parse(error.to_string()))?;
    let image_selector =
        Selector::parse("img").map_err(|error| SourceError::Parse(error.to_string()))?;
    let base = Url::parse(base_url).map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    let mut candidates = Vec::<MikanCandidate>::new();
    for link in document.select(&selector) {
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Some(id) = MIKAN_CANDIDATE_ID_REGEX
            .captures(href)
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str().to_owned())
        else {
            continue;
        };
        let title = link
            .value()
            .attr("title")
            .or_else(|| {
                link.select(&image_selector)
                    .find_map(|image| image.value().attr("alt"))
            })
            .map(str::trim)
            .filter(|value| value.len() > 1)
            .map(str::to_owned)
            .or_else(|| {
                let text = link.text().collect::<Vec<_>>().join(" ");
                let text = text.trim();
                (text.len() > 1).then(|| text.to_owned())
            });
        let Some(title) = title.filter(|value| !matches!(value.as_str(), "详情" | "订阅" | "更多"))
        else {
            continue;
        };
        let detail_url = base
            .join(href)
            .map_err(|error| SourceError::InvalidUrl(error.to_string()))?
            .to_string();
        let cover_url = link.select(&image_selector).find_map(|image| {
            ["src", "data-src", "data-original"]
                .into_iter()
                .find_map(|attribute| image.value().attr(attribute))
                .and_then(|value| base.join(value).ok())
                .map(|url| url.to_string())
        });
        let candidate = MikanCandidate {
            id: id.clone(),
            title,
            detail_url,
            cover_url,
        };
        if let Some(existing) = candidates.iter_mut().find(|item| item.id == id) {
            if existing.title.len() < candidate.title.len() {
                existing.title.clone_from(&candidate.title);
                existing.detail_url.clone_from(&candidate.detail_url);
            }
            if existing.cover_url.is_none() {
                existing.cover_url = candidate.cover_url;
            }
        } else {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn parse_mikan_detail(html: &str, detail_url: &str) -> MikanDetail {
    let title = [
        read_meta(html, "og:title"),
        read_tag_text(html, "h1"),
        read_tag_text(html, "title").and_then(strip_mikan_title_suffix),
    ]
    .into_iter()
    .flatten()
    .find(|value| !is_ignored_mikan_title(value));
    let summary = read_meta(html, "description")
        .or_else(|| read_meta(html, "og:description"))
        .filter(|value| {
            !value.contains("蜜柑计划") && !value.to_ascii_lowercase().contains("mikan project")
        });
    let cover_url = read_meta(html, "og:image")
        .or_else(|| read_mikan_image_source(html))
        .and_then(|value| {
            Url::parse(detail_url)
                .ok()
                .and_then(|base| base.join(&value).ok())
                .map(|url| url.to_string())
        });
    let genres = read_labeled(html, "类型")
        .or_else(|| read_labeled(html, "标签"))
        .map(|value| split_metadata(&value))
        .filter(|values| !values.is_empty());
    let studios = read_labeled(html, "动画制作")
        .or_else(|| read_labeled(html, "制作"))
        .map(|value| split_metadata(&value))
        .filter(|values| !values.is_empty());
    let staff = build_mikan_staff(html);
    MikanDetail {
        title,
        original_title: read_labeled(html, "原名").or_else(|| read_labeled(html, "日文名")),
        summary,
        cover_url,
        premiere_date: ["放送开始", "开播", "首播"]
            .into_iter()
            .find_map(|label| read_labeled(html, label).and_then(|value| normalize_date(&value)))
            .or_else(|| normalize_date(html)),
        bangumi_id: capture_id(html, &BANGUMI_SUBJECT_URL_REGEX),
        episode_count: read_labeled(html, "话数")
            .or_else(|| read_labeled(html, "集数"))
            .and_then(|value| first_positive_integer(&value)),
        broadcast: read_labeled(html, "放送星期")
            .or_else(|| read_labeled(html, "播放时间"))
            .as_deref()
            .and_then(parse_broadcast),
        genres,
        studios,
        staff,
        duration_minutes: read_labeled(html, "片长")
            .or_else(|| read_labeled(html, "时长"))
            .as_deref()
            .and_then(parse_duration_minutes),
    }
}

fn map_mikan(
    candidate: MikanCandidate,
    detail: MikanDetail,
    fallback_year: i64,
    fallback_month: i64,
) -> Anime {
    let id = format!("mikan-{}", candidate.id);
    let cover_url = detail.cover_url.or(candidate.cover_url.clone());
    let title = detail.title.unwrap_or_else(|| candidate.title.clone());
    let date = detail
        .premiere_date
        .unwrap_or_else(|| format!("{fallback_year:04}-{fallback_month:02}-01"));
    let (year, month) = parse_date(&date)
        .map(|(year, month, _)| (year, month))
        .unwrap_or((fallback_year, fallback_month));
    let mut external = Map::from_iter([("mikan".to_owned(), Value::String(candidate.id.clone()))]);
    if let Some(bangumi) = detail.bangumi_id {
        external.insert("bangumi".to_owned(), Value::String(bangumi));
    }
    let mut aliases = Vec::new();
    if normalize_title(&candidate.title) != normalize_title(&title) {
        push_alias(
            &mut aliases,
            &id,
            &candidate.title,
            infer_alias_language(&candidate.title, AnimeAliasLanguage::Zh),
            90,
        );
    }
    if let Some(original) = detail.original_title.as_deref() {
        if normalize_title(original) != normalize_title(&title) {
            push_alias(
                &mut aliases,
                &id,
                original,
                infer_alias_language(original, AnimeAliasLanguage::Ja),
                85,
            );
        }
    }
    let mut metadata = json!({
        "episodeCount": detail.episode_count,
        "broadcast": detail.broadcast,
        "genres": detail.genres,
        "studios": detail.studios,
        "staff": detail.staff,
        "durationMinutes": detail.duration_minutes,
        "metadataSources": ["mikan"],
        "refreshedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    });
    remove_nulls(&mut metadata);
    Anime {
        id,
        title,
        original_title: detail.original_title,
        aliases,
        premiere_date: Some(date),
        premiere_year: year,
        premiere_month: month,
        season: Some(season_value(month)),
        summary: detail.summary,
        cover_url,
        rating: None,
        external_ids: Value::Object(external),
        detail: Some(metadata),
    }
}

fn push_alias(
    aliases: &mut Vec<AnimeAlias>,
    anime_id: &str,
    value: impl AsRef<str>,
    language: AnimeAliasLanguage,
    priority: i64,
) {
    let value = value.as_ref().trim();
    if value.is_empty()
        || aliases
            .iter()
            .any(|alias| normalize_title(&alias.alias) == normalize_title(value))
    {
        return;
    }
    aliases.push(AnimeAlias {
        id: format!("{anime_id}-alias-{}", aliases.len() + 1),
        anime_id: anime_id.to_owned(),
        alias: value.to_owned(),
        language,
        priority,
    });
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn capture_id(value: &str, regex: &Regex) -> Option<String> {
    regex
        .captures(value)?
        .get(1)
        .map(|value| value.as_str().to_owned())
}

fn read_meta(html: &str, name: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("meta").ok()?;
    document.select(&selector).find_map(|element| {
        let value = element
            .value()
            .attr("name")
            .or_else(|| element.value().attr("property"));
        (value == Some(name))
            .then(|| element.value().attr("content"))
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn read_tag_text(html: &str, tag: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse(tag).ok()?;
    let value = document
        .select(&selector)
        .next()?
        .text()
        .collect::<Vec<_>>()
        .join(" ");
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// 去除 Mikan 标题标签追加的站点名称。
fn strip_mikan_title_suffix(value: String) -> Option<String> {
    let normalized = MIKAN_TITLE_SUFFIX_REGEX
        .replace(&value, "")
        .trim()
        .to_owned();
    (!normalized.is_empty()).then_some(normalized)
}

/// 排除详情页中不能作为番剧名称的导航或站点文本。
fn is_ignored_mikan_title(value: &str) -> bool {
    matches!(value.trim(), "详情" | "订阅" | "更多" | "Mikan Project")
}

fn read_labeled(html: &str, label: &str) -> Option<String> {
    let regex = {
        let mut cache = LABELED_VALUE_REGEX_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache
            .entry(label.to_owned())
            .or_insert_with(|| {
                let pattern = format!(r"{}\s*[：:]\s*([^<\n\r]+)", regex::escape(label));
                Regex::new(&pattern).expect("Mikan 固定标签正则必须有效")
            })
            .clone()
    };
    regex
        .captures(html)?
        .get(1)
        .map(|value| normalize_html_text(value.as_str()))
        .filter(|value| !value.is_empty())
}

fn normalize_date(value: &str) -> Option<String> {
    if let Some(capture) = YEAR_FIRST_DATE_REGEX.captures(value) {
        return Some(format!(
            "{}-{:02}-{:02}",
            capture.get(1)?.as_str(),
            capture.get(2)?.as_str().parse::<i64>().ok()?,
            capture
                .get(3)
                .and_then(|value| value.as_str().parse::<i64>().ok())
                .unwrap_or(1)
        ));
    }
    let capture = MONTH_FIRST_DATE_REGEX.captures(value)?;
    Some(format!(
        "{}-{:02}-{:02}",
        capture.get(3)?.as_str(),
        capture.get(1)?.as_str().parse::<i64>().ok()?,
        capture.get(2)?.as_str().parse::<i64>().ok()?
    ))
}

/// 仅接受包含年月日的日期，避免把不完整完结时间误判为已完结。
fn normalize_full_date(value: &str) -> Option<String> {
    let capture = FULL_DATE_REGEX.captures(value)?;
    Some(format!(
        "{}-{:02}-{:02}",
        capture.get(1)?.as_str(),
        capture.get(2)?.as_str().parse::<i64>().ok()?,
        capture.get(3)?.as_str().parse::<i64>().ok()?
    ))
}

/// 根据首播和完结日期推导当前放送状态。
fn infer_airing_status(premiere_date: &str, end_date: Option<&str>) -> Option<&'static str> {
    let today = Utc::now().date_naive();
    let premiere = NaiveDate::parse_from_str(premiere_date, "%Y-%m-%d").ok();
    let end = end_date.and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
    if premiere.is_some_and(|value| value > today) {
        Some("upcoming")
    } else if end.is_some_and(|value| value < today) {
        Some("finished")
    } else if premiere.is_some_and(|value| value <= today) {
        Some("airing")
    } else {
        None
    }
}

/// 从中文星期与时间文本解析统一放送计划。
fn parse_broadcast(value: &str) -> Option<Value> {
    let weekday = WEEKDAY_REGEX
        .captures(value)
        .and_then(|capture| capture.get(1))
        .and_then(|value| match value.as_str() {
            "日" | "天" => Some(0),
            "一" => Some(1),
            "二" => Some(2),
            "三" => Some(3),
            "四" => Some(4),
            "五" => Some(5),
            "六" => Some(6),
            _ => None,
        });
    let time = CLOCK_TIME_REGEX
        .find(value)
        .map(|value| value.as_str().to_owned());
    if weekday.is_none() && time.is_none() {
        return None;
    }
    let mut broadcast = json!({
        "weekday": weekday,
        "time": time,
        "timezone": "Asia/Tokyo"
    });
    remove_nulls(&mut broadcast);
    Some(broadcast)
}

/// 将小时与分钟混合文本换算为正整数分钟。
fn parse_duration_minutes(value: &str) -> Option<i64> {
    let hours = DURATION_HOURS_REGEX
        .captures(value)
        .and_then(|capture| capture.get(1))
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .unwrap_or_default();
    let minutes = DURATION_MINUTES_REGEX
        .captures(value)
        .and_then(|capture| capture.get(1))
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .unwrap_or_default();
    let total = (hours * 60.0 + minutes).round() as i64;
    (total > 0)
        .then_some(total)
        .or_else(|| first_positive_integer(value))
}

/// 从 Mikan 详情页的固定职员标签构建统一职员条目。
fn build_mikan_staff(html: &str) -> Option<Vec<Value>> {
    let roles = ["导演", "原作", "系列构成", "脚本", "人物设定", "音乐"];
    let staff = roles
        .into_iter()
        .flat_map(|role| {
            read_labeled(html, role)
                .map(|value| split_metadata(&value))
                .unwrap_or_default()
                .into_iter()
                .map(move |name| json!({"name": name, "role": role, "source": "mikan"}))
        })
        .collect::<Vec<_>>();
    (!staff.is_empty()).then_some(staff)
}

/// 在缺少 OpenGraph 封面时查找 Mikan 页面内的番剧封面图。
fn read_mikan_image_source(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("img").ok()?;
    document.select(&selector).find_map(|image| {
        let source = image.value().attr("src")?;
        let descriptor = [
            image.value().attr("class").unwrap_or_default(),
            image.value().attr("alt").unwrap_or_default(),
            source,
        ]
        .join(" ")
        .to_ascii_lowercase();
        (descriptor.contains("bangumi") || descriptor.contains("cover")).then(|| source.to_owned())
    })
}

/// 解码 HTML 实体并压缩标签字段中的空白。
fn normalize_html_text(value: &str) -> String {
    Html::parse_fragment(value)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_positive_integer(value: &str) -> Option<i64> {
    POSITIVE_INTEGER_REGEX
        .find(value)?
        .as_str()
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
}

fn split_metadata(value: &str) -> Vec<String> {
    value
        .split(['、', ',', '，', '/', '|'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn remove_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !value.is_null());
            for value in object.values_mut() {
                remove_nulls(value);
            }
        }
        Value::Array(values) => {
            values.retain(|value| !value.is_null());
            for value in values {
                remove_nulls(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};

    use ani_domain::RequestCircuitState;
    use ani_domain::{
        Anime, AnimeAlias, AnimeAliasLanguage, BangumiBrowseFilters, BangumiBrowseQuery,
        BangumiBrowseSort, BangumiBrowseYearRange,
    };
    use ani_repository::{RepositoryError, RepositoryResult};
    use chrono::Utc;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::{
        CircuitStateStore, NativeHttpConfig, NetworkRequestChannel, ProxyMode, SourceError,
        SourceNetworkService,
    };

    use super::{
        anilist_fields, detail_providers_for_item, detail_retry_after_ms, find, map_bangumi,
        map_bangumi_catalog, merge_anime, merge_anime_metadata_batches, parse_mikan_candidates,
        parse_mikan_detail, preserve_catalog_cover, shared_title, should_merge, union,
        unique_by_normalized_title, AnimeMetadataBatch, AnimeMetadataDetailProvider,
        AnimeMetadataDetailRequest, AnimeMetadataService, BangumiImages, BangumiInfoboxItem,
        BangumiRating, BangumiSubject, BangumiTag, MetadataEndpoints,
    };

    #[derive(Default)]
    struct MemoryCircuitStore {
        states: Mutex<HashMap<String, RequestCircuitState>>,
    }

    impl CircuitStateStore for MemoryCircuitStore {
        fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
            self.states
                .lock()
                .map(|states| states.get(key).cloned())
                .map_err(|error| RepositoryError::BackendUnavailable {
                    backend: "memory".to_owned(),
                    message: error.to_string(),
                })
        }

        fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
            self.states
                .lock()
                .map(|mut states| {
                    states.insert(state.key.clone(), state.clone());
                })
                .map_err(|error| RepositoryError::BackendUnavailable {
                    backend: "memory".to_owned(),
                    message: error.to_string(),
                })
        }
    }

    fn anime(id: &str, title: &str, external_ids: serde_json::Value) -> Anime {
        Anime {
            id: id.to_owned(),
            title: title.to_owned(),
            original_title: None,
            aliases: vec![AnimeAlias {
                id: format!("{id}-alias"),
                anime_id: id.to_owned(),
                alias: "Shared Title".to_owned(),
                language: AnimeAliasLanguage::En,
                priority: 80,
            }],
            premiere_date: Some("2026-07-03".to_owned()),
            premiere_year: 2026,
            premiere_month: 7,
            season: Some("summer".to_owned()),
            summary: None,
            cover_url: None,
            rating: None,
            external_ids,
            detail: None,
        }
    }

    /// 使用改造前的全量两两比较生成基准合并结果。
    fn quadratic_merge_reference(batches: &[AnimeMetadataBatch]) -> Vec<Anime> {
        let candidates = batches
            .iter()
            .flat_map(|batch| batch.items.iter().cloned())
            .collect::<Vec<_>>();
        let mut parents = (0..candidates.len()).collect::<Vec<_>>();
        for left in 0..candidates.len() {
            for right in left + 1..candidates.len() {
                if should_merge(&candidates[left], &candidates[right]) {
                    union(&mut parents, left, right);
                }
            }
        }
        let mut groups = Vec::<(usize, Vec<Anime>)>::new();
        for (index, item) in candidates.into_iter().enumerate() {
            let root = find(&mut parents, index);
            if let Some((_, items)) = groups.iter_mut().find(|(candidate, _)| *candidate == root) {
                items.push(item);
            } else {
                groups.push((root, vec![item]));
            }
        }
        groups
            .into_iter()
            .map(|(_, mut items)| {
                let first = items.remove(0);
                items.into_iter().fold(first, merge_anime)
            })
            .collect()
    }

    /// 使用改造前的线性标题扫描生成单来源去重基准。
    fn quadratic_unique_reference(items: Vec<Anime>) -> Vec<Anime> {
        let mut unique = Vec::<Anime>::new();
        for item in items {
            if let Some(index) = unique
                .iter()
                .position(|existing| shared_title(existing, &item))
            {
                let existing = unique.remove(index);
                unique.insert(index, merge_anime(existing, item));
            } else {
                unique.push(item);
            }
        }
        unique
    }

    /// 验证跨来源 external id 会合并，并保留本地主记录标识。
    #[test]
    fn merges_metadata_by_external_id() {
        let result = merge_anime_metadata_batches(&[
            AnimeMetadataBatch {
                source: "local".to_owned(),
                items: vec![anime("local-1", "本地标题", json!({"mal": "42"}))],
            },
            AnimeMetadataBatch {
                source: "anilist".to_owned(),
                items: vec![anime(
                    "anilist-1",
                    "日本語",
                    json!({"anilist": "1", "mal": "42"}),
                )],
            },
        ]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "local-1");
        assert_eq!(result[0].external_ids["anilist"], "1");
    }

    /// 验证索引分桶与原全量比较在冲突、标题和外部标识场景下结果一致。
    #[test]
    fn indexed_merge_matches_quadratic_reference() {
        let mut bangumi = anime("bangumi-1", "作品甲", json!({"bangumi": "1"}));
        bangumi.aliases[0].alias = "Work Alpha".to_owned();
        let mut anilist = anime(
            "anilist-1",
            "Work Alpha",
            json!({"bangumi": "1", "anilist": "10"}),
        );
        anilist.aliases[0].alias = "Alpha Alias".to_owned();
        let mut title_only = anime("mikan-1", "作品甲", json!({"mikan": "100"}));
        title_only.aliases[0].alias = "Alpha Mikan".to_owned();
        let mut conflicting = anime("bangumi-2", "作品甲", json!({"bangumi": "2"}));
        conflicting.aliases[0].alias = "Conflicting Alpha".to_owned();
        let mut isolated = anime("anilist-2", "作品乙", json!({"anilist": "20"}));
        isolated.aliases[0].alias = "Work Beta".to_owned();
        let batches = vec![
            AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![bangumi, conflicting],
            },
            AnimeMetadataBatch {
                source: "anilist".to_owned(),
                items: vec![anilist, isolated],
            },
            AnimeMetadataBatch {
                source: "mikan".to_owned(),
                items: vec![title_only],
            },
        ];

        assert_eq!(
            merge_anime_metadata_batches(&batches),
            quadratic_merge_reference(&batches)
        );
    }

    /// 验证单来源标题索引在连续别名合并时与旧扫描算法一致。
    #[test]
    fn indexed_title_deduplication_matches_quadratic_reference() {
        let mut first = anime("source-1", "Work Alpha", json!({"source": "1"}));
        first.aliases[0].alias = "Alpha Alias".to_owned();
        let mut second = anime("source-2", "作品甲", json!({"source": "2"}));
        second.aliases[0].alias = "Work Alpha".to_owned();
        let mut third = anime("source-3", "Alpha Alias", json!({"source": "3"}));
        third.aliases[0].alias = "第三别名".to_owned();
        let mut isolated = anime("source-4", "作品乙", json!({"source": "4"}));
        isolated.aliases[0].alias = "Work Beta".to_owned();
        let items = vec![first, second, isolated, third];

        assert_eq!(
            unique_by_normalized_title(items.clone()),
            quadratic_unique_reference(items)
        );
    }

    /// 验证 Bangumi 已覆盖时跳过 Mikan，Mikan 独有条目仍会补全详情。
    #[test]
    fn selects_only_required_detail_providers() {
        let merged = anime(
            "bangumi-1",
            "作品甲",
            json!({"bangumi": "1", "anilist": "10", "mikan": "100"}),
        );
        assert_eq!(
            detail_providers_for_item(&merged),
            vec![AnimeMetadataDetailProvider::Bangumi]
        );

        let mikan_only = anime("mikan-100", "作品乙", json!({"mikan": "100"}));
        assert_eq!(
            detail_providers_for_item(&mikan_only),
            vec![AnimeMetadataDetailProvider::Mikan]
        );

        let anilist_only = anime("anilist-10", "作品丙", json!({"anilist": "10"}));
        assert!(detail_providers_for_item(&anilist_only).is_empty());
    }

    /// 验证详情来源不能覆盖季度目录封面，目录无图时允许详情补齐。
    #[test]
    fn preserves_catalog_cover_during_detail_enrichment() {
        let mut catalog = anime("local-1", "本地标题", json!({"bangumi": "1"}));
        catalog.cover_url = Some("https://catalog.example/cover.jpg".to_owned());
        let mut enriched = anime("bangumi-1", "详情标题", json!({"bangumi": "1"}));
        enriched.cover_url = Some("https://detail.example/cover.jpg".to_owned());

        preserve_catalog_cover(&catalog, &mut enriched);
        assert_eq!(
            enriched.cover_url.as_deref(),
            Some("https://catalog.example/cover.jpg")
        );

        catalog.cover_url = None;
        enriched.cover_url = Some("https://detail.example/cover.jpg".to_owned());
        preserve_catalog_cover(&catalog, &mut enriched);
        assert_eq!(
            enriched.cover_url.as_deref(),
            Some("https://detail.example/cover.jpg")
        );
    }

    /// 验证仅瞬时网络与熔断错误进入详情补偿队列。
    #[test]
    fn classifies_retryable_detail_failures() {
        assert!(detail_retry_after_ms(&SourceError::HttpStatus {
            status: 503,
            detail: None,
        })
        .is_some());
        assert!(detail_retry_after_ms(&SourceError::HttpStatus {
            status: 429,
            detail: None,
        })
        .is_some());
        assert!(detail_retry_after_ms(&SourceError::CircuitOpen {
            backoff_until: (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
        })
        .is_some());
        assert!(detail_retry_after_ms(&SourceError::HttpStatus {
            status: 404,
            detail: None,
        })
        .is_none());
        assert!(detail_retry_after_ms(&SourceError::Parse("invalid payload".to_owned())).is_none());
    }

    /// 验证 AniList 公共字段选择集的大括号完整闭合。
    #[test]
    fn keeps_anilist_field_selection_balanced() {
        let fields = anilist_fields();

        assert_eq!(
            fields.chars().filter(|character| *character == '{').count(),
            fields.chars().filter(|character| *character == '}').count()
        );
        assert!(fields.contains("staff(perPage: 12, sort: RELEVANCE)"));
        assert!(fields.contains("} } rankings"));
    }

    /// 验证 Mikan 季度页解析去重并优先保留更完整标题。
    #[test]
    fn parses_mikan_candidates() {
        let items = parse_mikan_candidates(
            r#"<a href="/Home/Bangumi/123" title="短标题"><img data-src="/images/123.jpg"></a><a href="/Home/Bangumi/123" title="更完整的标题"></a>"#,
            "https://mikanani.me/",
        )
        .expect("parse mikan");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "更完整的标题");
        assert_eq!(
            items[0].cover_url.as_deref(),
            Some("https://mikanani.me/images/123.jpg")
        );
    }

    /// 验证 Bangumi 详情字段与桌面旧实现保持一致。
    #[test]
    fn maps_bangumi_extended_detail_fields() {
        let item = BangumiSubject {
            id: 100,
            subject_type: 2,
            name: "Test Anime".to_owned(),
            name_cn: "测试番".to_owned(),
            date: Some("2000-01-01".to_owned()),
            infobox: Some(vec![
                BangumiInfoboxItem {
                    key: Some("放送终了".to_owned()),
                    value: Some(json!("2000年3月20日")),
                },
                BangumiInfoboxItem {
                    key: Some("放送星期".to_owned()),
                    value: Some(json!("星期三 23:30")),
                },
                BangumiInfoboxItem {
                    key: Some("导演".to_owned()),
                    value: Some(json!([{"v": "测试导演"}])),
                },
                BangumiInfoboxItem {
                    key: Some("片长".to_owned()),
                    value: Some(json!("1小时30分钟")),
                },
                BangumiInfoboxItem {
                    key: Some("分级".to_owned()),
                    value: Some(json!("PG-13")),
                },
                BangumiInfoboxItem {
                    key: Some("受众".to_owned()),
                    value: Some(json!("少年")),
                },
                BangumiInfoboxItem {
                    key: Some("国家/地区".to_owned()),
                    value: Some(json!("日本")),
                },
            ]),
            rating: Some(BangumiRating {
                rank: Some(42),
                score: Some(8.2),
                total: Some(1234),
            }),
            ..BangumiSubject::default()
        };

        let anime = map_bangumi(item, 2000, 1);
        let detail = anime.detail.expect("bangumi detail");
        assert_eq!(detail["endDate"], "2000-03-20");
        assert_eq!(detail["airingStatus"], "finished");
        assert_eq!(detail["broadcast"]["weekday"], 3);
        assert_eq!(detail["broadcast"]["time"], "23:30");
        assert_eq!(detail["staff"][0]["name"], "测试导演");
        assert_eq!(detail["durationMinutes"], 90);
        assert_eq!(detail["contentRating"], "PG-13");
        assert_eq!(detail["demographic"], "少年");
        assert_eq!(detail["countryOfOrigin"], "日本");
        assert_eq!(detail["ranking"]["rank"], 42);
        assert_eq!(detail["ranking"]["source"], "bangumi");
    }

    /// 验证基础目录延后详情时，全部顶层业务字段保持一致。
    #[test]
    fn defers_bangumi_detail_without_changing_catalog_fields() {
        let subject = BangumiSubject {
            id: 101,
            subject_type: 2,
            name: "Test Anime".to_owned(),
            name_cn: "测试番".to_owned(),
            summary: Some("测试简介".to_owned()),
            date: Some("2026-07-03".to_owned()),
            images: Some(BangumiImages {
                large: Some("https://lain.bgm.tv/pic/cover/l/test.jpg".to_owned()),
                common: None,
                medium: None,
                grid: None,
            }),
            infobox: Some(vec![
                BangumiInfoboxItem {
                    key: Some("别名".to_owned()),
                    value: Some(json!(["Test Alias", "https://anilist.co/anime/202"])),
                },
                BangumiInfoboxItem {
                    key: Some("关联".to_owned()),
                    value: Some(json!("https://myanimelist.net/anime/303")),
                },
                BangumiInfoboxItem {
                    key: Some("话数".to_owned()),
                    value: Some(json!("12")),
                },
            ]),
            rating: Some(BangumiRating {
                rank: Some(42),
                score: Some(8.2),
                total: Some(1234),
            }),
            platform: Some("TV".to_owned()),
            total_episodes: Some(12),
            tags: Some(vec![BangumiTag {
                name: Some("动画".to_owned()),
                count: Some(100),
            }]),
        };

        let catalog = map_bangumi_catalog(subject.clone(), 2026, 7);
        let mut detailed = map_bangumi(subject, 2026, 7);
        assert!(catalog.detail.is_none());
        assert!(detailed.detail.is_some());
        detailed.detail = None;
        assert_eq!(catalog, detailed);
        assert_eq!(catalog.external_ids["anilist"], "202");
        assert_eq!(catalog.external_ids["mal"], "303");
    }

    /// 验证分阶段合并后的最终字段优先级与原流程一致。
    #[test]
    fn preserves_final_detail_precedence_after_deferring_bangumi_detail() {
        let catalog_subject = BangumiSubject {
            id: 101,
            subject_type: 2,
            name: "Test Anime".to_owned(),
            name_cn: "测试番".to_owned(),
            date: Some("2026-07-03".to_owned()),
            total_episodes: Some(12),
            ..BangumiSubject::default()
        };
        let api_subject = BangumiSubject {
            id: 101,
            subject_type: 2,
            name: "Test Anime".to_owned(),
            name_cn: "测试番".to_owned(),
            date: Some("2026-07-03".to_owned()),
            total_episodes: Some(24),
            ..BangumiSubject::default()
        };
        let mut anilist = anime(
            "anilist-202",
            "Test Anime",
            json!({"bangumi": "101", "anilist": "202"}),
        );
        anilist.detail = Some(json!({
            "episodeCount": 13,
            "durationMinutes": 30,
            "metadataSources": ["anilist"]
        }));

        let old_catalog = merge_anime_metadata_batches(&[
            AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![map_bangumi(catalog_subject.clone(), 2026, 7)],
            },
            AnimeMetadataBatch {
                source: "anilist".to_owned(),
                items: vec![anilist.clone()],
            },
        ])
        .remove(0);
        let mut old_final = merge_anime_metadata_batches(&[
            AnimeMetadataBatch {
                source: "local".to_owned(),
                items: vec![old_catalog],
            },
            AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![map_bangumi(api_subject.clone(), 2026, 7)],
            },
        ])
        .remove(0);

        let deferred_catalog = merge_anime_metadata_batches(&[
            AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![map_bangumi_catalog(catalog_subject.clone(), 2026, 7)],
            },
            AnimeMetadataBatch {
                source: "anilist".to_owned(),
                items: vec![anilist],
            },
        ])
        .remove(0);
        let mut deferred_final = merge_anime_metadata_batches(&[
            AnimeMetadataBatch {
                source: "bangumi-cache".to_owned(),
                items: vec![map_bangumi(catalog_subject, 2026, 7)],
            },
            AnimeMetadataBatch {
                source: "local".to_owned(),
                items: vec![deferred_catalog],
            },
            AnimeMetadataBatch {
                source: "bangumi".to_owned(),
                items: vec![map_bangumi(api_subject, 2026, 7)],
            },
        ])
        .remove(0);

        if let Some(detail) = old_final.detail.as_mut().and_then(Value::as_object_mut) {
            detail.remove("refreshedAt");
        }
        if let Some(detail) = deferred_final
            .detail
            .as_mut()
            .and_then(Value::as_object_mut)
        {
            detail.remove("refreshedAt");
        }
        assert_eq!(deferred_final, old_final);
        assert_eq!(deferred_final.detail.as_ref().unwrap()["episodeCount"], 12);
        assert_eq!(
            deferred_final.detail.as_ref().unwrap()["durationMinutes"],
            30
        );
    }

    /// 验证 Mikan HTML 可提取放送、职员和复合时长。
    #[test]
    fn parses_mikan_extended_detail_fields() {
        let detail = parse_mikan_detail(
            r#"
            <html><head><meta property="og:title" content="测试番"></head><body>
            <div>放送星期：星期五 22:15</div>
            <div>导演：甲、乙</div>
            <div>片长：1小时24分钟</div>
            </body></html>
            "#,
            "https://mikanani.me/Home/Bangumi/100",
        );

        assert_eq!(detail.broadcast.as_ref().expect("broadcast")["weekday"], 5);
        assert_eq!(
            detail.broadcast.as_ref().expect("broadcast")["time"],
            "22:15"
        );
        assert_eq!(detail.staff.as_ref().expect("staff").len(), 2);
        assert_eq!(detail.duration_minutes, Some(84));
    }

    /// 验证来源级补偿只请求声明的 Mikan，不会重复访问已有 Bangumi 来源。
    #[tokio::test]
    async fn retries_only_requested_detail_provider() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept mock");
            let mut buffer = vec![0u8; 8 * 1024];
            let read = stream.read(&mut buffer).await.expect("read request");
            let request = String::from_utf8_lossy(&buffer[..read]);
            assert!(request
                .lines()
                .next()
                .unwrap_or_default()
                .contains("/Home/Bangumi/100"));
            let body = r#"<html><head><meta property="og:title" content="测试番"></head><body>话数：12</body></html>"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        let base = format!("http://{address}/");
        let service = AnimeMetadataService {
            network: Arc::new(
                SourceNetworkService::new(NativeHttpConfig {
                    proxy_mode: ProxyMode::Off,
                    proxy_url: None,
                    timeout_ms: 5_000,
                    max_response_bytes: 1024 * 1024,
                    user_agent: "AniTracker-Test".to_owned(),
                })
                .expect("network service"),
            ),
            endpoints: MetadataEndpoints {
                bangumi: base.clone(),
                anilist: format!("{base}graphql"),
                mikan: base,
            },
            channel: NetworkRequestChannel::Interactive,
            bangumi_catalog_cache: Mutex::new(HashMap::new()),
        };
        let local = anime(
            "bangumi-1",
            "测试番",
            json!({"bangumi": "1", "mikan": "100"}),
        );
        let result = service
            .retry_details(
                &MemoryCircuitStore::default(),
                &[AnimeMetadataDetailRequest {
                    item: local,
                    providers: vec![AnimeMetadataDetailProvider::Mikan],
                    retry_after_ms: 0,
                }],
            )
            .await;
        server.await.expect("mock server");

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.settled_error_count, 0);
        assert!(result.retryable_requests.is_empty());
        assert_eq!(result.items[0].detail.as_ref().unwrap()["episodeCount"], 12);
    }

    /// 验证 Bangumi 浏览只访问 Bangumi，并保留在线分页总数。
    #[tokio::test]
    async fn browses_bangumi_online_with_filters() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..1 {
                let (mut stream, _) = listener.accept().await.expect("accept mock");
                let mut buffer = vec![0u8; 16 * 1024];
                let read = stream.read(&mut buffer).await.expect("read request");
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = json!({
                    "data": [{
                        "id": 101,
                        "type": 2,
                        "name": "Test Anime",
                        "name_cn": "测试番",
                        "date": "2026-07-03",
                        "rating": {"score": 8.6, "total": 5000, "rank": 12},
                        "tags": [{"name": "奇幻", "count": 100}]
                    }],
                    "total": 42,
                    "limit": 20,
                    "offset": 20
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
                requests.push(request);
            }
            requests
        });
        let base = format!("http://{address}/");
        let service = AnimeMetadataService {
            network: Arc::new(
                SourceNetworkService::new(NativeHttpConfig {
                    proxy_mode: ProxyMode::Off,
                    proxy_url: None,
                    timeout_ms: 5_000,
                    max_response_bytes: 1024 * 1024,
                    user_agent: "AniTracker-Test".to_owned(),
                })
                .expect("network service"),
            ),
            endpoints: MetadataEndpoints {
                bangumi: base.clone(),
                anilist: format!("{base}graphql"),
                mikan: base,
            },
            channel: NetworkRequestChannel::Interactive,
            bangumi_catalog_cache: Mutex::new(HashMap::new()),
        };
        let result = service
            .browse_bangumi(
                &MemoryCircuitStore::default(),
                BangumiBrowseQuery {
                    keyword: "测试".to_owned(),
                    sort: BangumiBrowseSort::Rating,
                    filters: BangumiBrowseFilters {
                        genres: vec!["fantasy".to_owned()],
                        years: vec![2026],
                        min_rating: 8.0,
                        ..BangumiBrowseFilters::default()
                    },
                    page: 2,
                    page_size: 20,
                },
            )
            .await
            .expect("browse Bangumi");
        let requests = server.await.expect("mock server");

        assert_eq!(result.source, "bangumi");
        assert_eq!(result.total, 42);
        assert!(result.has_more);
        assert_eq!(result.items[0].external_ids["bangumi"], "101");
        let browse_request = requests
            .iter()
            .find(|request| request.contains("/v0/search/subjects"))
            .expect("Bangumi browse request");
        assert!(browse_request.contains("\"sort\":\"score\""));
        assert!(browse_request.contains("\"tag\":[\"奇幻\"]"));
        assert!(browse_request.contains("\"air_date\":[\">=2026-01-01\",\"<2027-01-01\"]"));
    }

    /// 验证截图中的完整题材均映射为独立 Bangumi 标签。
    #[test]
    fn maps_complete_bangumi_browse_genres() {
        let genres = vec![
            "reasoning",
            "harem",
            "sciFi",
            "girlsLove",
            "horror",
            "romance",
            "music",
            "school",
            "timeTravel",
            "action",
            "sports",
            "martialArts",
            "fantasy",
            "thriller",
            "comedy",
            "sliceOfLife",
            "mystery",
            "adventure",
            "history",
            "otome",
            "food",
            "workplace",
            "xuanhuan",
            "mecha",
        ];
        let filters = BangumiBrowseFilters {
            genres: genres.into_iter().map(str::to_owned).collect(),
            ..BangumiBrowseFilters::default()
        };
        let actual = super::bangumi_browse_tags(&filters)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let expected = [
            "推理", "后宫", "科幻", "百合", "恐怖", "恋爱", "音乐", "校园", "穿越", "战斗", "运动",
            "武侠", "奇幻", "惊悚", "搞笑", "日常", "悬疑", "冒险", "历史", "乙女", "美食", "职场",
            "玄幻", "机战",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    /// 验证未来、单年和更早年份使用互不重叠的日期边界。
    #[test]
    fn maps_bangumi_browse_year_ranges() {
        let exact = BangumiBrowseFilters {
            years: vec![2026],
            ..BangumiBrowseFilters::default()
        };
        let future = BangumiBrowseFilters {
            year_range: Some(BangumiBrowseYearRange::Future { start_year: 2027 }),
            ..BangumiBrowseFilters::default()
        };
        let earlier = BangumiBrowseFilters {
            year_range: Some(BangumiBrowseYearRange::Earlier { end_year: 2017 }),
            ..BangumiBrowseFilters::default()
        };

        assert_eq!(
            super::bangumi_browse_air_date(&exact),
            Some(vec![">=2026-01-01".to_owned(), "<2027-01-01".to_owned()])
        );
        assert_eq!(
            super::bangumi_browse_air_date(&future),
            Some(vec![">=2027-01-01".to_owned()])
        );
        assert_eq!(
            super::bangumi_browse_air_date(&earlier),
            Some(vec!["<2017-01-01".to_owned()])
        );
    }

    /// 验证在线搜索聚合 Bangumi/AniList，并隔离 Mikan 单来源失败。
    #[tokio::test]
    async fn searches_metadata_and_preserves_partial_results() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.expect("accept mock");
                let mut buffer = vec![0u8; 16 * 1024];
                let read = stream.read(&mut buffer).await.expect("read request");
                let request = String::from_utf8_lossy(&buffer[..read]);
                let request_line = request.lines().next().unwrap_or_default();
                let (status, body) = if request_line.contains("/v0/search/subjects") {
                    (
                        "200 OK",
                        json!({
                            "data": [{"id": 101, "type": 2, "name": "Test Anime", "name_cn": "测试番", "date": "2026-07-03"}],
                            "total": 1,
                            "limit": 30,
                            "offset": 0
                        })
                        .to_string(),
                    )
                } else if request_line.contains("/v0/subjects/101") {
                    (
                        "200 OK",
                        json!({
                            "id": 101,
                            "type": 2,
                            "name": "Test Anime",
                            "name_cn": "测试番",
                            "date": "2026-07-03",
                            "infobox": [{"key": "别名", "value": [{"v": "https://myanimelist.net/anime/303"}]}]
                        })
                        .to_string(),
                    )
                } else if request_line.contains("/graphql") {
                    (
                        "200 OK",
                        json!({
                            "data": {"Page": {"media": [{
                                "id": 202,
                                "idMal": 303,
                                "title": {"native": "テストアニメ", "romaji": "Test Anime", "english": "Test Anime"},
                                "startDate": {"year": 2026, "month": 7, "day": 3},
                                "season": "SUMMER",
                                "synonyms": [],
                                "genres": [],
                                "rankings": []
                            }]}}
                        })
                        .to_string(),
                    )
                } else {
                    ("503 Service Unavailable", "unavailable".to_owned())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
        });
        let base = format!("http://{address}/");
        let service = AnimeMetadataService {
            network: Arc::new(
                SourceNetworkService::new(NativeHttpConfig {
                    proxy_mode: ProxyMode::Off,
                    proxy_url: None,
                    timeout_ms: 5_000,
                    max_response_bytes: 1024 * 1024,
                    user_agent: "AniTracker-Test".to_owned(),
                })
                .expect("network service"),
            ),
            endpoints: MetadataEndpoints {
                bangumi: base.clone(),
                anilist: format!("{base}graphql"),
                mikan: base,
            },
            channel: NetworkRequestChannel::Interactive,
            bangumi_catalog_cache: Mutex::new(HashMap::new()),
        };
        let result = service
            .search(&MemoryCircuitStore::default(), "测试番")
            .await;
        server.await.expect("mock server");

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.source, "bangumi+anilist");
        assert_eq!(result.items[0].external_ids["bangumi"], "101");
        assert_eq!(result.items[0].external_ids["anilist"], "202");
        assert!(result
            .errors
            .iter()
            .any(|error| error.starts_with("mikan:")));
    }
}
