use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ani_automation::{
    AnimeDiscoverySyncStore, AutomationDownloadReference, AutomationScanStore, EpisodeSyncStore,
    SourceSyncStore,
};
use ani_domain::{
    Anime, AnimeDetailRefreshState, AnimeSeasonSyncState, AnimeSourceBinding, AnimeSourceExclusion,
    Episode, FansubGroup, MyAnime, NotificationRecord, Release, ReleaseSourceConfig,
    ReleaseSourceSyncState, RequestCircuitState,
};
use ani_repository::{
    AnimeCatalogWriteResult, ApplicationRepository, CachedReleaseQuery, ReleaseSearchCacheEntry,
    RepositoryError, RepositoryResult,
};
use ani_sources::{
    AnimeSourceBindingStore, CircuitStateStore, NativeHttpConfig, ProxyMode, ReleaseSearchStore,
    SourceError, SourceNetworkService,
};
use ani_storage::Storage;
use hyper_util::client::proxy::matcher::Matcher;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

/// 将共享 SQLite 单写者适配为来源搜索所需的窄存储端口。
#[derive(Clone)]
pub(crate) struct SharedReleaseSearchStore {
    storage: Arc<Mutex<Storage>>,
}

impl SharedReleaseSearchStore {
    /// 创建复用应用 SQLite 单写者的来源存储适配器。
    pub(crate) fn new(storage: Arc<Mutex<Storage>>) -> Self {
        Self { storage }
    }

    /// 在短临界区内执行来源 Repository 操作。
    fn with_repository<T>(
        &self,
        operation: impl FnOnce(&dyn ApplicationRepository) -> RepositoryResult<T>,
    ) -> RepositoryResult<T> {
        let storage = self
            .storage
            .lock()
            .map_err(|error| RepositoryError::BackendUnavailable {
                backend: "sqlite".to_owned(),
                message: error.to_string(),
            })?;
        operation(&storage.repository())
    }
}

impl CircuitStateStore for SharedReleaseSearchStore {
    /// 读取来源熔断状态。
    fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
        self.with_repository(|repository| repository.get_request_circuit_state(key))
    }

    /// 保存来源熔断状态。
    fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_request_circuit_state(state))
    }
}

impl AnimeDiscoverySyncStore for SharedReleaseSearchStore {
    /// 读取指定季度同步状态。
    fn get_season_sync_state(
        &self,
        year: i64,
        season: &str,
    ) -> RepositoryResult<Option<AnimeSeasonSyncState>> {
        self.with_repository(|repository| repository.get_anime_season_sync_state(year, season))
    }

    /// 保存指定季度同步状态。
    fn save_season_sync_state(&self, state: &AnimeSeasonSyncState) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_anime_season_sync_state(state))
    }

    /// 合并季度目录。
    fn save_season_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult> {
        self.with_repository(|repository| repository.upsert_anime_catalog(items))
    }

    /// 合并详情补全结果并更新时间戳。
    fn save_detail_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult> {
        self.with_repository(|repository| repository.upsert_anime_catalog_details(items))
    }

    /// 读取季度中的指定月份。
    fn list_season_catalog_month(&self, year: i64, month: i64) -> RepositoryResult<Vec<Anime>> {
        self.with_repository(|repository| repository.list_anime_catalog(Some(year), Some(month)))
    }

    /// 读取全部目录供周期详情矫正。
    fn list_all_season_catalog(&self) -> RepositoryResult<Vec<Anime>> {
        self.with_repository(|repository| repository.list_anime_catalog(None, None))
    }

    /// 读取来源级详情刷新状态。
    fn list_detail_refresh_states(&self) -> RepositoryResult<Vec<AnimeDetailRefreshState>> {
        self.with_repository(|repository| repository.list_anime_detail_refresh_states())
    }

    /// 保存来源级详情刷新状态。
    fn save_detail_refresh_states(
        &self,
        states: &[AnimeDetailRefreshState],
    ) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_anime_detail_refresh_states(states))
    }
}

