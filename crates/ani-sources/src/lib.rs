use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ani_domain::{ReleaseSourceConfig, RequestCircuitState};
use ani_repository::{ReleaseSourceRepository, RepositoryError, RepositoryResult};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method, Proxy};

mod bindings;
mod metadata;
mod parsers;
mod release;
mod search;

pub use bindings::{AnimeSourceBindingService, AnimeSourceBindingStore};
pub use metadata::{
    detail_requests_for_items, merge_anime_metadata_batches, AnimeMetadataBatch,
    AnimeMetadataCollection, AnimeMetadataDetailCollection, AnimeMetadataDetailProvider,
    AnimeMetadataDetailProviderOutcome, AnimeMetadataDetailRequest, AnimeMetadataRefresh,
    AnimeMetadataService,
};

pub use parsers::{
    parse_acgnx_api_response, parse_acgnx_html, parse_anibt_rss, parse_dmhy_list,
    parse_mikan_release_list, parse_mikan_subgroups, parse_rss_releases, parse_torznab_releases,
    MikanSubgroup, TorznabPage,
};
pub use release::{
    build_anime_release_search_terms, classify_anime_release, create_discovered_fansub_id,
    detect_series_season_no, enrich_release_from_title, evaluate_automatic_download,
    is_meaningful_fansub_name, matches_anime_release_title, normalize_fansub_name,
    normalize_release_search_text, parse_release_title, rank_releases, release_matches_episode,
    release_satisfies_subtitle_requirement, score_release, sort_releases_by_rules,
    AnimeReleaseCompatibility, ParsedReleaseTitle, AUTOMATIC_DOWNLOAD_MIN_LEAD,
    AUTOMATIC_DOWNLOAD_MIN_MATCH_SCORE, AUTOMATIC_DOWNLOAD_MIN_SCORE,
};
pub use search::{
    build_acgrip_rss_url, build_anibt_anime_rss_url, build_dmhy_list_url, build_mikan_search_url,
    build_nyaa_rss_url, create_anibt_headers, is_supported_source, ReleaseSearchService,
    ReleaseSearchStore, SourceSyncFetchResult, COMPLETED_ANIME_RELEASE_CACHE_TTL_MS,
    MAX_RELEASE_SOURCE_FETCH_LIMIT, MAX_RELEASE_SOURCE_RESULT_LIMIT,
};

pub const DEFAULT_SOURCE_REQUEST_INTERVAL_MS: u64 = 1_500;
pub const MIN_SOURCE_REQUEST_INTERVAL_MS: u64 = 250;
pub const MAX_SOURCE_REQUEST_INTERVAL_MS: u64 = 60_000;
pub const ANIBT_MIN_REQUEST_INTERVAL_MS: u64 = 500;
const RELEASE_SOURCE_CIRCUIT_GROUP: &str = "release-source";
const FORBIDDEN_BACKOFF_SECONDS: &[u64] = &[10 * 60, 20 * 60, 30 * 60];
const RATE_LIMIT_BACKOFF_SECONDS: &[u64] = &[60, 5 * 60, 15 * 60, 30 * 60];
const TRANSIENT_BACKOFF_SECONDS: &[u64] = &[30, 2 * 60, 30 * 60];
const BACKGROUND_TRANSPORT_RETRY_DELAYS_MS: &[u64] = &[300, 900];
const BACKGROUND_TRANSPORT_RETRY_JITTER_MS: u64 = 250;

/// 元数据请求使用的代理模式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyMode {
    Off,
    System,
    Manual,
}

/// Native HTTP 客户端配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHttpConfig {
    pub proxy_mode: ProxyMode,
    pub proxy_url: Option<String>,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
    pub user_agent: String,
}

impl Default for NativeHttpConfig {
    /// 创建适合元数据和下载源请求的受限默认配置。
    fn default() -> Self {
        Self {
            proxy_mode: ProxyMode::System,
            proxy_url: None,
            timeout_ms: 30_000,
            max_response_bytes: 16 * 1024 * 1024,
            user_agent: "AniTracker/0.1".to_owned(),
        }
    }
}

/// Native HTTP 支持的请求方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// 区分用户交互与后台采集，避免后台等待占用搜索限流通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NetworkRequestChannel {
    Interactive,
    Background,
}

impl NetworkRequestChannel {
    /// 返回用于限流键和熔断键的稳定名称。
    fn key(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
        }
    }
}

/// 单次来源请求参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHttpRequest {
    pub source_id: String,
    pub method: HttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
    pub request_interval_ms: u64,
}

