use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};

use ani_domain::{
    Anime, AnimeAlias, AnimeAliasLanguage, AnimeDetailPartialError, AnimeDetailRefreshState,
    AnimeDetailResult, AnimeDiscoverySearchResult, AnimeRating, AnimeRssSubscription,
    AnimeSeasonSyncState, AnimeSourceBinding, AnimeSourceBindingMatchMethod, AnimeSourceExclusion,
    AnimeSourceExclusionScope, AnimeStatus, AnimeWatchProgress, AppSettings, DailyReminderItem,
    DailyReminderSummary, DashboardData, DownloadStatus, DownloadTask, Episode, EpisodePreference,
    EpisodeStatus, EpisodeSummary, FansubGroup, MediaAvailability, MediaContentKind, MediaFile,
    MediaOrigin, MyAnime, NormalizedVideoCodec, NotificationKind, NotificationRecord,
    NotificationSeverity, PendingAction, PlaybackCheckpoint, Release, ReleaseResolution,
    ReleaseSourceConfig, ReleaseSourceSyncState, ReportPlaybackProgressInput, RequestCircuitState,
    SavePlaybackCheckpointInput, SecretReference, SecretValue, SecureStore,
    SetAnimeWatchProgressInput, SourceHealth, SourceKind, SubtitleLanguage, SubtitlePreference,
    TorrentEngineKind, TorrentFile, WeeklyScheduleDay,
};
use ani_repository::{
    AnimeCatalogRepository, AnimeCatalogWriteResult, AnimeSourceBindingRepository,
    AnimeTrackingRepository, CachedReleaseQuery, DashboardRepository, DownloadRepository,
    MediaRepository, NotificationRepository, PlaybackRepository, ReleaseCacheRepository,
    ReleaseSearchCacheEntry, ReleaseSourceRepository, RepositoryError, RepositoryResult,
    SettingsRepository,
};
use chrono::{DateTime, Duration, Local, Utc};
use log::{debug, info, warn};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Params, Row,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{now_iso, SecureStoreError, StorageError};

const SECURE_MARKER_PREFIX: &str = "secure-store:v1:";
const SETTINGS_QBITTORRENT_PASSWORD_KEY: &str = "settings.download.qbittorrent.password";

/// 为平台安全存储生成稳定命名空间与键。
fn secure_reference(key: &str) -> SecretReference {
    SecretReference {
        namespace: "ani-tracker".to_owned(),
        key: key.to_owned(),
    }
}

/// 将平台安全存储错误转换为带字段上下文的数据层错误。
fn secure_store_error(
    action: &'static str,
    key: &str,
    error: impl std::fmt::Display,
) -> StorageError {
    StorageError::SecureStoreOperation {
        action,
        key: key.to_owned(),
        detail: error.to_string(),
    }
}

/// 基于单个 SQLite 连接实现公共业务 Repository 端口。
pub struct SqliteRepository<'connection> {
    connection: &'connection Connection,
    transaction_active: bool,
    secure_store: Option<&'connection dyn SecureStore<Error = SecureStoreError>>,
}

impl<'connection> SqliteRepository<'connection> {
    /// 使用已完成迁移的 SQLite 连接创建 Repository。
    pub(crate) fn new(
        connection: &'connection Connection,
        secure_store: Option<&'connection dyn SecureStore<Error = SecureStoreError>>,
    ) -> Self {
        Self {
            connection,
            transaction_active: false,
            secure_store,
        }
    }

    /// 创建绑定外层工作单元的 Repository，内部原子操作复用同一事务。
    pub(crate) fn in_unit_of_work(
        connection: &'connection Connection,
        secure_store: Option<&'connection dyn SecureStore<Error = SecureStoreError>>,
    ) -> Self {
        Self {
            connection,
            transaction_active: true,
            secure_store,
        }
    }

    /// 读取设置，并用当前平台默认值递归补齐新增字段。
    pub(crate) fn get_settings(
        &self,
        platform_defaults: &AppSettings,
    ) -> Result<AppSettings, StorageError> {
        let stored = self.read_json_state("app_settings", "settings", "应用设置")?;
        let mut merged = platform_defaults.clone();
        if let Some(stored) = stored {
            let merged_players =
                merge_player_profiles(platform_defaults.get("players"), stored.get("players"));
            merge_json(&mut merged, stored);
            if let (Some(settings), Some(players)) = (merged.as_object_mut(), merged_players) {
                settings.insert("players".to_owned(), players);
            }
        }
        preserve_host_storage_paths(&mut merged, platform_defaults);
        if self.hydrate_settings_secrets(&mut merged) {
            self.save_settings(&merged)?;
            info!("Rust 应用设置敏感字段已迁移到平台安全存储");
        }
        Ok(merged)
    }

    /// 递归合并设置补丁，并保护宿主控制的平台路径。
    pub(crate) fn update_settings(
        &self,
        patch: &Value,
        platform_defaults: &AppSettings,
    ) -> Result<AppSettings, StorageError> {
        if !patch.is_object() {
            return invalid_input("settings", "设置补丁必须是 JSON 对象");
        }
        let mut settings = self.get_settings(platform_defaults)?;
        merge_json(&mut settings, patch.clone());
        preserve_host_storage_paths(&mut settings, platform_defaults);
        self.save_settings(&settings)?;
        info!("Rust 应用设置更新完成");
        Ok(settings)
    }

    /// 覆盖保存当前宿主生成的平台默认设置。
    pub(crate) fn reset_settings(
        &self,
        platform_defaults: &AppSettings,
    ) -> Result<AppSettings, StorageError> {
        self.save_settings(platform_defaults)?;
        info!("Rust 应用设置已恢复平台默认值");
        Ok(platform_defaults.clone())
    }

    /// 按创建时间倒序读取提醒中心通知。
    pub(crate) fn list_notifications(&self) -> Result<Vec<NotificationRecord>, StorageError> {
        let rows = query_all(
            self.connection,
            "SELECT * FROM notification ORDER BY created_at DESC",
            map_notification_row,
        )?;
        rows.into_iter().map(NotificationRow::into_domain).collect()
    }