impl ReleaseSearchStore for SharedReleaseSearchStore {
    /// 读取未过期的资源搜索缓存。
    fn get_search_cache(
        &self,
        cache_key: &str,
        current_time: &str,
    ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>> {
        self.with_repository(|repository| {
            repository.get_release_search_cache(cache_key, current_time)
        })
    }

    /// 保存资源搜索缓存。
    fn save_search_cache(
        &self,
        cache_key: &str,
        entry: &ReleaseSearchCacheEntry,
    ) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_release_search_cache(cache_key, entry))
    }

    /// 读取跨重启原始资源缓存。
    fn list_release_cache(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>> {
        self.with_repository(|repository| repository.list_cached_releases(query))
    }

    /// 保存网络返回的原始资源缓存。
    fn save_release_cache(&self, releases: &[Release]) -> RepositoryResult<usize> {
        self.with_repository(|repository| repository.upsert_cached_releases(releases))
    }
}

impl AnimeSourceBindingStore for SharedReleaseSearchStore {
    /// 读取全部追番。
    fn list_followed_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
        self.with_repository(|repository| repository.list_my_anime())
    }

    /// 读取全部来源配置。
    fn list_binding_sources(&self) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
        self.with_repository(|repository| repository.list_sources())
    }

    /// 读取指定番剧的单集。
    fn list_binding_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
        self.with_repository(|repository| repository.list_episodes(anime_id))
    }

    /// 读取指定番剧的来源绑定。
    fn list_bindings(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        self.with_repository(|repository| repository.list_anime_source_bindings(anime_id))
    }

    /// 保存一条来源绑定。
    fn save_binding(
        &self,
        binding: &AnimeSourceBinding,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        self.with_repository(|repository| repository.upsert_anime_source_binding(binding))
    }

    /// 读取指定番剧的来源排除记录。
    fn list_exclusions(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        self.with_repository(|repository| repository.list_anime_source_exclusions(anime_id))
    }

    /// 保存一条来源排除记录。
    fn save_exclusion(
        &self,
        exclusion: &AnimeSourceExclusion,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        self.with_repository(|repository| repository.upsert_anime_source_exclusion(exclusion))
    }

    /// 删除一条候选或整来源排除记录。
    fn delete_exclusion(
        &self,
        anime_id: &str,
        source_id: &str,
        source_anime_id: Option<&str>,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        self.with_repository(|repository| {
            repository.remove_anime_source_exclusion(anime_id, source_id, source_anime_id)
        })
    }
}

impl SourceSyncStore for SharedReleaseSearchStore {
    /// 读取全部来源同步游标。
    fn list_sync_states(&self) -> RepositoryResult<Vec<ReleaseSourceSyncState>> {
        self.with_repository(|repository| repository.list_source_sync_states())
    }

    /// 保存一个来源同步游标。
    fn save_sync_state(&self, state: &ReleaseSourceSyncState) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_source_sync_state(state))
    }

    /// 读取全部追番。
    fn list_sync_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
        self.with_repository(|repository| repository.list_my_anime())
    }

    /// 读取指定番剧的来源绑定。
    fn list_sync_bindings(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        self.with_repository(|repository| repository.list_anime_source_bindings(anime_id))
    }

    /// 保存同步采集的资源。
    fn save_synced_releases(&self, releases: &[Release]) -> RepositoryResult<usize> {
        self.with_repository(|repository| repository.upsert_cached_releases(releases))
    }

    /// 观察同步资源中的番剧字幕组。
    fn observe_sync_fansubs(
        &self,
        anime_id: &str,
        releases: &[Release],
    ) -> RepositoryResult<Vec<FansubGroup>> {
        self.with_repository(|repository| repository.observe_anime_fansubs(anime_id, releases))
    }

    /// 清理过期资源缓存。
    fn prune_synced_releases(&self, before: &str) -> RepositoryResult<usize> {
        self.with_repository(|repository| repository.prune_cached_releases(before))
    }

    /// 写入同步失败通知。
    fn add_sync_notifications(
        &self,
        records: &[NotificationRecord],
    ) -> RepositoryResult<Vec<NotificationRecord>> {
        self.with_repository(|repository| repository.add_notifications(records))
    }
}

impl EpisodeSyncStore for SharedReleaseSearchStore {
    /// 读取自动同步所需的单集。
    fn list_sync_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
        self.with_repository(|repository| repository.list_episodes(anime_id))
    }

    /// 幂等保存自动同步单集。
    fn save_sync_episode(&self, episode: &Episode) -> RepositoryResult<Vec<Episode>> {
        self.with_repository(|repository| repository.upsert_episode(episode))
    }

    /// 读取番剧跨重启资源缓存。
    fn list_sync_cached_releases(&self, anime_id: &str) -> RepositoryResult<Vec<Release>> {
        self.with_repository(|repository| {
            repository.list_cached_releases(&CachedReleaseQuery {
                source_ids: None,
                anime_id: Some(anime_id.to_owned()),
                limit: Some(2_000),
            })
        })
    }
}

