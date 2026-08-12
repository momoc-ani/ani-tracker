use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use ani_domain::{
    Anime, AnimeSourceBinding, AnimeSourceBindingMatchMethod, AnimeSourceBindingState,
    AnimeSourceCandidate, AnimeSourceExclusion, AnimeSourceExclusionScope,
    ConfirmAnimeSourceBindingInput, Episode, ExcludedAnimeSource, MyAnime, ReleaseSearchError,
    ReleaseSourceConfig, RemoveAnimeSourceCandidateMismatchInput,
    ReportAnimeSourceCandidateMismatchInput, SetAnimeSourceExclusionInput, SourceKind,
};
use ani_repository::{
    AnimeSourceBindingRepository, AnimeTrackingRepository, ReleaseSourceRepository,
    RepositoryError, RepositoryResult,
};
use chrono::{SecondsFormat, Utc};
use futures_util::future::join_all;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::release::normalize_release_search_text;
use crate::search::{create_anibt_headers, is_anibt_config, is_mikan_config, is_mikan_site_config};
use crate::{CircuitStateStore, HttpMethod, NativeHttpRequest, SourceError, SourceNetworkService};

const MAX_CANDIDATES_PER_SOURCE: usize = 6;
const MAX_ANIBT_CANDIDATE_TERMS: usize = 4;

/// 来源绑定服务需要的最小业务存储端口。
pub trait AnimeSourceBindingStore: CircuitStateStore {
    /// 读取全部追番。
    fn list_followed_anime(&self) -> RepositoryResult<Vec<MyAnime>>;

    /// 读取全部来源配置。
    fn list_binding_sources(&self) -> RepositoryResult<Vec<ReleaseSourceConfig>>;

    /// 读取指定番剧的单集。
    fn list_binding_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>>;

    /// 读取指定番剧的来源绑定。
    fn list_bindings(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceBinding>>;

    /// 保存一条来源绑定。
    fn save_binding(
        &self,
        binding: &AnimeSourceBinding,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>>;

    /// 读取指定番剧的来源排除记录。
    fn list_exclusions(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceExclusion>>;

    /// 保存一条来源排除记录。
    fn save_exclusion(
        &self,
        exclusion: &AnimeSourceExclusion,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>>;

    /// 删除一条候选或整来源排除记录。
    fn delete_exclusion(
        &self,
        anime_id: &str,
        source_id: &str,
        source_anime_id: Option<&str>,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>>;
}

impl<T> AnimeSourceBindingStore for T
where
    T: AnimeSourceBindingRepository
        + AnimeTrackingRepository
        + ReleaseSourceRepository
        + CircuitStateStore,
{
    fn list_followed_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
        AnimeTrackingRepository::list_my_anime(self)
    }

    fn list_binding_sources(&self) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
        ReleaseSourceRepository::list_sources(self)
    }

    fn list_binding_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
        AnimeTrackingRepository::list_episodes(self, anime_id)
    }

    fn list_bindings(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        AnimeSourceBindingRepository::list_anime_source_bindings(self, anime_id)
    }

    fn save_binding(
        &self,
        binding: &AnimeSourceBinding,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        AnimeSourceBindingRepository::upsert_anime_source_binding(self, binding)
    }

    fn list_exclusions(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        AnimeSourceBindingRepository::list_anime_source_exclusions(self, anime_id)
    }

    fn save_exclusion(
        &self,
        exclusion: &AnimeSourceExclusion,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        AnimeSourceBindingRepository::upsert_anime_source_exclusion(self, exclusion)
    }

    fn delete_exclusion(
        &self,
        anime_id: &str,
        source_id: &str,
        source_anime_id: Option<&str>,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        AnimeSourceBindingRepository::remove_anime_source_exclusion(
            self,
            anime_id,
            source_id,
            source_anime_id,
        )
    }
}

/// 组合来源绑定持久化、外部 ID 同步、候选发现和候选评分。
pub struct AnimeSourceBindingService {
    network: Arc<SourceNetworkService>,
}

impl AnimeSourceBindingService {
    /// 创建复用来源连接池和熔断状态的绑定服务。
    pub fn new(network: Arc<SourceNetworkService>) -> Self {
        Self { network }
    }

    /// 读取绑定状态，并按需从 AniBT 和 Mikan 发现候选。
    pub async fn get_state<S>(
        &self,
        store: &S,
        anime_id: &str,
        discover_candidates: bool,
    ) -> Result<AnimeSourceBindingState, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        let anime = find_anime(store, anime_id)?;
        let sources = store.list_binding_sources()?;
        let episodes = store.list_binding_episodes(anime_id)?;
        let mut bindings = sync_external_id_bindings(store, &anime, &sources)?;
        let exclusions = store.list_exclusions(anime_id)?;
        let excluded_source_ids = exclusions
            .iter()
            .filter(|item| item.scope == AnimeSourceExclusionScope::Source)
            .map(|item| item.source_id.as_str())
            .collect::<HashSet<_>>();
        let excluded_candidate_keys = exclusions
            .iter()
            .filter(|item| item.scope == AnimeSourceExclusionScope::Candidate)
            .filter_map(|item| {
                item.source_anime_id
                    .as_deref()
                    .map(|id| candidate_key(&item.source_id, id))
            })
            .collect::<HashSet<_>>();
        let excluded_sources = sources
            .iter()
            .filter(|source| {
                source.enabled
                    && is_bindable_source(source)
                    && excluded_source_ids.contains(source.id.as_str())
            })
            .map(|source| ExcludedAnimeSource {
                source_id: source.id.clone(),
                source_name: source.name.clone(),
            })
            .collect();
        if !discover_candidates {
            return Ok(AnimeSourceBindingState {
                anime_id: anime_id.to_owned(),
                bindings,
                candidates: Vec::new(),
                excluded_sources,
                errors: Vec::new(),
            });
        }

        let bound_source_ids = bindings
            .iter()
            .filter(|binding| binding.confirmed)
            .map(|binding| binding.source_id.as_str())
            .collect::<HashSet<_>>();
        let targets = sources
            .iter()
            .filter(|source| {
                source.enabled
                    && is_bindable_source(source)
                    && !bound_source_ids.contains(source.id.as_str())
                    && !excluded_source_ids.contains(source.id.as_str())
            })
            .collect::<Vec<_>>();
        let discovered = join_all(targets.iter().map(|source| async {
            (
                *source,
                self.discover_source_candidates(store, &anime, source, &sources, episodes.len())
                    .await,
            )
        }))
        .await;
        let mut candidates = Vec::new();
        let mut errors = Vec::new();
        for (source, result) in discovered {
            match result {
                Ok(source_candidates) => {
                    let source_candidates = source_candidates
                        .into_iter()
                        .filter(|candidate| {
                            !excluded_candidate_keys.contains(&candidate_key(
                                &candidate.source_id,
                                &candidate.source_anime_id,
                            ))
                        })
                        .collect::<Vec<_>>();
                    let has_binding_record = bindings
                        .iter()
                        .any(|binding| binding.source_id == source.id);
                    if !has_binding_record {
                        if let Some(candidate) =
                            safe_auto_binding_candidate(&anime, &source_candidates)
                        {
                            bindings = save_scored_binding(store, &anime, candidate)?;
                            continue;
                        }
                    }
                    candidates.extend(source_candidates);
                }
                Err(error) => {
                    log::warn!(
                        "Rust 来源番剧候选发现失败：anime_id={}, source_id={}, error={}",
                        anime_id,
                        source.id,
                        error
                    );
                    errors.push(ReleaseSearchError {
                        source_id: source.id.clone(),
                        message: binding_error_message(&error),
                    });
                }
            }
        }
        candidates.sort_by_key(|candidate| Reverse(candidate.score));
        Ok(AnimeSourceBindingState {
            anime_id: anime_id.to_owned(),
            bindings: store.list_bindings(anime_id)?,
            candidates,
            excluded_sources,
            errors,
        })
    }

    /// 将用户确认的候选保存为稳定来源绑定。
    pub async fn confirm<S>(
        &self,
        store: &S,
        input: ConfirmAnimeSourceBindingInput,
    ) -> Result<AnimeSourceBindingState, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        let (_, _) = validate_anime_and_source(store, &input.anime_id, &input.source_id)?;
        let source_anime_id = require_text("sourceAnimeId", &input.source_anime_id)?;
        validate_optional_http_url(input.source_url.as_deref())?;
        let current = store.list_bindings(&input.anime_id)?;
        let existing = current
            .iter()
            .find(|binding| binding.source_id == input.source_id);
        let timestamp = now_iso();
        store.save_binding(&AnimeSourceBinding {
            id: existing.map_or_else(
                || binding_id(&input.anime_id, &input.source_id),
                |binding| binding.id.clone(),
            ),
            anime_id: input.anime_id.clone(),
            source_id: input.source_id.clone(),
            source_anime_id: source_anime_id.to_owned(),
            source_anime_title: normalized_optional_text(input.source_anime_title),
            source_url: normalized_optional_text(input.source_url),
            match_method: AnimeSourceBindingMatchMethod::Manual,
            confidence: input.confidence.unwrap_or(1.0).clamp(0.0, 1.0),
            confirmed: true,
            created_at: existing
                .map_or_else(|| timestamp.clone(), |binding| binding.created_at.clone()),
            updated_at: timestamp,
        })?;
        log::info!(
            "Rust 番剧来源绑定已确认：anime_id={}, source_id={}, source_anime_id={}",
            input.anime_id,
            input.source_id,
            source_anime_id
        );
        self.get_state(store, &input.anime_id, true).await
    }