    /// 统计当前未读通知数量。
    pub(crate) fn get_unread_notification_count(&self) -> Result<u64, StorageError> {
        let count = self.connection.query_row(
            "SELECT COUNT(*) FROM notification WHERE read_at IS NULL",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        Ok(count)
    }

    /// 增量写入提醒中心通知，相同标识保留已有已读状态。
    pub(crate) fn add_notifications(
        &self,
        records: &[NotificationRecord],
    ) -> Result<Vec<NotificationRecord>, StorageError> {
        self.with_transaction(|connection| {
            for record in records {
                validate_identifier("notification.id", &record.id)?;
                if record.title.trim().is_empty() {
                    return invalid_input("notification.title", "通知标题不能为空");
                }
                if record.created_at.trim().is_empty() {
                    return invalid_input("notification.createdAt", "通知创建时间不能为空");
                }
                connection.execute(
                    "INSERT INTO notification (
                       id, kind, title, body, severity, anime_id, episode_id,
                       download_task_id, created_at, read_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(id) DO UPDATE SET
                       kind = excluded.kind, title = excluded.title, body = excluded.body,
                       severity = excluded.severity, anime_id = excluded.anime_id,
                       episode_id = excluded.episode_id,
                       download_task_id = excluded.download_task_id,
                       created_at = excluded.created_at,
                       read_at = COALESCE(notification.read_at, excluded.read_at)",
                    params![
                        &record.id,
                        notification_kind_value(&record.kind),
                        record.title.trim(),
                        &record.body,
                        notification_severity_value(&record.severity),
                        record.anime_id.as_deref(),
                        record.episode_id.as_deref(),
                        record.download_task_id.as_deref(),
                        &record.created_at,
                        record.read_at.as_deref(),
                    ],
                )?;
            }
            Ok(())
        })?;
        info!("Rust 通知增量写入完成：count={}", records.len());
        self.list_notifications()
    }

    /// 将指定提醒标记为已读，重复调用保持原已读时间。
    pub(crate) fn mark_notification_read(
        &self,
        notification_id: &str,
    ) -> Result<Vec<NotificationRecord>, StorageError> {
        validate_identifier("notification.id", notification_id)?;
        self.connection.execute(
            "UPDATE notification SET read_at = COALESCE(read_at, ?1) WHERE id = ?2",
            params![now_iso(), notification_id],
        )?;
        info!("Rust 通知已标记已读：id={notification_id}");
        self.list_notifications()
    }

    /// 将全部提醒标记为已读，重复调用保持原已读时间。
    pub(crate) fn mark_all_notifications_read(
        &self,
    ) -> Result<Vec<NotificationRecord>, StorageError> {
        self.connection.execute(
            "UPDATE notification SET read_at = COALESCE(read_at, ?1)",
            params![now_iso()],
        )?;
        info!("Rust 全部通知已标记已读");
        self.list_notifications()
    }

    /// 清空提醒中心全部记录。
    pub(crate) fn clear_notifications(&self) -> Result<Vec<NotificationRecord>, StorageError> {
        let removed = self.connection.execute("DELETE FROM notification", [])?;
        info!("Rust 通知已清空：removed={removed}");
        Ok(Vec::new())
    }

    /// 按可选年月读取并排序本地番剧目录。
    pub(crate) fn list_anime_catalog(
        &self,
        year: Option<i64>,
        month: Option<i64>,
    ) -> Result<Vec<Anime>, StorageError> {
        if month.is_some_and(|value| !(1..=12).contains(&value)) {
            return invalid_input("month", "月份必须在 1 到 12 之间");
        }
        let aliases = self.list_aliases_by_anime()?;
        let rows = match (year, month) {
            (Some(year), Some(month)) => query_all_with_params(
                self.connection,
                "SELECT * FROM anime_catalog WHERE premiere_year = ?1 AND premiere_month = ?2",
                params![year, month],
                map_anime_row,
            )?,
            _ => query_all(
                self.connection,
                "SELECT * FROM anime_catalog",
                map_anime_row,
            )?,
        };
        let mut items = rows
            .into_iter()
            .map(|row| {
                let anime_aliases = aliases.get(&row.id).cloned().unwrap_or_default();
                row.into_domain(anime_aliases)
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_anime_catalog(&mut items);
        Ok(items)
    }

    /// 按目录标识读取一部番剧及其别名。
    pub(crate) fn get_anime_catalog_by_id(
        &self,
        anime_id: &str,
    ) -> Result<Option<Anime>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT * FROM anime_catalog WHERE id = ?1",
                [anime_id],
                map_anime_row,
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let aliases = query_all_with_params(
            self.connection,
            "SELECT * FROM anime_alias WHERE anime_id = ?1 ORDER BY priority DESC",
            [anime_id],
            map_alias_row,
        )?
        .into_iter()
        .map(AnimeAliasRow::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
        row.into_domain(aliases).map(Some)
    }

    /// 按标题、原名和别名搜索本地番剧目录。
    pub(crate) fn search_anime_catalog(
        &self,
        keyword: &str,
    ) -> Result<AnimeDiscoverySearchResult, StorageError> {
        let keyword = keyword.trim();
        let normalized = keyword.to_lowercase();
        let items = self
            .list_anime_catalog(None, None)?
            .into_iter()
            .filter(|anime| {
                normalized.is_empty()
                    || anime.title.to_lowercase().contains(&normalized)
                    || anime
                        .original_title
                        .as_deref()
                        .is_some_and(|title| title.to_lowercase().contains(&normalized))
                    || anime
                        .aliases
                        .iter()
                        .any(|alias| alias.alias.to_lowercase().contains(&normalized))
            })
            .collect();
        Ok(AnimeDiscoverySearchResult {
            keyword: keyword.to_owned(),
            items,
            source: "local".to_owned(),
            errors: Vec::new(),
        })
    }

    /// 合并并原子保存一批番剧目录记录。
    pub(crate) fn upsert_anime_catalog(
        &self,
        items: &[Anime],
    ) -> Result<AnimeCatalogWriteResult, StorageError> {
        self.persist_anime_catalog(items, None, true)
    }

    /// 增量写入详情补全结果，详情刷新时间变化也需要落库。
    pub(crate) fn upsert_anime_catalog_details(
        &self,
        items: &[Anime],
    ) -> Result<AnimeCatalogWriteResult, StorageError> {
        self.persist_anime_catalog(items, None, false)
    }

    /// 原子替换指定月份的未引用缓存，并保留业务引用记录。
    pub(crate) fn replace_anime_catalog_month(
        &self,
        year: i64,
        month: i64,
        items: &[Anime],
    ) -> Result<AnimeCatalogWriteResult, StorageError> {
        if !(1..=12).contains(&month) {
            return invalid_input("month", "月份必须在 1 到 12 之间");
        }
        self.persist_anime_catalog(items, Some((year, month)), false)
    }

    /// 读取指定季度的新番目录同步状态。
    pub(crate) fn get_anime_season_sync_state(
        &self,
        year: i64,
        season: &str,
    ) -> Result<Option<AnimeSeasonSyncState>, StorageError> {
        validate_anime_season(season)?;
        self.connection
            .query_row(
                "SELECT * FROM anime_season_sync_state WHERE year = ?1 AND season = ?2",
                params![year, season],
                map_anime_season_sync_state_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// 保存指定季度的新番目录同步状态。
    pub(crate) fn upsert_anime_season_sync_state(
        &self,
        state: &AnimeSeasonSyncState,
    ) -> Result<(), StorageError> {
        validate_anime_season(&state.season)?;
        self.connection.execute(
            "INSERT INTO anime_season_sync_state (
               year, season, last_attempt_at, last_successful_sync_at,
               completed_at, last_anilist_error, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(year, season) DO UPDATE SET
               last_attempt_at = excluded.last_attempt_at,
               last_successful_sync_at = excluded.last_successful_sync_at,
               completed_at = excluded.completed_at,
               last_anilist_error = excluded.last_anilist_error,
               updated_at = excluded.updated_at",
            params![
                state.year,
                state.season,
                state.last_attempt_at,
                state.last_successful_sync_at,
                state.completed_at,
                state.last_anilist_error,
                now_iso(),
            ],
        )?;
        Ok(())
    }

    /// 读取全部来源级番剧详情刷新状态。
    pub(crate) fn list_anime_detail_refresh_states(
        &self,
    ) -> Result<Vec<AnimeDetailRefreshState>, StorageError> {
        query_all(
            self.connection,
            "SELECT * FROM anime_detail_refresh_state",
            map_anime_detail_refresh_state_row,
        )
    }

    /// 原子保存一批来源级番剧详情刷新状态。
    pub(crate) fn upsert_anime_detail_refresh_states(
        &self,
        states: &[AnimeDetailRefreshState],
    ) -> Result<(), StorageError> {
        for state in states {
            validate_identifier("anime_detail_refresh_state.anime_id", &state.anime_id)?;
            if !matches!(state.provider.as_str(), "bangumi" | "mikan") {
                return invalid_input(
                    "anime_detail_refresh_state.provider",
                    "详情来源仅支持 bangumi 或 mikan",
                );
            }
            if state.external_id.trim().is_empty() {
                return invalid_input("anime_detail_refresh_state.external_id", "外部标识不能为空");
            }
            if !(0..=6).contains(&state.slot_day) {
                return invalid_input(
                    "anime_detail_refresh_state.slot_day",
                    "周期分片必须在 0 到 6 之间",
                );
            }
            if state.failure_count < 0 {
                return invalid_input(
                    "anime_detail_refresh_state.failure_count",
                    "失败次数不能为负数",
                );
            }
        }
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            for state in states {
                connection.execute(
                    "INSERT INTO anime_detail_refresh_state (
                       anime_id, provider, external_id, slot_day, last_completed_cycle,
                       last_attempt_at, last_success_at, failure_count, next_retry_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(anime_id, provider) DO UPDATE SET
                       external_id = excluded.external_id, slot_day = excluded.slot_day,
                       last_completed_cycle = excluded.last_completed_cycle,
                       last_attempt_at = excluded.last_attempt_at,
                       last_success_at = excluded.last_success_at,
                       failure_count = excluded.failure_count,
                       next_retry_at = excluded.next_retry_at,
                       updated_at = excluded.updated_at",
                    params![
                        &state.anime_id,
                        &state.provider,
                        &state.external_id,
                        state.slot_day,
                        state.last_completed_cycle,
                        &state.last_attempt_at,
                        &state.last_success_at,
                        state.failure_count,
                        &state.next_retry_at,
                        &timestamp,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// 聚合本地番剧、追番、单集和字幕组供详情页首屏使用。
    pub(crate) fn get_anime_detail(
        &self,
        anime_id: &str,
    ) -> Result<AnimeDetailResult, StorageError> {
        let anime = self.get_anime_catalog_by_id(anime_id)?.ok_or_else(|| {
            StorageError::RecordNotFound {
                entity: "番剧",
                id: anime_id.to_owned(),
            }
        })?;
        let my_anime = self
            .list_my_anime()?
            .into_iter()
            .find(|item| item.anime.id == anime_id);
        let episodes = self.list_episodes(anime_id)?;
        let fansub_groups = self.list_fansubs(Some(anime_id))?;
        let refreshed_at = anime
            .detail
            .as_ref()
            .and_then(|detail| detail.get("refreshedAt"))
            .and_then(Value::as_str)
            .and_then(parse_timestamp);
        let stale = match refreshed_at {
            Some(value) => Utc::now() - value > Duration::hours(24),
            None => true,
        };
        debug!(
            "Rust 番剧详情聚合完成：anime_id={}, followed={}, episodes={}, fansubs={}, stale={}",
            anime_id,
            my_anime.is_some(),
            episodes.len(),
            fansub_groups.len(),
            stale
        );
        Ok(AnimeDetailResult {
            anime,
            my_anime,
            episodes,
            fansub_groups,
            stale,
            partial_errors: Vec::<AnimeDetailPartialError>::new(),
        })
    }

    /// 读取全部或指定番剧已观察到的字幕组。
    pub(crate) fn list_fansubs(
        &self,
        anime_id: Option<&str>,
    ) -> Result<Vec<FansubGroup>, StorageError> {
        match anime_id {
            Some(anime_id) => query_all_with_params(
                self.connection,
                "SELECT fansub_group.*
                 FROM fansub_group
                 INNER JOIN anime_fansub_group
                   ON anime_fansub_group.fansub_group_id = fansub_group.id
                 WHERE anime_fansub_group.anime_id = ?1
                 ORDER BY anime_fansub_group.last_seen_at DESC, fansub_group.name",
                [anime_id],
                map_fansub_group_row,
            )?
            .into_iter()
            .map(FansubGroupRow::into_domain)
            .collect(),
            None => query_all(
                self.connection,
                "SELECT * FROM fansub_group ORDER BY name",
                map_fansub_group_row,
            )?
            .into_iter()
            .map(FansubGroupRow::into_domain)
            .collect(),
        }
    }

    /// 合并资源中识别到的字幕组，并刷新番剧关联的最近发现时间。
    pub(crate) fn observe_anime_fansubs(
        &self,
        anime_id: &str,
        releases: &[Release],
    ) -> Result<Vec<FansubGroup>, StorageError> {
        validate_identifier("animeId", anime_id)?;
        let discovered = collect_discovered_fansubs(releases);
        if discovered.is_empty() {
            return self.list_fansubs(Some(anime_id));
        }
        let existing_by_id = self
            .list_fansubs(None)?
            .into_iter()
            .map(|group| (group.id.clone(), group))
            .collect::<HashMap<_, _>>();
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            for candidate in &discovered {
                let existing = existing_by_id.get(&candidate.id);
                let aliases = merge_unique_strings(
                    existing
                        .into_iter()
                        .flat_map(|group| group.aliases.iter().cloned())
                        .chain(candidate.aliases.iter().cloned())
                        .chain(
                            existing
                                .filter(|group| group.name != candidate.name)
                                .map(|_| candidate.name.clone()),
                        ),
                );
                let source_ids = merge_unique_strings(
                    existing
                        .into_iter()
                        .flat_map(|group| group.source_ids.iter().cloned())
                        .chain(candidate.source_ids.iter().cloned()),
                );
                let name = existing.map_or(candidate.name.as_str(), |group| group.name.as_str());
                let aliases_json = serde_json::to_string(&aliases).map_err(|source| {
                    StorageError::JsonData {
                        context: "字幕组别名",
                        source,
                    }
                })?;
                let source_ids_json = serde_json::to_string(&source_ids).map_err(|source| {
                    StorageError::JsonData {
                        context: "字幕组来源",
                        source,
                    }
                })?;
                connection.execute(
                    "INSERT INTO fansub_group (
                       id, name, aliases_json, source_ids_json, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                     ON CONFLICT(id) DO UPDATE SET
                       name = excluded.name, aliases_json = excluded.aliases_json,
                       source_ids_json = excluded.source_ids_json, updated_at = excluded.updated_at",
                    params![
                        &candidate.id,
                        name,
                        aliases_json,
                        source_ids_json,
                        &timestamp
                    ],
                )?;
                connection.execute(
                    "INSERT INTO anime_fansub_group (
                       anime_id, fansub_group_id, first_seen_at, last_seen_at
                     ) VALUES (?1, ?2, ?3, ?3)
                     ON CONFLICT(anime_id, fansub_group_id) DO UPDATE SET
                       last_seen_at = excluded.last_seen_at",
                    params![anime_id, &candidate.id, &timestamp],
                )?;
            }
            Ok(())
        })?;
        info!(
            "Rust 番剧字幕组观察完成：anime_id={}, group_count={}",
            anime_id,
            discovered.len()
        );
        self.list_fansubs(Some(anime_id))
    }

    /// 读取指定番剧的全部来源绑定。
    pub(crate) fn list_anime_source_bindings(
        &self,
        anime_id: &str,
    ) -> Result<Vec<AnimeSourceBinding>, StorageError> {
        validate_identifier("animeId", anime_id)?;
        query_all_with_params(
            self.connection,
            "SELECT * FROM anime_source_binding WHERE anime_id = ?1 ORDER BY source_id",
            [anime_id],
            map_anime_source_binding_row,
        )?
        .into_iter()
        .map(AnimeSourceBindingRow::into_domain)
        .collect()
    }

    /// 保存来源绑定，同一番剧和来源仅保留一项。
    pub(crate) fn upsert_anime_source_binding(
        &self,
        binding: &AnimeSourceBinding,
    ) -> Result<Vec<AnimeSourceBinding>, StorageError> {
        validate_identifier("binding.id", &binding.id)?;
        validate_identifier("binding.animeId", &binding.anime_id)?;
        validate_identifier("binding.sourceId", &binding.source_id)?;
        validate_identifier("binding.sourceAnimeId", &binding.source_anime_id)?;
        validate_optional_http_url("binding.sourceUrl", binding.source_url.as_deref())?;
        if !binding.confidence.is_finite() {
            return invalid_input("binding.confidence", "绑定置信度必须是有限数值");
        }
        self.connection.execute(
            "INSERT INTO anime_source_binding (
               id, anime_id, source_id, source_anime_id, source_anime_title, source_url,
               match_method, confidence, confirmed, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(anime_id, source_id) DO UPDATE SET
               id = excluded.id, source_anime_id = excluded.source_anime_id,
               source_anime_title = excluded.source_anime_title,
               source_url = excluded.source_url, match_method = excluded.match_method,
               confidence = excluded.confidence, confirmed = excluded.confirmed,
               updated_at = excluded.updated_at",
            params![
                &binding.id,
                &binding.anime_id,
                &binding.source_id,
                binding.source_anime_id.trim(),
                binding.source_anime_title.as_deref(),
                binding.source_url.as_deref(),
                anime_source_match_method_value(&binding.match_method),
                binding.confidence.clamp(0.0, 1.0),
                i64::from(binding.confirmed),
                &binding.created_at,
                &binding.updated_at,
            ],
        )?;
        info!(
            "Rust 番剧来源绑定保存完成：anime_id={}, source_id={}, confirmed={}",
            binding.anime_id, binding.source_id, binding.confirmed
        );
        self.list_anime_source_bindings(&binding.anime_id)
    }

    /// 删除指定番剧和来源的绑定。
    pub(crate) fn remove_anime_source_binding(
        &self,
        anime_id: &str,
        source_id: &str,
    ) -> Result<Vec<AnimeSourceBinding>, StorageError> {
        validate_identifier("animeId", anime_id)?;
        validate_identifier("sourceId", source_id)?;
        self.connection.execute(
            "DELETE FROM anime_source_binding WHERE anime_id = ?1 AND source_id = ?2",
            params![anime_id, source_id],
        )?;
        self.list_anime_source_bindings(anime_id)
    }

    /// 读取指定番剧的全部来源排除记录。
    pub(crate) fn list_anime_source_exclusions(
        &self,
        anime_id: &str,
    ) -> Result<Vec<AnimeSourceExclusion>, StorageError> {
        validate_identifier("animeId", anime_id)?;
        query_all_with_params(
            self.connection,
            "SELECT * FROM anime_source_exclusion
             WHERE anime_id = ?1 ORDER BY source_id, source_anime_id",
            [anime_id],
            map_anime_source_exclusion_row,
        )?
        .into_iter()
        .map(AnimeSourceExclusionRow::into_domain)
        .collect()
    }

    /// 保存单候选或整来源排除记录。
    pub(crate) fn upsert_anime_source_exclusion(
        &self,
        exclusion: &AnimeSourceExclusion,
    ) -> Result<Vec<AnimeSourceExclusion>, StorageError> {
        validate_identifier("exclusion.id", &exclusion.id)?;
        validate_identifier("exclusion.animeId", &exclusion.anime_id)?;
        validate_identifier("exclusion.sourceId", &exclusion.source_id)?;
        let source_anime_id = exclusion
            .source_anime_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if exclusion.scope == AnimeSourceExclusionScope::Candidate && source_anime_id.is_empty() {
            return invalid_input("exclusion.sourceAnimeId", "候选排除必须包含来源番剧 ID");
        }
        self.connection.execute(
            "INSERT INTO anime_source_exclusion (
               id, anime_id, source_id, scope, source_anime_id, source_anime_title,
               created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(anime_id, source_id, source_anime_id) DO UPDATE SET
               id = excluded.id, scope = excluded.scope,
               source_anime_title = excluded.source_anime_title,
               updated_at = excluded.updated_at",
            params![
                &exclusion.id,
                &exclusion.anime_id,
                &exclusion.source_id,
                anime_source_exclusion_scope_value(&exclusion.scope),
                source_anime_id,
                exclusion.source_anime_title.as_deref(),
                &exclusion.created_at,
                &exclusion.updated_at,
            ],
        )?;
        info!(
            "Rust 番剧来源排除保存完成：anime_id={}, source_id={}, scope={}",
            exclusion.anime_id,
            exclusion.source_id,
            anime_source_exclusion_scope_value(&exclusion.scope)
        );
        self.list_anime_source_exclusions(&exclusion.anime_id)
    }

    /// 删除单候选或整来源排除记录。
    pub(crate) fn remove_anime_source_exclusion(
        &self,
        anime_id: &str,
        source_id: &str,
        source_anime_id: Option<&str>,
    ) -> Result<Vec<AnimeSourceExclusion>, StorageError> {
        validate_identifier("animeId", anime_id)?;
        validate_identifier("sourceId", source_id)?;
        self.connection.execute(
            "DELETE FROM anime_source_exclusion
             WHERE anime_id = ?1 AND source_id = ?2 AND source_anime_id = ?3",
            params![anime_id, source_id, source_anime_id.unwrap_or_default()],
        )?;
        self.list_anime_source_exclusions(anime_id)
    }

    /// 按名称读取全部下载源配置。
    pub(crate) fn list_sources(&self) -> Result<Vec<ReleaseSourceConfig>, StorageError> {
        let mut sources = query_all(
            self.connection,
            "SELECT * FROM release_source ORDER BY name",
            map_release_source_row,
        )?
        .into_iter()
        .map(ReleaseSourceRow::into_domain)
        .collect::<Result<Vec<_>, _>>()?;
        for source in &mut sources {
            let Some(stored) = source.api_key.clone() else {
                continue;
            };
            let key = format!("sources.{}.api-key", source.id);
            let (hydrated, migrated) = self.hydrate_secret_value(&key, &stored);
            source.api_key = (!hydrated.is_empty()).then_some(hydrated);
            if migrated {
                self.connection.execute(
                    "UPDATE release_source SET api_key = ?1, updated_at = ?2 WHERE id = ?3",
                    params![
                        format!("{SECURE_MARKER_PREFIX}{key}"),
                        now_iso(),
                        &source.id
                    ],
                )?;
                info!(
                    "Rust 下载源 API Key 已迁移到平台安全存储 source_id={}",
                    source.id
                );
            }
        }
        Ok(sources)
    }

    /// 启用或停用一个下载源。
    pub(crate) fn set_source_enabled(
        &self,
        source_id: &str,
        enabled: bool,
    ) -> Result<Vec<ReleaseSourceConfig>, StorageError> {
        validate_identifier("sourceId", source_id)?;
        let changed = self.connection.execute(
            "UPDATE release_source SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(enabled), now_iso(), source_id],
        )?;
        if changed == 0 {
            return Err(StorageError::RecordNotFound {
                entity: "下载源",
                id: source_id.to_owned(),
            });
        }
        info!("Rust 下载源状态更新：source_id={source_id}, enabled={enabled}");
        self.list_sources()
    }

    /// 新增或更新下载源配置。
    pub(crate) fn upsert_source(
        &self,
        source: &ReleaseSourceConfig,
    ) -> Result<Vec<ReleaseSourceConfig>, StorageError> {
        validate_identifier("source.id", &source.id)?;
        if source.name.trim().is_empty() {
            return invalid_input("source.name", "下载源名称不能为空");
        }
        validate_optional_http_url("source.baseUrl", source.base_url.as_deref())?;
        validate_optional_http_url("source.rssUrl", source.rss_url.as_deref())?;
        let secret_key = format!("sources.{}.api-key", source.id);
        let persisted_api_key =
            self.protect_secret_value(&secret_key, source.api_key.as_deref().unwrap_or_default())?;
        let timestamp = now_iso();
        let tags_json =
            serde_json::to_string(&source.tags).map_err(|source| StorageError::JsonData {
                context: "下载源标签",
                source,
            })?;
        self.connection.execute(
            "INSERT INTO release_source (
               id, name, kind, enabled, use_proxy, request_interval_ms, base_url, api_key,
               rss_url, tags_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, kind = excluded.kind, enabled = excluded.enabled,
               use_proxy = excluded.use_proxy, request_interval_ms = excluded.request_interval_ms,
               base_url = excluded.base_url, api_key = excluded.api_key,
               rss_url = excluded.rss_url, tags_json = excluded.tags_json,
               updated_at = excluded.updated_at",
            params![
                &source.id,
                source.name.trim(),
                source_kind_value(&source.kind),
                i64::from(source.enabled),
                i64::from(source.use_proxy),
                normalize_source_request_interval(source.request_interval_ms),
                source.base_url.as_deref(),
                persisted_api_key.as_deref(),
                source.rss_url.as_deref(),
                tags_json,
                timestamp,
            ],
        )?;
        info!(
            "Rust 下载源保存完成：source_id={}, kind={}, enabled={}, request_interval_ms={}",
            source.id,
            source_kind_value(&source.kind),
            source.enabled,
            normalize_source_request_interval(source.request_interval_ms)
        );
        self.list_sources()
    }

    /// 读取全部来源同步和条件请求游标。
    pub(crate) fn list_source_sync_states(
        &self,
    ) -> Result<Vec<ReleaseSourceSyncState>, StorageError> {
        query_all(
            self.connection,
            "SELECT * FROM release_source_sync_state ORDER BY source_id",
            map_source_sync_state_row,
        )
    }

    /// 保存单个来源同步和条件请求游标。
    pub(crate) fn upsert_source_sync_state(
        &self,
        state: &ReleaseSourceSyncState,
    ) -> Result<(), StorageError> {
        validate_identifier("sourceSyncState.sourceId", &state.source_id)?;
        self.connection.execute(
            "INSERT INTO release_source_sync_state (
               source_id, request_host, last_request_at, request_failure_count, backoff_until,
               last_sync_attempt_at, last_successful_sync_at, last_sync_error, etag,
               last_modified, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(source_id) DO UPDATE SET
               request_host = excluded.request_host, last_request_at = excluded.last_request_at,
               request_failure_count = excluded.request_failure_count,
               backoff_until = excluded.backoff_until,
               last_sync_attempt_at = excluded.last_sync_attempt_at,
               last_successful_sync_at = excluded.last_successful_sync_at,
               last_sync_error = excluded.last_sync_error, etag = excluded.etag,
               last_modified = excluded.last_modified, updated_at = excluded.updated_at",
            params![
                &state.source_id,
                state.request_host.as_deref(),
                state.last_request_at.as_deref(),
                state.request_failure_count.max(0),
                state.backoff_until.as_deref(),
                state.last_sync_attempt_at.as_deref(),
                state.last_successful_sync_at.as_deref(),
                state.last_sync_error.as_deref(),
                state.etag.as_deref(),
                state.last_modified.as_deref(),
                now_iso(),
            ],
        )?;
        Ok(())
    }

    /// 读取一个通用网络熔断状态。
    pub(crate) fn get_request_circuit_state(
        &self,
        key: &str,
    ) -> Result<Option<RequestCircuitState>, StorageError> {
        self.connection
            .query_row(
                "SELECT * FROM request_circuit_state WHERE circuit_key = ?1",
                [key],
                map_request_circuit_state_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// 保存通用网络熔断状态。
    pub(crate) fn upsert_request_circuit_state(
        &self,
        state: &RequestCircuitState,
    ) -> Result<(), StorageError> {
        validate_identifier("requestCircuit.key", &state.key)?;
        validate_identifier("requestCircuit.group", &state.group)?;
        self.connection.execute(
            "INSERT INTO request_circuit_state (
               circuit_key, circuit_group, request_host, last_request_at,
               failure_count, backoff_until, network_context, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(circuit_key) DO UPDATE SET
               circuit_group = excluded.circuit_group, request_host = excluded.request_host,
               last_request_at = excluded.last_request_at, failure_count = excluded.failure_count,
               backoff_until = excluded.backoff_until,
               network_context = excluded.network_context, updated_at = excluded.updated_at",
            params![
                &state.key,
                &state.group,
                state.request_host.as_deref(),
                state.last_request_at.as_deref(),
                state.failure_count.max(0),
                state.backoff_until.as_deref(),
                state.network_context.as_deref(),
                now_iso(),
            ],
        )?;
        Ok(())
    }

    /// 删除一个已恢复的通用网络熔断状态。
    pub(crate) fn clear_request_circuit_state(&self, key: &str) -> Result<(), StorageError> {
        self.connection.execute(
            "DELETE FROM request_circuit_state WHERE circuit_key = ?1",
            [key],
        )?;
        Ok(())
    }

    /// 读取尚未过期的跨重启资源搜索缓存。
    pub(crate) fn get_release_search_cache(
        &self,
        cache_key: &str,
        current_time: &str,
    ) -> Result<Option<ReleaseSearchCacheEntry>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT result_json, expires_at FROM release_search_cache
                 WHERE cache_key = ?1 AND expires_at > ?2",
                params![cache_key, current_time],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(result_json, expires_at)| {
            Ok(ReleaseSearchCacheEntry {
                result: parse_json(&result_json, "资源搜索缓存")?,
                expires_at,
            })
        })
        .transpose()
    }

    /// 保存资源搜索结果并清理已过期缓存。
    pub(crate) fn upsert_release_search_cache(
        &self,
        cache_key: &str,
        entry: &ReleaseSearchCacheEntry,
    ) -> Result<(), StorageError> {
        validate_identifier("releaseSearchCache.cacheKey", cache_key)?;
        let result_json =
            serde_json::to_string(&entry.result).map_err(|source| StorageError::JsonData {
                context: "资源搜索缓存",
                source,
            })?;
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            connection.execute(
                "DELETE FROM release_search_cache WHERE expires_at <= ?1",
                [&timestamp],
            )?;
            connection.execute(
                "INSERT INTO release_search_cache (cache_key, result_json, expires_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(cache_key) DO UPDATE SET
                   result_json = excluded.result_json, expires_at = excluded.expires_at,
                   updated_at = excluded.updated_at",
                params![cache_key, result_json, &entry.expires_at, &timestamp],
            )?;
            Ok(())
        })
    }

    /// 按来源和番剧读取最近采集的原始资源缓存。
    pub(crate) fn list_cached_releases(
        &self,
        query: &CachedReleaseQuery,
    ) -> Result<Vec<Release>, StorageError> {
        let limit = query.limit.unwrap_or(2_000).clamp(1, 10_000);
        let mut conditions = Vec::new();
        let mut values = Vec::<SqlValue>::new();
        if let Some(source_ids) = query.source_ids.as_ref() {
            let source_ids = merge_unique_strings(
                source_ids
                    .iter()
                    .map(|source_id| source_id.trim().to_owned())
                    .filter(|source_id| !source_id.is_empty()),
            );
            if source_ids.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders = source_ids
                .into_iter()
                .map(|source_id| {
                    values.push(SqlValue::Text(source_id));
                    format!("?{}", values.len())
                })
                .collect::<Vec<_>>();
            conditions.push(format!("source_id IN ({})", placeholders.join(", ")));
        }
        if let Some(anime_id) = query.anime_id.as_deref() {
            let anime_id = anime_id.trim();
            if anime_id.is_empty() {
                return Ok(Vec::new());
            }
            values.push(SqlValue::Text(anime_id.to_owned()));
            conditions.push(format!("anime_id = ?{}", values.len()));
        }
        values.push(SqlValue::Integer(limit as i64));
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT * FROM release{where_clause} ORDER BY published_at DESC LIMIT ?{}",
            values.len()
        );
        query_all_with_params(
            self.connection,
            &sql,
            params_from_iter(values.iter()),
            map_cached_release_row,
        )?
        .into_iter()
        .map(CachedReleaseRow::into_domain)
        .collect()
    }

    /// 按稳定资源 ID 增量写入缓存，并返回首次出现数量。
    pub(crate) fn upsert_cached_releases(
        &self,
        releases: &[Release],
    ) -> Result<usize, StorageError> {
        let mut unique = HashMap::<String, Release>::new();
        for release in releases {
            validate_identifier("release.id", &release.id)?;
            validate_identifier("release.sourceId", &release.source_id)?;
            if release.title.trim().is_empty() {
                return invalid_input("release.title", "资源标题不能为空");
            }
            if release.published_at.trim().is_empty() {
                return invalid_input("release.publishedAt", "资源发布时间不能为空");
            }
            unique.insert(release.id.clone(), release.clone());
        }
        if unique.is_empty() {
            return Ok(0);
        }
        let mut added_count = 0;
        for release_id in unique.keys() {
            if !self
                .connection
                .query_row("SELECT 1 FROM release WHERE id = ?1", [release_id], |_| {
                    Ok(())
                })
                .optional()?
                .is_some()
            {
                added_count += 1;
            }
        }
        self.with_transaction(|connection| {
            for release in unique.values() {
                upsert_cached_release_row(connection, release)?;
            }
            Ok(())
        })?;
        info!(
            "Rust 资源缓存写入完成：total={}, added={}",
            unique.len(),
            added_count
        );
        Ok(added_count)
    }

    /// 清理指定发布时间之前的资源缓存。
    pub(crate) fn prune_cached_releases(&self, before: &str) -> Result<usize, StorageError> {
        if before.trim().is_empty() {
            return invalid_input("before", "资源缓存清理时间不能为空");
        }
        let deleted = self
            .connection
            .execute("DELETE FROM release WHERE published_at < ?1", [before])?;
        info!(
            "Rust 过期资源缓存清理完成：before={}, deleted={}",
            before, deleted
        );
        Ok(deleted)
    }

    /// 读取并按季度、标题排序我的追番。
    pub(crate) fn list_my_anime(&self) -> Result<Vec<MyAnime>, StorageError> {
        let anime = self.list_anime_catalog(None, None)?;
        let anime_by_id = anime
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let binding_aliases = self.list_confirmed_binding_title_aliases_by_anime()?;
        let subscriptions = self.list_rss_subscriptions_by_my_anime()?;
        let rows = query_all(self.connection, "SELECT * FROM my_anime", map_my_anime_row)?;
        let mut items = rows
            .into_iter()
            .filter_map(|row| {
                let mut anime = anime_by_id.get(&row.anime_id)?.clone();
                if let Some(aliases) = binding_aliases.get(&row.anime_id) {
                    anime.aliases = merge_anime_aliases(&anime.aliases, aliases, &anime.id);
                    anime.aliases.sort_by_key(|alias| Reverse(alias.priority));
                }
                let rss_subscriptions = subscriptions.get(&row.id).cloned().unwrap_or_default();
                Some(row.into_domain(anime, rss_subscriptions))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_my_anime(&mut items);
        Ok(items)
    }

    /// 在单个事务中保存番剧目录、追番规则和 RSS 订阅。
    pub(crate) fn upsert_my_anime(&self, mut item: MyAnime) -> Result<Vec<MyAnime>, StorageError> {
        validate_identifier("myAnime.id", &item.id)?;
        validate_identifier("myAnime.anime.id", &item.anime.id)?;
        if item.anime.title.trim().is_empty() {
            return invalid_input("myAnime.anime.title", "番剧标题不能为空");
        }

        let timestamp = now_iso();
        if item.added_at.trim().is_empty() {
            item.added_at = timestamp.clone();
        }
        item.updated_at = timestamp.clone();
        if matches!(item.status, AnimeStatus::Completed | AnimeStatus::Dropped) {
            item.auto_download = false;
        }

        self.with_transaction(|connection| {
            upsert_anime_row(connection, &item.anime, &timestamp)?;
            upsert_my_anime_row(connection, &item, &timestamp)
        })?;
        info!(
            "Rust 追番保存完成：item_id={}, anime_id={}, status={}",
            item.id,
            item.anime.id,
            anime_status_value(&item.status)
        );
        self.list_my_anime()
    }

    /// 删除追番及其单集业务数据，保留可复用的番剧目录记录。
    pub(crate) fn remove_my_anime(&self, item_id: &str) -> Result<Vec<MyAnime>, StorageError> {
        validate_identifier("itemId", item_id)?;
        let anime_id = self
            .connection
            .query_row(
                "SELECT anime_id FROM my_anime WHERE id = ?1",
                [item_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        self.with_transaction(|connection| {
            connection.execute("DELETE FROM my_anime WHERE id = ?1", [item_id])?;
            if let Some(anime_id) = anime_id.as_deref() {
                connection.execute("DELETE FROM episode WHERE anime_id = ?1", [anime_id])?;
            }
            Ok(())
        })?;
        info!("Rust 追番删除完成：item_id={item_id}");
        self.list_my_anime()
    }

    /// 读取指定番剧的全部单集。
    pub(crate) fn list_episodes(&self, anime_id: &str) -> Result<Vec<Episode>, StorageError> {
        query_all_with_params(
            self.connection,
            "SELECT * FROM episode WHERE anime_id = ?1 ORDER BY episode_no",
            [anime_id],
            map_episode_row,
        )?
        .into_iter()
        .map(EpisodeRow::into_domain)
        .collect()
    }

    /// 新增或更新一条单集记录。
    pub(crate) fn upsert_episode(&self, episode: &Episode) -> Result<Vec<Episode>, StorageError> {
        validate_episode(episode)?;
        let timestamp = now_iso();
        let (linked_tasks, linked_files) = self.with_transaction(|connection| {
            let (linked_tasks, linked_files) = upsert_episode_row(connection, episode, &timestamp)?;
            if linked_tasks > 0 || linked_files > 0 {
                sync_episode_statuses_from_downloads(connection, [episode.id.clone()])?;
            }
            Ok((linked_tasks, linked_files))
        })?;
        if linked_tasks > 0 || linked_files > 0 {
            info!(
                "Rust 单集写入后回填历史下载关联：episode_id={}, linked_tasks={}, linked_files={}",
                episode.id, linked_tasks, linked_files
            );
        }
        self.list_episodes(&episode.anime_id)
    }

    /// 汇总全部追番的连续观看进度。
    pub(crate) fn list_my_anime_watch_progress(
        &self,
    ) -> Result<Vec<AnimeWatchProgress>, StorageError> {
        self.list_my_anime()?
            .into_iter()
            .map(|item| {
                let episodes = self.list_episodes(&item.anime.id)?;
                Ok(build_anime_watch_progress(&item, &episodes))
            })
            .collect()
    }

    /// 在单个事务中补齐单集并批量调整已看状态。
    pub(crate) fn set_anime_watch_progress(
        &self,
        input: &SetAnimeWatchProgressInput,
    ) -> Result<AnimeWatchProgress, StorageError> {
        if !(0..=10_000).contains(&input.watched_episode_count) {
            return invalid_input(
                "watchedEpisodeCount",
                "观看进度必须是 0 到 10000 之间的整数",
            );
        }
        let item = self
            .list_my_anime()?
            .into_iter()
            .find(|item| item.anime.id == input.anime_id)
            .ok_or_else(|| StorageError::RecordNotFound {
                entity: "追番",
                id: input.anime_id.clone(),
            })?;
        let episodes = self.list_episodes(&input.anime_id)?;
        let episode_by_number = episodes
            .iter()
            .filter(|episode| is_positive_integer(episode.episode_no))
            .map(|episode| (episode.episode_no as i64, episode.clone()))
            .collect::<HashMap<_, _>>();
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            for episode_no in 1..=input.watched_episode_count {
                let mut episode =
                    episode_by_number
                        .get(&episode_no)
                        .cloned()
                        .unwrap_or_else(|| Episode {
                            id: create_download_episode_id(&input.anime_id, episode_no),
                            anime_id: input.anime_id.clone(),
                            episode_no: episode_no as f64,
                            title: None,
                            air_time: None,
                            status: EpisodeStatus::Aired,
                        });
                episode.status = EpisodeStatus::Watched;
                upsert_episode_row(connection, &episode, &timestamp)?;
            }

            for episode in episodes.iter().filter(|episode| {
                episode.episode_no > input.watched_episode_count as f64
                    && episode.status == EpisodeStatus::Watched
            }) {
                let mut episode = episode.clone();
                episode.status = resolve_episode_status_after_unwatch(connection, &episode)?;
                upsert_episode_row(connection, &episode, &timestamp)?;
            }
            connection.execute(
                "UPDATE my_anime SET updated_at = ?1 WHERE anime_id = ?2",
                params![&timestamp, &input.anime_id],
            )?;
            Ok(())
        })?;

        let progress = build_anime_watch_progress(&item, &self.list_episodes(&input.anime_id)?);
        info!(
            "Rust 观看进度更新完成：anime_id={}, watched={}, total={}",
            progress.anime_id, progress.watched_episode_count, progress.total_episode_count
        );
        Ok(progress)
    }

    /// 按下载任务和文件关联将达到阈值的单集标记为已看。
    pub(crate) fn report_playback_progress(
        &self,
        input: &ReportPlaybackProgressInput,
    ) -> Result<bool, StorageError> {
        if !input.percent.is_finite() || input.percent < PLAYBACK_COMPLETION_THRESHOLD_PERCENT {
            return Ok(false);
        }
        if input.file_index.is_some_and(|index| index < 0) {
            return invalid_input("fileIndex", "播放文件索引必须是非负整数");
        }
        let task = self
            .list_downloads()?
            .into_iter()
            .find(|task| task.id == input.task_id);
        let Some(task) = task else {
            warn!(
                "Rust 播放进度未找到下载任务：task_id={}, file_index={:?}",
                input.task_id, input.file_index
            );
            return Ok(false);
        };

        let task_file = input
            .file_index
            .and_then(|index| task.files.iter().find(|file| file.index == index));
        let media_file = self.list_media_files()?.into_iter().find(|media| {
            media.download_task_id.as_deref() == Some(task.id.as_str())
                && task_file
                    .map(|file| {
                        media.file_name == file.name || media.file_path.ends_with(&file.name)
                    })
                    .unwrap_or(true)
        });
        let anime_id = media_file
            .as_ref()
            .map(|media| media.anime_id.as_str())
            .or(task.anime_id.as_deref());
        let Some(anime_id) = anime_id else {
            warn!("Rust 播放进度缺少番剧关联：task_id={}", input.task_id);
            return Ok(false);
        };
        let episode_id = media_file
            .as_ref()
            .and_then(|media| media.episode_id.as_deref())
            .or_else(|| task_file.and_then(|file| file.episode_id.as_deref()))
            .or(task.episode_id.as_deref());
        let episode_no = task_file
            .and_then(|file| file.episode_no)
            .or(task.episode_no);
        let episode = self.list_episodes(anime_id)?.into_iter().find(|episode| {
            episode_id == Some(episode.id.as_str()) || episode_no == Some(episode.episode_no)
        });
        let Some(mut episode) = episode else {
            warn!(
                "Rust 播放进度缺少单集关联：task_id={}, anime_id={}",
                input.task_id, anime_id
            );
            return Ok(false);
        };
        if episode.status != EpisodeStatus::Watched {
            episode.status = EpisodeStatus::Watched;
            upsert_episode_row(self.connection, &episode, &now_iso())?;
            info!(
                "Rust 播放进度已标记单集：task_id={}, episode_id={}, percent={}",
                input.task_id, episode.id, input.percent
            );
        }
        Ok(true)
    }

    /// 读取指定下载文件最近一次可靠的播放位置。
    pub(crate) fn get_playback_checkpoint(
        &self,
        task_id: &str,
        file_index: Option<i64>,
    ) -> Result<Option<PlaybackCheckpoint>, StorageError> {
        let file_index = normalize_checkpoint_file_index(file_index);
        self.connection
            .query_row(
                "SELECT * FROM playback_checkpoint WHERE task_id = ?1 AND file_index = ?2",
                params![task_id, file_index],
                map_playback_checkpoint_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// 校验并保存续播位置，首次跨过 90% 时同步已看状态。
    pub(crate) fn save_playback_checkpoint(
        &self,
        input: &SavePlaybackCheckpointInput,
    ) -> Result<PlaybackCheckpoint, StorageError> {
        let normalized = normalize_playback_checkpoint_input(input)?;
        let existing = self.get_playback_checkpoint(&normalized.task_id, normalized.file_index)?;
        let percent =
            calculate_playback_percent(normalized.position_seconds, normalized.duration_seconds);
        let mut watched_reported = existing
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.watched_reported);
        if !watched_reported && percent >= PLAYBACK_COMPLETION_THRESHOLD_PERCENT {
            watched_reported = self.report_playback_progress(&ReportPlaybackProgressInput {
                task_id: normalized.task_id.clone(),
                file_index: normalized.file_index,
                percent,
            })?;
        }
        let completed = existing
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.completed)
            || normalized.completed == Some(true)
            || percent >= PLAYBACK_COMPLETION_THRESHOLD_PERCENT;
        let checkpoint = PlaybackCheckpoint {
            task_id: normalized.task_id,
            file_index: normalized.file_index,
            position_seconds: normalized.position_seconds,
            duration_seconds: normalized.duration_seconds,
            completed,
            watched_reported,
            updated_at: now_iso(),
        };
        self.connection.execute(
            "INSERT INTO playback_checkpoint (
               task_id, file_index, position_seconds, duration_seconds, completed, watched_reported, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(task_id, file_index) DO UPDATE SET
               position_seconds = excluded.position_seconds,
               duration_seconds = excluded.duration_seconds,
               completed = excluded.completed,
               watched_reported = excluded.watched_reported,
               updated_at = excluded.updated_at",
            params![
                &checkpoint.task_id,
                normalize_checkpoint_file_index(checkpoint.file_index),
                checkpoint.position_seconds,
                checkpoint.duration_seconds,
                i64::from(checkpoint.completed),
                i64::from(checkpoint.watched_reported),
                &checkpoint.updated_at,
            ],
        )?;
        info!(
            "Rust 续播位置保存完成：task_id={}, file_index={:?}, percent={:.2}, completed={}, watched_reported={}",
            checkpoint.task_id,
            checkpoint.file_index,
            percent,
            checkpoint.completed,
            checkpoint.watched_reported
        );
        Ok(checkpoint)
    }

    /// 读取指定番剧的单集级偏好。
    pub(crate) fn list_episode_preferences(
        &self,
        anime_id: &str,
    ) -> Result<Vec<EpisodePreference>, StorageError> {
        query_all_with_params(
            self.connection,
            "SELECT * FROM episode_preference WHERE anime_id = ?1 ORDER BY episode_id",
            [anime_id],
            map_episode_preference_row,
        )
    }

    /// 新增或更新一条单集级偏好。
    pub(crate) fn upsert_episode_preference(
        &self,
        preference: &EpisodePreference,
    ) -> Result<Vec<EpisodePreference>, StorageError> {
        validate_identifier("preference.id", &preference.id)?;
        validate_identifier("preference.animeId", &preference.anime_id)?;
        validate_identifier("preference.episodeId", &preference.episode_id)?;
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            upsert_episode_preference_row(connection, preference, &timestamp)
        })?;
        self.list_episode_preferences(&preference.anime_id)
    }

    /// 删除一条单集级偏好并返回同番剧剩余项。
    pub(crate) fn remove_episode_preference(
        &self,
        episode_id: &str,
    ) -> Result<Vec<EpisodePreference>, StorageError> {
        let anime_id = self
            .connection
            .query_row(
                "SELECT anime_id FROM episode_preference WHERE episode_id = ?1",
                [episode_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        self.connection.execute(
            "DELETE FROM episode_preference WHERE episode_id = ?1",
            [episode_id],
        )?;
        match anime_id {
            Some(anime_id) => self.list_episode_preferences(&anime_id),
            None => Ok(Vec::new()),
        }
    }

    /// 从追番、单集、下载和媒体表生成首页实时聚合数据。
    pub(crate) fn get_dashboard(&self) -> Result<DashboardData, StorageError> {
        let stored = self
            .read_json_state("app_state", "dashboard", "首页状态")?
            .unwrap_or_else(|| Value::Object(Default::default()));
        let weekly_schedule = read_dashboard_field::<Vec<WeeklyScheduleDay>>(
            &stored,
            "weeklySchedule",
            "首页周计划",
        )?
        .unwrap_or_default();
        let mut source_health =
            read_dashboard_field::<Vec<SourceHealth>>(&stored, "sourceHealth", "首页来源健康状态")?
                .unwrap_or_default();

        let my_anime = self.list_my_anime()?;
        let episodes = self.list_all_episodes()?;
        let downloads = self.list_downloads()?;
        let mut media_files = self.list_media_files()?;
        let fansub_names = self.list_fansub_names()?;
        let source_enabled = self.list_source_enabled()?;

        let daily_reminder = build_daily_reminder(&my_anime, &episodes, &downloads, &fansub_names);
        let today_episodes = daily_reminder
            .items
            .iter()
            .map(to_episode_summary)
            .collect();
        let pending_actions = build_pending_actions(&my_anime, &episodes, &downloads);
        let active_downloads = downloads
            .iter()
            .filter(|task| task.is_active())
            .cloned()
            .collect::<Vec<_>>();
        media_files.sort_by(|left, right| media_sort_key(right).cmp(media_sort_key(left)));
        media_files.truncate(10);
        for source in &mut source_health {
            if source_enabled.get(&source.source_id) == Some(&false) {
                source.status = "warning".to_owned();
            }
        }

        debug!(
            "Rust 首页聚合完成：followed={}, episodes={}, active_downloads={}, recent_completed={}",
            my_anime.len(),
            episodes.len(),
            active_downloads.len(),
            media_files.len()
        );
        Ok(DashboardData {
            daily_reminder,
            today_episodes,
            pending_actions,
            active_downloads,
            recent_completed: media_files,
            weekly_schedule,
            source_health,
        })
    }

    /// 在独立调用中创建事务，在工作单元内复用外层事务。
    fn with_transaction<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        if self.transaction_active {
            return operation(self.connection);
        }

        let transaction = self.connection.unchecked_transaction()?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    /// 读取番剧别名并按番剧分组。
    fn list_aliases_by_anime(&self) -> Result<HashMap<String, Vec<AnimeAlias>>, StorageError> {
        let rows = query_all(
            self.connection,
            "SELECT * FROM anime_alias ORDER BY priority DESC",
            map_alias_row,
        )?;
        let mut aliases = HashMap::<String, Vec<AnimeAlias>>::new();
        for row in rows {
            let anime_id = row.anime_id.clone();
            aliases
                .entry(anime_id)
                .or_default()
                .push(row.into_domain()?);
        }
        Ok(aliases)
    }

    /// 读取已确认来源绑定中的中文标题，作为追番视图的临时别名。
    fn list_confirmed_binding_title_aliases_by_anime(
        &self,
    ) -> Result<HashMap<String, Vec<AnimeAlias>>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id, anime_id, source_id, source_anime_title
             FROM anime_source_binding
             WHERE confirmed = 1
               AND TRIM(COALESCE(source_anime_title, '')) <> ''
             ORDER BY anime_id, source_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, String>("anime_id")?,
                row.get::<_, String>("source_id")?,
                row.get::<_, Option<String>>("source_anime_title")?,
            ))
        })?;
        let mut aliases = HashMap::<String, Vec<AnimeAlias>>::new();
        for row in rows {
            let (binding_id, anime_id, source_id, title) = row?;
            let Some(title) = title.map(|value| value.trim().to_owned()) else {
                continue;
            };
            if !is_likely_chinese_title(&title) {
                continue;
            }
            aliases
                .entry(anime_id.clone())
                .or_default()
                .push(AnimeAlias {
                    id: format!("source-binding-title:{anime_id}:{source_id}:{binding_id}"),
                    anime_id,
                    alias: title,
                    language: AnimeAliasLanguage::Zh,
                    priority: CONFIRMED_BINDING_TITLE_ALIAS_PRIORITY,
                });
        }
        Ok(aliases)
    }