/// Native HTTP 返回的状态、响应头和受限正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHttpResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl NativeHttpResponse {
    /// 将响应正文按 UTF-8 宽容解码为文本。
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// 读取一个不区分大小写的响应头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Native HTTP、代理、限流和熔断错误。
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("来源 URL 无效：{0}")]
    InvalidUrl(String),
    #[error("来源 URL 仅允许 HTTP 或 HTTPS：{0}")]
    UnsupportedScheme(String),
    #[error("手动代理配置无效：{0}")]
    InvalidProxy(String),
    #[error("HTTP 请求头无效：{0}")]
    InvalidHeader(String),
    #[error("HTTP 请求失败：{0}")]
    Transport(#[from] reqwest::Error),
    #[error("HTTP 响应状态异常：{status}")]
    HttpStatus { status: u16 },
    #[error("HTTP 响应超过 {limit} 字节限制")]
    ResponseTooLarge { limit: usize },
    #[error("来源响应解析失败：{0}")]
    Parse(String),
    #[error("来源请求正在熔断保护中，恢复时间：{backoff_until}")]
    CircuitOpen { backoff_until: String },
    #[error("来源状态持久化失败：{0}")]
    Repository(#[from] RepositoryError),
}

/// 读取并保存通用请求熔断状态的最小端口。
pub trait CircuitStateStore {
    /// 读取一个请求目标的熔断状态。
    fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>>;

    /// 保存一个请求目标的熔断状态。
    fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()>;
}

impl<T> CircuitStateStore for T
where
    T: ReleaseSourceRepository,
{
    /// 将完整来源 Repository 适配为最小熔断状态端口。
    fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
        ReleaseSourceRepository::get_request_circuit_state(self, key)
    }

    /// 将完整来源 Repository 适配为最小熔断状态端口。
    fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
        ReleaseSourceRepository::upsert_request_circuit_state(self, state)
    }
}

/// 单个请求通道与主机共用的最近请求时间槽。
type HostRateLimitSlot = Arc<tokio::sync::Mutex<Option<Instant>>>;

/// 所有请求通道与主机的限流时间槽表。
type HostRateLimitSlots = Arc<tokio::sync::Mutex<HashMap<String, HostRateLimitSlot>>>;

/// 多个传输实例共用的主机请求间隔控制器。
#[derive(Clone, Default)]
struct HostRateLimiter {
    slots: HostRateLimitSlots,
}

impl HostRateLimiter {
    /// 按主机串行等待最小请求间隔。
    async fn wait(&self, channel: NetworkRequestChannel, host: &str, request_interval_ms: u64) {
        let interval = Duration::from_millis(request_interval_ms.clamp(
            MIN_SOURCE_REQUEST_INTERVAL_MS,
            MAX_SOURCE_REQUEST_INTERVAL_MS,
        ));
        let key = format!("{}:{host}", channel.key());
        let slot = {
            let mut slots = self.slots.lock().await;
            Arc::clone(
                slots
                    .entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))),
            )
        };
        let mut last_request = slot.lock().await;
        if let Some(last_request) = *last_request {
            let elapsed = last_request.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        *last_request = Some(Instant::now());
    }
}

/// 使用 Rust 网络栈执行来源请求，并按主机串行限流。
pub struct NativeHttpClient {
    client: Client,
    max_response_bytes: usize,
    rate_limiter: HostRateLimiter,
}

impl NativeHttpClient {
    /// 校验代理配置并创建可跨请求复用的连接池。
    pub fn new(config: NativeHttpConfig) -> Result<Self, SourceError> {
        Self::new_with_rate_limiter(config, HostRateLimiter::default())
    }

    /// 创建使用指定共享主机限流器的连接池。
    fn new_with_rate_limiter(
        config: NativeHttpConfig,
        rate_limiter: HostRateLimiter,
    ) -> Result<Self, SourceError> {
        let mut builder = Client::builder()
            .timeout(Duration::from_millis(
                config.timeout_ms.clamp(1_000, 120_000),
            ))
            .user_agent(config.user_agent);
        builder = match config.proxy_mode {
            ProxyMode::Off => builder.no_proxy(),
            ProxyMode::System => builder,
            ProxyMode::Manual => {
                let proxy_url = config
                    .proxy_url
                    .as_deref()
                    .ok_or_else(|| SourceError::InvalidProxy("缺少代理 URL".to_owned()))?;
                let proxy = Proxy::all(proxy_url)
                    .map_err(|error| SourceError::InvalidProxy(error.to_string()))?;
                builder.proxy(proxy)
            }
        };
        let client = builder.build()?;
        Ok(Self {
            client,
            max_response_bytes: config.max_response_bytes.clamp(1_024, 64 * 1024 * 1024),
            rate_limiter,
        })
    }

    /// 执行经过协议白名单、主机限流和正文大小限制的请求。
    pub async fn execute(
        &self,
        request: NativeHttpRequest,
    ) -> Result<NativeHttpResponse, SourceError> {
        self.execute_in_channel(NetworkRequestChannel::Interactive, request)
            .await
    }

