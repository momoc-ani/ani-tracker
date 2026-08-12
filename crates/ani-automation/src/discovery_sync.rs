use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ani_domain::{
    is_restricted_anime_content, Anime, AnimeDetailRefreshState, AnimeDiscoverySeasonQuery,
    AnimeDiscoverySeasonResult, AnimeSeasonSyncState,
};
use ani_repository::{
    AnimeCatalogRepository, AnimeCatalogWriteResult, ReleaseSourceRepository, RepositoryResult,
};
use ani_sources::{
    detail_requests_for_items, AnimeMetadataDetailCollection, AnimeMetadataDetailProvider,
    AnimeMetadataDetailProviderOutcome, AnimeMetadataDetailRequest, AnimeMetadataService,
    CircuitStateStore, SourceError, SourceNetworkService,
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, SecondsFormat, Utc};

const DETAIL_COMPENSATION_MAX_ATTEMPTS: usize = 3;
const DETAIL_COMPENSATION_MIN_DELAY_MS: u64 = 500;
const DETAIL_COMPENSATION_MAX_DELAY_MS: u64 = 120_500;
const DETAIL_CORRECTION_BATCH_SIZE: usize = 12;
const DETAIL_RETRY_DELAYS_MINUTES: [i64; 4] = [30, 120, 360, 720];

/// 新番季度同步所需的目录与网络状态窄端口。
pub trait AnimeDiscoverySyncStore: CircuitStateStore {
    /// 读取指定季度同步状态。
    fn get_season_sync_state(
        &self,
        year: i64,
        season: &str,
    ) -> RepositoryResult<Option<AnimeSeasonSyncState>>;

    /// 保存指定季度同步状态。
    fn save_season_sync_state(&self, state: &AnimeSeasonSyncState) -> RepositoryResult<()>;

    /// 合并采集到的季度目录。
    fn save_season_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult>;

    /// 合并详情补全结果并更新时间戳。
    fn save_detail_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult>;

    /// 读取指定月份目录。
    fn list_season_catalog_month(&self, year: i64, month: i64) -> RepositoryResult<Vec<Anime>>;

    /// 读取全部目录，供周期详情矫正生成分片计划。
    fn list_all_season_catalog(&self) -> RepositoryResult<Vec<Anime>>;

    /// 读取全部来源级详情刷新状态。
    fn list_detail_refresh_states(&self) -> RepositoryResult<Vec<AnimeDetailRefreshState>>;

    /// 批量保存来源级详情刷新状态。
    fn save_detail_refresh_states(
        &self,
        states: &[AnimeDetailRefreshState],
    ) -> RepositoryResult<()>;
}

impl<T> AnimeDiscoverySyncStore for T
where
    T: AnimeCatalogRepository + ReleaseSourceRepository,
{
    fn get_season_sync_state(
        &self,
        year: i64,
        season: &str,
    ) -> RepositoryResult<Option<AnimeSeasonSyncState>> {
        AnimeCatalogRepository::get_anime_season_sync_state(self, year, season)
    }

    fn save_season_sync_state(&self, state: &AnimeSeasonSyncState) -> RepositoryResult<()> {
        AnimeCatalogRepository::upsert_anime_season_sync_state(self, state)
    }

    fn save_season_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult> {
        AnimeCatalogRepository::upsert_anime_catalog(self, items)
    }

    fn save_detail_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult> {
        AnimeCatalogRepository::upsert_anime_catalog_details(self, items)
    }

    fn list_season_catalog_month(&self, year: i64, month: i64) -> RepositoryResult<Vec<Anime>> {
        AnimeCatalogRepository::list_anime_catalog(self, Some(year), Some(month))
    }

    fn list_all_season_catalog(&self) -> RepositoryResult<Vec<Anime>> {
        AnimeCatalogRepository::list_anime_catalog(self, None, None)
    }

    fn list_detail_refresh_states(&self) -> RepositoryResult<Vec<AnimeDetailRefreshState>> {
        AnimeCatalogRepository::list_anime_detail_refresh_states(self)
    }

    fn save_detail_refresh_states(
        &self,
        states: &[AnimeDetailRefreshState],
    ) -> RepositoryResult<()> {
        AnimeCatalogRepository::upsert_anime_detail_refresh_states(self, states)
    }
}

/// 复用同一季度采集与持久化流程，支持交互和后台独立网络通道。
pub struct AnimeDiscoverySyncService {
    collector: AnimeMetadataService,
}

/// 一批季度详情补全及持久化结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeDiscoveryDetailBatchResult {
    pub completed_count: usize,
    pub error_count: usize,
    pub retryable_requests: Vec<AnimeMetadataDetailRequest>,
}