    /// 记录用户确认的不匹配候选。
    pub fn report_mismatch<S>(
        &self,
        store: &S,
        input: ReportAnimeSourceCandidateMismatchInput,
    ) -> Result<(), SourceError>
    where
        S: AnimeSourceBindingStore,
    {
        let (anime, source) = validate_anime_and_source(store, &input.anime_id, &input.source_id)?;
        let source_anime_id = require_text("sourceAnimeId", &input.source_anime_id)?;
        let timestamp = now_iso();
        store.save_exclusion(&AnimeSourceExclusion {
            id: exclusion_id(&anime.id, &source.id, Some(source_anime_id)),
            anime_id: anime.id.clone(),
            source_id: source.id.clone(),
            scope: AnimeSourceExclusionScope::Candidate,
            source_anime_id: Some(source_anime_id.to_owned()),
            source_anime_title: normalized_optional_text(Some(input.source_anime_title.clone())),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })?;
        log::info!(
            "Rust 来源候选已标记不匹配：anime_id={}, source_id={}, source_anime_id={}, score={}, reasons={:?}",
            anime.id,
            source.id,
            source_anime_id,
            input.score.round().clamp(0.0, 100.0),
            input.reasons.into_iter().take(6).collect::<Vec<_>>()
        );
        Ok(())
    }