    /// 在指定调度通道执行请求，各通道独立等待最小间隔。
    async fn execute_in_channel(
        &self,
        channel: NetworkRequestChannel,
        request: NativeHttpRequest,
    ) -> Result<NativeHttpResponse, SourceError> {
        let parsed = parse_http_url(&request.url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| SourceError::InvalidUrl("缺少请求主机".to_owned()))?
            .to_owned();
        self.rate_limiter
            .wait(channel, &host, request.request_interval_ms)
            .await;
        let headers = build_headers(request.headers)?;
        let method = match request.method {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
        };
        let started_at = Instant::now();
        let mut builder = self.client.request(method, parsed).headers(headers);
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let mut response = builder.send().await.map_err(|error| {
            log_transport_failure(&request.source_id, &host, started_at, &error);
            SourceError::Transport(error)
        })?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let response_headers = collect_headers(response.headers());
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(SourceError::ResponseTooLarge {
                limit: self.max_response_bytes,
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            log_transport_failure(&request.source_id, &host, started_at, &error);
            SourceError::Transport(error)
        })? {
            if body.len() + chunk.len() > self.max_response_bytes {
                return Err(SourceError::ResponseTooLarge {
                    limit: self.max_response_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        log::info!(
            "Rust 来源网络请求完成：source_id={}, host={}, status={}, elapsed_ms={}, bytes={}",
            request.source_id,
            host,
            status,
            started_at.elapsed().as_millis(),
            body.len()
        );
        Ok(NativeHttpResponse {
            status,
            final_url,
            headers: response_headers,
            body,
        })
    }
}

/// 脱敏记录来源传输失败，不输出请求路径、查询参数或响应内容。
fn log_transport_failure(source_id: &str, host: &str, started_at: Instant, error: &reqwest::Error) {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "response_body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    };
    let reason = transport_failure_reason(error);
    log::warn!(
        "Rust 来源网络请求失败：source_id={}, host={}, elapsed_ms={}, error_category={}, failure_reason={}",
        source_id,
        host,
        started_at.elapsed().as_millis(),
        category,
        reason
    );
}

/// 将错误链归类为不包含 URL 和请求参数的稳定失败原因。
fn transport_failure_reason(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        return "timeout";
    }
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(cause) = current {
        if let Some(reason) = classify_transport_failure_detail(&cause.to_string()) {
            return reason;
        }
        current = cause.source();
    }
    "unknown"
}

/// 从单层错误文本识别 DNS、TLS、代理和套接字失败类别。
fn classify_transport_failure_detail(detail: &str) -> Option<&'static str> {
    let detail = detail.to_ascii_lowercase();
    if detail.contains("dns")
        || detail.contains("failed to lookup")
        || detail.contains("name or service not known")
    {
        return Some("dns");
    }
    if detail.contains("certificate")
        || detail.contains("invalid peer certificate")
        || detail.contains("tls")
        || detail.contains("handshake")
    {
        return Some("tls");
    }
    if detail.contains("proxy") || detail.contains("tunnel") {
        return Some("proxy");
    }
    if detail.contains("connection refused") {
        return Some("connection_refused");
    }
    if detail.contains("network is unreachable") || detail.contains("no route to host") {
        return Some("network_unreachable");
    }
    if detail.contains("connection reset") || detail.contains("broken pipe") {
        return Some("connection_reset");
    }
    None
}

/// 组合直连/代理传输、持久化限流和熔断的来源网络服务。
pub struct SourceNetworkService {
    direct_client: NativeHttpClient,
    proxy_client: NativeHttpClient,
    circuit_breaker: CircuitBreaker,
}

impl SourceNetworkService {
    /// 创建共享主机限流状态的直连和代理连接池。
    pub fn new(proxy_config: NativeHttpConfig) -> Result<Self, SourceError> {
        let rate_limiter = HostRateLimiter::default();
        let mut direct_config = proxy_config.clone();
        direct_config.proxy_mode = ProxyMode::Off;
        direct_config.proxy_url = None;
        Ok(Self {
            direct_client: NativeHttpClient::new_with_rate_limiter(
                direct_config,
                rate_limiter.clone(),
            )?,
            proxy_client: NativeHttpClient::new_with_rate_limiter(proxy_config, rate_limiter)?,
            circuit_breaker: CircuitBreaker::default(),
        })
    }

    /// 执行带持久化访问保护的单次来源请求。
    pub async fn execute<S: CircuitStateStore>(
        &self,
        state_store: &S,
        source: &ReleaseSourceConfig,
        request: NativeHttpRequest,
    ) -> Result<NativeHttpResponse, SourceError> {
        self.execute_in_channel(
            state_store,
            source,
            request,
            NetworkRequestChannel::Interactive,
        )
        .await
    }

    /// 在独立后台通道执行请求，不占用手动搜索的限流和熔断状态。
    pub(crate) async fn execute_background<S: CircuitStateStore>(
        &self,
        state_store: &S,
        source: &ReleaseSourceConfig,
        request: NativeHttpRequest,
    ) -> Result<NativeHttpResponse, SourceError> {
        self.execute_in_channel(
            state_store,
            source,
            request,
            NetworkRequestChannel::Background,
        )
        .await
    }

    /// 执行指定调度通道的持久化限流和熔断流程。
    async fn execute_in_channel<S: CircuitStateStore>(
        &self,
        state_store: &S,
        source: &ReleaseSourceConfig,
        mut request: NativeHttpRequest,
        channel: NetworkRequestChannel,
    ) -> Result<NativeHttpResponse, SourceError> {
        let parsed = parse_http_url(&request.url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| SourceError::InvalidUrl("缺少请求主机".to_owned()))?
            .to_ascii_lowercase();
        let circuit_group = match channel {
            NetworkRequestChannel::Interactive => RELEASE_SOURCE_CIRCUIT_GROUP,
            NetworkRequestChannel::Background => "release-source-background",
        };
        let circuit_key = format!("{circuit_group}:{}", source.id);
        let previous = state_store.get_circuit_state(&circuit_key)?;
        let now = Utc::now();
        self.circuit_breaker
            .ensure_available(previous.as_ref(), now)?;
        let request_interval_ms = normalize_source_request_interval(source, &parsed);
        wait_for_persisted_interval(previous.as_ref(), request_interval_ms, now).await;

        request.source_id.clone_from(&source.id);
        request.request_interval_ms = request_interval_ms;
        let client = if should_use_source_proxy(source, &parsed) {
            &self.proxy_client
        } else {
            &self.direct_client
        };
        let response = execute_with_transport_retry(client, channel, request).await;
        match response {
            Ok(response) if (200..400).contains(&response.status) => {
                let state = self.circuit_breaker.record_success(
                    &circuit_key,
                    circuit_group,
                    Some(&host),
                    Utc::now(),
                );
                state_store.save_circuit_state(&state)?;
                Ok(response)
            }
            Ok(response) => {
                let retry_after = parse_retry_after(response.header("retry-after"));
                match self.circuit_breaker.record_http_failure(
                    previous.as_ref(),
                    &circuit_key,
                    circuit_group,
                    Some(&host),
                    response.status,
                    retry_after,
                    Utc::now(),
                ) {
                    Some(state) => self.persist_state_after_failure(state_store, &state),
                    None => {
                        let state = self.circuit_breaker.record_success(
                            &circuit_key,
                            circuit_group,
                            Some(&host),
                            Utc::now(),
                        );
                        state_store.save_circuit_state(&state)?;
                    }
                }
                Err(SourceError::HttpStatus {
                    status: response.status,
                })
            }
            Err(error) => {
                self.persist_transport_failure(
                    state_store,
                    previous.as_ref(),
                    &circuit_key,
                    circuit_group,
                    &host,
                );
                Err(error)
            }
        }
    }

    /// 保存失败退避状态，同时保留原始网络错误作为调用结果。
    fn persist_transport_failure<S: CircuitStateStore>(
        &self,
        state_store: &S,
        previous: Option<&RequestCircuitState>,
        circuit_key: &str,
        circuit_group: &str,
        host: &str,
    ) {
        let state = self.circuit_breaker.record_failure(
            previous,
            circuit_key,
            circuit_group,
            Some(host),
            None,
            Utc::now(),
        );
        self.persist_state_after_failure(state_store, &state);
    }

    /// 尽力保存失败状态，同时保留原始 HTTP 或传输错误。
    fn persist_state_after_failure<S: CircuitStateStore>(
        &self,
        state_store: &S,
        state: &RequestCircuitState,
    ) {
        if let Err(error) = state_store.save_circuit_state(state) {
            log::error!(
                "Rust 来源熔断状态保存失败：key={}, host={:?}, error={}",
                state.key,
                state.request_host,
                error
            );
        }
    }
}

/// 后台传输失败时先做有限重试，耗尽后才进入持久化熔断。
async fn execute_with_transport_retry(
    client: &NativeHttpClient,
    channel: NetworkRequestChannel,
    request: NativeHttpRequest,
) -> Result<NativeHttpResponse, SourceError> {
    let retry_delays = match channel {
        NetworkRequestChannel::Interactive => &[][..],
        NetworkRequestChannel::Background => BACKGROUND_TRANSPORT_RETRY_DELAYS_MS,
    };
    let mut retry_index = 0usize;
    loop {
        match client.execute_in_channel(channel, request.clone()).await {
            Err(SourceError::Transport(_)) if retry_index < retry_delays.len() => {
                let delay = transport_retry_delay(&request, retry_index, retry_delays[retry_index]);
                log::warn!(
                    "Rust 来源后台传输失败准备重试：source_id={}, attempt={}, delay_ms={}",
                    request.source_id,
                    retry_index + 2,
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
                retry_index += 1;
            }
            result => return result,
        }
    }
}

/// 为不同请求计算稳定抖动，避免同一批任务同时重试。
fn transport_retry_delay(
    request: &NativeHttpRequest,
    retry_index: usize,
    base_delay_ms: u64,
) -> Duration {
    let mut hasher = DefaultHasher::new();
    request.source_id.hash(&mut hasher);
    request.url.hash(&mut hasher);
    retry_index.hash(&mut hasher);
    let jitter = hasher.finish() % (BACKGROUND_TRANSPORT_RETRY_JITTER_MS + 1);
    Duration::from_millis(base_delay_ms.saturating_add(jitter))
}

/// 计算可持久化的指数退避熔断状态。
pub struct CircuitBreaker {}

impl Default for CircuitBreaker {
    /// 创建与现有来源服务一致的状态码分级熔断策略。
    fn default() -> Self {
        Self {}
    }
}

impl CircuitBreaker {
    /// 在退避截止前拒绝继续请求。
    pub fn ensure_available(
        &self,
        state: Option<&RequestCircuitState>,
        now: DateTime<Utc>,
    ) -> Result<(), SourceError> {
        let Some(backoff_until) = state.and_then(|state| state.backoff_until.as_deref()) else {
            return Ok(());
        };
        let parsed = DateTime::parse_from_rfc3339(backoff_until)
            .ok()
            .map(|value| value.with_timezone(&Utc));
        if parsed.is_some_and(|value| value > now) {
            return Err(SourceError::CircuitOpen {
                backoff_until: backoff_until.to_owned(),
            });
        }
        Ok(())
    }

    /// 成功请求后清空失败次数和退避时间。
    pub fn record_success(
        &self,
        key: &str,
        group: &str,
        host: Option<&str>,
        now: DateTime<Utc>,
    ) -> RequestCircuitState {
        RequestCircuitState {
            key: key.to_owned(),
            group: group.to_owned(),
            request_host: host.map(str::to_owned),
            last_request_at: Some(to_iso(now)),
            failure_count: 0,
            backoff_until: None,
        }
    }

    /// 失败请求按连续次数计算退避，并尊重服务端 Retry-After。
    pub fn record_failure(
        &self,
        previous: Option<&RequestCircuitState>,
        key: &str,
        group: &str,
        host: Option<&str>,
        retry_after: Option<Duration>,
        now: DateTime<Utc>,
    ) -> RequestCircuitState {
        let failure_count = previous.map_or(1, |state| state.failure_count.max(0) + 1);
        let configured = scheduled_delay(TRANSIENT_BACKOFF_SECONDS, failure_count);
        let delay = retry_after.map_or(configured, |value| value.max(configured));
        build_failure_state(key, group, host, failure_count, delay, now)
    }

    /// 按 Electron 基线对 403、429 和 5xx 选择退避曲线。
    #[allow(clippy::too_many_arguments)]
    pub fn record_http_failure(
        &self,
        previous: Option<&RequestCircuitState>,
        key: &str,
        group: &str,
        host: Option<&str>,
        status: u16,
        retry_after: Option<Duration>,
        now: DateTime<Utc>,
    ) -> Option<RequestCircuitState> {
        let failure_count = previous.map_or(1, |state| state.failure_count.max(0) + 1);
        let schedule = match status {
            403 => FORBIDDEN_BACKOFF_SECONDS,
            429 => RATE_LIMIT_BACKOFF_SECONDS,
            500..=599 => TRANSIENT_BACKOFF_SECONDS,
            _ => return None,
        };
        let configured = scheduled_delay(schedule, failure_count);
        let delay = retry_after.map_or(configured, |value| value.max(configured));
        Some(build_failure_state(
            key,
            group,
            host,
            failure_count,
            delay,
            now,
        ))
    }
}

/// 从固定退避曲线选择当前失败次数对应的延迟。
fn scheduled_delay(schedule_seconds: &[u64], failure_count: i64) -> Duration {
    let index = usize::try_from(failure_count.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(schedule_seconds.len().saturating_sub(1));
    Duration::from_secs(schedule_seconds[index])
}

/// 构建可跨进程持久化的失败熔断状态。
fn build_failure_state(
    key: &str,
    group: &str,
    host: Option<&str>,
    failure_count: i64,
    delay: Duration,
    now: DateTime<Utc>,
) -> RequestCircuitState {
    let backoff_until =
        now + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::minutes(30));
    RequestCircuitState {
        key: key.to_owned(),
        group: group.to_owned(),
        request_host: host.map(str::to_owned),
        last_request_at: Some(to_iso(now)),
        failure_count,
        backoff_until: Some(to_iso(backoff_until)),
    }
}

/// 校验来源请求 URL 仅使用 HTTP 或 HTTPS。
fn parse_http_url(value: &str) -> Result<url::Url, SourceError> {
    let parsed =
        url::Url::parse(value).map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(SourceError::UnsupportedScheme(parsed.scheme().to_owned()));
    }
    Ok(parsed)
}

/// 判断下载源或实际 URL 是否指向 AniBT。
pub fn is_anibt_request_target(source: &ReleaseSourceConfig, request_url: &url::Url) -> bool {
    let identity = format!("{} {}", source.id, source.name).to_ascii_lowercase();
    identity.contains("anibt")
        || [source.base_url.as_deref(), source.rss_url.as_deref()]
            .into_iter()
            .flatten()
            .any(is_anibt_url)
        || is_anibt_url(request_url.as_str())
}

/// 判断当前来源请求是否允许使用元数据代理；AniBT 固定直连。
pub fn should_use_source_proxy(source: &ReleaseSourceConfig, request_url: &url::Url) -> bool {
    source.use_proxy && !is_anibt_request_target(source, request_url)
}

/// 应用来源配置和站点下限，返回最终请求间隔。
pub fn normalize_source_request_interval(
    source: &ReleaseSourceConfig,
    request_url: &url::Url,
) -> u64 {
    let configured = u64::try_from(source.request_interval_ms)
        .unwrap_or(DEFAULT_SOURCE_REQUEST_INTERVAL_MS)
        .clamp(
            MIN_SOURCE_REQUEST_INTERVAL_MS,
            MAX_SOURCE_REQUEST_INTERVAL_MS,
        );
    let site_minimum = if is_anibt_request_target(source, request_url) {
        ANIBT_MIN_REQUEST_INTERVAL_MS
    } else {
        MIN_SOURCE_REQUEST_INTERVAL_MS
    };
    configured.max(site_minimum)
}

/// 请求前遵守上次进程持久化的主机访问时间。
async fn wait_for_persisted_interval(
    state: Option<&RequestCircuitState>,
    request_interval_ms: u64,
    now: DateTime<Utc>,
) {
    let Some(last_request_at) = state.and_then(|state| state.last_request_at.as_deref()) else {
        return;
    };
    let Some(last_request_at) = DateTime::parse_from_rfc3339(last_request_at)
        .ok()
        .map(|value| value.with_timezone(&Utc))
    else {
        return;
    };
    let next_request_at = last_request_at
        + chrono::Duration::milliseconds(i64::try_from(request_interval_ms).unwrap_or(i64::MAX));
    if let Ok(delay) = (next_request_at - now).to_std() {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

/// 解析 Retry-After 秒数并交给熔断策略封顶。
fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    value?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// 判断 URL 是否使用 AniBT 主域或子域。
fn is_anibt_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "anibt.net" || host.ends_with(".anibt.net"))
        || value.to_ascii_lowercase().contains("anibt.net")
}

/// 将调用方请求头转换为 reqwest 结构。
fn build_headers(values: BTreeMap<String, String>) -> Result<HeaderMap, SourceError> {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| SourceError::InvalidHeader(error.to_string()))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|error| SourceError::InvalidHeader(error.to_string()))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// 收集可序列化响应头，忽略非文本值。
fn collect_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// 生成与 TypeScript 一致的毫秒 UTC 时间戳。
fn to_iso(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use ani_domain::{ReleaseSourceConfig, RequestCircuitState, SourceKind};
    use ani_repository::RepositoryResult;
    use chrono::{TimeZone, Utc};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{
        classify_transport_failure_detail, normalize_source_request_interval,
        should_use_source_proxy, CircuitBreaker, CircuitStateStore, HostRateLimiter, HttpMethod,
        NativeHttpClient, NativeHttpConfig, NativeHttpRequest, NetworkRequestChannel, ProxyMode,
        SourceError, SourceNetworkService, ANIBT_MIN_REQUEST_INTERVAL_MS,
    };

    #[derive(Default)]
    struct MemoryCircuitStateStore {
        state: Mutex<Option<RequestCircuitState>>,
    }

    impl CircuitStateStore for MemoryCircuitStateStore {
        /// 返回测试内存中保存的熔断状态。
        fn get_circuit_state(&self, _key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
            Ok(self.state.lock().expect("lock circuit state").clone())
        }

        /// 覆盖测试内存中的熔断状态。
        fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
            *self.state.lock().expect("lock circuit state") = Some(state.clone());
            Ok(())
        }
    }