impl AutomationScanStore for SharedReleaseSearchStore {
    /// 读取全部追番。
    fn list_automation_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
        self.with_repository(|repository| repository.list_my_anime())
    }

    /// 读取指定番剧单集。
    fn list_automation_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
        self.with_repository(|repository| repository.list_episodes(anime_id))
    }

    /// 读取指定番剧单集偏好。
    fn list_automation_preferences(
        &self,
        anime_id: &str,
    ) -> RepositoryResult<Vec<ani_domain::EpisodePreference>> {
        self.with_repository(|repository| repository.list_episode_preferences(anime_id))
    }

    /// 读取指定番剧来源绑定。
    fn list_automation_bindings(
        &self,
        anime_id: &str,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        self.with_repository(|repository| repository.list_anime_source_bindings(anime_id))
    }

    /// 读取下载任务判重快照。
    fn list_automation_downloads(&self) -> RepositoryResult<Vec<AutomationDownloadReference>> {
        self.with_repository(|repository| {
            repository.list_downloads().map(|tasks| {
                tasks
                    .into_iter()
                    .map(|task| AutomationDownloadReference {
                        task_id: task.id,
                        anime_id: task.anime_id,
                        episode_id: task.episode_id,
                        episode_no: task.episode_no,
                    })
                    .collect()
            })
        })
    }

    /// 保存自动扫描推进后的单集状态。
    fn save_automation_episode(&self, episode: &Episode) -> RepositoryResult<()> {
        self.with_repository(|repository| repository.upsert_episode(episode).map(|_| ()))
    }

    /// 保存自动扫描发现的字幕组。
    fn observe_automation_fansubs(
        &self,
        anime_id: &str,
        releases: &[Release],
    ) -> RepositoryResult<Vec<FansubGroup>> {
        self.with_repository(|repository| repository.observe_anime_fansubs(anime_id, releases))
    }

    /// 写入自动扫描结果通知。
    fn add_automation_notifications(
        &self,
        records: &[NotificationRecord],
    ) -> RepositoryResult<Vec<NotificationRecord>> {
        self.with_repository(|repository| repository.add_notifications(records))
    }
}

struct NetworkRuntime {
    config: NativeHttpConfig,
    system_proxy_state: Option<SystemProxyState>,
    physical_network_state: PhysicalNetworkState,
    generation: u64,
    service: Arc<SourceNetworkService>,
}

/// 标识 Native HTTP 客户端创建时读取到的系统代理状态。
#[derive(Clone, Debug, PartialEq, Eq)]
struct SystemProxyState {
    fingerprint: u64,
    detected: bool,
}

impl SystemProxyState {
    /// 读取与 reqwest 相同的系统代理来源并生成不泄露凭据的进程内指纹。
    fn capture() -> Self {
        const PROXY_ENV_KEYS: &[&str] = &[
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "NO_PROXY",
            "no_proxy",
        ];

        let matcher = Matcher::from_system();
        let http_uri = "http://ani-tracker.invalid/"
            .parse()
            .expect("固定 HTTP 代理探测地址必须合法");
        let https_uri = "https://ani-tracker.invalid/"
            .parse()
            .expect("固定 HTTPS 代理探测地址必须合法");
        let detected =
            matcher.intercept(&http_uri).is_some() || matcher.intercept(&https_uri).is_some();

        let mut hasher = DefaultHasher::new();
        format!("{matcher:?}").hash(&mut hasher);
        for key in PROXY_ENV_KEYS {
            key.hash(&mut hasher);
            std::env::var_os(key).hash(&mut hasher);
        }
        Self {
            fingerprint: hasher.finish(),
            detected,
        }
    }
}

/// 标识操作系统为 IPv4/IPv6 默认路由选择的本地出口地址。
#[derive(Clone, Debug, PartialEq, Eq)]
struct PhysicalNetworkState {
    ipv4: Option<IpAddr>,
    ipv6: Option<IpAddr>,
}