    /// 撤销一个候选的不匹配记录并重新发现候选。
    pub async fn remove_candidate_mismatch<S>(
        &self,
        store: &S,
        input: RemoveAnimeSourceCandidateMismatchInput,
    ) -> Result<AnimeSourceBindingState, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        validate_anime_and_source(store, &input.anime_id, &input.source_id)?;
        let source_anime_id = require_text("sourceAnimeId", &input.source_anime_id)?;
        store.delete_exclusion(&input.anime_id, &input.source_id, Some(source_anime_id))?;
        self.get_state(store, &input.anime_id, true).await
    }

    /// 设置或取消当前番剧对整个来源的候选排除。
    pub async fn set_source_excluded<S>(
        &self,
        store: &S,
        input: SetAnimeSourceExclusionInput,
    ) -> Result<AnimeSourceBindingState, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        let (anime, source) = validate_anime_and_source(store, &input.anime_id, &input.source_id)?;
        if input.excluded {
            let existing = store.list_exclusions(&anime.id)?.into_iter().find(|item| {
                item.source_id == source.id && item.scope == AnimeSourceExclusionScope::Source
            });
            let timestamp = now_iso();
            store.save_exclusion(&AnimeSourceExclusion {
                id: existing.as_ref().map_or_else(
                    || exclusion_id(&anime.id, &source.id, None),
                    |item| item.id.clone(),
                ),
                anime_id: anime.id.clone(),
                source_id: source.id.clone(),
                scope: AnimeSourceExclusionScope::Source,
                source_anime_id: None,
                source_anime_title: None,
                created_at: existing
                    .as_ref()
                    .map_or_else(|| timestamp.clone(), |item| item.created_at.clone()),
                updated_at: timestamp,
            })?;
        } else {
            store.delete_exclusion(&anime.id, &source.id, None)?;
        }
        self.get_state(store, &anime.id, true).await
    }

    /// 取消来源绑定并重新允许该来源参与候选发现。
    pub async fn remove<S>(
        &self,
        store: &S,
        anime_id: &str,
        source_id: &str,
    ) -> Result<AnimeSourceBindingState, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        validate_anime_and_source(store, anime_id, source_id)?;
        if let Some(mut binding) = store
            .list_bindings(anime_id)?
            .into_iter()
            .find(|binding| binding.source_id == source_id)
        {
            binding.confidence = 0.0;
            binding.confirmed = false;
            binding.updated_at = now_iso();
            store.save_binding(&binding)?;
        }
        self.get_state(store, anime_id, true).await
    }

    async fn discover_source_candidates<S>(
        &self,
        store: &S,
        anime: &Anime,
        source: &ReleaseSourceConfig,
        sources: &[ReleaseSourceConfig],
        local_episode_count: usize,
    ) -> Result<Vec<AnimeSourceCandidate>, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        let candidates = if is_mikan_rss_config(source) {
            let site_source = sources
                .iter()
                .find(|candidate| candidate.enabled && is_mikan_site_config(candidate))
                .ok_or_else(|| {
                    invalid_input("sourceId", "请先在下载源设置中启用蜜柑计划站点以发现候选")
                })?;
            self.fetch_mikan_candidates(store, anime, source, site_source)
                .await?
        } else {
            self.fetch_anibt_candidates(store, anime, source).await?
        };
        let mut candidates = candidates
            .into_iter()
            .map(|candidate| score_candidate(anime, candidate, local_episode_count))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| Reverse(candidate.score));
        candidates.truncate(MAX_CANDIDATES_PER_SOURCE);
        Ok(candidates)
    }

    async fn fetch_mikan_candidates<S>(
        &self,
        store: &S,
        anime: &Anime,
        binding_source: &ReleaseSourceConfig,
        site_source: &ReleaseSourceConfig,
    ) -> Result<Vec<AnimeSourceCandidate>, SourceError>
    where
        S: AnimeSourceBindingStore,
    {
        let base = mikan_base_url(site_source)?;
        let mut url = base
            .join("/Home/BangumiCoverFlowByDayOfWeek")
            .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("year", &anime.premiere_year.to_string())
            .append_pair("seasonStr", mikan_season(anime.premiere_month));
        let response = self
            .network
            .execute(
                store,
                site_source,
                NativeHttpRequest {
                    source_id: site_source.id.clone(),
                    method: HttpMethod::Get,
                    url: url.to_string(),
                    headers: BTreeMap::from([(
                        "Accept".to_owned(),
                        "text/html,application/xhtml+xml".to_owned(),
                    )]),
                    body: None,
                    request_interval_ms: 0,
                },
            )
            .await?;
        Ok(parse_mikan_candidates(&response.text(), &base)
            .into_iter()
            .map(|mut candidate| {
                candidate.source_id.clone_from(&binding_source.id);
                candidate.source_name.clone_from(&binding_source.name);
                candidate.premiere_year = Some(anime.premiere_year);
                candidate.premiere_month = Some(season_start_month(anime.premiere_month));
                candidate
            })
            .collect())
    }

    async fn fetch_anibt_candidates<S>(
        &self,
        store: &S,
        anime: &Anime,
        source: &ReleaseSourceConfig,
    ) -> Result<Vec<AnimeSourceCandidate>, SourceError>
    where
        S: AnimeSourceBindingStore + Sync,
    {
        let terms = anime_titles(anime)
            .into_iter()
            .take(MAX_ANIBT_CANDIDATE_TERMS)
            .collect::<Vec<_>>();
        let batches = join_all(
            terms
                .iter()
                .map(|term| async { self.fetch_anibt_candidate_term(store, source, term).await }),
        )
        .await;
        let mut by_id = BTreeMap::new();
        for batch in batches {
            for candidate in batch? {
                by_id.insert(candidate.source_anime_id.clone(), candidate);
            }
        }
        Ok(by_id.into_values().collect())
    }

    async fn fetch_anibt_candidate_term<S>(
        &self,
        store: &S,
        source: &ReleaseSourceConfig,
        keyword: &str,
    ) -> Result<Vec<AnimeSourceCandidate>, SourceError>
    where
        S: AnimeSourceBindingStore,
    {
        let mut url = Url::parse(source.base_url.as_deref().unwrap_or("https://anibt.net/"))
            .and_then(|base| base.join("/api/bgm/search"))
            .map_err(|error| SourceError::InvalidUrl(error.to_string()))?;
        url.query_pairs_mut().append_pair("q", keyword);
        let response = self
            .network
            .execute(
                store,
                source,
                NativeHttpRequest {
                    source_id: source.id.clone(),
                    method: HttpMethod::Get,
                    url: url.to_string(),
                    headers: create_anibt_headers(source, "application/json"),
                    body: None,
                    request_interval_ms: 0,
                },
            )
            .await?;
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|error| SourceError::Parse(error.to_string()))?;
        if payload.get("ok") == Some(&Value::Bool(false)) {
            return Err(SourceError::Parse("AniBT BGM 查询返回错误".to_owned()));
        }
        let items = payload
            .get("data")
            .and_then(|data| data.as_array().or_else(|| data.get("items")?.as_array()))
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|item| map_anibt_candidate(item, source, keyword))
            .collect())
    }
}

fn find_anime<S>(store: &S, anime_id: &str) -> Result<Anime, SourceError>
where
    S: AnimeSourceBindingStore,
{
    store
        .list_followed_anime()?
        .into_iter()
        .find(|item| item.anime.id == anime_id)
        .map(|item| item.anime)
        .ok_or_else(|| {
            SourceError::Repository(RepositoryError::RecordNotFound {
                entity: "追番".to_owned(),
                id: anime_id.to_owned(),
            })
        })
}