/// 首次全量或后续增量详情请求计划。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimeDiscoveryDetailPlan {
    pub requests: Vec<AnimeMetadataDetailRequest>,
    pub skipped_count: usize,
    pub full_refresh: bool,
}

/// 一次七天分片详情矫正结果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnimeDiscoveryDetailCorrectionResult {
    pub planned_count: usize,
    pub completed_count: usize,
    pub error_count: usize,
}

impl AnimeDiscoverySyncService {
    /// 创建手动采集服务。
    pub fn new(network: Arc<SourceNetworkService>) -> Self {
        Self {
            collector: AnimeMetadataService::new(network),
        }
    }

    /// 创建后台采集服务，避免采集间隔阻塞用户搜索。
    pub fn new_background(network: Arc<SourceNetworkService>) -> Self {
        Self {
            collector: AnimeMetadataService::new_background(network),
        }
    }

    /// 完整同步一个季度，供需要等待详情的兼容调用复用。
    pub async fn sync_season<S>(
        &self,
        store: &S,
        query: AnimeDiscoverySeasonQuery,
        now: Option<DateTime<Utc>>,
    ) -> Result<AnimeDiscoverySeasonResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let run_at = now.unwrap_or_else(Utc::now);
        let full_refresh = query.force_refresh
            || store
                .get_season_sync_state(query.year, &query.season)?
                .and_then(|state| state.completed_at)
                .is_none();
        let mut result = self.sync_season_catalog(store, query, Some(run_at)).await?;
        let plan = self.plan_season_details(store, &result.items, full_refresh)?;
        if !plan.requests.is_empty() {
            let detail = self.enrich_detail_requests(store, &plan.requests).await?;
            if detail.error_count > 0 {
                result.errors.push(format!(
                    "details: {} 个来源详情补全失败",
                    detail.error_count
                ));
            }
            let months = months_for_season(&result.query.season)?;
            result.items.clear();
            for month in months {
                result
                    .items
                    .extend(store.list_season_catalog_month(result.query.year, month)?);
            }
            result
                .items
                .retain(|item| !is_restricted_anime_content(item));
        }
        Ok(result)
    }

    /// 根据首次同步标记和来源级成功状态生成全量或增量详情计划。
    pub fn plan_season_details<S>(
        &self,
        store: &S,
        items: &[Anime],
        full_refresh: bool,
    ) -> Result<AnimeDiscoveryDetailPlan, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let states = store
            .list_detail_refresh_states()?
            .into_iter()
            .map(|state| ((state.anime_id.clone(), state.provider.clone()), state))
            .collect::<HashMap<_, _>>();
        Ok(build_detail_plan(items, &states, full_refresh))
    }

    /// 采集并替换季度基础目录；仅 AniList 成功才写入季度完成标记。
    pub async fn sync_season_catalog<S>(
        &self,
        store: &S,
        query: AnimeDiscoverySeasonQuery,
        now: Option<DateTime<Utc>>,
    ) -> Result<AnimeDiscoverySeasonResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let months = months_for_season(&query.season)?;
        let now = now.unwrap_or_else(Utc::now);
        let attempt_at = to_iso(now);
        let mut state = store
            .get_season_sync_state(query.year, &query.season)?
            .unwrap_or_else(|| AnimeSeasonSyncState {
                year: query.year,
                season: query.season.clone(),
                last_attempt_at: None,
                last_successful_sync_at: None,
                completed_at: None,
                last_anilist_error: None,
            });
        state.last_attempt_at = Some(attempt_at.clone());
        store.save_season_sync_state(&state)?;

        let collected = self
            .collector
            .collect_season_catalog(store, query.year, &query.season)
            .await?;
        for error in &collected.errors {
            log::warn!(
                "Rust 新番季度来源采集失败：year={}, season={}, error={}",
                query.year,
                query.season,
                error
            );
        }

        let anilist_succeeded = collected
            .successful_sources
            .iter()
            .any(|source| source == "anilist");
        let anilist_error = collected
            .errors
            .iter()
            .find(|error| error.starts_with("anilist:"))
            .cloned();
        let mut added_count = 0usize;
        let mut existing_count = 0usize;
        let catalog_write_started = Instant::now();
        if !collected.items.is_empty() {
            let persisted = store.save_season_catalog(&collected.items)?;
            added_count = persisted.added_count;
            existing_count = persisted.existing_count;
        }
        log::info!(
            "Rust 新番阶段耗时 phase=sqlite-catalog-upsert year={} season={} items={} added={} existing={} duration_ms={}",
            query.year,
            query.season,
            collected.items.len(),
            added_count,
            existing_count,
            catalog_write_started.elapsed().as_millis()
        );

        if anilist_succeeded {
            state.last_successful_sync_at = Some(attempt_at.clone());
            state.completed_at.get_or_insert(attempt_at);
            state.last_anilist_error = None;
        } else {
            state.last_anilist_error = anilist_error
                .clone()
                .or_else(|| Some("anilist: 未返回新番数据".to_owned()));
        }
        store.save_season_sync_state(&state)?;

        let catalog_read_started = Instant::now();
        let mut items = Vec::new();
        for month in months {
            items.extend(store.list_season_catalog_month(query.year, month)?);
        }
        log::info!(
            "Rust 新番阶段耗时 phase=sqlite-catalog-read year={} season={} items={} duration_ms={}",
            query.year,
            query.season,
            items.len(),
            catalog_read_started.elapsed().as_millis()
        );
        if collected.items.is_empty() {
            existing_count = items.len();
        }
        items.retain(|item| {
            item.premiere_year == query.year
                && months.contains(&item.premiere_month)
                && !is_restricted_anime_content(item)
        });
        log::info!(
            "Rust 新番季度同步完成：year={}, season={}, items={}, added={}, anilist_succeeded={}",
            query.year,
            query.season,
            items.len(),
            added_count,
            anilist_succeeded
        );
        Ok(AnimeDiscoverySeasonResult {
            query,
            items,
            added_count,
            existing_count,
            source: collected.source,
            errors: collected.errors,
        })
    }

    /// 补全一批目录详情并增量写回，不替换其他月份缓存。
    pub async fn enrich_detail_batch<S>(
        &self,
        store: &S,
        items: &[Anime],
    ) -> Result<AnimeDiscoveryDetailBatchResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        self.enrich_detail_requests(store, &detail_requests_for_items(items))
            .await
    }

    /// 补全一批已经过全量或增量筛选的来源级详情请求。
    pub async fn enrich_detail_requests<S>(
        &self,
        store: &S,
        requests: &[AnimeMetadataDetailRequest],
    ) -> Result<AnimeDiscoveryDetailBatchResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let initial = self.enrich_detail_requests_initial(store, requests).await?;
        if initial.retryable_requests.is_empty() {
            return Ok(initial);
        }
        let compensation = self
            .compensate_detail_requests(store, &initial.retryable_requests)
            .await?;
        Ok(AnimeDiscoveryDetailBatchResult {
            completed_count: initial
                .completed_count
                .saturating_add(compensation.completed_count),
            error_count: initial.error_count.saturating_add(compensation.error_count),
            retryable_requests: Vec::new(),
        })
    }

    /// 执行一批详情首轮补全，暂不等待瞬时失败来源恢复。
    pub async fn enrich_detail_batch_initial<S>(
        &self,
        store: &S,
        items: &[Anime],
    ) -> Result<AnimeDiscoveryDetailBatchResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        self.enrich_detail_requests_initial(store, &detail_requests_for_items(items))
            .await
    }

    /// 执行一批已规划详情请求的首轮补全，并记录来源级成功状态。
    pub async fn enrich_detail_requests_initial<S>(
        &self,
        store: &S,
        requests: &[AnimeMetadataDetailRequest],
    ) -> Result<AnimeDiscoveryDetailBatchResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        if requests.is_empty() {
            return Ok(AnimeDiscoveryDetailBatchResult {
                completed_count: 0,
                error_count: 0,
                retryable_requests: Vec::new(),
            });
        }
        let collected = self.collector.retry_details(store, requests).await;
        save_detail_items(store, &collected.items, "initial")?;
        save_detail_outcomes(store, requests, &collected, Utc::now())?;
        let deferred_count = collected.retryable_requests.len();
        log::info!(
            "Rust 新番季度详情首轮完成：count={}, completed={}, deferred={}, errors={}",
            requests.len(),
            requests.len().saturating_sub(deferred_count),
            deferred_count,
            collected.settled_error_count
        );
        Ok(AnimeDiscoveryDetailBatchResult {
            completed_count: requests.len().saturating_sub(deferred_count),
            error_count: collected.settled_error_count,
            retryable_requests: collected.retryable_requests,
        })
    }

    /// 在全部首轮请求结束后统一补偿来源级瞬时失败。
    pub async fn compensate_detail_requests<S>(
        &self,
        store: &S,
        requests: &[AnimeMetadataDetailRequest],
    ) -> Result<AnimeDiscoveryDetailBatchResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let total_count = requests.len();
        let mut pending = requests.to_vec();
        let mut error_count = 0usize;
        for attempt in 2..=DETAIL_COMPENSATION_MAX_ATTEMPTS {
            let delay_ms = pending
                .iter()
                .map(|request| request.retry_after_ms)
                .max()
                .unwrap_or_default()
                .clamp(
                    DETAIL_COMPENSATION_MIN_DELAY_MS,
                    DETAIL_COMPENSATION_MAX_DELAY_MS,
                );
            log::warn!(
                "Rust 新番季度详情统一补偿：attempt={attempt}, deferred={}, delay_ms={delay_ms}",
                pending.len()
            );
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;

            let collected = self.collector.retry_details(store, &pending).await;
            save_detail_items(store, &collected.items, &format!("retry-{attempt}"))?;
            save_detail_outcomes(store, &pending, &collected, Utc::now())?;
            error_count = error_count.saturating_add(collected.settled_error_count);
            let deferred_error_count = collected.deferred_error_count;
            pending = collected.retryable_requests;
            if pending.is_empty() {
                log::info!(
                    "Rust 新番季度详情统一补偿完成：count={total_count}, errors={error_count}, attempts={attempt}"
                );
                return Ok(AnimeDiscoveryDetailBatchResult {
                    completed_count: total_count,
                    error_count,
                    retryable_requests: Vec::new(),
                });
            }
            if attempt == DETAIL_COMPENSATION_MAX_ATTEMPTS {
                error_count = error_count.saturating_add(deferred_error_count);
            }
        }

        save_deferred_failures(store, &pending, Utc::now())?;

        log::warn!(
            "Rust 新番季度详情统一补偿耗尽：count={total_count}, deferred={}, errors={error_count}",
            pending.len()
        );
        Ok(AnimeDiscoveryDetailBatchResult {
            completed_count: total_count,
            error_count,
            retryable_requests: Vec::new(),
        })
    }

    /// 按稳定七天分片补偿详情，遗漏周期和失败来源会优先补跑。
    pub async fn correct_due_details<S>(
        &self,
        store: &S,
        now: DateTime<Utc>,
    ) -> Result<AnimeDiscoveryDetailCorrectionResult, SourceError>
    where
        S: AnimeDiscoverySyncStore + Sync,
    {
        let items = store.list_all_season_catalog()?;
        let states = store
            .list_detail_refresh_states()?
            .into_iter()
            .map(|state| ((state.anime_id.clone(), state.provider.clone()), state))
            .collect::<HashMap<_, _>>();
        let (cycle, cycle_day) = detail_cycle_position(now.date_naive());
        let now_text = to_iso(now);
        let mut overdue_count = 0usize;
        let mut requests = detail_requests_for_items(&items)
            .into_iter()
            .filter_map(|mut request| {
                request.providers.retain(|provider| {
                    let provider_name = provider.as_str();
                    let external_id = detail_external_id(&request.item, *provider);
                    let state = states.get(&(request.item.id.clone(), provider_name.to_owned()));
                    let due = is_detail_correction_due(
                        state,
                        external_id.as_deref(),
                        cycle,
                        cycle_day,
                        &now_text,
                    );
                    if due
                        && state
                            .and_then(|state| state.last_completed_cycle)
                            .is_none_or(|completed| completed < cycle.saturating_sub(1))
                    {
                        overdue_count += 1;
                    }
                    due
                });
                (!request.providers.is_empty()).then_some(request)
            })
            .collect::<Vec<_>>();
        requests.sort_by_key(|request| correction_order_key(request, cycle));
        let planned_count = requests.len();
        log::info!(
            "Rust 新番周期详情矫正计划 cycle={cycle} day={cycle_day} catalog={} planned={planned_count} overdue={overdue_count}",
            items.len()
        );
        if requests.is_empty() {
            return Ok(AnimeDiscoveryDetailCorrectionResult::default());
        }

        let mut completed_count = 0usize;
        let mut error_count = 0usize;
        let mut retryable_requests = Vec::new();
        for chunk in requests.chunks(DETAIL_CORRECTION_BATCH_SIZE) {
            let batch = self.enrich_detail_requests_initial(store, chunk).await?;
            completed_count = completed_count.saturating_add(batch.completed_count);
            error_count = error_count.saturating_add(batch.error_count);
            retryable_requests.extend(batch.retryable_requests);
        }
        if !retryable_requests.is_empty() {
            let compensation = self
                .compensate_detail_requests(store, &retryable_requests)
                .await?;
            completed_count = completed_count.saturating_add(compensation.completed_count);
            error_count = error_count.saturating_add(compensation.error_count);
        }
        log::info!(
            "Rust 新番周期详情矫正完成 cycle={cycle} planned={planned_count} completed={completed_count} errors={error_count}"
        );
        Ok(AnimeDiscoveryDetailCorrectionResult {
            planned_count,
            completed_count,
            error_count,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncrementalDetailReason {
    Missing,
    ExternalIdChanged,
    Failed,
    Complete,
}

/// 使用来源级完成状态生成可测试的全量或增量详情计划。
fn build_detail_plan(
    items: &[Anime],
    states: &HashMap<(String, String), AnimeDetailRefreshState>,
    full_refresh: bool,
) -> AnimeDiscoveryDetailPlan {
    let mut skipped_count = 0usize;
    let mut missing_count = 0usize;
    let mut changed_id_count = 0usize;
    let mut failed_count = 0usize;
    let requests = detail_requests_for_items(items)
        .into_iter()
        .filter_map(|mut request| {
            if !full_refresh {
                request.providers.retain(|provider| {
                    let provider_name = provider.as_str();
                    let external_id = detail_external_id(&request.item, *provider);
                    let Some(state) =
                        states.get(&(request.item.id.clone(), provider_name.to_owned()))
                    else {
                        missing_count += 1;
                        return true;
                    };
                    match incremental_detail_reason(state, external_id.as_deref()) {
                        IncrementalDetailReason::ExternalIdChanged => {
                            changed_id_count += 1;
                            true
                        }
                        IncrementalDetailReason::Missing => {
                            missing_count += 1;
                            true
                        }
                        IncrementalDetailReason::Failed => {
                            failed_count += 1;
                            true
                        }
                        IncrementalDetailReason::Complete => {
                            skipped_count += 1;
                            false
                        }
                    }
                });
            }
            (!request.providers.is_empty()).then_some(request)
        })
        .collect::<Vec<_>>();
    log::info!(
        "Rust 新番详情增量计划 mode={} items={} requests={} skipped={} missing={} external_id_changed={} failed={}",
        if full_refresh { "full" } else { "incremental" },
        items.len(),
        requests.len(),
        skipped_count,
        missing_count,
        changed_id_count,
        failed_count
    );
    AnimeDiscoveryDetailPlan {
        requests,
        skipped_count,
        full_refresh,
    }
}

/// 判断已有来源状态是否仍需进入本次增量详情队列。
fn incremental_detail_reason(
    state: &AnimeDetailRefreshState,
    external_id: Option<&str>,
) -> IncrementalDetailReason {
    if external_id != Some(state.external_id.as_str()) {
        IncrementalDetailReason::ExternalIdChanged
    } else if state.last_success_at.is_none() {
        IncrementalDetailReason::Missing
    } else if state.failure_count > 0 {
        IncrementalDetailReason::Failed
    } else {
        IncrementalDetailReason::Complete
    }
}

/// 读取单个详情来源当前使用的外部标识。
fn detail_external_id(anime: &Anime, provider: AnimeMetadataDetailProvider) -> Option<String> {
    anime
        .external_ids
        .get(provider.as_str())
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 将成功和不可重试失败写入来源级周期状态。
fn save_detail_outcomes<S>(
    store: &S,
    requests: &[AnimeMetadataDetailRequest],
    collected: &AnimeMetadataDetailCollection,
    now: DateTime<Utc>,
) -> Result<(), SourceError>
where
    S: AnimeDiscoverySyncStore + Sync,
{
    let request_ids = detail_request_external_ids(requests);
    let mut states = store
        .list_detail_refresh_states()?
        .into_iter()
        .map(|state| ((state.anime_id.clone(), state.provider.clone()), state))
        .collect::<HashMap<_, _>>();
    let mut changed = HashMap::new();
    for outcome in &collected.successful_providers {
        if let Some(external_id) = request_ids.get(&detail_outcome_key(outcome)) {
            let state = updated_detail_success_state(
                states.remove(&detail_outcome_key(outcome)),
                outcome,
                external_id,
                now,
            );
            changed.insert(detail_outcome_key(outcome), state);
        }
    }
    for outcome in &collected.settled_failed_providers {
        if let Some(external_id) = request_ids.get(&detail_outcome_key(outcome)) {
            let key = detail_outcome_key(outcome);
            let current = changed.remove(&key).or_else(|| states.remove(&key));
            changed.insert(
                key,
                updated_detail_failure_state(current, outcome, external_id, now),
            );
        }
    }
    if !changed.is_empty() {
        store.save_detail_refresh_states(&changed.into_values().collect::<Vec<_>>())?;
    }
    Ok(())
}

/// 将最终仍处于瞬时失败的来源写入退避状态。
fn save_deferred_failures<S>(
    store: &S,
    requests: &[AnimeMetadataDetailRequest],
    now: DateTime<Utc>,
) -> Result<(), SourceError>
where
    S: AnimeDiscoverySyncStore + Sync,
{
    if requests.is_empty() {
        return Ok(());
    }
    let mut states = store
        .list_detail_refresh_states()?
        .into_iter()
        .map(|state| ((state.anime_id.clone(), state.provider.clone()), state))
        .collect::<HashMap<_, _>>();
    let mut changed = Vec::new();
    for request in requests {
        for provider in &request.providers {
            let Some(external_id) = detail_external_id(&request.item, *provider) else {
                continue;
            };
            let outcome = AnimeMetadataDetailProviderOutcome {
                anime_id: request.item.id.clone(),
                provider: *provider,
            };
            let key = detail_outcome_key(&outcome);
            changed.push(updated_detail_failure_state(
                states.remove(&key),
                &outcome,
                &external_id,
                now,
            ));
        }
    }
    store.save_detail_refresh_states(&changed)?;
    Ok(())
}

/// 建立详情请求到当前外部标识的索引。
fn detail_request_external_ids(
    requests: &[AnimeMetadataDetailRequest],
) -> HashMap<(String, String), String> {
    requests
        .iter()
        .flat_map(|request| {
            request.providers.iter().filter_map(|provider| {
                detail_external_id(&request.item, *provider).map(|external_id| {
                    (
                        (request.item.id.clone(), provider.as_str().to_owned()),
                        external_id,
                    )
                })
            })
        })
        .collect()
}

/// 返回来源结果对应的状态主键。
fn detail_outcome_key(outcome: &AnimeMetadataDetailProviderOutcome) -> (String, String) {
    (
        outcome.anime_id.clone(),
        outcome.provider.as_str().to_owned(),
    )
}

/// 基于成功详情构建新的周期完成状态。
fn updated_detail_success_state(
    existing: Option<AnimeDetailRefreshState>,
    outcome: &AnimeMetadataDetailProviderOutcome,
    external_id: &str,
    now: DateTime<Utc>,
) -> AnimeDetailRefreshState {
    let (cycle, _) = detail_cycle_position(now.date_naive());
    let mut state = existing.unwrap_or_else(|| empty_detail_refresh_state(outcome, external_id));
    state.anime_id.clone_from(&outcome.anime_id);
    state.provider = outcome.provider.as_str().to_owned();
    state.external_id = external_id.to_owned();
    state.slot_day = stable_detail_slot(&outcome.anime_id, outcome.provider);
    state.last_completed_cycle = Some(cycle);
    state.last_attempt_at = Some(to_iso(now));
    state.last_success_at = Some(to_iso(now));
    state.failure_count = 0;
    state.next_retry_at = None;
    state
}

/// 基于失败详情构建保留历史成功时间的退避状态。
fn updated_detail_failure_state(
    existing: Option<AnimeDetailRefreshState>,
    outcome: &AnimeMetadataDetailProviderOutcome,
    external_id: &str,
    now: DateTime<Utc>,
) -> AnimeDetailRefreshState {
    let mut state = existing.unwrap_or_else(|| empty_detail_refresh_state(outcome, external_id));
    state.external_id = external_id.to_owned();
    state.slot_day = stable_detail_slot(&outcome.anime_id, outcome.provider);
    state.last_attempt_at = Some(to_iso(now));
    state.failure_count = state.failure_count.saturating_add(1);
    let delay_index = usize::try_from(state.failure_count.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(DETAIL_RETRY_DELAYS_MINUTES.len() - 1);
    state.next_retry_at = Some(to_iso(
        now + ChronoDuration::minutes(DETAIL_RETRY_DELAYS_MINUTES[delay_index]),
    ));
    state
}

/// 创建尚未完成过详情刷新的初始来源状态。
fn empty_detail_refresh_state(
    outcome: &AnimeMetadataDetailProviderOutcome,
    external_id: &str,
) -> AnimeDetailRefreshState {
    AnimeDetailRefreshState {
        anime_id: outcome.anime_id.clone(),
        provider: outcome.provider.as_str().to_owned(),
        external_id: external_id.to_owned(),
        slot_day: stable_detail_slot(&outcome.anime_id, outcome.provider),
        last_completed_cycle: None,
        last_attempt_at: None,
        last_success_at: None,
        failure_count: 0,
        next_retry_at: None,
    }
}

/// 判断单个来源在当前周期是否应该进入矫正队列。
fn is_detail_correction_due(
    state: Option<&AnimeDetailRefreshState>,
    external_id: Option<&str>,
    cycle: i64,
    cycle_day: i64,
    now: &str,
) -> bool {
    let Some(state) = state else {
        return external_id.is_some();
    };
    if external_id != Some(state.external_id.as_str()) || state.last_success_at.is_none() {
        return state
            .next_retry_at
            .as_deref()
            .is_none_or(|retry_at| retry_at <= now);
    }
    if state.failure_count > 0 {
        return state
            .next_retry_at
            .as_deref()
            .is_none_or(|retry_at| retry_at <= now);
    }
    match state.last_completed_cycle {
        Some(completed) if completed >= cycle => false,
        Some(completed) if completed == cycle.saturating_sub(1) => cycle_day >= state.slot_day,
        _ => true,
    }
}

/// 将日期转换为从固定星期一起点开始的周期号和周期内天数。
fn detail_cycle_position(date: NaiveDate) -> (i64, i64) {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 5).expect("详情周期起点必须有效");
    let days = date.signed_duration_since(epoch).num_days();
    (days.div_euclid(7), days.rem_euclid(7))
}

/// 使用稳定 FNV-1a 哈希把番剧来源分配到一周七天。
fn stable_detail_slot(anime_id: &str, provider: AnimeMetadataDetailProvider) -> i64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in anime_id
        .as_bytes()
        .iter()
        .chain(provider.as_str().as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    i64::try_from(hash % 7).unwrap_or_default()
}

/// 返回周期内稳定但跨周期变化的矫正顺序。
fn correction_order_key(request: &AnimeMetadataDetailRequest, cycle: i64) -> u64 {
    stable_detail_hash(&[
        &request.item.id,
        request
            .providers
            .first()
            .map_or("", |provider| provider.as_str()),
        &cycle.to_string(),
    ])
}

/// 计算无需随机种子的稳定 64 位哈希。
fn stable_detail_hash(parts: &[&str]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for part in parts {
        for byte in part.as_bytes().iter().chain(std::iter::once(&0xff)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// 增量保存详情补全结果并记录 SQLite 分段耗时。
fn save_detail_items<S>(store: &S, items: &[Anime], stage: &str) -> Result<(), SourceError>
where
    S: AnimeDiscoverySyncStore + Sync,
{
    if items.is_empty() {
        return Ok(());
    }
    let detail_write_started = Instant::now();
    let persisted = store.save_detail_catalog(items)?;
    log::info!(
        "Rust 新番阶段耗时 phase=sqlite-detail-write stage={stage} items={} added={} existing={} duration_ms={}",
        items.len(),
        persisted.added_count,
        persisted.existing_count,
        detail_write_started.elapsed().as_millis()
    );
    Ok(())
}

/// 返回季度对应的三个自然月。
pub fn months_for_season(season: &str) -> Result<[i64; 3], SourceError> {
    match season {
        "winter" => Ok([1, 2, 3]),
        "spring" => Ok([4, 5, 6]),
        "summer" => Ok([7, 8, 9]),
        "fall" => Ok([10, 11, 12]),
        _ => Err(SourceError::Parse(format!("季度无效：{season}"))),
    }
}

/// 将 UTC 时间序列化为毫秒精度 ISO 字符串。
fn to_iso(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ani_domain::{Anime, AnimeDetailRefreshState};
    use ani_sources::AnimeMetadataDetailProvider;
    use chrono::NaiveDate;
    use serde_json::json;

    use super::{
        build_detail_plan, detail_cycle_position, incremental_detail_reason,
        is_detail_correction_due, stable_detail_slot, IncrementalDetailReason,
    };

    /// 创建仅含详情计划必要字段的季度目录记录。
    fn detail_plan_anime(id: &str, provider: &str, external_id: &str) -> Anime {
        let mut external_ids = serde_json::Map::new();
        external_ids.insert(provider.to_owned(), json!(external_id));
        Anime {
            id: id.to_owned(),
            title: format!("测试番剧 {id}"),
            original_title: None,
            aliases: Vec::new(),
            premiere_date: Some("2026-07-01".to_owned()),
            premiere_year: 2026,
            premiere_month: 7,
            season: Some("summer".to_owned()),
            summary: None,
            cover_url: None,
            rating: None,
            external_ids: serde_json::Value::Object(external_ids),
            detail: None,
        }
    }

    /// 创建已经成功补全的来源状态。
    fn completed_detail_state(
        anime_id: &str,
        provider: &str,
        external_id: &str,
    ) -> AnimeDetailRefreshState {
        AnimeDetailRefreshState {
            anime_id: anime_id.to_owned(),
            provider: provider.to_owned(),
            external_id: external_id.to_owned(),
            slot_day: 3,
            last_completed_cycle: Some(2951),
            last_attempt_at: Some("2026-07-28T00:00:00.000Z".to_owned()),
            last_success_at: Some("2026-07-28T00:00:00.000Z".to_owned()),
            failure_count: 0,
            next_retry_at: None,
        }
    }

    /// 验证相同番剧来源始终进入同一分片，且样本能够覆盖七天。
    #[test]
    fn assigns_stable_detail_slots_across_seven_days() {
        let slot = stable_detail_slot("bangumi-100", AnimeMetadataDetailProvider::Bangumi);
        assert_eq!(slot, 0);
        assert_eq!(
            slot,
            stable_detail_slot("bangumi-100", AnimeMetadataDetailProvider::Bangumi)
        );
        assert!((0..=6).contains(&slot));
        let slots = (0..100)
            .map(|index| {
                stable_detail_slot(
                    &format!("bangumi-{index}"),
                    AnimeMetadataDetailProvider::Bangumi,
                )
            })
            .collect::<HashSet<_>>();
        assert_eq!(slots.len(), 7);
    }

    /// 验证增量同步跳过已成功来源，并补齐缺失、失败和外部标识变化来源。
    #[test]
    fn selects_only_incomplete_incremental_details() {
        let mut state = AnimeDetailRefreshState {
            anime_id: "bangumi-100".to_owned(),
            provider: "bangumi".to_owned(),
            external_id: "100".to_owned(),
            slot_day: 3,
            last_completed_cycle: Some(2951),
            last_attempt_at: Some("2026-07-28T00:00:00.000Z".to_owned()),
            last_success_at: Some("2026-07-28T00:00:00.000Z".to_owned()),
            failure_count: 0,
            next_retry_at: None,
        };
        assert_eq!(
            incremental_detail_reason(&state, Some("100")),
            IncrementalDetailReason::Complete
        );
        assert_eq!(
            incremental_detail_reason(&state, Some("101")),
            IncrementalDetailReason::ExternalIdChanged
        );
        state.last_success_at = None;
        assert_eq!(
            incremental_detail_reason(&state, Some("100")),
            IncrementalDetailReason::Missing
        );
        state.last_success_at = Some("2026-07-28T00:00:00.000Z".to_owned());
        state.failure_count = 1;
        assert_eq!(
            incremental_detail_reason(&state, Some("100")),
            IncrementalDetailReason::Failed
        );
    }

    /// 验证首次全量请求全部必要来源，后续增量仅重试变化或未完成来源。
    #[test]
    fn builds_full_and_incremental_detail_plans() {
        let items = vec![
            detail_plan_anime("anime-bangumi", "bangumi", "100"),
            detail_plan_anime("anime-mikan", "mikan", "200"),
        ];
        let states = [
            completed_detail_state("anime-bangumi", "bangumi", "100"),
            completed_detail_state("anime-mikan", "mikan", "200"),
        ]
        .into_iter()
        .map(|state| ((state.anime_id.clone(), state.provider.clone()), state))
        .collect::<HashMap<_, _>>();

        let full = build_detail_plan(&items, &states, true);
        assert!(full.full_refresh);
        assert_eq!(full.requests.len(), 2);
        assert_eq!(full.skipped_count, 0);

        let unchanged = build_detail_plan(&items, &states, false);
        assert!(!unchanged.full_refresh);
        assert!(unchanged.requests.is_empty());
        assert_eq!(unchanged.skipped_count, 2);

        let mut changed = items;
        changed[0].external_ids = json!({"bangumi": "101"});
        let incremental = build_detail_plan(&changed, &states, false);
        assert_eq!(incremental.requests.len(), 1);
        assert_eq!(incremental.requests[0].item.id, "anime-bangumi");
        assert_eq!(
            incremental.requests[0].providers,
            vec![AnimeMetadataDetailProvider::Bangumi]
        );
    }

    /// 验证当天分片、漏掉整个周期和失败退避的优先级。
    #[test]
    fn selects_due_and_overdue_detail_corrections() {
        let monday = NaiveDate::from_ymd_opt(2026, 7, 27).expect("monday");
        let (cycle, day) = detail_cycle_position(monday);
        assert_eq!(day, 0);
        let base = AnimeDetailRefreshState {
            anime_id: "bangumi-100".to_owned(),
            provider: "bangumi".to_owned(),
            external_id: "100".to_owned(),
            slot_day: 3,
            last_completed_cycle: Some(cycle - 1),
            last_attempt_at: None,
            last_success_at: Some("2026-07-23T00:00:00.000Z".to_owned()),
            failure_count: 0,
            next_retry_at: None,
        };
        assert!(!is_detail_correction_due(
            Some(&base),
            Some("100"),
            cycle,
            day,
            "2026-07-27T00:00:00.000Z"
        ));
        assert!(is_detail_correction_due(
            Some(&base),
            Some("100"),
            cycle,
            3,
            "2026-07-30T00:00:00.000Z"
        ));

        let mut overdue = base.clone();
        overdue.last_completed_cycle = Some(cycle - 2);
        assert!(is_detail_correction_due(
            Some(&overdue),
            Some("100"),
            cycle,
            day,
            "2026-07-27T00:00:00.000Z"
        ));

        let mut failed = overdue;
        failed.failure_count = 1;
        failed.next_retry_at = Some("2026-07-27T01:00:00.000Z".to_owned());
        assert!(!is_detail_correction_due(
            Some(&failed),
            Some("100"),
            cycle,
            day,
            "2026-07-27T00:00:00.000Z"
        ));
    }
}