    /// 验证关闭代理和无效手动代理使用确定配置结果。
    #[test]
    fn validates_proxy_configuration() {
        assert!(NativeHttpClient::new(NativeHttpConfig {
            proxy_mode: ProxyMode::Off,
            ..NativeHttpConfig::default()
        })
        .is_ok());
        assert!(matches!(
            NativeHttpClient::new(NativeHttpConfig {
                proxy_mode: ProxyMode::Manual,
                proxy_url: None,
                ..NativeHttpConfig::default()
            }),
            Err(SourceError::InvalidProxy(_))
        ));
    }

    /// 验证后台采集等待不会占用手动搜索的主机限流槽。
    #[tokio::test]
    async fn isolates_background_and_interactive_rate_limits() {
        let limiter = HostRateLimiter::default();
        limiter
            .wait(NetworkRequestChannel::Background, "example.test", 250)
            .await;
        let background_limiter = limiter.clone();
        let background = tokio::spawn(async move {
            background_limiter
                .wait(NetworkRequestChannel::Background, "example.test", 250)
                .await;
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let interactive = tokio::time::timeout(
            Duration::from_millis(100),
            limiter.wait(NetworkRequestChannel::Interactive, "example.test", 250),
        )
        .await;

        assert!(interactive.is_ok());
        background.abort();
    }

    /// 网络错误分类不需要记录原始 URL 或请求内容。
    #[test]
    fn classifies_sanitized_transport_failure_reasons() {
        assert_eq!(
            classify_transport_failure_detail("dns error: failed to lookup address"),
            Some("dns")
        );
        assert_eq!(
            classify_transport_failure_detail("invalid peer certificate: UnknownIssuer"),
            Some("tls")
        );
        assert_eq!(
            classify_transport_failure_detail("tcp connect error: Connection refused"),
            Some("connection_refused")
        );
    }

    /// 验证熔断失败次数跨请求指数退避并在成功后清零。
    #[test]
    fn calculates_persistable_circuit_backoff() {
        let breaker = CircuitBreaker::default();
        let now = Utc.with_ymd_and_hms(2026, 7, 25, 0, 0, 0).unwrap();
        let first = breaker.record_failure(
            None,
            "source:test",
            "release-source",
            Some("example.test"),
            None,
            now,
        );
        let second = breaker.record_failure(
            Some(&first),
            "source:test",
            "release-source",
            Some("example.test"),
            Some(Duration::from_secs(120)),
            now,
        );

        assert_eq!(first.failure_count, 1);
        assert_eq!(second.failure_count, 2);
        assert!(breaker.ensure_available(Some(&second), now).is_err());
        let success =
            breaker.record_success("source:test", "release-source", Some("example.test"), now);
        assert_eq!(success.failure_count, 0);
        assert!(success.backoff_until.is_none());
    }

    /// 验证 403、429、5xx 和普通 4xx 使用与现有来源服务一致的分级策略。
    #[test]
    fn applies_status_specific_circuit_backoff() {
        let breaker = CircuitBreaker::default();
        let now = Utc.with_ymd_and_hms(2026, 7, 25, 0, 0, 0).unwrap();
        let forbidden = breaker
            .record_http_failure(
                None,
                "source:test",
                "release-source",
                Some("example.test"),
                403,
                None,
                now,
            )
            .expect("403 must trigger circuit");
        let forbidden_until = chrono::DateTime::parse_from_rfc3339(
            forbidden
                .backoff_until
                .as_deref()
                .expect("403 backoff timestamp"),
        )
        .expect("parse 403 backoff")
        .with_timezone(&Utc);
        assert_eq!((forbidden_until - now).num_minutes(), 10);

        let rate_limited = breaker
            .record_http_failure(
                None,
                "source:test",
                "release-source",
                Some("example.test"),
                429,
                Some(Duration::from_secs(120)),
                now,
            )
            .expect("429 must trigger circuit");
        let rate_limit_until = chrono::DateTime::parse_from_rfc3339(
            rate_limited
                .backoff_until
                .as_deref()
                .expect("429 backoff timestamp"),
        )
        .expect("parse 429 backoff")
        .with_timezone(&Utc);
        assert_eq!((rate_limit_until - now).num_minutes(), 2);
        assert!(breaker
            .record_http_failure(
                None,
                "source:test",
                "release-source",
                Some("example.test"),
                404,
                None,
                now,
            )
            .is_none());
    }

    /// 验证 AniBT 固定直连并执行站点最小访问间隔。
    #[test]
    fn applies_source_proxy_and_site_interval_policy() {
        let request_url = url::Url::parse("https://api.anibt.net/rss").expect("parse AniBT URL");
        let source = test_source("anibt", true, 250);
        assert_eq!(ANIBT_MIN_REQUEST_INTERVAL_MS, 500);
        assert!(!should_use_source_proxy(&source, &request_url));
        assert_eq!(
            normalize_source_request_interval(&source, &request_url),
            ANIBT_MIN_REQUEST_INTERVAL_MS
        );

        let regular_url = url::Url::parse("https://example.test/rss").expect("parse source URL");
        let regular = test_source("regular", true, 1_500);
        assert!(should_use_source_proxy(&regular, &regular_url));
        assert_eq!(
            normalize_source_request_interval(&regular, &regular_url),
            1_500
        );
    }

    /// 验证 Native HTTP 成功响应会保存已恢复的熔断状态。
    #[tokio::test]
    async fn executes_native_http_and_persists_success_state() {
        let url = serve_once("200 OK", &[], "source-ok").await;
        let service = SourceNetworkService::new(NativeHttpConfig {
            proxy_mode: ProxyMode::Off,
            ..NativeHttpConfig::default()
        })
        .expect("create source network service");
        let store = MemoryCircuitStateStore::default();
        let response = service
            .execute(&store, &test_source("local", false, 250), get_request(url))
            .await
            .expect("execute local native HTTP request");

        assert_eq!(response.status, 200);
        assert_eq!(response.text(), "source-ok");
        let state = store
            .state
            .lock()
            .expect("lock success state")
            .clone()
            .expect("persisted success state");
        assert_eq!(state.key, "release-source:local");
        assert_eq!(state.failure_count, 0);
        assert!(state.backoff_until.is_none());
    }

    /// 验证后台传输瞬断会在熔断前重试，并在恢复后保存成功状态。
    #[tokio::test]
    async fn retries_background_transport_before_opening_circuit() {
        let (url, request_count) = serve_after_disconnects(1, "recovered").await;
        let service = SourceNetworkService::new(NativeHttpConfig {
            proxy_mode: ProxyMode::Off,
            ..NativeHttpConfig::default()
        })
        .expect("create source network service");
        let store = MemoryCircuitStateStore::default();
        let response = service
            .execute_background(&store, &test_source("local", false, 250), get_request(url))
            .await
            .expect("retry background request");

        assert_eq!(response.text(), "recovered");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        let state = store
            .state
            .lock()
            .expect("lock recovered state")
            .clone()
            .expect("persisted recovered state");
        assert_eq!(state.failure_count, 0);
        assert!(state.backoff_until.is_none());
    }

    /// 验证非成功状态会持久化失败次数和 Retry-After 退避。
    #[tokio::test]
    async fn persists_http_failure_circuit_state() {
        let url = serve_once("503 Service Unavailable", &[("Retry-After", "120")], "busy").await;
        let service = SourceNetworkService::new(NativeHttpConfig {
            proxy_mode: ProxyMode::Off,
            ..NativeHttpConfig::default()
        })
        .expect("create source network service");
        let store = MemoryCircuitStateStore::default();
        let error = service
            .execute(&store, &test_source("local", false, 250), get_request(url))
            .await
            .expect_err("503 request must fail");

        assert!(matches!(error, SourceError::HttpStatus { status: 503 }));
        let state = store
            .state
            .lock()
            .expect("lock failure state")
            .clone()
            .expect("persisted failure state");
        assert_eq!(state.failure_count, 1);
        assert!(state.backoff_until.is_some());
    }

    /// 创建测试下载源配置。
    fn test_source(id: &str, use_proxy: bool, request_interval_ms: i64) -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: id.to_owned(),
            name: id.to_owned(),
            kind: SourceKind::Rss,
            enabled: true,
            use_proxy,
            request_interval_ms,
            base_url: None,
            api_key: None,
            rss_url: None,
            tags: Vec::new(),
        }
    }