fn validate_anime_and_source<S>(
    store: &S,
    anime_id: &str,
    source_id: &str,
) -> Result<(Anime, ReleaseSourceConfig), SourceError>
where
    S: AnimeSourceBindingStore,
{
    let anime = find_anime(store, anime_id)?;
    let source = store
        .list_binding_sources()?
        .into_iter()
        .find(|source| source.id == source_id && is_bindable_source(source))
        .ok_or_else(|| {
            SourceError::Repository(RepositoryError::RecordNotFound {
                entity: "可绑定下载源".to_owned(),
                id: source_id.to_owned(),
            })
        })?;
    Ok((anime, source))
}

fn sync_external_id_bindings<S>(
    store: &S,
    anime: &Anime,
    sources: &[ReleaseSourceConfig],
) -> Result<Vec<AnimeSourceBinding>, SourceError>
where
    S: AnimeSourceBindingStore,
{
    let mut bindings = store.list_bindings(&anime.id)?;
    for source in sources
        .iter()
        .filter(|source| source.enabled && is_bindable_source(source))
    {
        if bindings
            .iter()
            .any(|binding| binding.source_id == source.id)
        {
            continue;
        }
        let external_id = if is_mikan_rss_config(source) {
            external_id(anime, "mikan")
        } else {
            external_id(anime, "bangumi")
        };
        let Some(external_id) = external_id else {
            continue;
        };
        let timestamp = now_iso();
        bindings = store.save_binding(&AnimeSourceBinding {
            id: binding_id(&anime.id, &source.id),
            anime_id: anime.id.clone(),
            source_id: source.id.clone(),
            source_anime_id: external_id.to_owned(),
            source_anime_title: Some(anime.title.clone()),
            source_url: Some(build_source_anime_url(source, external_id)?),
            match_method: AnimeSourceBindingMatchMethod::ExternalId,
            confidence: 1.0,
            confirmed: true,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        })?;
        log::info!(
            "Rust 外部 ID 来源绑定已创建：anime_id={}, source_id={}, source_anime_id={}",
            anime.id,
            source.id,
            external_id
        );
    }
    Ok(bindings)
}

fn score_candidate(
    anime: &Anime,
    mut candidate: AnimeSourceCandidate,
    local_episode_count: usize,
) -> AnimeSourceCandidate {
    let title_similarity = best_title_similarity(anime, &candidate);
    let season_similarity = season_similarity(anime, &candidate);
    let episode_similarity = episode_similarity(local_episode_count, candidate.episode_count);
    candidate.score =
        ((title_similarity * 0.7 + season_similarity * 0.2 + episode_similarity * 0.1) * 100.0)
            .round() as i64;
    candidate.reasons = vec![format!("标题 {}%", (title_similarity * 100.0).round())];
    if season_similarity > 0.0 {
        candidate
            .reasons
            .push(format!("季度 {}%", (season_similarity * 100.0).round()));
    }
    if episode_similarity > 0.0 {
        candidate
            .reasons
            .push(format!("集数 {}%", (episode_similarity * 100.0).round()));
    }
    candidate
}

/// 仅返回同来源唯一且标题、年份、季度均可确认的高分候选。
fn safe_auto_binding_candidate<'candidate>(
    anime: &Anime,
    candidates: &'candidate [AnimeSourceCandidate],
) -> Option<&'candidate AnimeSourceCandidate> {
    let mut safe_candidates = candidates.iter().filter(|candidate| {
        candidate.score >= 90
            && candidate_titles(candidate).any(|title| {
                let normalized = normalize_title(title);
                !normalized.is_empty()
                    && anime_titles(anime)
                        .into_iter()
                        .any(|local| normalize_title(local) == normalized)
            })
            && candidate.premiere_year == Some(anime.premiere_year)
            && candidate.premiere_month.is_some_and(|month| {
                (1..=12).contains(&month)
                    && season_index(month) == season_index(anime.premiere_month)
            })
    });
    let candidate = safe_candidates.next()?;
    safe_candidates.next().is_none().then_some(candidate)
}

/// 将安全评分候选保存为已确认绑定。
fn save_scored_binding<S>(
    store: &S,
    anime: &Anime,
    candidate: &AnimeSourceCandidate,
) -> Result<Vec<AnimeSourceBinding>, SourceError>
where
    S: AnimeSourceBindingStore,
{
    let timestamp = now_iso();
    let bindings = store.save_binding(&AnimeSourceBinding {
        id: binding_id(&anime.id, &candidate.source_id),
        anime_id: anime.id.clone(),
        source_id: candidate.source_id.clone(),
        source_anime_id: candidate.source_anime_id.clone(),
        source_anime_title: normalized_optional_text(Some(candidate.title.clone())),
        source_url: normalized_optional_text(candidate.source_url.clone()),
        match_method: AnimeSourceBindingMatchMethod::Scored,
        confidence: (candidate.score as f64 / 100.0).clamp(0.0, 1.0),
        confirmed: true,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    })?;
    log::info!(
        "Rust 高置信来源候选已自动绑定：anime_id={}, source_id={}, source_anime_id={}, score={}",
        anime.id,
        candidate.source_id,
        candidate.source_anime_id,
        candidate.score
    );
    Ok(bindings)
}

fn best_title_similarity(anime: &Anime, candidate: &AnimeSourceCandidate) -> f64 {
    let local = anime_titles(anime);
    let source = std::iter::once(candidate.title.as_str())
        .chain(candidate.original_title.as_deref())
        .chain(candidate.aliases.iter().map(String::as_str));
    local
        .iter()
        .flat_map(|left| {
            source
                .clone()
                .map(move |right| title_similarity(left, right))
        })
        .fold(0.0, f64::max)
}