    /// 读取 RSS 订阅并按追番记录分组。
    fn list_rss_subscriptions_by_my_anime(
        &self,
    ) -> Result<HashMap<String, Vec<AnimeRssSubscription>>, StorageError> {
        let rows = query_all(
            self.connection,
            "SELECT * FROM my_anime_rss_subscription ORDER BY created_at, name",
            map_rss_subscription_row,
        )?;
        let mut subscriptions = HashMap::<String, Vec<AnimeRssSubscription>>::new();
        for row in rows {
            let my_anime_id = row.my_anime_id.clone();
            subscriptions
                .entry(my_anime_id)
                .or_default()
                .push(row.into_domain()?);
        }
        Ok(subscriptions)
    }

    /// 读取全部单集供跨番剧聚合。
    fn list_all_episodes(&self) -> Result<Vec<Episode>, StorageError> {
        query_all(
            self.connection,
            "SELECT * FROM episode ORDER BY episode_no",
            map_episode_row,
        )?
        .into_iter()
        .map(EpisodeRow::into_domain)
        .collect()
    }

    /// 读取下载任务与文件快照。
    fn list_downloads(&self) -> Result<Vec<DownloadTask>, StorageError> {
        let file_rows = query_all(
            self.connection,
            "SELECT * FROM torrent_file ORDER BY file_index",
            map_torrent_file_row,
        )?;
        let mut files_by_task = HashMap::<String, Vec<TorrentFile>>::new();
        for row in file_rows {
            files_by_task
                .entry(row.download_task_id.clone())
                .or_default()
                .push(row.into_domain());
        }

        query_all(
            self.connection,
            "SELECT * FROM download_task ORDER BY created_at DESC",
            map_download_row,
        )?
        .into_iter()
        .map(|row| {
            let files = files_by_task.remove(&row.id).unwrap_or_default();
            row.into_domain(files)
        })
        .collect()
    }

    /// 原子保存下载任务和完整文件快照，并同步关联单集状态。
    fn upsert_download_task(&self, task: &DownloadTask) -> Result<Vec<DownloadTask>, StorageError> {
        validate_download_task(task)?;
        self.with_transaction(|connection| {
            let previous_episode_ids = linked_episode_ids_for_task(connection, &task.id)?;
            upsert_download_task_row(connection, task, &now_iso())?;
            sync_linked_episode_from_download(connection, task, &previous_episode_ids)?;
            Ok(())
        })?;
        info!("Rust 下载任务快照已保存：task_id={}", task.id);
        self.list_downloads()
    }

    /// 按应用内唯一标识删除下载任务，并恢复关联单集状态。
    fn remove_download_task(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> Result<Vec<DownloadTask>, StorageError> {
        let existing = self
            .list_downloads()?
            .into_iter()
            .find(|task| task.id == task_id);
        let Some(existing) = existing else {
            return self.list_downloads();
        };
        self.with_transaction(|connection| {
            if delete_files {
                connection.execute(
                    "DELETE FROM media_file WHERE download_task_id = ?1",
                    [&existing.id],
                )?;
            }
            connection.execute("DELETE FROM download_task WHERE id = ?1", [task_id])?;
            restore_linked_episode_after_download_removal(connection, &existing)?;
            Ok(())
        })?;
        info!(
            "Rust 下载任务记录已删除：task_id={}, delete_files={delete_files}",
            existing.id
        );
        self.list_downloads()
    }

    /// 读取并排序全部媒体文件。
    pub(crate) fn list_media_files(&self) -> Result<Vec<MediaFile>, StorageError> {
        query_all(
            self.connection,
            "SELECT * FROM media_file",
            map_media_file_row,
        )?
        .into_iter()
        .map(MediaFileRow::into_domain)
        .collect()
    }

    /// 原子新增或更新媒体文件，文件路径冲突时保留最新记录。
    pub(crate) fn upsert_media_files(
        &self,
        media_files: &[MediaFile],
    ) -> Result<Vec<MediaFile>, StorageError> {
        self.with_transaction(|connection| {
            let mut previous_episode_ids = HashSet::new();
            for media in media_files {
                validate_identifier("mediaFile.id", &media.id)?;
                validate_identifier("mediaFile.animeId", &media.anime_id)?;
                if media.file_path.trim().is_empty() {
                    return invalid_input("mediaFile.filePath", "媒体文件路径不能为空");
                }
                if media.file_name.trim().is_empty() {
                    return invalid_input("mediaFile.fileName", "媒体文件名不能为空");
                }
                if media.size < 0 {
                    return invalid_input("mediaFile.size", "媒体文件大小不能为负数");
                }
                if media.content_kind != MediaContentKind::Episode && media.episode_id.is_some() {
                    return invalid_input("mediaFile.episodeId", "非正片媒体不能关联正片单集");
                }
                previous_episode_ids.extend(query_all_with_params(
                    connection,
                    "SELECT episode_id FROM media_file
                     WHERE (id = ?1 OR file_path = ?2) AND episode_id IS NOT NULL",
                    params![&media.id, &media.file_path],
                    |row| row.get::<_, String>(0),
                )?);
                let audio_codecs_json =
                    serde_json::to_string(&media.audio_codecs).map_err(|source| {
                        StorageError::JsonData {
                            context: "媒体音轨",
                            source,
                        }
                    })?;
                let subtitle_tracks_json =
                    serde_json::to_string(&media.subtitle_tracks).map_err(|source| {
                        StorageError::JsonData {
                            context: "媒体字幕轨",
                            source,
                        }
                    })?;
                connection.execute(
                    "DELETE FROM media_file WHERE file_path = ?1 AND id <> ?2",
                    params![&media.file_path, &media.id],
                )?;
                connection.execute(
                    "INSERT INTO media_file (
                       id, anime_id, episode_id, download_task_id, content_kind, special_no,
                       file_path, file_name, size,
                       container, declared_video_codec, detected_video_codec,
                       normalized_video_codec, resolution, bit_depth, audio_codecs_json,
                       subtitle_tracks_json, duration_seconds, downloaded_at, probed_at,
                       origin, source_root, fingerprint, file_modified_at, availability,
                       last_verified_at, availability_error
                     ) VALUES (
                       ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                       ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
                     ) ON CONFLICT(id) DO UPDATE SET
                       anime_id = excluded.anime_id,
                       episode_id = excluded.episode_id,
                       download_task_id = excluded.download_task_id,
                       content_kind = excluded.content_kind,
                       special_no = excluded.special_no,
                       file_path = excluded.file_path,
                       file_name = excluded.file_name,
                       size = excluded.size,
                       container = excluded.container,
                       declared_video_codec = excluded.declared_video_codec,
                       detected_video_codec = excluded.detected_video_codec,
                       normalized_video_codec = excluded.normalized_video_codec,
                       resolution = excluded.resolution,
                       bit_depth = excluded.bit_depth,
                       audio_codecs_json = excluded.audio_codecs_json,
                       subtitle_tracks_json = excluded.subtitle_tracks_json,
                       duration_seconds = excluded.duration_seconds,
                       downloaded_at = excluded.downloaded_at,
                       probed_at = excluded.probed_at,
                       origin = excluded.origin,
                       source_root = excluded.source_root,
                       fingerprint = excluded.fingerprint,
                       file_modified_at = excluded.file_modified_at,
                       availability = excluded.availability,
                       last_verified_at = excluded.last_verified_at,
                       availability_error = excluded.availability_error",
                    params![
                        &media.id,
                        &media.anime_id,
                        media.episode_id.as_deref(),
                        media.download_task_id.as_deref(),
                        media_content_kind_value(&media.content_kind),
                        media.special_no.as_deref(),
                        &media.file_path,
                        &media.file_name,
                        media.size,
                        media.container.as_deref(),
                        media.declared_video_codec.as_deref(),
                        media.detected_video_codec.as_deref(),
                        &media.normalized_video_codec,
                        media.resolution.as_deref(),
                        media.bit_depth,
                        audio_codecs_json,
                        subtitle_tracks_json,
                        media.duration_seconds,
                        media.downloaded_at.as_deref(),
                        media.probed_at.as_deref(),
                        media_origin_value(&media.origin),
                        media.source_root.as_deref(),
                        media.fingerprint.as_deref(),
                        media.file_modified_at.as_deref(),
                        media_availability_value(&media.availability),
                        media.last_verified_at.as_deref(),
                        media.availability_error.as_deref(),
                    ],
                )?;
            }
            sync_episode_statuses_from_downloads(
                connection,
                orphaned_media_episode_ids(connection, previous_episode_ids)?,
            )?;
            Ok(())
        })?;
        info!("Rust 媒体文件批量写入完成：count={}", media_files.len());
        self.list_media_files()
    }