    /// 创建本地 GET 请求。
    fn get_request(url: String) -> NativeHttpRequest {
        NativeHttpRequest {
            source_id: String::new(),
            method: HttpMethod::Get,
            url,
            headers: BTreeMap::new(),
            body: None,
            request_interval_ms: 250,
        }
    }

    /// 启动只处理一次请求的本地 HTTP 服务。
    async fn serve_once(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local HTTP listener");
        let address = listener.local_addr().expect("read local HTTP address");
        let status = status.to_owned();
        let headers = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        let body = body.as_bytes().to_vec();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept local HTTP request");
            let mut request = [0_u8; 2_048];
            let _ = stream
                .read(&mut request)
                .await
                .expect("read local HTTP request");
            let extra_headers = headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect::<String>();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
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

    /// 启动先断开指定次数、随后返回成功响应的本地服务。
    async fn serve_after_disconnects(
        disconnect_count: usize,
        body: &str,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind retry HTTP listener");
        let address = listener.local_addr().expect("read retry HTTP address");
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        let body = body.as_bytes().to_vec();
        tokio::spawn(async move {
            for attempt in 0..=disconnect_count {
                let (mut stream, _) = listener.accept().await.expect("accept retry request");
                server_request_count.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 2_048];
                let _ = stream.read(&mut request).await.expect("read retry request");
                if attempt < disconnect_count {
                    continue;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write retry HTTP headers");
                stream
                    .write_all(&body)
                    .await
                    .expect("write retry HTTP body");
            }
        });
        (format!("http://{address}/source"), request_count)
    }
}