impl PhysicalNetworkState {
    /// 只执行本地路由选择，不发送探测数据包。
    fn capture() -> Self {
        Self {
            ipv4: outbound_local_ip("0.0.0.0:0", "192.0.2.1:9"),
            ipv6: outbound_local_ip("[::]:0", "[2001:db8::1]:9"),
        }
    }

    /// 判断当前至少存在一种可选择的默认出口。
    fn is_detected(&self) -> bool {
        self.ipv4.is_some() || self.ipv6.is_some()
    }
}

/// 通过 UDP connect 查询默认路由选择的本地地址，不产生远端流量。
fn outbound_local_ip(bind_address: &str, route_target: &str) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind_address).ok()?;
    socket.connect(route_target).ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified()).then_some(address)
}

/// 创建只用于本进程熔断隔离的匿名标识。
fn process_network_context() -> String {
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    started_at.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 根据当前代理设置复用或重建 Rust 来源网络服务。
#[derive(Clone)]
pub(crate) struct AppSourceState {
    runtime: Arc<AsyncMutex<Option<NetworkRuntime>>>,
    process_network_context: Arc<str>,
}

impl AppSourceState {
    /// 创建尚未初始化连接池的来源状态。
    pub(crate) fn new() -> Self {
        Self {
            runtime: Arc::new(AsyncMutex::new(None)),
            process_network_context: Arc::from(process_network_context()),
        }
    }

    /// 返回匹配当前设置的连接池，代理设置变化时原子替换。
    pub(crate) async fn network_service(
        &self,
        settings: &Value,
    ) -> Result<Arc<SourceNetworkService>, SourceError> {
        let config = native_http_config(settings);
        let system_proxy_state =
            (config.proxy_mode == ProxyMode::System).then(SystemProxyState::capture);
        self.network_service_for_config(config, system_proxy_state, PhysicalNetworkState::capture())
            .await
    }

    /// 按配置和系统代理快照复用连接池，供正式调用与缓存行为测试共用。
    async fn network_service_for_config(
        &self,
        config: NativeHttpConfig,
        observed_system_proxy_state: Option<SystemProxyState>,
        physical_network_state: PhysicalNetworkState,
    ) -> Result<Arc<SourceNetworkService>, SourceError> {
        let system_proxy_state = (config.proxy_mode == ProxyMode::System)
            .then_some(observed_system_proxy_state)
            .flatten();
        let mut runtime = self.runtime.lock().await;
        if let Some(current) = runtime.as_ref().filter(|current| {
            current.config == config
                && current.system_proxy_state == system_proxy_state
                && current.physical_network_state == physical_network_state
        }) {
            return Ok(Arc::clone(&current.service));
        }
        let rebuild_reason = match runtime.as_ref() {
            None => "initial",
            Some(current) if current.config != config => "config_changed",
            Some(current) if current.system_proxy_state != system_proxy_state => {
                "system_proxy_changed"
            }
            Some(_) => "physical_network_changed",
        };
        let generation = runtime
            .as_ref()
            .map_or(1, |current| current.generation.checked_add(1).unwrap_or(1));
        let network_context = format!("{}-{generation}", self.process_network_context);
        let service = Arc::new(SourceNetworkService::new_with_network_context(
            config.clone(),
            network_context,
        )?);
        log::info!(
            "Tauri 来源网络连接池已装配 reason={} proxy_mode={:?} system_proxy_detected={} physical_route_detected={} network_generation={} timeout_ms={} response_limit={}",
            rebuild_reason,
            config.proxy_mode,
            system_proxy_state
                .as_ref()
                .is_some_and(|state| state.detected),
            physical_network_state.is_detected(),
            generation,
            config.timeout_ms,
            config.max_response_bytes
        );
        *runtime = Some(NetworkRuntime {
            config,
            system_proxy_state,
            physical_network_state,
            generation,
            service: Arc::clone(&service),
        });
        Ok(service)
    }
}

/// 从版本化设置中读取代理模式、地址和超时。
pub(crate) fn native_http_config(settings: &Value) -> NativeHttpConfig {
    let proxy = settings.pointer("/network/metadataProxy");
    let mode = match proxy
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str)
    {
        Some("off") => ProxyMode::Off,
        Some("manual") => ProxyMode::Manual,
        _ => ProxyMode::System,
    };
    NativeHttpConfig {
        proxy_mode: mode,
        proxy_url: proxy
            .and_then(|value| value.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        timeout_ms: proxy
            .and_then(|value| value.get("timeoutMs"))
            .and_then(Value::as_u64)
            .unwrap_or(30_000),
        max_response_bytes: 16 * 1024 * 1024,
        user_agent: "AniTracker/0.1".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use ani_sources::ProxyMode;
    use serde_json::json;
    use std::sync::Arc;

    use super::{native_http_config, AppSourceState, PhysicalNetworkState, SystemProxyState};

    /// 构造不依赖主机代理设置的缓存测试快照。
    fn proxy_state(fingerprint: u64, detected: bool) -> SystemProxyState {
        SystemProxyState {
            fingerprint,
            detected,
        }
    }

    /// 构造不依赖主机路由的网络测试快照。
    fn physical_state(ipv4: [u8; 4]) -> PhysicalNetworkState {
        PhysicalNetworkState {
            ipv4: Some(ipv4.into()),
            ipv6: None,
        }
    }

    /// 验证设置中的代理模式、地址和超时映射到 Native HTTP 配置。
    #[test]
    fn maps_proxy_settings_to_native_http_config() {
        let config = native_http_config(&json!({
            "network": {
                "metadataProxy": {
                    "mode": "manual",
                    "url": "http://127.0.0.1:7890",
                    "timeoutMs": 23_000
                }
            }
        }));
        assert_eq!(config.proxy_mode, ProxyMode::Manual);
        assert_eq!(config.proxy_url.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(config.timeout_ms, 23_000);
    }

    /// 验证缺少用户设置时使用 30 秒元数据请求超时。
    #[test]
    fn defaults_native_http_timeout_to_thirty_seconds() {
        let config = native_http_config(&json!({}));
        assert_eq!(config.timeout_ms, 30_000);
    }

    /// 验证相同系统代理指纹复用连接池，指纹变化后立即重建。
    #[tokio::test]
    async fn refreshes_network_service_when_system_proxy_changes() {
        let state = AppSourceState::new();
        let config = native_http_config(&json!({}));
        let first = state
            .network_service_for_config(
                config.clone(),
                Some(proxy_state(1, true)),
                physical_state([192, 0, 2, 10]),
            )
            .await
            .expect("create initial system proxy service");
        let reused = state
            .network_service_for_config(
                config.clone(),
                Some(proxy_state(1, true)),
                physical_state([192, 0, 2, 10]),
            )
            .await
            .expect("reuse unchanged system proxy service");
        let refreshed = state
            .network_service_for_config(
                config,
                Some(proxy_state(2, false)),
                physical_state([192, 0, 2, 10]),
            )
            .await
            .expect("refresh changed system proxy service");

        assert!(Arc::ptr_eq(&first, &reused));
        assert!(!Arc::ptr_eq(&first, &refreshed));
    }

    /// 验证关闭和手动代理模式不会因系统代理指纹变化而重建连接池。
    #[tokio::test]
    async fn ignores_system_proxy_changes_outside_system_mode() {
        for settings in [
            json!({"network": {"metadataProxy": {"mode": "off"}}}),
            json!({
                "network": {
                    "metadataProxy": {
                        "mode": "manual",
                        "url": "http://127.0.0.1:7890"
                    }
                }
            }),
        ] {
            let state = AppSourceState::new();
            let config = native_http_config(&settings);
            let first = state
                .network_service_for_config(
                    config.clone(),
                    Some(proxy_state(1, true)),
                    physical_state([192, 0, 2, 10]),
                )
                .await
                .expect("create non-system proxy service");
            let reused = state
                .network_service_for_config(
                    config,
                    Some(proxy_state(2, false)),
                    physical_state([192, 0, 2, 10]),
                )
                .await
                .expect("reuse non-system proxy service");

            assert!(Arc::ptr_eq(&first, &reused));
        }
    }

    /// 验证默认物理出口变化后立即重建来源连接池。
    #[tokio::test]
    async fn refreshes_network_service_when_physical_route_changes() {
        let state = AppSourceState::new();
        let config = native_http_config(&json!({"network": {"metadataProxy": {"mode": "off"}}}));
        let first = state
            .network_service_for_config(config.clone(), None, physical_state([192, 0, 2, 10]))
            .await
            .expect("create first physical network service");
        let refreshed = state
            .network_service_for_config(config, None, physical_state([198, 51, 100, 20]))
            .await
            .expect("refresh changed physical network service");

        assert!(!Arc::ptr_eq(&first, &refreshed));
    }
}