    /// 按标识批量删除媒体文件，并恢复不再拥有媒体的单集状态。
    pub(crate) fn remove_media_files(
        &self,
        media_file_ids: &[String],
    ) -> Result<Vec<MediaFile>, StorageError> {
        if media_file_ids.is_empty() {
            return self.list_media_files();
        }
        let removed_count = self.with_transaction(|connection| {
            let mut linked_episode_ids = HashSet::new();
            let mut removed_count = 0usize;
            for media_file_id in media_file_ids {
                validate_identifier("mediaFile.id", media_file_id)?;
                let episode_id = connection
                    .query_row(
                        "SELECT episode_id FROM media_file WHERE id = ?1",
                        [media_file_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(episode_id) = episode_id {
                    linked_episode_ids.insert(episode_id);
                }
                removed_count +=
                    connection.execute("DELETE FROM media_file WHERE id = ?1", [media_file_id])?;
            }
            sync_episode_statuses_from_downloads(
                connection,
                orphaned_media_episode_ids(connection, linked_episode_ids)?,
            )?;
            Ok(removed_count)
        })?;
        info!("Rust 媒体文件批量删除完成：count={removed_count}");
        self.list_media_files()
    }

    /// 清理由扫描导入创建且已无媒体、无用户状态的本地番剧。
    pub(crate) fn cleanup_orphaned_imported_anime(
        &self,
        anime_ids: &[String],
    ) -> Result<Vec<String>, StorageError> {
        if anime_ids.is_empty() {
            return Ok(Vec::new());
        }
        let removed = self.with_transaction(|connection| {
            let mut removed = Vec::new();
            for anime_id in anime_ids {
                validate_identifier("animeId", anime_id)?;
                if !is_disposable_imported_anime(connection, anime_id)? {
                    continue;
                }
                connection.execute("DELETE FROM my_anime WHERE anime_id = ?1", [anime_id])?;
                connection.execute("DELETE FROM episode WHERE anime_id = ?1", [anime_id])?;
                connection.execute("DELETE FROM anime_catalog WHERE id = ?1", [anime_id])?;
                removed.push(anime_id.clone());
            }
            Ok(removed)
        })?;
        if !removed.is_empty() {
            info!(
                "Rust 无引用导入番剧清理完成：count={}, anime_ids={}",
                removed.len(),
                removed.join(",")
            );
        }
        Ok(removed)
    }

    /// 读取字幕组名称映射。
    fn list_fansub_names(&self) -> Result<HashMap<String, String>, StorageError> {
        let rows = query_all(
            self.connection,
            "SELECT id, name FROM fansub_group",
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }

    /// 读取下载源启用状态映射。
    fn list_source_enabled(&self) -> Result<HashMap<String, bool>, StorageError> {
        let rows = query_all(
            self.connection,
            "SELECT id, enabled FROM release_source",
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
        )?;
        Ok(rows.into_iter().collect())
    }

    /// 合并目录写入；指定月份时先移除该月未引用缓存。
    fn persist_anime_catalog(
        &self,
        items: &[Anime],
        replace_month: Option<(i64, i64)>,
        ignore_refresh_timestamp: bool,
    ) -> Result<AnimeCatalogWriteResult, StorageError> {
        let current = self.list_anime_catalog(None, None)?;
        let replacement_counts = replace_month.map(|_| {
            let existing_count = items
                .iter()
                .filter(|item| current.iter().any(|existing| is_same_anime(existing, item)))
                .count();
            (items.len().saturating_sub(existing_count), existing_count)
        });
        let referenced_ids = self.read_referenced_anime_ids()?;
        let followed_ids = query_all(self.connection, "SELECT anime_id FROM my_anime", |row| {
            row.get::<_, String>(0)
        })?
        .into_iter()
        .collect::<HashSet<_>>();
        let mut catalog = match replace_month {
            Some((year, month)) => current
                .into_iter()
                .filter(|anime| {
                    anime.premiere_year != year
                        || anime.premiere_month != month
                        || referenced_ids.contains(&anime.id)
                })
                .collect::<Vec<_>>(),
            None => current,
        };
        let mut added_count = 0;
        let mut existing_count = 0;
        let mut changed_ids = HashSet::new();
        for item in items {
            validate_identifier("anime.id", &item.id)?;
            if item.title.trim().is_empty() {
                return invalid_input("anime.title", "番剧标题不能为空");
            }
            if let Some(index) = catalog
                .iter()
                .position(|existing| is_same_anime(existing, item))
            {
                let preserve_rating = followed_ids.contains(&catalog[index].id);
                let merged = merge_anime(&catalog[index], item, preserve_rating);
                let unchanged = if ignore_refresh_timestamp {
                    anime_catalog_content_equal(&merged, &catalog[index])
                } else {
                    merged == catalog[index]
                };
                if !unchanged {
                    changed_ids.insert(catalog[index].id.clone());
                    catalog[index] = merged;
                }
                existing_count += 1;
            } else {
                catalog.push(item.clone());
                changed_ids.insert(item.id.clone());
                added_count += 1;
            }
        }
        if let Some((replacement_added, replacement_existing)) = replacement_counts {
            added_count = replacement_added;
            existing_count = replacement_existing;
        }

        let delete_ids = if replace_month.is_some() {
            let keep_ids = catalog
                .iter()
                .map(|anime| anime.id.clone())
                .chain(referenced_ids.iter().cloned())
                .collect::<HashSet<_>>();
            query_all(self.connection, "SELECT id FROM anime_catalog", |row| {
                row.get::<_, String>(0)
            })?
            .into_iter()
            .filter(|id| !keep_ids.contains(id))
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let rows_to_upsert = if replace_month.is_some() {
            catalog.iter().collect::<Vec<_>>()
        } else {
            catalog
                .iter()
                .filter(|anime| changed_ids.contains(&anime.id))
                .collect::<Vec<_>>()
        };
        let timestamp = now_iso();
        self.with_transaction(|connection| {
            for id in &delete_ids {
                connection.execute("DELETE FROM anime_catalog WHERE id = ?1", [id])?;
            }
            for anime in &rows_to_upsert {
                upsert_anime_row(connection, anime, &timestamp)?;
            }
            Ok(())
        })?;
        if replace_month.is_none() {
            info!(
                "Rust 番剧目录增量写入完成：received={}, changed={}, unchanged={}",
                items.len(),
                rows_to_upsert.len(),
                items.len().saturating_sub(rows_to_upsert.len())
            );
        }
        if let Some((year, month)) = replace_month {
            info!(
                "Rust 番剧月度目录替换完成：year={}, month={}, removed={}, collected={}, retained_referenced={}",
                year,
                month,
                delete_ids.len(),
                items.len(),
                referenced_ids.len()
            );
        }
        Ok(AnimeCatalogWriteResult {
            items: self.list_anime_catalog(None, None)?,
            added_count,
            existing_count,
        })
    }

    /// 读取不能随目录缓存清理的番剧标识。
    fn read_referenced_anime_ids(&self) -> Result<HashSet<String>, StorageError> {
        Ok(query_all(
            self.connection,
            "SELECT anime_id AS id FROM my_anime
             UNION SELECT anime_id AS id FROM episode
             UNION SELECT anime_id AS id FROM download_task WHERE anime_id IS NOT NULL
             UNION SELECT anime_id AS id FROM media_file WHERE anime_id IS NOT NULL",
            |row| row.get::<_, String>(0),
        )?
        .into_iter()
        .collect())
    }

    /// 从固定表读取 JSON 状态。
    fn read_json_state(
        &self,
        table: &'static str,
        key: &'static str,
        context: &'static str,
    ) -> Result<Option<Value>, StorageError> {
        let sql = match table {
            "app_settings" => "SELECT value_json FROM app_settings WHERE key = ?1",
            "app_state" => "SELECT value_json FROM app_state WHERE key = ?1",
            _ => unreachable!("repository only reads fixed state tables"),
        };
        let raw = self
            .connection
            .query_row(sql, [key], |row| row.get::<_, String>(0))
            .optional()?;
        raw.map(|value| parse_json(&value, context)).transpose()
    }

    /// 将设置中的安全引用解析为明文，并尽力迁移历史明文字段。
    fn hydrate_settings_secrets(&self, settings: &mut AppSettings) -> bool {
        let Some(value) = settings
            .pointer_mut("/download/qbittorrent/password")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        else {
            return false;
        };
        let (hydrated, migrated) =
            self.hydrate_secret_value(SETTINGS_QBITTORRENT_PASSWORD_KEY, &value);
        if let Some(target) = settings.pointer_mut("/download/qbittorrent/password") {
            *target = Value::String(hydrated);
        }
        migrated
    }

    /// 将设置中的敏感明文写入平台安全存储，并仅持久化引用。
    fn protect_settings_secrets(&self, settings: &mut AppSettings) -> Result<(), StorageError> {
        let Some(value) = settings
            .pointer_mut("/download/qbittorrent/password")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
        else {
            return Ok(());
        };
        let protected = self.protect_secret_value(SETTINGS_QBITTORRENT_PASSWORD_KEY, &value)?;
        if let Some(target) = settings.pointer_mut("/download/qbittorrent/password") {
            *target = protected.map_or(Value::Null, Value::String);
        }
        Ok(())
    }

    /// 从安全存储读取引用；历史明文会尽力写入安全存储后标记迁移。
    fn hydrate_secret_value(&self, key: &str, stored: &str) -> (String, bool) {
        let Some(secure_store) = self.secure_store else {
            return (stored.to_owned(), false);
        };
        let reference = secure_reference(key);
        if let Some(marker_key) = stored.strip_prefix(SECURE_MARKER_PREFIX) {
            if marker_key != key {
                warn!("安全存储引用与字段不匹配 key={key} marker={marker_key}");
                return (String::new(), false);
            }
            return match secure_store.read_secret(&reference) {
                Ok(Some(secret)) => match String::from_utf8(secret.expose().to_vec()) {
                    Ok(value) => (value, false),
                    Err(error) => {
                        warn!("安全存储值不是 UTF-8 key={key} error={error}");
                        (String::new(), false)
                    }
                },
                Ok(None) => {
                    warn!("安全存储引用不存在 key={key}");
                    (String::new(), false)
                }
                Err(error) => {
                    warn!("安全存储读取失败 key={key} error={error}");
                    (String::new(), false)
                }
            };
        }
        if stored.is_empty() {
            return (String::new(), false);
        }
        match secure_store.write_secret(&reference, &SecretValue::new(stored.as_bytes())) {
            Ok(()) => (stored.to_owned(), true),
            Err(error) => {
                warn!("历史敏感字段迁移失败 key={key} error={error}");
                (stored.to_owned(), false)
            }
        }
    }

    /// 保存敏感值并生成引用；空值会同步删除安全存储记录。
    fn protect_secret_value(&self, key: &str, value: &str) -> Result<Option<String>, StorageError> {
        let Some(secure_store) = self.secure_store else {
            return Ok(Some(value.to_owned()));
        };
        if value.strip_prefix(SECURE_MARKER_PREFIX) == Some(key) {
            return Ok(Some(value.to_owned()));
        }
        let reference = secure_reference(key);
        if value.is_empty() {
            secure_store
                .delete_secret(&reference)
                .map_err(|error| secure_store_error("删除", key, error))?;
            return Ok(None);
        }
        secure_store
            .write_secret(&reference, &SecretValue::new(value.as_bytes()))
            .map_err(|error| secure_store_error("写入", key, error))?;
        Ok(Some(format!("{SECURE_MARKER_PREFIX}{key}")))
    }

    /// 将完整设置 JSON 原子写入固定设置记录。
    fn save_settings(&self, settings: &AppSettings) -> Result<(), StorageError> {
        let mut persisted = settings.clone();
        self.protect_settings_secrets(&mut persisted)?;
        let value_json =
            serde_json::to_string(&persisted).map_err(|source| StorageError::JsonData {
                context: "应用设置",
                source,
            })?;
        self.connection.execute(
            "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('settings', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET
               value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![value_json, now_iso()],
        )?;
        Ok(())
    }
}

impl SettingsRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取应用设置。
    fn get_settings(&self, platform_defaults: &AppSettings) -> RepositoryResult<AppSettings> {
        SqliteRepository::get_settings(self, platform_defaults).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器合并应用设置。
    fn update_settings(
        &self,
        patch: &Value,
        platform_defaults: &AppSettings,
    ) -> RepositoryResult<AppSettings> {
        SqliteRepository::update_settings(self, patch, platform_defaults)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器恢复平台默认设置。
    fn reset_settings(&self, platform_defaults: &AppSettings) -> RepositoryResult<AppSettings> {
        SqliteRepository::reset_settings(self, platform_defaults).map_err(RepositoryError::from)
    }
}

impl NotificationRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取通知。
    fn list_notifications(&self) -> RepositoryResult<Vec<NotificationRecord>> {
        SqliteRepository::list_notifications(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器统计未读通知。
    fn get_unread_notification_count(&self) -> RepositoryResult<u64> {
        SqliteRepository::get_unread_notification_count(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器增量写入通知。
    fn add_notifications(
        &self,
        records: &[NotificationRecord],
    ) -> RepositoryResult<Vec<NotificationRecord>> {
        SqliteRepository::add_notifications(self, records).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器标记单条通知已读。
    fn mark_notification_read(
        &self,
        notification_id: &str,
    ) -> RepositoryResult<Vec<NotificationRecord>> {
        SqliteRepository::mark_notification_read(self, notification_id)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器标记全部通知已读。
    fn mark_all_notifications_read(&self) -> RepositoryResult<Vec<NotificationRecord>> {
        SqliteRepository::mark_all_notifications_read(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器清空通知。
    fn clear_notifications(&self) -> RepositoryResult<Vec<NotificationRecord>> {
        SqliteRepository::clear_notifications(self).map_err(RepositoryError::from)
    }
}

impl AnimeCatalogRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取番剧目录。
    fn list_anime_catalog(
        &self,
        year: Option<i64>,
        month: Option<i64>,
    ) -> RepositoryResult<Vec<Anime>> {
        SqliteRepository::list_anime_catalog(self, year, month).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器按标识读取番剧。
    fn get_anime_catalog_by_id(&self, anime_id: &str) -> RepositoryResult<Option<Anime>> {
        SqliteRepository::get_anime_catalog_by_id(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器搜索番剧目录。
    fn search_anime_catalog(&self, keyword: &str) -> RepositoryResult<AnimeDiscoverySearchResult> {
        SqliteRepository::search_anime_catalog(self, keyword).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器合并番剧目录。
    fn upsert_anime_catalog(&self, items: &[Anime]) -> RepositoryResult<AnimeCatalogWriteResult> {
        SqliteRepository::upsert_anime_catalog(self, items).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器写入详情补全结果。
    fn upsert_anime_catalog_details(
        &self,
        items: &[Anime],
    ) -> RepositoryResult<AnimeCatalogWriteResult> {
        SqliteRepository::upsert_anime_catalog_details(self, items).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器替换月度番剧目录。
    fn replace_anime_catalog_month(
        &self,
        year: i64,
        month: i64,
        items: &[Anime],
    ) -> RepositoryResult<AnimeCatalogWriteResult> {
        SqliteRepository::replace_anime_catalog_month(self, year, month, items)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取季度同步状态。
    fn get_anime_season_sync_state(
        &self,
        year: i64,
        season: &str,
    ) -> RepositoryResult<Option<AnimeSeasonSyncState>> {
        SqliteRepository::get_anime_season_sync_state(self, year, season)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存季度同步状态。
    fn upsert_anime_season_sync_state(&self, state: &AnimeSeasonSyncState) -> RepositoryResult<()> {
        SqliteRepository::upsert_anime_season_sync_state(self, state).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取来源级详情刷新状态。
    fn list_anime_detail_refresh_states(&self) -> RepositoryResult<Vec<AnimeDetailRefreshState>> {
        SqliteRepository::list_anime_detail_refresh_states(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存来源级详情刷新状态。
    fn upsert_anime_detail_refresh_states(
        &self,
        states: &[AnimeDetailRefreshState],
    ) -> RepositoryResult<()> {
        SqliteRepository::upsert_anime_detail_refresh_states(self, states)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取番剧详情。
    fn get_anime_detail(&self, anime_id: &str) -> RepositoryResult<AnimeDetailResult> {
        SqliteRepository::get_anime_detail(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取字幕组。
    fn list_fansubs(&self, anime_id: Option<&str>) -> RepositoryResult<Vec<FansubGroup>> {
        SqliteRepository::list_fansubs(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器观察并合并番剧字幕组。
    fn observe_anime_fansubs(
        &self,
        anime_id: &str,
        releases: &[Release],
    ) -> RepositoryResult<Vec<FansubGroup>> {
        SqliteRepository::observe_anime_fansubs(self, anime_id, releases)
            .map_err(RepositoryError::from)
    }
}

impl ReleaseSourceRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取下载源。
    fn list_sources(&self) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
        SqliteRepository::list_sources(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器更新下载源启用状态。
    fn set_source_enabled(
        &self,
        source_id: &str,
        enabled: bool,
    ) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
        SqliteRepository::set_source_enabled(self, source_id, enabled)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存下载源。
    fn upsert_source(
        &self,
        source: &ReleaseSourceConfig,
    ) -> RepositoryResult<Vec<ReleaseSourceConfig>> {
        SqliteRepository::upsert_source(self, source).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取来源同步状态。
    fn list_source_sync_states(&self) -> RepositoryResult<Vec<ReleaseSourceSyncState>> {
        SqliteRepository::list_source_sync_states(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存来源同步状态。
    fn upsert_source_sync_state(&self, state: &ReleaseSourceSyncState) -> RepositoryResult<()> {
        SqliteRepository::upsert_source_sync_state(self, state).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取请求熔断状态。
    fn get_request_circuit_state(
        &self,
        key: &str,
    ) -> RepositoryResult<Option<RequestCircuitState>> {
        SqliteRepository::get_request_circuit_state(self, key).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存请求熔断状态。
    fn upsert_request_circuit_state(&self, state: &RequestCircuitState) -> RepositoryResult<()> {
        SqliteRepository::upsert_request_circuit_state(self, state).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器清理请求熔断状态。
    fn clear_request_circuit_state(&self, key: &str) -> RepositoryResult<()> {
        SqliteRepository::clear_request_circuit_state(self, key).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取资源搜索缓存。
    fn get_release_search_cache(
        &self,
        cache_key: &str,
        current_time: &str,
    ) -> RepositoryResult<Option<ReleaseSearchCacheEntry>> {
        SqliteRepository::get_release_search_cache(self, cache_key, current_time)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存资源搜索缓存。
    fn upsert_release_search_cache(
        &self,
        cache_key: &str,
        entry: &ReleaseSearchCacheEntry,
    ) -> RepositoryResult<()> {
        SqliteRepository::upsert_release_search_cache(self, cache_key, entry)
            .map_err(RepositoryError::from)
    }
}

impl ReleaseCacheRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取原始资源缓存。
    fn list_cached_releases(&self, query: &CachedReleaseQuery) -> RepositoryResult<Vec<Release>> {
        SqliteRepository::list_cached_releases(self, query).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器增量保存原始资源缓存。
    fn upsert_cached_releases(&self, releases: &[Release]) -> RepositoryResult<usize> {
        SqliteRepository::upsert_cached_releases(self, releases).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器清理过期资源缓存。
    fn prune_cached_releases(&self, before: &str) -> RepositoryResult<usize> {
        SqliteRepository::prune_cached_releases(self, before).map_err(RepositoryError::from)
    }
}

impl AnimeSourceBindingRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取来源绑定。
    fn list_anime_source_bindings(
        &self,
        anime_id: &str,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        SqliteRepository::list_anime_source_bindings(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存来源绑定。
    fn upsert_anime_source_binding(
        &self,
        binding: &AnimeSourceBinding,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        SqliteRepository::upsert_anime_source_binding(self, binding).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器删除来源绑定。
    fn remove_anime_source_binding(
        &self,
        anime_id: &str,
        source_id: &str,
    ) -> RepositoryResult<Vec<AnimeSourceBinding>> {
        SqliteRepository::remove_anime_source_binding(self, anime_id, source_id)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取来源排除记录。
    fn list_anime_source_exclusions(
        &self,
        anime_id: &str,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        SqliteRepository::list_anime_source_exclusions(self, anime_id)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存来源排除记录。
    fn upsert_anime_source_exclusion(
        &self,
        exclusion: &AnimeSourceExclusion,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        SqliteRepository::upsert_anime_source_exclusion(self, exclusion)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器删除来源排除记录。
    fn remove_anime_source_exclusion(
        &self,
        anime_id: &str,
        source_id: &str,
        source_anime_id: Option<&str>,
    ) -> RepositoryResult<Vec<AnimeSourceExclusion>> {
        SqliteRepository::remove_anime_source_exclusion(self, anime_id, source_id, source_anime_id)
            .map_err(RepositoryError::from)
    }
}

impl AnimeTrackingRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取追番。
    fn list_my_anime(&self) -> RepositoryResult<Vec<MyAnime>> {
        SqliteRepository::list_my_anime(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存追番。
    fn upsert_my_anime(&self, item: MyAnime) -> RepositoryResult<Vec<MyAnime>> {
        SqliteRepository::upsert_my_anime(self, item).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器删除追番。
    fn remove_my_anime(&self, item_id: &str) -> RepositoryResult<Vec<MyAnime>> {
        SqliteRepository::remove_my_anime(self, item_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取单集。
    fn list_episodes(&self, anime_id: &str) -> RepositoryResult<Vec<Episode>> {
        SqliteRepository::list_episodes(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存单集。
    fn upsert_episode(&self, episode: &Episode) -> RepositoryResult<Vec<Episode>> {
        SqliteRepository::upsert_episode(self, episode).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取观看进度。
    fn list_my_anime_watch_progress(&self) -> RepositoryResult<Vec<AnimeWatchProgress>> {
        SqliteRepository::list_my_anime_watch_progress(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器更新观看进度。
    fn set_anime_watch_progress(
        &self,
        input: &SetAnimeWatchProgressInput,
    ) -> RepositoryResult<AnimeWatchProgress> {
        SqliteRepository::set_anime_watch_progress(self, input).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取单集偏好。
    fn list_episode_preferences(&self, anime_id: &str) -> RepositoryResult<Vec<EpisodePreference>> {
        SqliteRepository::list_episode_preferences(self, anime_id).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存单集偏好。
    fn upsert_episode_preference(
        &self,
        preference: &EpisodePreference,
    ) -> RepositoryResult<Vec<EpisodePreference>> {
        SqliteRepository::upsert_episode_preference(self, preference).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器删除单集偏好。
    fn remove_episode_preference(
        &self,
        episode_id: &str,
    ) -> RepositoryResult<Vec<EpisodePreference>> {
        SqliteRepository::remove_episode_preference(self, episode_id).map_err(RepositoryError::from)
    }
}

impl PlaybackRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器回写播放进度。
    fn report_playback_progress(
        &self,
        input: &ReportPlaybackProgressInput,
    ) -> RepositoryResult<bool> {
        SqliteRepository::report_playback_progress(self, input).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器读取续播位置。
    fn get_playback_checkpoint(
        &self,
        task_id: &str,
        file_index: Option<i64>,
    ) -> RepositoryResult<Option<PlaybackCheckpoint>> {
        SqliteRepository::get_playback_checkpoint(self, task_id, file_index)
            .map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器保存续播位置。
    fn save_playback_checkpoint(
        &self,
        input: &SavePlaybackCheckpointInput,
    ) -> RepositoryResult<PlaybackCheckpoint> {
        SqliteRepository::save_playback_checkpoint(self, input).map_err(RepositoryError::from)
    }
}

impl DownloadRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取下载任务及文件快照。
    fn list_downloads(&self) -> RepositoryResult<Vec<DownloadTask>> {
        SqliteRepository::list_downloads(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器原子保存下载任务。
    fn upsert_download_task(&self, task: &DownloadTask) -> RepositoryResult<Vec<DownloadTask>> {
        SqliteRepository::upsert_download_task(self, task).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器删除下载任务。
    fn remove_download_task(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> RepositoryResult<Vec<DownloadTask>> {
        SqliteRepository::remove_download_task(self, task_id, delete_files)
            .map_err(RepositoryError::from)
    }
}

impl MediaRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取媒体文件。
    fn list_media_files(&self) -> RepositoryResult<Vec<MediaFile>> {
        SqliteRepository::list_media_files(self).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器原子写入媒体文件。
    fn upsert_media_files(&self, media_files: &[MediaFile]) -> RepositoryResult<Vec<MediaFile>> {
        SqliteRepository::upsert_media_files(self, media_files).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器批量删除媒体文件。
    fn remove_media_files(&self, media_file_ids: &[String]) -> RepositoryResult<Vec<MediaFile>> {
        SqliteRepository::remove_media_files(self, media_file_ids).map_err(RepositoryError::from)
    }

    /// 通过 SQLite 适配器清理无引用的导入番剧。
    fn cleanup_orphaned_imported_anime(
        &self,
        anime_ids: &[String],
    ) -> RepositoryResult<Vec<String>> {
        SqliteRepository::cleanup_orphaned_imported_anime(self, anime_ids)
            .map_err(RepositoryError::from)
    }
}

impl DashboardRepository for SqliteRepository<'_> {
    /// 通过 SQLite 适配器读取首页聚合数据。
    fn get_dashboard(&self) -> RepositoryResult<DashboardData> {
        SqliteRepository::get_dashboard(self).map_err(RepositoryError::from)
    }
}

/// 读取查询全部结果，统一转换 SQLite 错误。
fn query_all<T>(
    connection: &Connection,
    sql: &str,
    mut mapper: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StorageError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| mapper(row))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

/// 读取带参数查询的全部结果。
fn query_all_with_params<T, P: Params>(
    connection: &Connection,
    sql: &str,
    params: P,
    mut mapper: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StorageError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params, |row| mapper(row))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

/// 写入一条跨重启复用的原始资源缓存。
fn upsert_cached_release_row(
    connection: &Connection,
    release: &Release,
) -> Result<(), StorageError> {
    let anime_id = match release.anime_id.as_deref() {
        Some(anime_id) if record_exists(connection, "anime_catalog", anime_id)? => Some(anime_id),
        _ => None,
    };
    let fansub_group_id = match release.fansub_group_id.as_deref() {
        Some(fansub_group_id) if record_exists(connection, "fansub_group", fansub_group_id)? => {
            Some(fansub_group_id)
        }
        _ => None,
    };
    let subtitle_languages_json =
        serde_json::to_string(&release.subtitle_languages).map_err(|source| {
            StorageError::JsonData {
                context: "资源字幕语言",
                source,
            }
        })?;
    let raw_json = serde_json::to_string(release).map_err(|source| StorageError::JsonData {
        context: "资源原始数据",
        source,
    })?;
    connection.execute(
        "INSERT INTO release (
           id, title, anime_id, episode_no, fansub_group_id, source_id, source_name,
           magnet_url, torrent_url, info_hash, size, resolution, declared_video_codec,
           normalized_video_codec, bit_depth, subtitle, subtitle_languages_json,
           published_at, seeders, raw_json
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
           ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
         ) ON CONFLICT(id) DO UPDATE SET
           title = excluded.title,
           anime_id = COALESCE(excluded.anime_id, release.anime_id),
           episode_no = excluded.episode_no,
           fansub_group_id = excluded.fansub_group_id,
           source_name = excluded.source_name,
           magnet_url = excluded.magnet_url,
           torrent_url = excluded.torrent_url,
           info_hash = excluded.info_hash,
           size = excluded.size,
           resolution = excluded.resolution,
           declared_video_codec = excluded.declared_video_codec,
           normalized_video_codec = excluded.normalized_video_codec,
           bit_depth = excluded.bit_depth,
           subtitle = excluded.subtitle,
           subtitle_languages_json = excluded.subtitle_languages_json,
           published_at = excluded.published_at,
           seeders = excluded.seeders,
           raw_json = excluded.raw_json",
        params![
            &release.id,
            release.title.trim(),
            anime_id,
            release.episode_no,
            fansub_group_id,
            &release.source_id,
            &release.source_name,
            release.magnet_url.as_deref(),
            release.torrent_url.as_deref(),
            release.info_hash.as_deref(),
            release.size,
            release.resolution.as_ref().map(ReleaseResolution::as_str),
            release.declared_video_codec.as_deref(),
            release
                .normalized_video_codec
                .as_ref()
                .map(NormalizedVideoCodec::as_str),
            release.bit_depth,
            release.subtitle.as_ref().map(subtitle_preference_value),
            subtitle_languages_json,
            &release.published_at,
            release.seeders,
            raw_json,
        ],
    )?;
    Ok(())
}

/// 判断指定业务表中是否存在稳定标识。
fn record_exists(
    connection: &Connection,
    table: &'static str,
    id: &str,
) -> Result<bool, StorageError> {
    debug_assert!(matches!(table, "anime_catalog" | "fansub_group"));
    Ok(connection
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE id = ?1"),
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// 从归一化资源汇总可持久化的字幕组。
fn collect_discovered_fansubs(releases: &[Release]) -> Vec<FansubGroup> {
    let mut groups = HashMap::<String, FansubGroup>::new();
    for release in releases {
        let Some(id) = release
            .fansub_group_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(name) = release
            .fansub_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let group = groups.entry(id.to_owned()).or_insert_with(|| FansubGroup {
            id: id.to_owned(),
            name: name.to_owned(),
            aliases: Vec::new(),
            source_ids: Vec::new(),
        });
        if group.name != name && !group.aliases.iter().any(|alias| alias == name) {
            group.aliases.push(name.to_owned());
        }
        if !group
            .source_ids
            .iter()
            .any(|source_id| source_id == &release.source_id)
        {
            group.source_ids.push(release.source_id.clone());
        }
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|left, right| left.id.cmp(&right.id));
    groups
}

/// 去重并稳定排序一组非空文本。
fn merge_unique_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    values.sort();
    values
}

/// 写入番剧目录和规范化别名。
fn upsert_anime_row(
    connection: &Connection,
    anime: &Anime,
    timestamp: &str,
) -> Result<(), StorageError> {
    let external_ids_json =
        serde_json::to_string(&anime.external_ids).map_err(|source| StorageError::JsonData {
            context: "番剧外部标识",
            source,
        })?;
    let detail_json = serde_json::to_string(
        anime
            .detail
            .as_ref()
            .unwrap_or(&Value::Object(Default::default())),
    )
    .map_err(|source| StorageError::JsonData {
        context: "番剧详情",
        source,
    })?;
    connection.execute(
        "INSERT INTO anime_catalog (
           id, title, original_title, premiere_date, premiere_year, premiere_month, season, summary,
           cover_url, rating_score, rating_count, rating_source, external_ids_json, detail_json,
           created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
         ON CONFLICT(id) DO UPDATE SET
           title = excluded.title, original_title = excluded.original_title,
           premiere_date = excluded.premiere_date, premiere_year = excluded.premiere_year,
           premiere_month = excluded.premiere_month, season = excluded.season,
           summary = excluded.summary, cover_url = excluded.cover_url,
           rating_score = excluded.rating_score, rating_count = excluded.rating_count,
           rating_source = excluded.rating_source, external_ids_json = excluded.external_ids_json,
           detail_json = excluded.detail_json, updated_at = excluded.updated_at",
        params![
            &anime.id,
            anime.title.trim(),
            anime.original_title.as_deref(),
            anime.premiere_date.as_deref(),
            anime.premiere_year,
            anime.premiere_month,
            anime.season.as_deref(),
            anime.summary.as_deref(),
            anime.cover_url.as_deref(),
            anime.rating.as_ref().map(|rating| rating.score),
            anime.rating.as_ref().and_then(|rating| rating.count),
            anime.rating.as_ref().map(|rating| rating.source.as_str()),
            external_ids_json,
            detail_json,
            timestamp,
        ],
    )?;

    connection.execute("DELETE FROM anime_alias WHERE anime_id = ?1", [&anime.id])?;
    for alias in normalize_anime_aliases(anime) {
        connection.execute(
            "INSERT INTO anime_alias (id, anime_id, alias, language, priority)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                alias.id,
                alias.anime_id,
                alias.alias,
                alias_language_value(&alias.language),
                alias.priority,
            ],
        )?;
    }
    Ok(())
}

/// 写入追番规则并以当前草稿替换 RSS 订阅。
fn upsert_my_anime_row(
    connection: &Connection,
    item: &MyAnime,
    timestamp: &str,
) -> Result<(), StorageError> {
    let subtitle_languages = resolve_subtitle_languages(
        item.preferred_subtitle_languages.clone(),
        item.preferred_subtitle.as_deref(),
    );
    let subtitle_languages_json =
        serde_json::to_string(&subtitle_languages).map_err(|source| StorageError::JsonData {
            context: "追番字幕语言",
            source,
        })?;
    connection.execute(
        "INSERT INTO my_anime (
           id, anime_id, status, default_fansub_group_id, auto_download, download_dir,
           preferred_resolution, preferred_codec, preferred_subtitle,
           preferred_subtitle_languages_json, preferred_bit_depth, added_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET
           anime_id = excluded.anime_id, status = excluded.status,
           default_fansub_group_id = excluded.default_fansub_group_id,
           auto_download = excluded.auto_download, download_dir = excluded.download_dir,
           preferred_resolution = excluded.preferred_resolution,
           preferred_codec = excluded.preferred_codec,
           preferred_subtitle = excluded.preferred_subtitle,
           preferred_subtitle_languages_json = excluded.preferred_subtitle_languages_json,
           preferred_bit_depth = excluded.preferred_bit_depth, updated_at = excluded.updated_at",
        params![
            &item.id,
            &item.anime.id,
            anime_status_value(&item.status),
            item.default_fansub_group_id.as_deref(),
            i64::from(item.auto_download),
            item.download_dir.as_deref(),
            item.preferred_resolution.as_deref(),
            item.preferred_codec.as_deref(),
            to_legacy_subtitle_preference(&subtitle_languages),
            subtitle_languages_json,
            item.preferred_bit_depth,
            &item.added_at,
            &item.updated_at,
        ],
    )?;
    if let Some(fansub_group_id) = item.default_fansub_group_id.as_deref() {
        connection.execute(
            "INSERT INTO anime_fansub_group (
               anime_id, fansub_group_id, first_seen_at, last_seen_at
             ) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(anime_id, fansub_group_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![&item.anime.id, fansub_group_id, timestamp],
        )?;
    }

    connection.execute(
        "DELETE FROM my_anime_rss_subscription WHERE my_anime_id = ?1",
        [&item.id],
    )?;
    for subscription in &item.rss_subscriptions {
        validate_identifier("rssSubscription.id", &subscription.id)?;
        let languages = resolve_subtitle_languages(
            subscription.preferred_subtitle_languages.clone(),
            subscription.preferred_subtitle.as_deref(),
        );
        let languages_json =
            serde_json::to_string(&languages).map_err(|source| StorageError::JsonData {
                context: "RSS 字幕语言",
                source,
            })?;
        let created_at = if subscription.created_at.trim().is_empty() {
            timestamp
        } else {
            &subscription.created_at
        };
        let updated_at = if subscription.updated_at.trim().is_empty() {
            timestamp
        } else {
            &subscription.updated_at
        };
        connection.execute(
            "INSERT INTO my_anime_rss_subscription (
               id, my_anime_id, name, url, enabled, preferred_subtitle,
               preferred_subtitle_languages_json, refresh_interval_minutes, last_fetched_at,
               created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &subscription.id,
                &item.id,
                subscription.name.trim(),
                subscription.url.trim(),
                i64::from(subscription.enabled),
                to_legacy_subtitle_preference(&languages),
                languages_json,
                subscription.refresh_interval_minutes,
                subscription.last_fetched_at.as_deref(),
                created_at,
                updated_at,
            ],
        )?;
    }
    Ok(())
}

/// 覆盖保存下载任务及完整文件快照，保留任务首次创建时间。
fn upsert_download_task_row(
    connection: &Connection,
    task: &DownloadTask,
    timestamp: &str,
) -> Result<(), StorageError> {
    let subtitle_languages_json =
        serde_json::to_string(&task.subtitle_languages).map_err(|source| {
            StorageError::JsonData {
                context: "下载任务字幕语言",
                source,
            }
        })?;
    connection.execute(
        "INSERT INTO download_task (
           id, release_id, anime_id, episode_id, anime_title, episode_no,
           fansub_group_id, fansub_name, resolution, declared_video_codec,
           normalized_video_codec, bit_depth, subtitle_languages_json, subtitle,
           correlation_tag, engine, torrent_hash, name, status, progress,
           download_speed, upload_speed, eta_seconds, save_path, created_at,
           completed_at, updated_at
         ) VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
           ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
         )
         ON CONFLICT(id) DO UPDATE SET
           release_id = excluded.release_id,
           anime_id = excluded.anime_id,
           episode_id = excluded.episode_id,
           anime_title = excluded.anime_title,
           episode_no = excluded.episode_no,
           fansub_group_id = excluded.fansub_group_id,
           fansub_name = excluded.fansub_name,
           resolution = excluded.resolution,
           declared_video_codec = excluded.declared_video_codec,
           normalized_video_codec = excluded.normalized_video_codec,
           bit_depth = excluded.bit_depth,
           subtitle_languages_json = excluded.subtitle_languages_json,
           subtitle = excluded.subtitle,
           correlation_tag = excluded.correlation_tag,
           engine = excluded.engine,
           torrent_hash = excluded.torrent_hash,
           name = excluded.name,
           status = excluded.status,
           progress = excluded.progress,
           download_speed = excluded.download_speed,
           upload_speed = excluded.upload_speed,
           eta_seconds = excluded.eta_seconds,
           save_path = excluded.save_path,
           completed_at = excluded.completed_at,
           updated_at = excluded.updated_at",
        params![
            &task.id,
            task.release_id.as_deref(),
            task.anime_id.as_deref(),
            task.episode_id.as_deref(),
            task.anime_title.as_deref(),
            task.episode_no,
            task.fansub_group_id.as_deref(),
            task.fansub_name.as_deref(),
            task.resolution.as_deref(),
            task.declared_video_codec.as_deref(),
            task.normalized_video_codec.as_deref(),
            task.bit_depth,
            subtitle_languages_json,
            task.subtitle.as_deref(),
            task.correlation_tag.as_deref(),
            torrent_engine_value(&task.engine),
            task.torrent_hash.as_deref(),
            task.name.trim(),
            download_status_value(&task.status),
            task.progress,
            task.download_speed,
            task.upload_speed,
            task.eta_seconds,
            &task.save_path,
            &task.created_at,
            task.completed_at.as_deref(),
            timestamp,
        ],
    )?;

    connection.execute(
        "DELETE FROM torrent_file WHERE download_task_id = ?1",
        [&task.id],
    )?;
    for file in &task.files {
        connection.execute(
            "INSERT INTO torrent_file (
               id, download_task_id, file_index, name, episode_id, episode_no,
               size, progress, priority, selected
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &file.id,
                &task.id,
                file.index,
                file.name.trim(),
                file.episode_id.as_deref(),
                file.episode_no,
                file.size,
                file.progress,
                file.priority,
                i64::from(file.selected),
            ],
        )?;
    }
    Ok(())
}

/// 读取一条已持久化下载任务当前关联的全部单集。
fn linked_episode_ids_for_task(
    connection: &Connection,
    task_id: &str,
) -> Result<Vec<String>, StorageError> {
    query_all_with_params(
        connection,
        "SELECT episode.id
           FROM download_task
           JOIN episode ON episode.id = download_task.episode_id
          WHERE download_task.id = ?1
         UNION
         SELECT episode.id
           FROM download_task
           JOIN episode
             ON episode.anime_id = download_task.anime_id
            AND episode.episode_no = download_task.episode_no
          WHERE download_task.id = ?1
         UNION
         SELECT episode.id
           FROM torrent_file
           JOIN episode ON episode.id = torrent_file.episode_id
          WHERE torrent_file.download_task_id = ?1
         UNION
         SELECT episode.id
           FROM torrent_file
           JOIN download_task ON download_task.id = torrent_file.download_task_id
           JOIN episode
             ON episode.anime_id = download_task.anime_id
            AND episode.episode_no = torrent_file.episode_no
          WHERE torrent_file.download_task_id = ?1",
        [task_id],
        |row| row.get(0),
    )
}

/// 从已加载任务快照解析删除后仍需重新计算的单集标识。
fn linked_episode_ids_from_download(
    connection: &Connection,
    task: &DownloadTask,
) -> Result<Vec<String>, StorageError> {
    let mut episode_ids = HashSet::new();
    if let Some(episode_id) = task.episode_id.as_deref() {
        episode_ids.insert(episode_id.to_owned());
    }
    if let (Some(anime_id), Some(episode_no)) = (task.anime_id.as_deref(), task.episode_no) {
        if let Some(episode_id) = find_episode_id(connection, anime_id, episode_no)? {
            episode_ids.insert(episode_id);
        }
    }
    for file in &task.files {
        if let Some(episode_id) = file.episode_id.as_deref() {
            episode_ids.insert(episode_id.to_owned());
        }
        if let (Some(anime_id), Some(episode_no)) = (task.anime_id.as_deref(), file.episode_no) {
            if let Some(episode_id) = find_episode_id(connection, anime_id, episode_no)? {
                episode_ids.insert(episode_id);
            }
        }
    }
    let mut episode_ids = episode_ids.into_iter().collect::<Vec<_>>();
    episode_ids.sort();
    Ok(episode_ids)
}

/// 根据当前任务集合更新新旧关联单集，避免任务换绑后遗留下载状态。
fn sync_linked_episode_from_download(
    connection: &Connection,
    task: &DownloadTask,
    previous_episode_ids: &[String],
) -> Result<(), StorageError> {
    let mut episode_ids = previous_episode_ids.iter().cloned().collect::<HashSet<_>>();
    episode_ids.extend(linked_episode_ids_for_task(connection, &task.id)?);
    sync_episode_statuses_from_downloads(connection, episode_ids)
}

/// 删除任务后按剩余任务恢复其原有关联单集状态。
fn restore_linked_episode_after_download_removal(
    connection: &Connection,
    task: &DownloadTask,
) -> Result<(), StorageError> {
    sync_episode_statuses_from_downloads(
        connection,
        linked_episode_ids_from_download(connection, task)?,
    )
}

/// 以数据库中的任务和文件进度为准重算指定单集状态。
fn sync_episode_statuses_from_downloads(
    connection: &Connection,
    episode_ids: impl IntoIterator<Item = String>,
) -> Result<(), StorageError> {
    let timestamp = now_iso();
    for episode_id in episode_ids {
        let Some(row) = connection
            .query_row(
                "SELECT * FROM episode WHERE id = ?1",
                [&episode_id],
                map_episode_row,
            )
            .optional()?
        else {
            continue;
        };
        let episode = row.into_domain()?;
        if episode.status == EpisodeStatus::Watched {
            continue;
        }
        let status = resolve_episode_status_from_downloads(connection, &episode)?;
        if status == episode.status {
            continue;
        }
        connection.execute(
            "UPDATE episode SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![episode_status_value(&status), &timestamp, &episode.id],
        )?;
        debug!(
            "下载任务同步单集状态：episode_id={}, status={}",
            episode.id,
            episode_status_value(&status)
        );
    }
    Ok(())
}

/// 筛出媒体变更后已经失去全部媒体关联的单集。
fn orphaned_media_episode_ids(
    connection: &Connection,
    episode_ids: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, StorageError> {
    let mut orphaned = Vec::new();
    for episode_id in episode_ids {
        let has_media = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_file WHERE episode_id = ?1)",
            [&episode_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !has_media {
            orphaned.push(episode_id);
        }
    }
    Ok(orphaned)
}

/// 判断本地导入番剧是否没有剩余媒体和任何用户维护状态。
fn is_disposable_imported_anime(
    connection: &Connection,
    anime_id: &str,
) -> Result<bool, StorageError> {
    let external_ids_json = connection
        .query_row(
            "SELECT external_ids_json FROM anime_catalog WHERE id = ?1",
            [anime_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(external_ids_json) = external_ids_json else {
        return Ok(false);
    };
    let external_ids: Value = parse_json(&external_ids_json, "导入番剧外部标识")?;
    if external_ids
        .pointer("/localImport")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Ok(false);
    }

    let has_protected_state = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM media_file WHERE anime_id = ?1
           UNION ALL SELECT 1 FROM download_task WHERE anime_id = ?1
           UNION ALL SELECT 1 FROM episode WHERE anime_id = ?1 AND status = 'watched'
           UNION ALL SELECT 1 FROM anime_source_binding WHERE anime_id = ?1
           UNION ALL SELECT 1 FROM anime_source_exclusion WHERE anime_id = ?1
           UNION ALL SELECT 1 FROM episode_preference WHERE anime_id = ?1
           UNION ALL
             SELECT 1 FROM my_anime_rss_subscription subscription
             JOIN my_anime tracking ON tracking.id = subscription.my_anime_id
             WHERE tracking.anime_id = ?1
         )",
        [anime_id],
        |row| row.get::<_, bool>(0),
    )?;
    if has_protected_state {
        return Ok(false);
    }

    let tracking_rows = query_all_with_params(
        connection,
        "SELECT status, default_fansub_group_id, auto_download, download_dir,
                preferred_resolution, preferred_codec, preferred_subtitle,
                preferred_subtitle_languages_json, preferred_bit_depth
         FROM my_anime WHERE anime_id = ?1",
        [anime_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        },
    )?;
    for (
        status,
        default_fansub_group_id,
        auto_download,
        download_dir,
        preferred_resolution,
        preferred_codec,
        preferred_subtitle,
        preferred_subtitle_languages_json,
        preferred_bit_depth,
    ) in tracking_rows
    {
        let preferred_subtitle_languages: Vec<String> =
            parse_json(&preferred_subtitle_languages_json, "导入番剧字幕语言")?;
        let untouched = status == "planned"
            && default_fansub_group_id.is_none()
            && !auto_download
            && download_dir.as_deref().is_none_or(str::is_empty)
            && preferred_resolution.as_deref() == Some("1080p")
            && preferred_codec.as_deref() == Some("H.265/HEVC")
            && preferred_subtitle.as_deref() == Some("chs")
            && preferred_subtitle_languages == ["chs"]
            && preferred_bit_depth == Some(10);
        if !untouched {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 根据剩余下载、资源缓存和放送时间解析单集生命周期状态。
fn resolve_episode_status_from_downloads(
    connection: &Connection,
    episode: &Episode,
) -> Result<EpisodeStatus, StorageError> {
    let status = resolve_episode_status_after_unwatch(connection, episode)?;
    if status != EpisodeStatus::Aired {
        return Ok(status);
    }
    let matched = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM release
            WHERE anime_id = ?1 AND episode_no = ?2
         )",
        params![&episode.anime_id, episode.episode_no],
        |row| row.get::<_, i64>(0),
    )? != 0;
    Ok(if matched {
        EpisodeStatus::Matched
    } else {
        EpisodeStatus::Aired
    })
}

/// 按番剧和集数查找稳定的单集标识。
fn find_episode_id(
    connection: &Connection,
    anime_id: &str,
    episode_no: f64,
) -> Result<Option<String>, StorageError> {
    connection
        .query_row(
            "SELECT id FROM episode WHERE anime_id = ?1 AND episode_no = ?2",
            params![anime_id, episode_no],
            |row| row.get(0),
        )
        .optional()
        .map_err(StorageError::from)
}

/// 写入单集并回填同番剧同集数的历史下载关联。
fn upsert_episode_row(
    connection: &Connection,
    episode: &Episode,
    timestamp: &str,
) -> Result<(usize, usize), StorageError> {
    validate_episode(episode)?;
    connection.execute(
        "INSERT INTO episode (
           id, anime_id, episode_no, title, air_time, status, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(id) DO UPDATE SET
           anime_id = excluded.anime_id, episode_no = excluded.episode_no,
           title = excluded.title, air_time = excluded.air_time,
           status = excluded.status, updated_at = excluded.updated_at",
        params![
            &episode.id,
            &episode.anime_id,
            episode.episode_no,
            episode.title.as_deref(),
            episode.air_time.as_deref(),
            episode_status_value(&episode.status),
            timestamp,
        ],
    )?;
    let linked_tasks = connection.execute(
        "UPDATE download_task
            SET episode_id = ?1, updated_at = ?2
          WHERE episode_id IS NULL
            AND anime_id = ?3
            AND episode_no = ?4",
        params![
            &episode.id,
            timestamp,
            &episode.anime_id,
            episode.episode_no
        ],
    )?;
    let linked_files = connection.execute(
        "UPDATE torrent_file
            SET episode_id = ?1
          WHERE episode_id IS NULL
            AND episode_no = ?2
            AND download_task_id IN (
              SELECT id FROM download_task WHERE anime_id = ?3
            )",
        params![&episode.id, episode.episode_no, &episode.anime_id],
    )?;
    Ok((linked_tasks, linked_files))
}

/// 写入单集偏好并维护番剧与字幕组的发现关联。
fn upsert_episode_preference_row(
    connection: &Connection,
    preference: &EpisodePreference,
    timestamp: &str,
) -> Result<(), StorageError> {
    connection.execute(
        "INSERT INTO episode_preference (
           id, anime_id, episode_id, fansub_group_id, release_id, is_manual_override, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(episode_id) DO UPDATE SET
           id = excluded.id, anime_id = excluded.anime_id,
           fansub_group_id = excluded.fansub_group_id, release_id = excluded.release_id,
           is_manual_override = excluded.is_manual_override, updated_at = excluded.updated_at",
        params![
            &preference.id,
            &preference.anime_id,
            &preference.episode_id,
            preference.fansub_group_id.as_deref(),
            preference.release_id.as_deref(),
            i64::from(preference.is_manual_override),
            timestamp,
        ],
    )?;
    if let Some(fansub_group_id) = preference.fansub_group_id.as_deref() {
        connection.execute(
            "INSERT INTO anime_fansub_group (
               anime_id, fansub_group_id, first_seen_at, last_seen_at
             ) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(anime_id, fansub_group_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![&preference.anime_id, fansub_group_id, timestamp],
        )?;
    }
    Ok(())
}

/// 将 SQLite 单集偏好行映射为领域对象。
fn map_episode_preference_row(row: &Row<'_>) -> rusqlite::Result<EpisodePreference> {
    Ok(EpisodePreference {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        episode_id: row.get("episode_id")?,
        fansub_group_id: row.get("fansub_group_id")?,
        release_id: row.get("release_id")?,
        is_manual_override: row.get::<_, i64>("is_manual_override")? != 0,
    })
}

/// 将 SQLite 续播位置行映射为领域对象。
fn map_playback_checkpoint_row(row: &Row<'_>) -> rusqlite::Result<PlaybackCheckpoint> {
    let file_index = row.get::<_, i64>("file_index")?;
    Ok(PlaybackCheckpoint {
        task_id: row.get("task_id")?,
        file_index: (file_index >= 0).then_some(file_index),
        position_seconds: row.get("position_seconds")?,
        duration_seconds: row.get("duration_seconds")?,
        completed: row.get::<_, i64>("completed")? != 0,
        watched_reported: row.get::<_, i64>("watched_reported")? != 0,
        updated_at: row.get("updated_at")?,
    })
}

/// 根据单集状态和元数据生成观看进度摘要。
fn build_anime_watch_progress(item: &MyAnime, episodes: &[Episode]) -> AnimeWatchProgress {
    let known_episode_count = episodes
        .iter()
        .filter(|episode| is_positive_integer(episode.episode_no))
        .map(|episode| episode.episode_no as i64)
        .max()
        .unwrap_or_default();
    let watched_episode_count = episodes
        .iter()
        .filter(|episode| {
            episode.status == EpisodeStatus::Watched && is_positive_integer(episode.episode_no)
        })
        .map(|episode| episode.episode_no as i64)
        .max()
        .unwrap_or_default();
    let metadata_episode_count = item
        .anime
        .detail
        .as_ref()
        .and_then(|detail| detail.get("episodeCount"))
        .and_then(Value::as_i64)
        .filter(|count| *count > 0)
        .unwrap_or_default();
    AnimeWatchProgress {
        anime_id: item.anime.id.clone(),
        watched_episode_count,
        total_episode_count: metadata_episode_count
            .max(known_episode_count)
            .max(watched_episode_count),
    }
}

/// 取消已看时根据下载关联和放送时间恢复单集状态。
fn resolve_episode_status_after_unwatch(
    connection: &Connection,
    episode: &Episode,
) -> Result<EpisodeStatus, StorageError> {
    let rows = query_all_with_params(
        connection,
        "SELECT download_task.status, download_task.progress,
                torrent_file.progress AS file_progress
         FROM download_task
         LEFT JOIN torrent_file
           ON torrent_file.download_task_id = download_task.id
          AND torrent_file.selected = 1
          AND (torrent_file.episode_id = ?1 OR torrent_file.episode_no = ?2)
         WHERE download_task.anime_id = ?3
           AND (download_task.episode_id = ?1 OR download_task.episode_no = ?2 OR torrent_file.id IS NOT NULL)",
        params![&episode.id, episode.episode_no, &episode.anime_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, Option<f64>>(2)?,
            ))
        },
    )?;
    if rows.iter().any(|(status, progress, file_progress)| {
        !matches!(status.as_str(), "error" | "missing_files")
            && (matches!(status.as_str(), "completed" | "seeding")
                || progress.max(file_progress.unwrap_or_default()) >= 1.0)
    }) {
        return Ok(EpisodeStatus::Downloaded);
    }
    if rows.iter().any(|(status, progress, file_progress)| {
        matches!(
            status.as_str(),
            "queued"
                | "fetching_metadata"
                | "downloading"
                | "stalled"
                | "paused"
                | "checking"
                | "moving"
        ) && progress.max(file_progress.unwrap_or_default()) < 1.0
    }) {
        return Ok(EpisodeStatus::Downloading);
    }
    if episode
        .air_time
        .as_deref()
        .and_then(parse_timestamp)
        .is_some_and(|air_time| air_time > Utc::now())
    {
        return Ok(EpisodeStatus::Upcoming);
    }
    Ok(EpisodeStatus::Aired)
}

/// 校验并规范化续播写入参数。
fn normalize_playback_checkpoint_input(
    input: &SavePlaybackCheckpointInput,
) -> Result<SavePlaybackCheckpointInput, StorageError> {
    let task_id = input.task_id.trim();
    if task_id.is_empty()
        || task_id.len() > 160
        || !task_id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"._:-".contains(&value))
    {
        return invalid_input("taskId", "下载任务标识格式无效");
    }
    if input.file_index.is_some_and(|index| index < 0) {
        return invalid_input("fileIndex", "播放文件索引必须是非负整数");
    }
    if !is_playback_seconds(input.position_seconds) || !is_playback_seconds(input.duration_seconds)
    {
        return invalid_input("playbackSeconds", "播放位置和时长必须是有效的非负秒数");
    }
    let position_seconds = if input.duration_seconds > 0.0 {
        input.position_seconds.min(input.duration_seconds)
    } else {
        input.position_seconds
    };
    Ok(SavePlaybackCheckpointInput {
        task_id: task_id.to_owned(),
        file_index: input.file_index,
        position_seconds,
        duration_seconds: input.duration_seconds,
        completed: Some(input.completed == Some(true)),
    })
}

/// 将播放位置换算为受限百分比。
fn calculate_playback_percent(position_seconds: f64, duration_seconds: f64) -> f64 {
    if !position_seconds.is_finite() || !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return 0.0;
    }
    (position_seconds / duration_seconds * 100.0).clamp(0.0, 100.0)
}

const PLAYBACK_COMPLETION_THRESHOLD_PERCENT: f64 = 90.0;

/// 判断秒数是否处于播放器允许持久化的范围。
fn is_playback_seconds(value: f64) -> bool {
    const MAX_PLAYBACK_SECONDS: f64 = 31.0 * 24.0 * 60.0 * 60.0;
    value.is_finite() && (0.0..=MAX_PLAYBACK_SECONDS).contains(&value)
}

/// 使用 -1 表示未指定文件索引，确保复合主键稳定去重。
fn normalize_checkpoint_file_index(file_index: Option<i64>) -> i64 {
    file_index.unwrap_or(-1)
}

/// 已确认来源标题的临时别名优先级，低于官方目录别名。
const CONFIRMED_BINDING_TITLE_ALIAS_PRIORITY: i64 = 80;

/// 判断标题是否更接近中文，避免把带日文假名的来源标题注入为中文别名。
fn is_likely_chinese_title(value: &str) -> bool {
    let has_han = value.chars().any(|character| {
        matches!(
            character as u32,
            0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF
                | 0x20000..=0x2FA1F
        )
    });
    let has_kana = value
        .chars()
        .any(|character| matches!(character as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF));
    has_han && !has_kana
}

/// 写库前按别名文本去重并重建番剧内稳定标识。
fn normalize_anime_aliases(anime: &Anime) -> Vec<AnimeAlias> {
    let mut aliases = Vec::<AnimeAlias>::new();
    let mut index_by_key = HashMap::<String, usize>::new();
    for alias in &anime.aliases {
        let value = alias.alias.trim();
        if value.is_empty() {
            continue;
        }
        let key = value.to_lowercase();
        if let Some(index) = index_by_key.get(&key).copied() {
            if alias.priority > aliases[index].priority {
                aliases[index] = AnimeAlias {
                    alias: value.to_owned(),
                    ..alias.clone()
                };
            }
            continue;
        }
        index_by_key.insert(key, aliases.len());
        aliases.push(AnimeAlias {
            alias: value.to_owned(),
            ..alias.clone()
        });
    }
    aliases
        .into_iter()
        .enumerate()
        .map(|(index, alias)| AnimeAlias {
            id: format!("{}-alias-{}", anime.id, index + 1),
            anime_id: anime.id.clone(),
            ..alias
        })
        .collect()
}

/// 将字幕语言集合转换为旧单值字段。
fn to_legacy_subtitle_preference(values: &[String]) -> Option<&str> {
    match values {
        [] => None,
        [value] => Some(value.as_str()),
        _ => Some("multi"),
    }
}

/// 校验业务标识符不为空且长度受限。
fn validate_identifier(field: &'static str, value: &str) -> Result<(), StorageError> {
    if value.trim().is_empty() || value.len() > 200 {
        return invalid_input(field, "标识不能为空且不能超过 200 个字符");
    }
    Ok(())
}

/// 校验季度值属于稳定枚举。
fn validate_anime_season(season: &str) -> Result<(), StorageError> {
    if matches!(season, "winter" | "spring" | "summer" | "fall") {
        return Ok(());
    }
    invalid_input("season", "季度必须是 winter、spring、summer 或 fall")
}

/// 校验单集标识、番剧关联和集数。
fn validate_episode(episode: &Episode) -> Result<(), StorageError> {
    validate_identifier("episode.id", &episode.id)?;
    validate_identifier("episode.animeId", &episode.anime_id)?;
    if !episode.episode_no.is_finite() || episode.episode_no <= 0.0 {
        return invalid_input("episode.episodeNo", "单集编号必须是正数");
    }
    Ok(())
}

/// 校验下载任务及其完整文件快照可安全写入 SQLite。
fn validate_download_task(task: &DownloadTask) -> Result<(), StorageError> {
    validate_identifier("downloadTask.id", &task.id)?;
    if task.id
        != task
            .engine
            .scope_task_id(task.engine.unscoped_task_id(&task.id))
        || task.engine.unscoped_task_id(&task.id) == task.id
    {
        return invalid_input("downloadTask.id", "下载任务标识必须包含所属引擎命名空间");
    }
    if task.name.trim().is_empty() {
        return invalid_input("downloadTask.name", "下载任务名称不能为空");
    }
    if task.created_at.trim().is_empty() {
        return invalid_input("downloadTask.createdAt", "下载任务创建时间不能为空");
    }
    if !is_unit_progress(task.progress) {
        return invalid_input("downloadTask.progress", "下载任务进度必须在 0 到 1 之间");
    }
    if task.download_speed < 0 || task.upload_speed < 0 {
        return invalid_input("downloadTask.speed", "下载和上传速度不能为负数");
    }
    if task.eta_seconds.is_some_and(|value| value < 0) {
        return invalid_input("downloadTask.etaSeconds", "剩余时间不能为负数");
    }
    if task.bit_depth.is_some_and(|value| value <= 0) {
        return invalid_input("downloadTask.bitDepth", "视频位深必须为正整数");
    }
    if task
        .episode_no
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        return invalid_input("downloadTask.episodeNo", "下载任务集数必须是正数");
    }
    for (field, value) in [
        ("downloadTask.releaseId", task.release_id.as_deref()),
        ("downloadTask.animeId", task.anime_id.as_deref()),
        ("downloadTask.episodeId", task.episode_id.as_deref()),
        (
            "downloadTask.fansubGroupId",
            task.fansub_group_id.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            validate_identifier(field, value)?;
        }
    }

    let mut file_ids = HashSet::new();
    let mut file_indexes = HashSet::new();
    for file in &task.files {
        validate_identifier("torrentFile.id", &file.id)?;
        if file.id != format!("{}:{}", task.id, file.index) {
            return invalid_input("torrentFile.id", "种子文件标识必须隶属于下载任务");
        }
        if !file_ids.insert(file.id.as_str()) {
            return invalid_input("torrentFile.id", "同一任务内文件标识不能重复");
        }
        if file.index < 0 || !file_indexes.insert(file.index) {
            return invalid_input("torrentFile.index", "文件索引必须非负且不能重复");
        }
        if file.name.trim().is_empty() {
            return invalid_input("torrentFile.name", "种子文件名称不能为空");
        }
        if file.size < 0 {
            return invalid_input("torrentFile.size", "种子文件大小不能为负数");
        }
        if !is_unit_progress(file.progress) {
            return invalid_input("torrentFile.progress", "种子文件进度必须在 0 到 1 之间");
        }
        if !(0..=7).contains(&file.priority) {
            return invalid_input("torrentFile.priority", "种子文件优先级必须在 0 到 7 之间");
        }
        if file
            .episode_no
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return invalid_input("torrentFile.episodeNo", "种子文件集数必须是正数");
        }
        if let Some(episode_id) = file.episode_id.as_deref() {
            validate_identifier("torrentFile.episodeId", episode_id)?;
        }
    }
    Ok(())
}

/// 判断下载进度是否为有限的单位区间数值。
fn is_unit_progress(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// 创建统一的业务输入错误。
fn invalid_input<T>(field: &'static str, message: &str) -> Result<T, StorageError> {
    Err(StorageError::InvalidInput {
        field,
        message: message.to_owned(),
    })
}

/// 判断集数是否为正整数。
fn is_positive_integer(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value.fract() == 0.0
}

/// 为观看进度补建单集生成稳定标识。
fn create_download_episode_id(anime_id: &str, episode_no: i64) -> String {
    format!("episode-{anime_id}-{episode_no}")
}

/// 返回追番状态的 SQLite 字面量。
fn anime_status_value(status: &AnimeStatus) -> &'static str {
    match status {
        AnimeStatus::Watching => "watching",
        AnimeStatus::Planned => "planned",
        AnimeStatus::Completed => "completed",
        AnimeStatus::Paused => "paused",
        AnimeStatus::Dropped => "dropped",
    }
}

/// 返回单集状态的 SQLite 字面量。
fn episode_status_value(status: &EpisodeStatus) -> &'static str {
    match status {
        EpisodeStatus::Upcoming => "upcoming",
        EpisodeStatus::Aired => "aired",
        EpisodeStatus::Matched => "matched",
        EpisodeStatus::Downloading => "downloading",
        EpisodeStatus::Downloaded => "downloaded",
        EpisodeStatus::Watched => "watched",
    }
}

/// 返回下载任务状态的 SQLite 字面量。
fn download_status_value(status: &DownloadStatus) -> &'static str {
    match status {
        DownloadStatus::Queued => "queued",
        DownloadStatus::FetchingMetadata => "fetching_metadata",
        DownloadStatus::Downloading => "downloading",
        DownloadStatus::Stalled => "stalled",
        DownloadStatus::WaitingNetwork => "waiting_network",
        DownloadStatus::Paused => "paused",
        DownloadStatus::Checking => "checking",
        DownloadStatus::Moving => "moving",
        DownloadStatus::Completed => "completed",
        DownloadStatus::Seeding => "seeding",
        DownloadStatus::Error => "error",
        DownloadStatus::MissingFiles => "missing_files",
    }
}

/// 返回下载引擎类型的 SQLite 字面量。
fn torrent_engine_value(engine: &TorrentEngineKind) -> &'static str {
    engine.as_key()
}

/// 返回番剧别名语言的 SQLite 字面量。
fn alias_language_value(language: &AnimeAliasLanguage) -> &'static str {
    match language {
        AnimeAliasLanguage::Zh => "zh",
        AnimeAliasLanguage::Ja => "ja",
        AnimeAliasLanguage::En => "en",
        AnimeAliasLanguage::Romaji => "romaji",
        AnimeAliasLanguage::Custom => "custom",
    }
}

/// 返回下载源类型的 SQLite 字面量。
fn source_kind_value(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::Rss => "rss",
        SourceKind::Torznab => "torznab",
        SourceKind::SiteAdapter => "site_adapter",
        SourceKind::Manual => "manual",
    }
}

/// 返回资源字幕偏好的 SQLite 字面量。
fn subtitle_preference_value(preference: &SubtitlePreference) -> &'static str {
    match preference {
        SubtitlePreference::Chs => "chs",
        SubtitlePreference::Cht => "cht",
        SubtitlePreference::Jpn => "jpn",
        SubtitlePreference::Eng => "eng",
        SubtitlePreference::Multi => "multi",
    }
}

/// 解析资源分辨率字面量。
fn parse_release_resolution(value: &str) -> Result<ReleaseResolution, StorageError> {
    match value {
        "720p" => Ok(ReleaseResolution::P720),
        "1080p" => Ok(ReleaseResolution::P1080),
        "2160p" => Ok(ReleaseResolution::P2160),
        _ => invalid_value("release.resolution", value),
    }
}

/// 解析资源标准视频编码字面量。
fn parse_normalized_video_codec(value: &str) -> Result<NormalizedVideoCodec, StorageError> {
    match value {
        "H.264/AVC" => Ok(NormalizedVideoCodec::H264Avc),
        "H.265/HEVC" => Ok(NormalizedVideoCodec::H265Hevc),
        "AV1" => Ok(NormalizedVideoCodec::Av1),
        "VP9" => Ok(NormalizedVideoCodec::Vp9),
        "Unknown" => Ok(NormalizedVideoCodec::Unknown),
        _ => invalid_value("release.normalized_video_codec", value),
    }
}

/// 解析资源字幕偏好字面量。
fn parse_subtitle_preference(value: &str) -> Result<SubtitlePreference, StorageError> {
    match value {
        "chs" => Ok(SubtitlePreference::Chs),
        "cht" => Ok(SubtitlePreference::Cht),
        "jpn" => Ok(SubtitlePreference::Jpn),
        "eng" => Ok(SubtitlePreference::Eng),
        "multi" => Ok(SubtitlePreference::Multi),
        _ => invalid_value("release.subtitle", value),
    }
}

/// 返回来源绑定方式的 SQLite 字面量。
fn anime_source_match_method_value(method: &AnimeSourceBindingMatchMethod) -> &'static str {
    match method {
        AnimeSourceBindingMatchMethod::Manual => "manual",
        AnimeSourceBindingMatchMethod::ExternalId => "external_id",
        AnimeSourceBindingMatchMethod::Scored => "scored",
    }
}

/// 解析来源绑定方式。
fn parse_anime_source_match_method(
    value: &str,
) -> Result<AnimeSourceBindingMatchMethod, StorageError> {
    match value {
        "manual" => Ok(AnimeSourceBindingMatchMethod::Manual),
        "external_id" => Ok(AnimeSourceBindingMatchMethod::ExternalId),
        "scored" => Ok(AnimeSourceBindingMatchMethod::Scored),
        _ => invalid_value("anime_source_binding.match_method", value),
    }
}

/// 返回来源排除作用域的 SQLite 字面量。
fn anime_source_exclusion_scope_value(scope: &AnimeSourceExclusionScope) -> &'static str {
    match scope {
        AnimeSourceExclusionScope::Candidate => "candidate",
        AnimeSourceExclusionScope::Source => "source",
    }
}

/// 解析来源排除作用域。
fn parse_anime_source_exclusion_scope(
    value: &str,
) -> Result<AnimeSourceExclusionScope, StorageError> {
    match value {
        "candidate" => Ok(AnimeSourceExclusionScope::Candidate),
        "source" => Ok(AnimeSourceExclusionScope::Source),
        _ => invalid_value("anime_source_exclusion.scope", value),
    }
}

/// 解析下载源类型。
fn parse_source_kind(value: &str) -> Result<SourceKind, StorageError> {
    match value {
        "rss" => Ok(SourceKind::Rss),
        "torznab" => Ok(SourceKind::Torznab),
        "site_adapter" => Ok(SourceKind::SiteAdapter),
        "manual" => Ok(SourceKind::Manual),
        _ => invalid_value("release_source.kind", value),
    }
}

/// 将来源采集间隔限制在 250 毫秒到 60 秒之间。
fn normalize_source_request_interval(value: i64) -> i64 {
    value.clamp(250, 60_000)
}

/// 校验可选来源 URL 只使用 HTTP 或 HTTPS。
fn validate_optional_http_url(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), StorageError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let parsed = url::Url::parse(value).map_err(|error| StorageError::InvalidInput {
        field,
        message: format!("URL 格式无效：{error}"),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return invalid_input(field, "仅允许 HTTP 或 HTTPS URL");
    }
    Ok(())
}

/// 将持久化设置递归覆盖到平台默认设置。
fn merge_json(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

/// 按稳定标识合并播放器配置，并保留新版本新增的默认项。
fn merge_player_profiles(defaults: Option<&Value>, patch: Option<&Value>) -> Option<Value> {
    let defaults = defaults?.as_array()?;
    let Some(patch) = patch.and_then(Value::as_array) else {
        return Some(Value::Array(defaults.clone()));
    };
    let patch_by_id = patch
        .iter()
        .filter_map(|profile| Some((profile.get("id")?.as_str()?, profile)))
        .collect::<HashMap<_, _>>();
    let mut merged = defaults
        .iter()
        .map(|profile| {
            let mut profile = profile.clone();
            if let Some(profile_patch) = profile
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| patch_by_id.get(id))
            {
                merge_json(&mut profile, (*profile_patch).clone());
            }
            profile
        })
        .collect::<Vec<_>>();
    let default_ids = defaults
        .iter()
        .filter_map(|profile| profile.get("id")?.as_str())
        .collect::<Vec<_>>();
    merged.extend(
        patch
            .iter()
            .filter(|profile| match profile.get("id").and_then(Value::as_str) {
                Some(id) => !default_ids.contains(&id),
                None => true,
            })
            .cloned(),
    );
    Some(Value::Array(merged))
}

/// 强制使用当前宿主拥有的数据目录，避免复制旧库后继续暴露 Electron 路径。
fn preserve_host_storage_paths(settings: &mut Value, platform_defaults: &Value) {
    let Some(default_storage) = platform_defaults.get("storage").cloned() else {
        return;
    };
    if let Some(settings) = settings.as_object_mut() {
        settings.insert("storage".to_owned(), default_storage);
    }
}

/// 解析数据库 JSON 字段并附带业务上下文。
fn parse_json<T: DeserializeOwned>(value: &str, context: &'static str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|source| StorageError::JsonData { context, source })
}

/// 解析首页状态中的单个字段，缺失时交由调用方使用默认值。
fn read_dashboard_field<T: DeserializeOwned>(
    dashboard: &Value,
    key: &str,
    context: &'static str,
) -> Result<Option<T>, StorageError> {
    dashboard
        .get(key)
        .cloned()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|source| StorageError::JsonData { context, source })
        })
        .transpose()
}

/// 规范化字幕语言集合，并在空集合时兼容旧单值字段。
fn resolve_subtitle_languages(values: Vec<String>, legacy: Option<&str>) -> Vec<String> {
    let mut normalized = ["chs", "cht", "jpn", "eng"]
        .into_iter()
        .filter(|language| values.iter().any(|value| value == language))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !normalized.is_empty() {
        return normalized;
    }
    normalized = match legacy {
        Some("multi") => vec!["chs".to_owned(), "cht".to_owned()],
        Some(language @ ("chs" | "cht" | "jpn" | "eng")) => vec![language.to_owned()],
        _ => Vec::new(),
    };
    normalized
}

/// 构建当天追番提醒和状态计数。
fn build_daily_reminder(
    my_anime: &[MyAnime],
    episodes: &[Episode],
    downloads: &[DownloadTask],
    fansub_names: &HashMap<String, String>,
) -> DailyReminderSummary {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let followed = my_anime
        .iter()
        .map(|item| (item.anime.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let mut items = Vec::new();

    for episode in episodes {
        let Some(air_time) = episode.air_time.as_deref() else {
            continue;
        };
        if local_date_key(air_time).as_deref() != Some(today.as_str()) {
            continue;
        }
        let Some(followed_anime) = followed.get(episode.anime_id.as_str()) else {
            continue;
        };
        let download = find_episode_download(downloads, episode);
        let status = resolve_reminder_status(episode, download.as_ref());
        let fansub_name = followed_anime
            .default_fansub_group_id
            .as_ref()
            .and_then(|id| fansub_names.get(id))
            .cloned();
        items.push(DailyReminderItem {
            id: format!("daily-{}", episode.id),
            anime_id: episode.anime_id.clone(),
            anime_title: followed_anime.anime.title.clone(),
            episode_id: episode.id.clone(),
            episode_no: episode.episode_no,
            air_time: episode.air_time.clone(),
            status,
            fansub_name,
            download_task_id: download.map(|link| link.task.id.clone()),
        });
    }
    items.sort_by(|left, right| left.air_time.cmp(&right.air_time));

    DailyReminderSummary {
        date: today,
        total: items.len(),
        upcoming: count_status(&items, &[EpisodeStatus::Upcoming]),
        aired: count_status(&items, &[EpisodeStatus::Aired, EpisodeStatus::Matched]),
        downloading: count_status(&items, &[EpisodeStatus::Downloading]),
        downloaded: count_status(&items, &[EpisodeStatus::Downloaded, EpisodeStatus::Watched]),
        items,
    }
}

/// 生成首页默认字幕组等待事项。
fn build_pending_actions(
    my_anime: &[MyAnime],
    episodes: &[Episode],
    downloads: &[DownloadTask],
) -> Vec<PendingAction> {
    let followed = my_anime
        .iter()
        .map(|item| (item.anime.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let now = Utc::now();
    let mut candidates = episodes
        .iter()
        .filter_map(|episode| {
            let item = followed.get(episode.anime_id.as_str())?;
            let fansub_group_id = item.default_fansub_group_id.as_deref()?;
            if episode.status != EpisodeStatus::Aired {
                return None;
            }
            if episode
                .air_time
                .as_deref()
                .and_then(parse_timestamp)
                .is_some_and(|air_time| air_time > now)
            {
                return None;
            }
            let already_matched = downloads.iter().any(|task| {
                task.anime_id.as_deref() == Some(episode.anime_id.as_str())
                    && (task.episode_id.as_deref() == Some(episode.id.as_str())
                        || task.episode_no == Some(episode.episode_no))
                    && task.fansub_group_id.as_deref() == Some(fansub_group_id)
            });
            (!already_matched).then_some((*item, episode))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.1.air_time.cmp(&left.1.air_time).then_with(|| {
            right
                .1
                .episode_no
                .partial_cmp(&left.1.episode_no)
                .unwrap_or(Ordering::Equal)
        })
    });
    candidates
        .into_iter()
        .take(8)
        .map(|(item, episode)| PendingAction {
            id: format!("pending-default-fansub-{}", episode.id),
            title: format!("《{}》第 {} 集", item.anime.title, episode.episode_no),
            description: format!(
                "《{}》第 {} 集已开播，但默认字幕组还没有发布资源。",
                item.anime.title, episode.episode_no
            ),
            severity: "warning".to_owned(),
            anime_id: Some(episode.anime_id.clone()),
            episode_id: Some(episode.id.clone()),
            episode_no: Some(episode.episode_no),
        })
        .collect()
}

/// 将每日提醒转换为首页精简单集。
fn to_episode_summary(item: &DailyReminderItem) -> EpisodeSummary {
    EpisodeSummary {
        id: item.id.clone(),
        anime_title: item.anime_title.clone(),
        episode_no: item.episode_no,
        air_time: item.air_time.as_deref().and_then(format_local_time),
        status: item.status.clone(),
        fansub_name: item.fansub_name.clone(),
        download_task_id: item.download_task_id.clone(),
    }
}

/// 计算指定状态集合中的提醒数量。
fn count_status(items: &[DailyReminderItem], statuses: &[EpisodeStatus]) -> usize {
    items
        .iter()
        .filter(|item| statuses.contains(&item.status))
        .count()
}

/// 查找任务级或文件级单集下载关联。
fn find_episode_download<'a>(
    downloads: &'a [DownloadTask],
    episode: &Episode,
) -> Option<EpisodeDownload<'a>> {
    for task in downloads {
        if task.anime_id.as_deref() != Some(episode.anime_id.as_str()) {
            continue;
        }
        if task.episode_id.as_deref() == Some(episode.id.as_str())
            || task.episode_no == Some(episode.episode_no)
        {
            return Some(EpisodeDownload { task, file: None });
        }
        if let Some(file) = task.files.iter().find(|file| {
            file.selected
                && (file.episode_id.as_deref() == Some(episode.id.as_str())
                    || file.episode_no == Some(episode.episode_no))
        }) {
            return Some(EpisodeDownload {
                task,
                file: Some(file),
            });
        }
    }
    None
}

/// 根据下载关联解析首页提醒状态。
fn resolve_reminder_status(
    episode: &Episode,
    download: Option<&EpisodeDownload<'_>>,
) -> EpisodeStatus {
    let Some(download) = download else {
        return episode.status.clone();
    };
    if download.task.is_completed() || download.file.is_some_and(|file| file.progress >= 1.0) {
        return EpisodeStatus::Downloaded;
    }
    if download.task.status.is_active() {
        return EpisodeStatus::Downloading;
    }
    episode.status.clone()
}

/// 将时间戳转换为当前时区日期键。
fn local_date_key(value: &str) -> Option<String> {
    parse_timestamp(value).map(|date| date.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

/// 将时间戳转换为当前时区时分。
fn format_local_time(value: &str) -> Option<String> {
    parse_timestamp(value).map(|date| date.with_timezone(&Local).format("%H:%M").to_string())
}

/// 解析数据库使用的 RFC 3339 时间戳。
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

/// 返回媒体文件最近探测或下载时间。
fn media_sort_key(media: &MediaFile) -> &str {
    media
        .probed_at
        .as_deref()
        .or(media.downloaded_at.as_deref())
        .unwrap_or("")
}

/// 按季度和标题排序番剧目录。
fn sort_anime_catalog(items: &mut [Anime]) {
    items.sort_by(|left, right| {
        right
            .premiere_year
            .cmp(&left.premiere_year)
            .then_with(|| right.premiere_month.cmp(&left.premiere_month))
            .then_with(|| left.title.cmp(&right.title))
    });
}

/// 根据 ID、同来源外部 ID 或标题判断两条目录记录是否相同。
fn is_same_anime(left: &Anime, right: &Anime) -> bool {
    if left.id == right.id {
        return true;
    }
    let shared_external_id = right.external_ids.as_object().is_some_and(|right_ids| {
        left.external_ids.as_object().is_some_and(|left_ids| {
            right_ids.iter().any(|(key, value)| {
                is_meaningful_json(value)
                    && left_ids.get(key).is_some_and(|left_value| {
                        is_meaningful_json(left_value) && left_value == value
                    })
            })
        })
    });
    if shared_external_id {
        return true;
    }
    let left_titles = [Some(left.title.as_str()), left.original_title.as_deref()];
    let right_titles = [Some(right.title.as_str()), right.original_title.as_deref()];
    left_titles
        .into_iter()
        .flatten()
        .filter(|title| !title.trim().is_empty())
        .any(|left_title| {
            right_titles
                .into_iter()
                .flatten()
                .filter(|title| !title.trim().is_empty())
                .any(|right_title| left_title == right_title)
        })
}

/// 以新采集字段为主合并目录记录，同时保持已有稳定标识和业务保护字段。
fn merge_anime(existing: &Anime, incoming: &Anime, preserve_rating: bool) -> Anime {
    let incoming_premiere_date = non_empty_text(incoming.premiere_date.as_deref());
    let preserve_existing_window = incoming_premiere_date.is_none()
        && non_empty_text(existing.premiere_date.as_deref()).is_some();
    Anime {
        id: existing.id.clone(),
        title: incoming.title.clone(),
        original_title: prefer_non_empty_text(
            incoming.original_title.as_deref(),
            existing.original_title.as_deref(),
        ),
        aliases: merge_anime_aliases(&existing.aliases, &incoming.aliases, &existing.id),
        premiere_date: incoming_premiere_date
            .or_else(|| non_empty_text(existing.premiere_date.as_deref())),
        premiere_year: if preserve_existing_window {
            existing.premiere_year
        } else {
            incoming.premiere_year
        },
        premiere_month: if preserve_existing_window {
            existing.premiere_month
        } else {
            incoming.premiere_month
        },
        season: prefer_non_empty_text(incoming.season.as_deref(), existing.season.as_deref()),
        summary: prefer_non_empty_text(incoming.summary.as_deref(), existing.summary.as_deref()),
        cover_url: prefer_non_empty_text(
            incoming.cover_url.as_deref(),
            existing.cover_url.as_deref(),
        ),
        rating: if preserve_rating {
            existing.rating.clone().or_else(|| incoming.rating.clone())
        } else {
            incoming.rating.clone().or_else(|| existing.rating.clone())
        },
        external_ids: merge_json_objects(&existing.external_ids, &incoming.external_ids),
        detail: merge_optional_json_objects(existing.detail.as_ref(), incoming.detail.as_ref()),
    }
}

/// 比较目录业务内容，忽略每次网络映射都会变化的详情刷新时间。
fn anime_catalog_content_equal(left: &Anime, right: &Anime) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    remove_detail_refresh_timestamp(&mut left.detail);
    remove_detail_refresh_timestamp(&mut right.detail);
    left == right
}

/// 去除不参与目录差异判断的详情刷新时间。
fn remove_detail_refresh_timestamp(detail: &mut Option<Value>) {
    if let Some(object) = detail.as_mut().and_then(Value::as_object_mut) {
        object.remove("refreshedAt");
    }
}

/// 合并别名并忽略大小写重复项。
fn merge_anime_aliases(
    existing: &[AnimeAlias],
    incoming: &[AnimeAlias],
    anime_id: &str,
) -> Vec<AnimeAlias> {
    let mut aliases = existing.to_vec();
    for alias in incoming {
        if !alias.alias.trim().is_empty()
            && !aliases
                .iter()
                .any(|item| item.alias.eq_ignore_ascii_case(&alias.alias))
        {
            aliases.push(AnimeAlias {
                anime_id: anime_id.to_owned(),
                ..alias.clone()
            });
        }
    }
    aliases
}

/// 递归合并两个 JSON 对象，空值不覆盖已有字段。
fn merge_json_objects(existing: &Value, incoming: &Value) -> Value {
    if !is_meaningful_json(incoming) {
        return existing.clone();
    }
    match (existing.as_object(), incoming.as_object()) {
        (Some(existing), Some(incoming)) => {
            let mut merged = existing.clone();
            for (key, incoming_value) in incoming {
                if !is_meaningful_json(incoming_value) {
                    continue;
                }
                let value = merged.get(key).map_or_else(
                    || incoming_value.clone(),
                    |existing_value| merge_json_objects(existing_value, incoming_value),
                );
                merged.insert(key.clone(), value);
            }
            Value::Object(merged)
        }
        _ => incoming.clone(),
    }
}

/// 判断采集值是否包含可用于覆盖旧数据的有效内容。
fn is_meaningful_json(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => values.iter().any(is_meaningful_json),
        Value::Object(values) => values.values().any(is_meaningful_json),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

/// 优先返回非空的新文本，否则保留已有文本。
fn prefer_non_empty_text(incoming: Option<&str>, existing: Option<&str>) -> Option<String> {
    non_empty_text(incoming).or_else(|| non_empty_text(existing))
}

/// 将非空白文本复制为可持久化字符串。
fn non_empty_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 合并可选详情对象并保留任一侧已有字段。
fn merge_optional_json_objects(
    existing: Option<&Value>,
    incoming: Option<&Value>,
) -> Option<Value> {
    match (existing, incoming) {
        (Some(existing), Some(incoming)) => Some(merge_json_objects(existing, incoming)),
        (Some(existing), None) => Some(existing.clone()),
        (None, Some(incoming)) if is_meaningful_json(incoming) => Some(incoming.clone()),
        (None, Some(_)) => None,
        (None, None) => None,
    }
}

/// 按季度和标题排序追番列表。
fn sort_my_anime(items: &mut [MyAnime]) {
    items.sort_by(|left, right| {
        right
            .anime
            .premiere_year
            .cmp(&left.anime.premiere_year)
            .then_with(|| right.anime.premiere_month.cmp(&left.anime.premiere_month))
            .then_with(|| left.anime.title.cmp(&right.anime.title))
    });
}

/// 单集与下载任务的关联结果。
struct EpisodeDownload<'task> {
    task: &'task DownloadTask,
    file: Option<&'task TorrentFile>,
}

struct AnimeAliasRow {
    id: String,
    anime_id: String,
    alias: String,
    language: String,
    priority: i64,
}

impl AnimeAliasRow {
    /// 将 SQLite 别名行转换为领域对象。
    fn into_domain(self) -> Result<AnimeAlias, StorageError> {
        Ok(AnimeAlias {
            id: self.id,
            anime_id: self.anime_id,
            alias: self.alias,
            language: parse_alias_language(&self.language)?,
            priority: self.priority,
        })
    }
}

struct AnimeRow {
    id: String,
    title: String,
    original_title: Option<String>,
    premiere_date: Option<String>,
    premiere_year: i64,
    premiere_month: i64,
    season: Option<String>,
    summary: Option<String>,
    cover_url: Option<String>,
    rating_score: Option<f64>,
    rating_count: Option<i64>,
    rating_source: Option<String>,
    external_ids_json: String,
    detail_json: String,
}

impl AnimeRow {
    /// 将 SQLite 番剧行转换为领域对象，详情损坏时保留基础信息。
    fn into_domain(self, aliases: Vec<AnimeAlias>) -> Result<Anime, StorageError> {
        let rating = self
            .rating_score
            .zip(self.rating_source)
            .map(|(score, source)| AnimeRating {
                score,
                count: self.rating_count,
                source,
            });
        let detail = match serde_json::from_str::<Value>(&self.detail_json) {
            Ok(Value::Object(object)) if object.is_empty() => None,
            Ok(Value::Null) => None,
            Ok(value) => Some(value),
            Err(detail_error) => {
                warn!(
                    "SQLite 番剧详情 JSON 解析失败：anime_id={}, error={}",
                    self.id, detail_error
                );
                None
            }
        };
        Ok(Anime {
            id: self.id,
            title: self.title,
            original_title: self.original_title,
            aliases,
            premiere_date: self.premiere_date,
            premiere_year: self.premiere_year,
            premiere_month: self.premiere_month,
            season: self.season,
            summary: self.summary,
            cover_url: self.cover_url,
            rating,
            external_ids: parse_json(&self.external_ids_json, "番剧外部标识")?,
            detail,
        })
    }
}

struct FansubGroupRow {
    id: String,
    name: String,
    aliases_json: String,
    source_ids_json: String,
}

impl FansubGroupRow {
    /// 将 SQLite 字幕组行转换为领域对象。
    fn into_domain(self) -> Result<FansubGroup, StorageError> {
        Ok(FansubGroup {
            id: self.id,
            name: self.name,
            aliases: parse_json(&self.aliases_json, "字幕组别名")?,
            source_ids: parse_json(&self.source_ids_json, "字幕组来源")?,
        })
    }
}

struct CachedReleaseRow {
    id: String,
    title: String,
    anime_id: Option<String>,
    episode_no: Option<f64>,
    fansub_group_id: Option<String>,
    source_id: String,
    source_name: String,
    magnet_url: Option<String>,
    torrent_url: Option<String>,
    info_hash: Option<String>,
    size: Option<i64>,
    resolution: Option<String>,
    declared_video_codec: Option<String>,
    normalized_video_codec: Option<String>,
    bit_depth: Option<i64>,
    subtitle: Option<String>,
    subtitle_languages_json: String,
    published_at: String,
    seeders: Option<i64>,
    raw_json: String,
}

impl CachedReleaseRow {
    /// 将 SQLite 资源缓存行与完整原始字段合并为领域对象。
    fn into_domain(self) -> Result<Release, StorageError> {
        let mut release: Release = parse_json(&self.raw_json, "资源原始数据")?;
        release.id = self.id;
        release.title = self.title;
        release.anime_id = self.anime_id.or(release.anime_id);
        release.episode_no = self.episode_no.or(release.episode_no);
        release.fansub_group_id = self.fansub_group_id.or(release.fansub_group_id);
        release.source_id = self.source_id;
        release.source_name = self.source_name;
        release.magnet_url = self.magnet_url;
        release.torrent_url = self.torrent_url;
        release.info_hash = self.info_hash;
        release.size = self.size;
        release.resolution = self
            .resolution
            .as_deref()
            .map(parse_release_resolution)
            .transpose()?;
        release.declared_video_codec = self.declared_video_codec;
        release.normalized_video_codec = self
            .normalized_video_codec
            .as_deref()
            .map(parse_normalized_video_codec)
            .transpose()?;
        release.bit_depth = self.bit_depth;
        release.subtitle = self
            .subtitle
            .as_deref()
            .map(parse_subtitle_preference)
            .transpose()?;
        release.subtitle_languages =
            parse_json::<Vec<SubtitleLanguage>>(&self.subtitle_languages_json, "资源字幕语言")?;
        release.published_at = self.published_at;
        release.seeders = self.seeders;
        Ok(release)
    }
}

struct AnimeSourceBindingRow {
    id: String,
    anime_id: String,
    source_id: String,
    source_anime_id: String,
    source_anime_title: Option<String>,
    source_url: Option<String>,
    match_method: String,
    confidence: f64,
    confirmed: bool,
    created_at: String,
    updated_at: String,
}

impl AnimeSourceBindingRow {
    /// 将 SQLite 来源绑定行转换为领域对象。
    fn into_domain(self) -> Result<AnimeSourceBinding, StorageError> {
        Ok(AnimeSourceBinding {
            id: self.id,
            anime_id: self.anime_id,
            source_id: self.source_id,
            source_anime_id: self.source_anime_id,
            source_anime_title: self.source_anime_title,
            source_url: self.source_url,
            match_method: parse_anime_source_match_method(&self.match_method)?,
            confidence: self.confidence,
            confirmed: self.confirmed,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct AnimeSourceExclusionRow {
    id: String,
    anime_id: String,
    source_id: String,
    scope: String,
    source_anime_id: Option<String>,
    source_anime_title: Option<String>,
    created_at: String,
    updated_at: String,
}

impl AnimeSourceExclusionRow {
    /// 将 SQLite 来源排除行转换为领域对象。
    fn into_domain(self) -> Result<AnimeSourceExclusion, StorageError> {
        Ok(AnimeSourceExclusion {
            id: self.id,
            anime_id: self.anime_id,
            source_id: self.source_id,
            scope: parse_anime_source_exclusion_scope(&self.scope)?,
            source_anime_id: self.source_anime_id.filter(|value| !value.is_empty()),
            source_anime_title: self.source_anime_title,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct ReleaseSourceRow {
    id: String,
    name: String,
    kind: String,
    enabled: bool,
    use_proxy: bool,
    request_interval_ms: i64,
    base_url: Option<String>,
    api_key: Option<String>,
    rss_url: Option<String>,
    tags_json: String,
}

impl ReleaseSourceRow {
    /// 将 SQLite 下载源行转换为领域对象。
    fn into_domain(self) -> Result<ReleaseSourceConfig, StorageError> {
        Ok(ReleaseSourceConfig {
            id: self.id,
            name: self.name,
            kind: parse_source_kind(&self.kind)?,
            enabled: self.enabled,
            use_proxy: self.use_proxy,
            request_interval_ms: normalize_source_request_interval(self.request_interval_ms),
            base_url: self.base_url,
            api_key: self.api_key,
            rss_url: self.rss_url,
            tags: parse_json(&self.tags_json, "下载源标签")?,
        })
    }
}

struct MyAnimeRow {
    id: String,
    anime_id: String,
    status: String,
    default_fansub_group_id: Option<String>,
    auto_download: bool,
    download_dir: Option<String>,
    preferred_resolution: Option<String>,
    preferred_codec: Option<String>,
    preferred_subtitle: Option<String>,
    preferred_subtitle_languages_json: String,
    preferred_bit_depth: Option<i64>,
    added_at: String,
    updated_at: String,
}

impl MyAnimeRow {
    /// 将 SQLite 追番行转换为领域对象。
    fn into_domain(
        self,
        anime: Anime,
        rss_subscriptions: Vec<AnimeRssSubscription>,
    ) -> Result<MyAnime, StorageError> {
        let preferred_subtitle_languages = resolve_subtitle_languages(
            parse_json(&self.preferred_subtitle_languages_json, "追番字幕语言")?,
            self.preferred_subtitle.as_deref(),
        );
        Ok(MyAnime {
            id: self.id,
            anime,
            status: parse_anime_status(&self.status)?,
            default_fansub_group_id: self.default_fansub_group_id,
            auto_download: self.auto_download,
            download_dir: self.download_dir,
            rss_subscriptions,
            preferred_resolution: self.preferred_resolution,
            preferred_codec: self.preferred_codec,
            preferred_bit_depth: self.preferred_bit_depth,
            preferred_subtitle_languages,
            preferred_subtitle: self.preferred_subtitle,
            added_at: self.added_at,
            updated_at: self.updated_at,
        })
    }
}

struct RssSubscriptionRow {
    id: String,
    my_anime_id: String,
    name: String,
    url: String,
    enabled: bool,
    preferred_subtitle: Option<String>,
    preferred_subtitle_languages_json: String,
    refresh_interval_minutes: Option<i64>,
    last_fetched_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl RssSubscriptionRow {
    /// 将 SQLite RSS 订阅行转换为领域对象。
    fn into_domain(self) -> Result<AnimeRssSubscription, StorageError> {
        let preferred_subtitle_languages = resolve_subtitle_languages(
            parse_json(&self.preferred_subtitle_languages_json, "RSS 字幕语言")?,
            self.preferred_subtitle.as_deref(),
        );
        Ok(AnimeRssSubscription {
            id: self.id,
            my_anime_id: self.my_anime_id,
            name: self.name,
            url: self.url,
            enabled: self.enabled,
            preferred_subtitle_languages,
            preferred_subtitle: self.preferred_subtitle,
            refresh_interval_minutes: self.refresh_interval_minutes,
            last_fetched_at: self.last_fetched_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct EpisodeRow {
    id: String,
    anime_id: String,
    episode_no: f64,
    title: Option<String>,
    air_time: Option<String>,
    status: String,
}

impl EpisodeRow {
    /// 将 SQLite 单集行转换为领域对象。
    fn into_domain(self) -> Result<Episode, StorageError> {
        Ok(Episode {
            id: self.id,
            anime_id: self.anime_id,
            episode_no: self.episode_no,
            title: self.title,
            air_time: self.air_time,
            status: parse_episode_status(&self.status)?,
        })
    }
}

struct TorrentFileRow {
    download_task_id: String,
    id: String,
    index: i64,
    name: String,
    episode_id: Option<String>,
    episode_no: Option<f64>,
    size: i64,
    progress: f64,
    priority: i64,
    selected: bool,
}

impl TorrentFileRow {
    /// 将 SQLite 种子文件行转换为领域对象。
    fn into_domain(self) -> TorrentFile {
        TorrentFile {
            id: self.id,
            index: self.index,
            name: self.name,
            episode_id: self.episode_id,
            episode_no: self.episode_no,
            size: self.size,
            progress: self.progress,
            priority: self.priority,
            selected: self.selected,
        }
    }
}

struct DownloadRow {
    id: String,
    release_id: Option<String>,
    anime_id: Option<String>,
    episode_id: Option<String>,
    anime_title: Option<String>,
    episode_no: Option<f64>,
    fansub_group_id: Option<String>,
    fansub_name: Option<String>,
    resolution: Option<String>,
    declared_video_codec: Option<String>,
    normalized_video_codec: Option<String>,
    bit_depth: Option<i64>,
    subtitle_languages_json: String,
    subtitle: Option<String>,
    correlation_tag: Option<String>,
    engine: String,
    torrent_hash: Option<String>,
    name: String,
    status: String,
    progress: f64,
    download_speed: i64,
    upload_speed: i64,
    eta_seconds: Option<i64>,
    save_path: String,
    created_at: String,
    completed_at: Option<String>,
}

impl DownloadRow {
    /// 将 SQLite 下载任务行转换为领域对象。
    fn into_domain(self, files: Vec<TorrentFile>) -> Result<DownloadTask, StorageError> {
        let subtitle_languages = resolve_subtitle_languages(
            parse_json(&self.subtitle_languages_json, "下载任务字幕语言")?,
            self.subtitle.as_deref(),
        );
        Ok(DownloadTask {
            id: self.id,
            release_id: self.release_id,
            anime_id: self.anime_id,
            episode_id: self.episode_id,
            anime_title: self.anime_title,
            episode_no: self.episode_no,
            fansub_group_id: self.fansub_group_id,
            fansub_name: self.fansub_name,
            resolution: self.resolution,
            declared_video_codec: self.declared_video_codec,
            normalized_video_codec: self.normalized_video_codec,
            bit_depth: self.bit_depth,
            subtitle_languages,
            subtitle: self.subtitle,
            correlation_tag: self.correlation_tag,
            engine: parse_torrent_engine(&self.engine)?,
            torrent_hash: self.torrent_hash,
            name: self.name,
            status: parse_download_status(&self.status)?,
            progress: self.progress,
            download_speed: self.download_speed,
            upload_speed: self.upload_speed,
            eta_seconds: self.eta_seconds,
            save_path: self.save_path,
            files,
            created_at: self.created_at,
            completed_at: self.completed_at,
        })
    }
}

struct MediaFileRow {
    id: String,
    anime_id: String,
    episode_id: Option<String>,
    download_task_id: Option<String>,
    content_kind: String,
    special_no: Option<String>,
    file_path: String,
    file_name: String,
    size: i64,
    container: Option<String>,
    declared_video_codec: Option<String>,
    detected_video_codec: Option<String>,
    normalized_video_codec: String,
    resolution: Option<String>,
    bit_depth: Option<i64>,
    audio_codecs_json: String,
    subtitle_tracks_json: String,
    duration_seconds: Option<i64>,
    downloaded_at: Option<String>,
    probed_at: Option<String>,
    origin: String,
    source_root: Option<String>,
    fingerprint: Option<String>,
    file_modified_at: Option<String>,
    availability: String,
    last_verified_at: Option<String>,
    availability_error: Option<String>,
}

impl MediaFileRow {
    /// 将 SQLite 媒体文件行转换为领域对象。
    fn into_domain(self) -> Result<MediaFile, StorageError> {
        Ok(MediaFile {
            id: self.id,
            anime_id: self.anime_id,
            episode_id: self.episode_id,
            download_task_id: self.download_task_id,
            content_kind: parse_media_content_kind(&self.content_kind)?,
            special_no: self.special_no,
            file_path: self.file_path,
            file_name: self.file_name,
            size: self.size,
            container: self.container,
            declared_video_codec: self.declared_video_codec,
            detected_video_codec: self.detected_video_codec,
            normalized_video_codec: self.normalized_video_codec,
            resolution: self.resolution,
            bit_depth: self.bit_depth,
            audio_codecs: parse_json(&self.audio_codecs_json, "媒体音轨")?,
            subtitle_tracks: parse_json(&self.subtitle_tracks_json, "媒体字幕轨")?,
            duration_seconds: self.duration_seconds,
            downloaded_at: self.downloaded_at,
            probed_at: self.probed_at,
            origin: parse_media_origin(&self.origin)?,
            source_root: self.source_root,
            fingerprint: self.fingerprint,
            file_modified_at: self.file_modified_at,
            availability: parse_media_availability(&self.availability)?,
            last_verified_at: self.last_verified_at,
            availability_error: self.availability_error,
        })
    }
}

struct NotificationRow {
    id: String,
    kind: String,
    title: String,
    body: String,
    severity: String,
    anime_id: Option<String>,
    episode_id: Option<String>,
    download_task_id: Option<String>,
    created_at: String,
    read_at: Option<String>,
}

impl NotificationRow {
    /// 将 SQLite 通知行转换为领域对象。
    fn into_domain(self) -> Result<NotificationRecord, StorageError> {
        Ok(NotificationRecord {
            id: self.id,
            kind: parse_notification_kind(&self.kind)?,
            title: self.title,
            body: self.body,
            severity: parse_notification_severity(&self.severity)?,
            anime_id: self.anime_id,
            episode_id: self.episode_id,
            download_task_id: self.download_task_id,
            created_at: self.created_at,
            read_at: self.read_at,
        })
    }
}

/// 映射 SQLite 番剧别名行。
fn map_alias_row(row: &Row<'_>) -> rusqlite::Result<AnimeAliasRow> {
    Ok(AnimeAliasRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        alias: row.get("alias")?,
        language: row.get("language")?,
        priority: row.get("priority")?,
    })
}

/// 映射 SQLite 番剧目录行。
fn map_anime_row(row: &Row<'_>) -> rusqlite::Result<AnimeRow> {
    Ok(AnimeRow {
        id: row.get("id")?,
        title: row.get("title")?,
        original_title: row.get("original_title")?,
        premiere_date: row.get("premiere_date")?,
        premiere_year: row.get("premiere_year")?,
        premiere_month: row.get("premiere_month")?,
        season: row.get("season")?,
        summary: row.get("summary")?,
        cover_url: row.get("cover_url")?,
        rating_score: row.get("rating_score")?,
        rating_count: row.get("rating_count")?,
        rating_source: row.get("rating_source")?,
        external_ids_json: row.get("external_ids_json")?,
        detail_json: row.get("detail_json")?,
    })
}

/// 映射 SQLite 字幕组行。
fn map_fansub_group_row(row: &Row<'_>) -> rusqlite::Result<FansubGroupRow> {
    Ok(FansubGroupRow {
        id: row.get("id")?,
        name: row.get("name")?,
        aliases_json: row.get("aliases_json")?,
        source_ids_json: row.get("source_ids_json")?,
    })
}

/// 映射 SQLite 原始资源缓存行。
fn map_cached_release_row(row: &Row<'_>) -> rusqlite::Result<CachedReleaseRow> {
    Ok(CachedReleaseRow {
        id: row.get("id")?,
        title: row.get("title")?,
        anime_id: row.get("anime_id")?,
        episode_no: row.get("episode_no")?,
        fansub_group_id: row.get("fansub_group_id")?,
        source_id: row.get("source_id")?,
        source_name: row.get("source_name")?,
        magnet_url: row.get("magnet_url")?,
        torrent_url: row.get("torrent_url")?,
        info_hash: row.get("info_hash")?,
        size: row.get("size")?,
        resolution: row.get("resolution")?,
        declared_video_codec: row.get("declared_video_codec")?,
        normalized_video_codec: row.get("normalized_video_codec")?,
        bit_depth: row.get("bit_depth")?,
        subtitle: row.get("subtitle")?,
        subtitle_languages_json: row.get("subtitle_languages_json")?,
        published_at: row.get("published_at")?,
        seeders: row.get("seeders")?,
        raw_json: row.get("raw_json")?,
    })
}

/// 映射 SQLite 来源绑定行。
fn map_anime_source_binding_row(row: &Row<'_>) -> rusqlite::Result<AnimeSourceBindingRow> {
    Ok(AnimeSourceBindingRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        source_id: row.get("source_id")?,
        source_anime_id: row.get("source_anime_id")?,
        source_anime_title: row.get("source_anime_title")?,
        source_url: row.get("source_url")?,
        match_method: row.get("match_method")?,
        confidence: row.get("confidence")?,
        confirmed: row.get::<_, i64>("confirmed")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 映射 SQLite 来源排除行。
fn map_anime_source_exclusion_row(row: &Row<'_>) -> rusqlite::Result<AnimeSourceExclusionRow> {
    Ok(AnimeSourceExclusionRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        source_id: row.get("source_id")?,
        scope: row.get("scope")?,
        source_anime_id: row.get("source_anime_id")?,
        source_anime_title: row.get("source_anime_title")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 映射 SQLite 下载源行。
fn map_release_source_row(row: &Row<'_>) -> rusqlite::Result<ReleaseSourceRow> {
    Ok(ReleaseSourceRow {
        id: row.get("id")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        use_proxy: row.get::<_, i64>("use_proxy")? != 0,
        request_interval_ms: row.get("request_interval_ms")?,
        base_url: row.get("base_url")?,
        api_key: row.get("api_key")?,
        rss_url: row.get("rss_url")?,
        tags_json: row.get("tags_json")?,
    })
}

/// 映射 SQLite 来源同步游标。
fn map_source_sync_state_row(row: &Row<'_>) -> rusqlite::Result<ReleaseSourceSyncState> {
    Ok(ReleaseSourceSyncState {
        source_id: row.get("source_id")?,
        request_host: row.get("request_host")?,
        last_request_at: row.get("last_request_at")?,
        request_failure_count: row.get("request_failure_count")?,
        backoff_until: row.get("backoff_until")?,
        last_sync_attempt_at: row.get("last_sync_attempt_at")?,
        last_successful_sync_at: row.get("last_successful_sync_at")?,
        last_sync_error: row.get("last_sync_error")?,
        etag: row.get("etag")?,
        last_modified: row.get("last_modified")?,
    })
}

/// 映射 SQLite 通用网络熔断状态。
fn map_request_circuit_state_row(row: &Row<'_>) -> rusqlite::Result<RequestCircuitState> {
    Ok(RequestCircuitState {
        key: row.get("circuit_key")?,
        group: row.get("circuit_group")?,
        request_host: row.get("request_host")?,
        last_request_at: row.get("last_request_at")?,
        failure_count: row.get("failure_count")?,
        backoff_until: row.get("backoff_until")?,
        network_context: row.get("network_context")?,
    })
}

/// 映射 SQLite 新番季度同步状态。
fn map_anime_season_sync_state_row(row: &Row<'_>) -> rusqlite::Result<AnimeSeasonSyncState> {
    Ok(AnimeSeasonSyncState {
        year: row.get("year")?,
        season: row.get("season")?,
        last_attempt_at: row.get("last_attempt_at")?,
        last_successful_sync_at: row.get("last_successful_sync_at")?,
        completed_at: row.get("completed_at")?,
        last_anilist_error: row.get("last_anilist_error")?,
    })
}

/// 映射 SQLite 来源级番剧详情刷新状态。
fn map_anime_detail_refresh_state_row(row: &Row<'_>) -> rusqlite::Result<AnimeDetailRefreshState> {
    Ok(AnimeDetailRefreshState {
        anime_id: row.get("anime_id")?,
        provider: row.get("provider")?,
        external_id: row.get("external_id")?,
        slot_day: row.get("slot_day")?,
        last_completed_cycle: row.get("last_completed_cycle")?,
        last_attempt_at: row.get("last_attempt_at")?,
        last_success_at: row.get("last_success_at")?,
        failure_count: row.get("failure_count")?,
        next_retry_at: row.get("next_retry_at")?,
    })
}

/// 映射 SQLite 追番行。
fn map_my_anime_row(row: &Row<'_>) -> rusqlite::Result<MyAnimeRow> {
    Ok(MyAnimeRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        status: row.get("status")?,
        default_fansub_group_id: row.get("default_fansub_group_id")?,
        auto_download: row.get::<_, i64>("auto_download")? != 0,
        download_dir: row.get("download_dir")?,
        preferred_resolution: row.get("preferred_resolution")?,
        preferred_codec: row.get("preferred_codec")?,
        preferred_subtitle: row.get("preferred_subtitle")?,
        preferred_subtitle_languages_json: row.get("preferred_subtitle_languages_json")?,
        preferred_bit_depth: row.get("preferred_bit_depth")?,
        added_at: row.get("added_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 映射 SQLite RSS 订阅行。
fn map_rss_subscription_row(row: &Row<'_>) -> rusqlite::Result<RssSubscriptionRow> {
    Ok(RssSubscriptionRow {
        id: row.get("id")?,
        my_anime_id: row.get("my_anime_id")?,
        name: row.get("name")?,
        url: row.get("url")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        preferred_subtitle: row.get("preferred_subtitle")?,
        preferred_subtitle_languages_json: row.get("preferred_subtitle_languages_json")?,
        refresh_interval_minutes: row.get("refresh_interval_minutes")?,
        last_fetched_at: row.get("last_fetched_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 映射 SQLite 单集行。
fn map_episode_row(row: &Row<'_>) -> rusqlite::Result<EpisodeRow> {
    Ok(EpisodeRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        episode_no: row.get("episode_no")?,
        title: row.get("title")?,
        air_time: row.get("air_time")?,
        status: row.get("status")?,
    })
}

/// 映射 SQLite 种子文件行。
fn map_torrent_file_row(row: &Row<'_>) -> rusqlite::Result<TorrentFileRow> {
    Ok(TorrentFileRow {
        download_task_id: row.get("download_task_id")?,
        id: row.get("id")?,
        index: row.get("file_index")?,
        name: row.get("name")?,
        episode_id: row.get("episode_id")?,
        episode_no: row.get("episode_no")?,
        size: row.get("size")?,
        progress: row.get("progress")?,
        priority: row.get("priority")?,
        selected: row.get::<_, i64>("selected")? != 0,
    })
}

/// 映射 SQLite 下载任务行。
fn map_download_row(row: &Row<'_>) -> rusqlite::Result<DownloadRow> {
    Ok(DownloadRow {
        id: row.get("id")?,
        release_id: row.get("release_id")?,
        anime_id: row.get("anime_id")?,
        episode_id: row.get("episode_id")?,
        anime_title: row.get("anime_title")?,
        episode_no: row.get("episode_no")?,
        fansub_group_id: row.get("fansub_group_id")?,
        fansub_name: row.get("fansub_name")?,
        resolution: row.get("resolution")?,
        declared_video_codec: row.get("declared_video_codec")?,
        normalized_video_codec: row.get("normalized_video_codec")?,
        bit_depth: row.get("bit_depth")?,
        subtitle_languages_json: row.get("subtitle_languages_json")?,
        subtitle: row.get("subtitle")?,
        correlation_tag: row.get("correlation_tag")?,
        engine: row.get("engine")?,
        torrent_hash: row.get("torrent_hash")?,
        name: row.get("name")?,
        status: row.get("status")?,
        progress: row.get("progress")?,
        download_speed: row.get("download_speed")?,
        upload_speed: row.get("upload_speed")?,
        eta_seconds: row.get("eta_seconds")?,
        save_path: row.get("save_path")?,
        created_at: row.get("created_at")?,
        completed_at: row.get("completed_at")?,
    })
}

/// 映射 SQLite 媒体文件行。
fn map_media_file_row(row: &Row<'_>) -> rusqlite::Result<MediaFileRow> {
    Ok(MediaFileRow {
        id: row.get("id")?,
        anime_id: row.get("anime_id")?,
        episode_id: row.get("episode_id")?,
        download_task_id: row.get("download_task_id")?,
        content_kind: row.get("content_kind")?,
        special_no: row.get("special_no")?,
        file_path: row.get("file_path")?,
        file_name: row.get("file_name")?,
        size: row.get("size")?,
        container: row.get("container")?,
        declared_video_codec: row.get("declared_video_codec")?,
        detected_video_codec: row.get("detected_video_codec")?,
        normalized_video_codec: row.get("normalized_video_codec")?,
        resolution: row.get("resolution")?,
        bit_depth: row.get("bit_depth")?,
        audio_codecs_json: row.get("audio_codecs_json")?,
        subtitle_tracks_json: row.get("subtitle_tracks_json")?,
        duration_seconds: row.get("duration_seconds")?,
        downloaded_at: row.get("downloaded_at")?,
        probed_at: row.get("probed_at")?,
        origin: row.get("origin")?,
        source_root: row.get("source_root")?,
        fingerprint: row.get("fingerprint")?,
        file_modified_at: row.get("file_modified_at")?,
        availability: row.get("availability")?,
        last_verified_at: row.get("last_verified_at")?,
        availability_error: row.get("availability_error")?,
    })
}

/// 映射 SQLite 通知行。
fn map_notification_row(row: &Row<'_>) -> rusqlite::Result<NotificationRow> {
    Ok(NotificationRow {
        id: row.get("id")?,
        kind: row.get("kind")?,
        title: row.get("title")?,
        body: row.get("body")?,
        severity: row.get("severity")?,
        anime_id: row.get("anime_id")?,
        episode_id: row.get("episode_id")?,
        download_task_id: row.get("download_task_id")?,
        created_at: row.get("created_at")?,
        read_at: row.get("read_at")?,
    })
}

/// 解析番剧别名语言。
fn parse_alias_language(value: &str) -> Result<AnimeAliasLanguage, StorageError> {
    match value {
        "zh" => Ok(AnimeAliasLanguage::Zh),
        "ja" => Ok(AnimeAliasLanguage::Ja),
        "en" => Ok(AnimeAliasLanguage::En),
        "romaji" => Ok(AnimeAliasLanguage::Romaji),
        "custom" => Ok(AnimeAliasLanguage::Custom),
        _ => invalid_value("anime_alias.language", value),
    }
}

/// 解析追番状态。
fn parse_anime_status(value: &str) -> Result<AnimeStatus, StorageError> {
    match value {
        "watching" => Ok(AnimeStatus::Watching),
        "planned" => Ok(AnimeStatus::Planned),
        "completed" => Ok(AnimeStatus::Completed),
        "paused" => Ok(AnimeStatus::Paused),
        "dropped" => Ok(AnimeStatus::Dropped),
        _ => invalid_value("my_anime.status", value),
    }
}

/// 解析单集状态。
fn parse_episode_status(value: &str) -> Result<EpisodeStatus, StorageError> {
    match value {
        "upcoming" => Ok(EpisodeStatus::Upcoming),
        "aired" => Ok(EpisodeStatus::Aired),
        "matched" => Ok(EpisodeStatus::Matched),
        "downloading" => Ok(EpisodeStatus::Downloading),
        "downloaded" => Ok(EpisodeStatus::Downloaded),
        "watched" => Ok(EpisodeStatus::Watched),
        _ => invalid_value("episode.status", value),
    }
}

/// 解析下载状态。
fn parse_download_status(value: &str) -> Result<DownloadStatus, StorageError> {
    match value {
        "queued" => Ok(DownloadStatus::Queued),
        "fetching_metadata" => Ok(DownloadStatus::FetchingMetadata),
        "downloading" => Ok(DownloadStatus::Downloading),
        "stalled" => Ok(DownloadStatus::Stalled),
        "waiting_network" => Ok(DownloadStatus::WaitingNetwork),
        "paused" => Ok(DownloadStatus::Paused),
        "checking" => Ok(DownloadStatus::Checking),
        "moving" => Ok(DownloadStatus::Moving),
        "completed" => Ok(DownloadStatus::Completed),
        "seeding" => Ok(DownloadStatus::Seeding),
        "error" => Ok(DownloadStatus::Error),
        "missing_files" => Ok(DownloadStatus::MissingFiles),
        _ => invalid_value("download_task.status", value),
    }
}

/// 解析下载引擎类型。
fn parse_torrent_engine(value: &str) -> Result<TorrentEngineKind, StorageError> {
    match value {
        "embedded" => Ok(TorrentEngineKind::Embedded),
        "qbittorrent" => Ok(TorrentEngineKind::Qbittorrent),
        _ => invalid_value("download_task.engine", value),
    }
}

/// 解析媒体登记来源。
fn parse_media_origin(value: &str) -> Result<MediaOrigin, StorageError> {
    match value {
        "download" => Ok(MediaOrigin::Download),
        "imported" => Ok(MediaOrigin::Imported),
        _ => invalid_value("media_file.origin", value),
    }
}

/// 返回媒体登记来源的 SQLite 字面量。
fn media_origin_value(value: &MediaOrigin) -> &'static str {
    match value {
        MediaOrigin::Download => "download",
        MediaOrigin::Imported => "imported",
    }
}

/// 解析媒体内容类型。
fn parse_media_content_kind(value: &str) -> Result<MediaContentKind, StorageError> {
    match value {
        "episode" => Ok(MediaContentKind::Episode),
        "special" => Ok(MediaContentKind::Special),
        "ova" => Ok(MediaContentKind::Ova),
        "oad" => Ok(MediaContentKind::Oad),
        "opening" => Ok(MediaContentKind::Opening),
        "ending" => Ok(MediaContentKind::Ending),
        "pv" => Ok(MediaContentKind::Pv),
        "cm" => Ok(MediaContentKind::Cm),
        "extra" => Ok(MediaContentKind::Extra),
        "unknown" => Ok(MediaContentKind::Unknown),
        _ => invalid_value("media_file.content_kind", value),
    }
}

/// 返回媒体内容类型的 SQLite 字面量。
fn media_content_kind_value(value: &MediaContentKind) -> &'static str {
    match value {
        MediaContentKind::Episode => "episode",
        MediaContentKind::Special => "special",
        MediaContentKind::Ova => "ova",
        MediaContentKind::Oad => "oad",
        MediaContentKind::Opening => "opening",
        MediaContentKind::Ending => "ending",
        MediaContentKind::Pv => "pv",
        MediaContentKind::Cm => "cm",
        MediaContentKind::Extra => "extra",
        MediaContentKind::Unknown => "unknown",
    }
}

/// 解析媒体文件当前可用状态。
fn parse_media_availability(value: &str) -> Result<MediaAvailability, StorageError> {
    match value {
        "available" => Ok(MediaAvailability::Available),
        "changed" => Ok(MediaAvailability::Changed),
        "missing" => Ok(MediaAvailability::Missing),
        "unavailable" => Ok(MediaAvailability::Unavailable),
        _ => invalid_value("media_file.availability", value),
    }
}

/// 返回媒体可用状态的 SQLite 字面量。
fn media_availability_value(value: &MediaAvailability) -> &'static str {
    match value {
        MediaAvailability::Available => "available",
        MediaAvailability::Changed => "changed",
        MediaAvailability::Missing => "missing",
        MediaAvailability::Unavailable => "unavailable",
    }
}

/// 解析通知类别。
fn parse_notification_kind(value: &str) -> Result<NotificationKind, StorageError> {
    match value {
        "automation" => Ok(NotificationKind::Automation),
        "download" => Ok(NotificationKind::Download),
        "reminder" => Ok(NotificationKind::Reminder),
        "system" => Ok(NotificationKind::System),
        _ => invalid_value("notification.kind", value),
    }
}

/// 解析通知严重程度。
fn parse_notification_severity(value: &str) -> Result<NotificationSeverity, StorageError> {
    match value {
        "info" => Ok(NotificationSeverity::Info),
        "success" => Ok(NotificationSeverity::Success),
        "warning" => Ok(NotificationSeverity::Warning),
        "error" => Ok(NotificationSeverity::Error),
        _ => invalid_value("notification.severity", value),
    }
}

/// 返回通知类别的 SQLite 字面量。
fn notification_kind_value(value: &NotificationKind) -> &'static str {
    match value {
        NotificationKind::Automation => "automation",
        NotificationKind::Download => "download",
        NotificationKind::Reminder => "reminder",
        NotificationKind::System => "system",
    }
}

/// 返回通知严重程度的 SQLite 字面量。
fn notification_severity_value(value: &NotificationSeverity) -> &'static str {
    match value {
        NotificationSeverity::Info => "info",
        NotificationSeverity::Success => "success",
        NotificationSeverity::Warning => "warning",
        NotificationSeverity::Error => "error",
    }
}

/// 创建统一的非法领域字段错误。
fn invalid_value<T>(field: &'static str, value: &str) -> Result<T, StorageError> {
    Err(StorageError::InvalidDomainValue {
        field,
        value: value.to_owned(),
    })
}