fn title_similarity(left: &str, right: &str) -> f64 {
    let left = normalize_title(left);
    let right = normalize_title(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    if left == right {
        return 1.0;
    }
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    let shorter = left_chars.len().min(right_chars.len());
    let longer = left_chars.len().max(right_chars.len());
    if shorter >= 4 && (left.contains(&right) || right.contains(&left)) {
        return 0.75_f64.max(shorter as f64 / longer as f64);
    }
    1.0 - levenshtein_distance(&left_chars, &right_chars) as f64 / longer as f64
}

fn normalize_title(value: &str) -> String {
    normalize_release_search_text(value)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn season_similarity(anime: &Anime, candidate: &AnimeSourceCandidate) -> f64 {
    let Some(year) = candidate.premiere_year else {
        return 0.0;
    };
    let year_score = if year == anime.premiere_year {
        0.6
    } else {
        0.0
    };
    let Some(month) = candidate.premiere_month else {
        return year_score;
    };
    year_score
        + if season_index(month) == season_index(anime.premiere_month) {
            0.4
        } else {
            0.0
        }
}

fn episode_similarity(local_count: usize, source_count: Option<i64>) -> f64 {
    let Some(source_count) = source_count.filter(|count| *count > 0) else {
        return 0.0;
    };
    if local_count == 0 {
        return 0.0;
    }
    local_count.min(source_count as usize) as f64 / local_count.max(source_count as usize) as f64
}

fn levenshtein_distance(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_value) in left.iter().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_value) in right.iter().enumerate() {
            current.push(
                (current[right_index] + 1)
                    .min(previous[right_index + 1] + 1)
                    .min(previous[right_index] + usize::from(left_value != right_value)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

fn parse_mikan_candidates(html: &str, base: &Url) -> Vec<AnimeSourceCandidate> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href*='/Home/Bangumi/']")
        .expect("Mikan candidate selector must be valid");
    let image_selector = Selector::parse("img[alt]").expect("Mikan image selector must be valid");
    let mut by_id = BTreeMap::new();
    for anchor in document.select(&selector) {
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let Ok(detail_url) = base.join(href) else {
            continue;
        };
        let Some(id) = detail_url
            .path_segments()
            .and_then(|segments| {
                segments
                    .collect::<Vec<_>>()
                    .windows(2)
                    .find_map(|pair| pair[0].eq_ignore_ascii_case("Bangumi").then_some(pair[1]))
            })
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let title = anchor
            .value()
            .attr("title")
            .or_else(|| {
                anchor
                    .select(&image_selector)
                    .find_map(|image| image.value().attr("alt"))
            })
            .map(str::to_owned)
            .unwrap_or_else(|| anchor.text().collect::<Vec<_>>().join(" "));
        let title = normalize_html_text(&title);
        if title.is_empty() {
            continue;
        }
        let candidate = AnimeSourceCandidate {
            source_id: String::new(),
            source_name: String::new(),
            source_anime_id: id.to_owned(),
            title,
            original_title: None,
            aliases: Vec::new(),
            premiere_year: None,
            premiere_month: None,
            episode_count: None,
            source_url: Some(detail_url.to_string()),
            score: 0,
            reasons: Vec::new(),
        };
        if by_id.get(id).is_none_or(|existing: &AnimeSourceCandidate| {
            existing.title.len() < candidate.title.len()
        }) {
            by_id.insert(id.to_owned(), candidate);
        }
    }
    by_id.into_values().collect()
}

fn map_anibt_candidate(
    item: &Value,
    source: &ReleaseSourceConfig,
    fallback_title: &str,
) -> Option<AnimeSourceCandidate> {
    let source_anime_id = value_to_string(item.get("bgmId")?)?;
    let title = ["nameCn", "title", "name"]
        .into_iter()
        .find_map(|key| item.get(key).and_then(Value::as_str))
        .unwrap_or(fallback_title)
        .to_owned();
    Some(AnimeSourceCandidate {
        source_id: source.id.clone(),
        source_name: source.name.clone(),
        source_anime_id: source_anime_id.clone(),
        title,
        original_title: item
            .get("originalTitle")
            .and_then(Value::as_str)
            .map(str::to_owned),
        aliases: Vec::new(),
        premiere_year: value_to_i64(item.get("year")),
        premiere_month: value_to_i64(item.get("month")),
        episode_count: value_to_i64(item.get("episodeCount")),
        source_url: Some(format!("https://bgm.tv/subject/{source_anime_id}")),
        score: 0,
        reasons: Vec::new(),
    })
}

fn is_bindable_source(source: &ReleaseSourceConfig) -> bool {
    is_mikan_rss_config(source) || is_anibt_config(source)
}

fn is_mikan_rss_config(source: &ReleaseSourceConfig) -> bool {
    source.kind == SourceKind::Rss && is_mikan_config(source)
}

fn external_id<'anime>(anime: &'anime Anime, key: &str) -> Option<&'anime str> {
    anime
        .external_ids
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn build_source_anime_url(
    source: &ReleaseSourceConfig,
    source_anime_id: &str,
) -> Result<String, SourceError> {
    if is_mikan_rss_config(source) {
        return mikan_base_url(source)?
            .join(&format!("/Home/Bangumi/{source_anime_id}"))
            .map(|url| url.to_string())
            .map_err(|error| SourceError::InvalidUrl(error.to_string()));
    }
    Ok(format!("https://bgm.tv/subject/{source_anime_id}"))
}

fn mikan_base_url(source: &ReleaseSourceConfig) -> Result<Url, SourceError> {
    let value = source
        .base_url
        .as_deref()
        .or(source.rss_url.as_deref())
        .unwrap_or("https://mikanani.me/");
    Url::parse(value).map_err(|error| SourceError::InvalidUrl(error.to_string()))
}

fn mikan_season(month: i64) -> &'static str {
    match month {
        ..=3 => "冬",
        4..=6 => "春",
        7..=9 => "夏",
        _ => "秋",
    }
}

fn season_start_month(month: i64) -> i64 {
    ((month.clamp(1, 12) - 1) / 3) * 3 + 1
}

fn season_index(month: i64) -> i64 {
    (month.clamp(1, 12) - 1) / 3
}

fn anime_titles(anime: &Anime) -> Vec<&str> {
    std::iter::once(anime.title.as_str())
        .chain(anime.original_title.as_deref())
        .chain(anime.aliases.iter().map(|alias| alias.alias.as_str()))
        .filter(|value| !value.trim().is_empty())
        .collect()
}

/// 返回来源候选可用于精确匹配的全部标题。
fn candidate_titles(candidate: &AnimeSourceCandidate) -> impl Iterator<Item = &str> {
    std::iter::once(candidate.title.as_str())
        .chain(candidate.original_title.as_deref())
        .chain(candidate.aliases.iter().map(String::as_str))
        .filter(|value| !value.trim().is_empty())
}

fn candidate_key(source_id: &str, source_anime_id: &str) -> String {
    format!("{source_id}:{source_anime_id}")
}

fn binding_id(anime_id: &str, source_id: &str) -> String {
    format!("source-binding:{anime_id}:{source_id}")
}

fn exclusion_id(anime_id: &str, source_id: &str, source_anime_id: Option<&str>) -> String {
    [
        "source-exclusion",
        anime_id,
        source_id,
        source_anime_id.unwrap_or("*"),
    ]
    .into_iter()
    .map(encode_component)
    .collect::<Vec<_>>()
    .join(":")
}

fn encode_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn require_text<'value>(
    field: &'static str,
    value: &'value str,
) -> Result<&'value str, SourceError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_input(field, "字段不能为空"));
    }
    Ok(value)
}

fn normalized_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn validate_optional_http_url(value: Option<&str>) -> Result<(), SourceError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let url = Url::parse(value).map_err(|error| invalid_input("sourceUrl", &error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_input("sourceUrl", "来源地址仅支持 HTTP 或 HTTPS"));
    }
    Ok(())
}

fn invalid_input(field: &'static str, message: &str) -> SourceError {
    SourceError::Repository(RepositoryError::InvalidInput {
        field: field.to_owned(),
        message: message.to_owned(),
    })
}

fn binding_error_message(error: &SourceError) -> String {
    match error {
        SourceError::Transport(error) if error.is_timeout() => {
            "来源请求超时，请稍后重试".to_owned()
        }
        SourceError::Transport(_) => "来源网络请求失败，请检查网络或代理设置".to_owned(),
        SourceError::Repository(RepositoryError::InvalidInput { message, .. }) => message.clone(),
        _ => error.to_string(),
    }
}

fn normalize_html_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_to_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| match value {
        Value::Number(value) => value.as_i64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    })
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use ani_domain::{
        Anime, AnimeAlias, AnimeAliasLanguage, AnimeSourceBinding, AnimeSourceBindingMatchMethod,
        AnimeSourceCandidate, AnimeSourceExclusion, AnimeStatus, ConfirmAnimeSourceBindingInput,
        Episode, MyAnime, ReleaseSourceConfig, RequestCircuitState, SourceKind,
    };
    use ani_repository::RepositoryResult;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use url::Url;

    use super::{
        parse_mikan_candidates, score_candidate, title_similarity, AnimeSourceBindingService,
        AnimeSourceBindingStore,
    };
    use crate::{CircuitStateStore, NativeHttpConfig, ProxyMode, SourceNetworkService};

    #[derive(Default)]
    struct MemoryBindingStore {
        followed: Vec<MyAnime>,
        sources: Vec<ReleaseSourceConfig>,
        episodes: Vec<Episode>,
        bindings: Mutex<Vec<AnimeSourceBinding>>,
        exclusions: Mutex<Vec<AnimeSourceExclusion>>,
        circuits: Mutex<BTreeMap<String, RequestCircuitState>>,
    }

    impl CircuitStateStore for MemoryBindingStore {
        /// 读取绑定测试使用的熔断状态。
        fn get_circuit_state(&self, key: &str) -> RepositoryResult<Option<RequestCircuitState>> {
            Ok(self
                .circuits
                .lock()
                .expect("lock binding circuit states")
                .get(key)
                .cloned())
        }

        /// 保存绑定测试使用的熔断状态。
        fn save_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
            self.circuits
                .lock()
                .expect("lock binding circuit states")
                .insert(state.key.clone(), state.clone());
            Ok(())
        }
    }

    impl AnimeSourceBindingStore for MemoryBindingStore {
        /// 返回测试追番快照。
        fn list_followed_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
            Ok(self.followed.clone())
        }

        /// 返回测试来源快照。
        fn list_binding_sources(&self) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
            Ok(self.sources.clone())
        }

        /// 返回指定番剧的测试单集。
        fn list_binding_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
            Ok(self
                .episodes
                .iter()
                .filter(|episode| episode.anime_id == anime_id)
                .cloned()
                .collect())
        }

        /// 返回指定番剧的内存绑定。
        fn list_bindings(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceBinding>> {
            Ok(self
                .bindings
                .lock()
                .expect("lock memory bindings")
                .iter()
                .filter(|binding| binding.anime_id == anime_id)
                .cloned()
                .collect())
        }

        /// 按番剧和来源覆盖一条内存绑定。
        fn save_binding(
            &self,
            binding: &AnimeSourceBinding,
        ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
            let mut bindings = self.bindings.lock().expect("lock memory bindings");
            bindings.retain(|item| {
                item.anime_id != binding.anime_id || item.source_id != binding.source_id
            });
            bindings.push(binding.clone());
            Ok(bindings
                .iter()
                .filter(|item| item.anime_id == binding.anime_id)
                .cloned()
                .collect())
        }

        /// 返回指定番剧的内存排除记录。
        fn list_exclusions(&self, anime_id: &str) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
            Ok(self
                .exclusions
                .lock()
                .expect("lock memory exclusions")
                .iter()
                .filter(|item| item.anime_id == anime_id)
                .cloned()
                .collect())
        }

        /// 按番剧、来源和候选覆盖一条排除记录。
        fn save_exclusion(
            &self,
            exclusion: &AnimeSourceExclusion,
        ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
            let mut exclusions = self.exclusions.lock().expect("lock memory exclusions");
            exclusions.retain(|item| {
                item.anime_id != exclusion.anime_id
                    || item.source_id != exclusion.source_id
                    || item.source_anime_id != exclusion.source_anime_id
            });
            exclusions.push(exclusion.clone());
            Ok(exclusions
                .iter()
                .filter(|item| item.anime_id == exclusion.anime_id)
                .cloned()
                .collect())
        }

        /// 删除指定作用域的内存排除记录。
        fn delete_exclusion(
            &self,
            anime_id: &str,
            source_id: &str,
            source_anime_id: Option<&str>,
        ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
            let mut exclusions = self.exclusions.lock().expect("lock memory exclusions");
            exclusions.retain(|item| {
                item.anime_id != anime_id
                    || item.source_id != source_id
                    || item.source_anime_id.as_deref() != source_anime_id
            });
            Ok(exclusions
                .iter()
                .filter(|item| item.anime_id == anime_id)
                .cloned()
                .collect())
        }
    }

    /// 验证标题、季度和集数评分与现有来源绑定规则一致。
    #[test]
    fn scores_anime_source_candidates() {
        let anime = test_anime();
        let candidate = AnimeSourceCandidate {
            source_id: "anibt".to_owned(),
            source_name: "AniBT".to_owned(),
            source_anime_id: "528828".to_owned(),
            title: "来源绑定测试番".to_owned(),
            original_title: None,
            aliases: Vec::new(),
            premiere_year: Some(2026),
            premiere_month: Some(7),
            episode_count: Some(12),
            source_url: None,
            score: 0,
            reasons: Vec::new(),
        };

        let scored = score_candidate(&anime, candidate, 12);

        assert_eq!(scored.score, 100);
        assert_eq!(scored.reasons, vec!["标题 100%", "季度 100%", "集数 100%"]);
        assert!(title_similarity("测试番 第二季", "测试番 第2季") > 0.75);
    }

    /// 验证 Mikan 季度页候选按番组 ID 去重并保留可访问详情地址。
    #[test]
    fn parses_mikan_season_candidates() {
        let candidates = parse_mikan_candidates(
            r#"<a href="/Home/Bangumi/3941" title="测试番"><img alt="短名"></a><a href="/Home/Bangumi/3941"><img alt="来源绑定测试番"></a>"#,
            &Url::parse("https://mikanani.me/").expect("Mikan base URL"),
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_anime_id, "3941");
        assert_eq!(candidates[0].title, "来源绑定测试番");
        assert_eq!(
            candidates[0].source_url.as_deref(),
            Some("https://mikanani.me/Home/Bangumi/3941")
        );
    }

    /// 验证 Bangumi 与 Mikan 外部 ID 会自动生成已确认绑定。
    #[tokio::test]
    async fn syncs_external_ids_into_confirmed_bindings() {
        let mut anime = test_anime();
        anime.external_ids = json!({"bangumi": "528828", "mikan": "3941"});
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![anibt_source("https://anibt.net/"), mikan_rss_source()],
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .get_state(&store, "anime-binding-test", false)
            .await
            .expect("synchronize external source bindings");

        assert_eq!(state.bindings.len(), 2);
        assert!(state.bindings.iter().all(|binding| {
            binding.confirmed
                && binding.match_method == AnimeSourceBindingMatchMethod::ExternalId
                && binding.confidence == 1.0
        }));
        assert!(state
            .bindings
            .iter()
            .any(|binding| binding.source_id == "anibt" && binding.source_anime_id == "528828"));
        assert!(state
            .bindings
            .iter()
            .any(|binding| binding.source_id == "mikan-rss" && binding.source_anime_id == "3941"));
    }

    /// 验证唯一安全高分候选会自动保存为评分绑定。
    #[tokio::test]
    async fn auto_binds_unique_safe_scored_candidate() {
        let base_url = serve_repeated(
            2,
            "200 OK",
            r#"{"ok":true,"data":[{"bgmId":"528828","nameCn":"来源绑定测试番","year":2026,"month":7,"episodeCount":12}]}"#,
        )
        .await;
        let mut anime = test_anime();
        anime.external_ids = Value::Object(Default::default());
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![anibt_source(&base_url)],
            episodes: test_episodes(12),
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .get_state(&store, "anime-binding-test", true)
            .await
            .expect("auto bind unique safe candidate");

        assert!(state.candidates.is_empty());
        assert_eq!(state.bindings.len(), 1);
        assert_eq!(
            state.bindings[0].match_method,
            AnimeSourceBindingMatchMethod::Scored
        );
        assert_eq!(state.bindings[0].confidence, 1.0);
        assert!(state.bindings[0].confirmed);
    }

    /// 验证同来源存在多个安全候选时仍交由用户确认。
    #[tokio::test]
    async fn keeps_ambiguous_safe_candidates_unbound() {
        let base_url = serve_repeated(
            2,
            "200 OK",
            r#"{"ok":true,"data":[{"bgmId":"528828","nameCn":"来源绑定测试番","year":2026,"month":7,"episodeCount":12},{"bgmId":"528829","nameCn":"来源绑定测试番","year":2026,"month":7,"episodeCount":12}]}"#,
        )
        .await;
        let mut anime = test_anime();
        anime.external_ids = Value::Object(Default::default());
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![anibt_source(&base_url)],
            episodes: test_episodes(12),
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .get_state(&store, "anime-binding-test", true)
            .await
            .expect("keep ambiguous candidates");

        assert!(state.bindings.is_empty());
        assert_eq!(state.candidates.len(), 2);
    }

    /// 验证用户解除过的绑定记录不会被高分候选再次自动启用。
    #[tokio::test]
    async fn does_not_rebind_user_removed_candidate() {
        let base_url = serve_repeated(
            2,
            "200 OK",
            r#"{"ok":true,"data":[{"bgmId":"528828","nameCn":"来源绑定测试番","year":2026,"month":7,"episodeCount":12}]}"#,
        )
        .await;
        let mut anime = test_anime();
        anime.external_ids = Value::Object(Default::default());
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![anibt_source(&base_url)],
            episodes: test_episodes(12),
            bindings: Mutex::new(vec![inactive_binding()]),
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .get_state(&store, "anime-binding-test", true)
            .await
            .expect("preserve removed binding state");

        assert_eq!(state.bindings.len(), 1);
        assert!(!state.bindings[0].confirmed);
        assert_eq!(state.bindings[0].confidence, 0.0);
        assert_eq!(state.candidates.len(), 1);
    }

    /// 验证单来源候选发现失败不会丢失其他来源的候选。
    #[tokio::test]
    async fn preserves_candidates_when_one_binding_source_fails() {
        let mikan_base = serve_repeated(
            1,
            "200 OK",
            r#"<a href="/Home/Bangumi/3941" title="来源绑定候选测试番"></a>"#,
        )
        .await;
        let anibt_base = serve_repeated(2, "503 Service Unavailable", "busy").await;
        let mut anime = test_anime();
        anime.external_ids = Value::Object(Default::default());
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![
                mikan_rss_source(),
                ReleaseSourceConfig {
                    id: "mikan-site".to_owned(),
                    name: "Mikan Site".to_owned(),
                    kind: SourceKind::SiteAdapter,
                    enabled: true,
                    use_proxy: false,
                    request_interval_ms: 250,
                    base_url: Some(mikan_base),
                    api_key: None,
                    rss_url: None,
                    tags: Vec::new(),
                },
                anibt_source(&anibt_base),
            ],
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .get_state(&store, "anime-binding-test", true)
            .await
            .expect("discover partial binding candidates");

        assert_eq!(state.candidates.len(), 1);
        assert_eq!(state.candidates[0].source_id, "mikan-rss");
        assert_eq!(state.candidates[0].source_anime_id, "3941");
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].source_id, "anibt");
    }

    /// 验证手动确认会保存稳定绑定且不依赖额外候选请求。
    #[tokio::test]
    async fn confirms_manual_binding() {
        let anime = test_anime();
        let store = MemoryBindingStore {
            followed: vec![followed(anime)],
            sources: vec![anibt_source("https://anibt.net/")],
            ..MemoryBindingStore::default()
        };

        let state = AnimeSourceBindingService::new(test_network())
            .confirm(
                &store,
                ConfirmAnimeSourceBindingInput {
                    anime_id: "anime-binding-test".to_owned(),
                    source_id: "anibt".to_owned(),
                    source_anime_id: "528828".to_owned(),
                    source_anime_title: Some("来源绑定测试番".to_owned()),
                    source_url: Some("https://bgm.tv/subject/528828".to_owned()),
                    confidence: Some(0.94),
                },
            )
            .await
            .expect("confirm manual binding");

        assert_eq!(state.bindings.len(), 1);
        assert_eq!(
            state.bindings[0].match_method,
            AnimeSourceBindingMatchMethod::Manual
        );
        assert_eq!(state.bindings[0].confidence, 0.94);
        assert!(state.bindings[0].confirmed);
    }

    fn test_anime() -> Anime {
        Anime {
            id: "anime-binding-test".to_owned(),
            title: "来源绑定测试番".to_owned(),
            original_title: None,
            aliases: vec![AnimeAlias {
                id: "alias-binding-test".to_owned(),
                anime_id: "anime-binding-test".to_owned(),
                alias: "绑定测试番".to_owned(),
                language: AnimeAliasLanguage::Zh,
                priority: 1,
            }],
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

    /// 包装绑定测试需要的最小追番记录。
    fn followed(anime: Anime) -> MyAnime {
        MyAnime {
            id: "my-anime-binding-test".to_owned(),
            anime,
            status: AnimeStatus::Watching,
            default_fansub_group_id: None,
            auto_download: false,
            download_dir: None,
            rss_subscriptions: Vec::new(),
            preferred_resolution: None,
            preferred_codec: None,
            preferred_bit_depth: None,
            preferred_subtitle_languages: Vec::new(),
            preferred_subtitle: None,
            added_at: "2026-07-25T00:00:00.000Z".to_owned(),
            updated_at: "2026-07-25T00:00:00.000Z".to_owned(),
        }
    }

    /// 创建指定数量的绑定评分测试单集。
    fn test_episodes(count: i64) -> Vec<Episode> {
        (1..=count)
            .map(|episode_no| {
                serde_json::from_value(json!({
                    "id": format!("episode-binding-test-{episode_no}"),
                    "animeId": "anime-binding-test",
                    "episodeNo": episode_no,
                    "status": "aired"
                }))
                .expect("decode binding test episode")
            })
            .collect()
    }

    /// 创建一条代表用户已解除的未确认绑定。
    fn inactive_binding() -> AnimeSourceBinding {
        AnimeSourceBinding {
            id: "source-binding:anime-binding-test:anibt".to_owned(),
            anime_id: "anime-binding-test".to_owned(),
            source_id: "anibt".to_owned(),
            source_anime_id: "528828".to_owned(),
            source_anime_title: Some("来源绑定测试番".to_owned()),
            source_url: Some("https://bgm.tv/subject/528828".to_owned()),
            match_method: AnimeSourceBindingMatchMethod::Scored,
            confidence: 0.0,
            confirmed: false,
            created_at: "2026-08-05T00:00:00.000Z".to_owned(),
            updated_at: "2026-08-05T00:01:00.000Z".to_owned(),
        }
    }

    /// 创建 AniBT 绑定来源。
    fn anibt_source(base_url: &str) -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: "anibt".to_owned(),
            name: "AniBT".to_owned(),
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

    /// 创建 Mikan 精确 RSS 绑定来源。
    fn mikan_rss_source() -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: "mikan-rss".to_owned(),
            name: "蜜柑计划 RSS".to_owned(),
            kind: SourceKind::Rss,
            enabled: true,
            use_proxy: false,
            request_interval_ms: 250,
            base_url: Some("https://mikanani.me/".to_owned()),
            api_key: None,
            rss_url: Some("https://mikanani.me/RSS/Bangumi".to_owned()),
            tags: Vec::new(),
        }
    }

    /// 创建关闭代理的绑定测试网络服务。
    fn test_network() -> Arc<SourceNetworkService> {
        Arc::new(
            SourceNetworkService::new(NativeHttpConfig {
                proxy_mode: ProxyMode::Off,
                ..NativeHttpConfig::default()
            })
            .expect("create binding test network"),
        )
    }

    /// 启动固定次数的候选发现测试服务。
    async fn serve_repeated(count: usize, status: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind binding HTTP listener");
        let address = listener.local_addr().expect("read binding HTTP address");
        let status = status.to_owned();
        let body = body.as_bytes().to_vec();
        tokio::spawn(async move {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().await.expect("accept binding request");
                let mut request = [0_u8; 2_048];
                let _ = stream
                    .read(&mut request)
                    .await
                    .expect("read binding request");
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write binding HTTP headers");
                stream
                    .write_all(&body)
                    .await
                    .expect("write binding HTTP body");
            }
        });
        format!("http://{address}/")
    }
}
