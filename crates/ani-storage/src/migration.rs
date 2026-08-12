use ani_domain::TorrentEngineKind;
use log::info;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    now_iso, ReleaseSourceSeed, StorageError, StorageSeed, APP_DATA_VERSION, SQLITE_SCHEMA_VERSION,
};

const CURRENT_SCHEMA: &str = include_str!("schema_v23.sql");
const MAX_RELEASE_ID_BYTES: usize = 200;

/// 数据库中记录的结构和应用数据版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatabaseVersions {
    pub schema_version: Option<u32>,
    pub app_data_version: Option<u32>,
}

/// 读取数据库版本；空库返回两个空值。
pub(crate) fn read_database_versions(
    connection: &Connection,
) -> Result<DatabaseVersions, StorageError> {
    if !table_exists(connection, "app_meta")? {
        return Ok(DatabaseVersions {
            schema_version: None,
            app_data_version: None,
        });
    }

    Ok(DatabaseVersions {
        schema_version: read_version(connection, "schema_version")?,
        app_data_version: read_version(connection, "app_data_version")?,
    })
}

/// 在单个立即事务中完成结构迁移、业务数据迁移和首次 seed。
pub(crate) fn initialize_database(
    connection: &mut Connection,
    seed: &StorageSeed,
) -> Result<(), StorageError> {
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    let versions = read_database_versions(connection)?;
    validate_supported_versions(versions)?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(CURRENT_SCHEMA)?;
    ensure_legacy_columns(&transaction)?;
    ensure_current_indexes(&transaction)?;
    migrate_schema_data(&transaction, versions.schema_version.unwrap_or(0))?;

    match versions.app_data_version {
        None => seed_database(&transaction, seed)?,
        Some(version) if version < APP_DATA_VERSION => migrate_app_data(&transaction, version)?,
        Some(_) => {}
    }

    set_meta(
        &transaction,
        "schema_version",
        &SQLITE_SCHEMA_VERSION.to_string(),
    )?;
    set_meta(
        &transaction,
        "app_data_version",
        &APP_DATA_VERSION.to_string(),
    )?;
    transaction.commit()?;
    Ok(())
}

/// 拒绝由更高版本应用创建的数据库，避免破坏未知结构。
fn validate_supported_versions(versions: DatabaseVersions) -> Result<(), StorageError> {
    if let Some(actual) = versions
        .schema_version
        .filter(|version| *version > SQLITE_SCHEMA_VERSION)
    {
        return Err(StorageError::UnsupportedSchemaVersion {
            actual,
            supported: SQLITE_SCHEMA_VERSION,
        });
    }
    if let Some(actual) = versions
        .app_data_version
        .filter(|version| *version > APP_DATA_VERSION)
    {
        return Err(StorageError::UnsupportedAppDataVersion {
            actual,
            supported: APP_DATA_VERSION,
        });
    }
    Ok(())
}

/// 补齐历史数据库中版本号与真实列不一致的情况。
fn ensure_legacy_columns(transaction: &Transaction<'_>) -> Result<(), StorageError> {
    for (table, column, definition) in [
        ("anime_catalog", "rating_score", "rating_score REAL"),
        ("anime_catalog", "rating_count", "rating_count INTEGER"),
        ("anime_catalog", "rating_source", "rating_source TEXT"),
        (
            "anime_catalog",
            "detail_json",
            "detail_json TEXT NOT NULL DEFAULT '{}'",
        ),
        (
            "my_anime",
            "preferred_subtitle_languages_json",
            "preferred_subtitle_languages_json TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "my_anime",
            "preferred_bit_depth",
            "preferred_bit_depth INTEGER",
        ),
        (
            "my_anime_rss_subscription",
            "preferred_subtitle",
            "preferred_subtitle TEXT",
        ),
        (
            "my_anime_rss_subscription",
            "preferred_subtitle_languages_json",
            "preferred_subtitle_languages_json TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "my_anime_rss_subscription",
            "refresh_interval_minutes",
            "refresh_interval_minutes INTEGER",
        ),
        (
            "my_anime_rss_subscription",
            "last_fetched_at",
            "last_fetched_at TEXT",
        ),
        ("release", "bit_depth", "bit_depth INTEGER"),
        (
            "release",
            "subtitle_languages_json",
            "subtitle_languages_json TEXT NOT NULL DEFAULT '[]'",
        ),
        ("download_task", "resolution", "resolution TEXT"),
        (
            "download_task",
            "declared_video_codec",
            "declared_video_codec TEXT",
        ),
        (
            "download_task",
            "normalized_video_codec",
            "normalized_video_codec TEXT",
        ),
        ("download_task", "bit_depth", "bit_depth INTEGER"),
        (
            "download_task",
            "subtitle_languages_json",
            "subtitle_languages_json TEXT NOT NULL DEFAULT '[]'",
        ),
        ("download_task", "subtitle", "subtitle TEXT"),
        (
            "release_source",
            "use_proxy",
            "use_proxy INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "release_source",
            "request_interval_ms",
            "request_interval_ms INTEGER NOT NULL DEFAULT 1000",
        ),
        (
            "release_source_sync_state",
            "request_host",
            "request_host TEXT",
        ),
        (
            "request_circuit_state",
            "network_context",
            "network_context TEXT",
        ),
        ("torrent_file", "episode_id", "episode_id TEXT"),
        ("torrent_file", "episode_no", "episode_no REAL"),
        (
            "media_file",
            "origin",
            "origin TEXT NOT NULL DEFAULT 'download'",
        ),
        ("media_file", "source_root", "source_root TEXT"),
        ("media_file", "fingerprint", "fingerprint TEXT"),
        ("media_file", "file_modified_at", "file_modified_at TEXT"),
        (
            "media_file",
            "availability",
            "availability TEXT NOT NULL DEFAULT 'available'",
        ),
        ("media_file", "last_verified_at", "last_verified_at TEXT"),
        (
            "media_file",
            "availability_error",
            "availability_error TEXT",
        ),
        (
            "media_file",
            "content_kind",
            "content_kind TEXT NOT NULL DEFAULT 'unknown'",
        ),
        ("media_file", "special_no", "special_no TEXT"),
    ] {
        ensure_column(transaction, table, column, definition)?;
    }
    Ok(())
}

/// 在依赖列补齐后创建当前版本索引，兼容历史表结构。
fn ensure_current_indexes(transaction: &Transaction<'_>) -> Result<(), StorageError> {
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_media_file_source_root \
         ON media_file (source_root, availability);",
    )?;
    Ok(())
}

/// 执行现有 TypeScript Repository 已发布的结构数据迁移。
fn migrate_schema_data(
    transaction: &Transaction<'_>,
    current_schema_version: u32,
) -> Result<(), StorageError> {
    if current_schema_version < 5 {
        transaction.execute_batch(
            r#"
            INSERT OR IGNORE INTO anime_fansub_group
              (anime_id, fansub_group_id, first_seen_at, last_seen_at)
            SELECT anime_id, default_fansub_group_id, updated_at, updated_at
            FROM my_anime WHERE default_fansub_group_id IS NOT NULL;

            INSERT OR IGNORE INTO anime_fansub_group
              (anime_id, fansub_group_id, first_seen_at, last_seen_at)
            SELECT anime_id, fansub_group_id, updated_at, updated_at
            FROM episode_preference WHERE fansub_group_id IS NOT NULL;

            INSERT OR IGNORE INTO anime_fansub_group
              (anime_id, fansub_group_id, first_seen_at, last_seen_at)
            SELECT anime_id, fansub_group_id, created_at, updated_at
            FROM download_task
            WHERE anime_id IS NOT NULL AND fansub_group_id IS NOT NULL
              AND fansub_group_id IN (SELECT id FROM fansub_group);
            "#,
        )?;
    }

    if current_schema_version < 8 {
        transaction.execute_batch(
            r#"
            UPDATE my_anime SET preferred_subtitle_languages_json =
              CASE preferred_subtitle WHEN 'chs' THEN '["chs"]' WHEN 'cht' THEN '["cht"]'
                WHEN 'jpn' THEN '["jpn"]' WHEN 'eng' THEN '["eng"]'
                WHEN 'multi' THEN '["chs","cht"]' ELSE '[]' END
            WHERE preferred_subtitle_languages_json = '[]' AND preferred_subtitle IS NOT NULL;

            UPDATE my_anime_rss_subscription SET preferred_subtitle_languages_json =
              CASE preferred_subtitle WHEN 'chs' THEN '["chs"]' WHEN 'cht' THEN '["cht"]'
                WHEN 'jpn' THEN '["jpn"]' WHEN 'eng' THEN '["eng"]'
                WHEN 'multi' THEN '["chs","cht"]' ELSE '[]' END
            WHERE preferred_subtitle_languages_json = '[]' AND preferred_subtitle IS NOT NULL;

            UPDATE release SET subtitle_languages_json =
              CASE subtitle WHEN 'chs' THEN '["chs"]' WHEN 'cht' THEN '["cht"]'
                WHEN 'jpn' THEN '["jpn"]' WHEN 'eng' THEN '["eng"]'
                WHEN 'multi' THEN '["chs","cht"]' ELSE '[]' END
            WHERE subtitle_languages_json = '[]' AND subtitle IS NOT NULL;
            "#,
        )?;
    }

    if current_schema_version < 10 {
        transaction.execute_batch(
            r#"
            UPDATE release_source SET use_proxy = 1, request_interval_ms = 1500
            WHERE id IN ('mikan', 'dmhy', 'mikan-site', 'anibt', 'acgnx');
            UPDATE release_source SET use_proxy = 0, request_interval_ms = 250
            WHERE id = 'prowlarr';
            "#,
        )?;
    }

    if current_schema_version < 14 {
        transaction.execute_batch(
            r#"
            INSERT OR IGNORE INTO request_circuit_state
              (circuit_key, circuit_group, request_host, last_request_at, failure_count, backoff_until, updated_at)
            SELECT 'release-source:' || source_id, 'release-source', request_host, last_request_at,
              request_failure_count, backoff_until, updated_at
            FROM release_source_sync_state
            WHERE request_host IS NOT NULL OR last_request_at IS NOT NULL
              OR request_failure_count > 0 OR backoff_until IS NOT NULL;

            UPDATE release_source_sync_state SET request_host = NULL, last_request_at = NULL,
              request_failure_count = 0, backoff_until = NULL;
            "#,
        )?;
    }

    if current_schema_version < 15 {
        transaction.execute("DELETE FROM release WHERE anime_id IS NOT NULL", [])?;
        transaction.execute("DELETE FROM release_search_cache", [])?;
    }
    if current_schema_version < 22 {
        let updated = transaction.execute(
            "UPDATE media_file SET content_kind = 'episode' \
             WHERE episode_id IS NOT NULL AND content_kind = 'unknown'",
            [],
        )?;
        info!("SQLite 媒体内容类型迁移完成：episode_count={updated}");
    }
    if current_schema_version < 23 {
        let cleared = transaction.execute("DELETE FROM request_circuit_state", [])?;
        info!("SQLite 旧版熔断状态迁移完成：cleared={cleared}");
    }
    Ok(())
}

/// 按应用数据版本顺序执行幂等业务数据迁移。
fn migrate_app_data(
    transaction: &Transaction<'_>,
    current_app_data_version: u32,
) -> Result<(), StorageError> {
    if current_app_data_version < 22 {
        transaction.execute("DELETE FROM release_source WHERE id = 'prowlarr'", [])?;
        transaction.execute(
            "DELETE FROM request_circuit_state WHERE circuit_key = 'release-source:prowlarr'",
            [],
        )?;
        transaction.execute("DELETE FROM release_search_cache", [])?;
        transaction.execute(
            "UPDATE release_source SET use_proxy = 0, updated_at = ?1 WHERE id = 'anibt'",
            [now_iso()],
        )?;
    }
    if current_app_data_version < 23 {
        let stats = migrate_historical_release_ids(transaction)?;
        info!(
            "SQLite 历史资源标识迁移完成：references={}, releases={}, removed_releases={}, cleared_cache={}",
            stats.updated_references,
            stats.updated_releases,
            stats.removed_releases,
            stats.cleared_cache
        );
    }
    if current_app_data_version < 24 {
        let stats = migrate_download_task_engine_ids(transaction)?;
        info!(
            "SQLite 下载任务引擎身份迁移完成：tasks={}, references={}",
            stats.updated_tasks, stats.updated_references
        );
    }
    if current_app_data_version < 25 && migrate_metadata_proxy_timeout_default(transaction)? {
        info!("SQLite 元数据请求超时默认值迁移完成：15000ms -> 30000ms");
    }
    Ok(())
}

/// 将旧版默认超时升级到 30 秒，同时保留用户主动设置的其他值。
fn migrate_metadata_proxy_timeout_default(
    transaction: &Transaction<'_>,
) -> Result<bool, StorageError> {
    let Some(settings_json) = transaction
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'settings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let mut settings: Value =
        serde_json::from_str(&settings_json).map_err(|source| StorageError::JsonData {
            context: "元数据请求超时迁移",
            source,
        })?;
    let Some(timeout) = settings.pointer_mut("/network/metadataProxy/timeoutMs") else {
        return Ok(false);
    };
    if timeout.as_u64() != Some(15_000) {
        return Ok(false);
    }
    *timeout = Value::from(30_000);
    transaction.execute(
        "UPDATE app_settings SET value_json = ?1, updated_at = ?2 WHERE key = 'settings'",
        params![settings.to_string(), now_iso()],
    )?;
    Ok(true)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DownloadTaskIdMigrationStats {
    updated_tasks: usize,
    updated_references: usize,
}

/// 为历史任务和关联记录补充下载引擎命名空间。
fn migrate_download_task_engine_ids(
    transaction: &Transaction<'_>,
) -> Result<DownloadTaskIdMigrationStats, StorageError> {
    let tasks = {
        let mut statement = transaction.prepare("SELECT id, engine FROM download_task")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    transaction.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
    let mut stats = DownloadTaskIdMigrationStats::default();

    for (legacy_id, engine_value) in tasks {
        let engine = match engine_value.as_str() {
            "embedded" => TorrentEngineKind::Embedded,
            "qbittorrent" => TorrentEngineKind::Qbittorrent,
            _ => {
                return Err(StorageError::InvalidDomainValue {
                    field: "download_task.engine",
                    value: engine_value,
                });
            }
        };
        let scoped_id = engine.scope_task_id(&legacy_id);
        if scoped_id == legacy_id {
            continue;
        }

        stats.updated_tasks += transaction.execute(
            "UPDATE download_task SET id = ?1 WHERE id = ?2",
            params![&scoped_id, &legacy_id],
        )?;
        stats.updated_references += transaction.execute(
            "UPDATE torrent_file
             SET id = ?1 || ':' || CAST(file_index AS TEXT), download_task_id = ?1
             WHERE download_task_id = ?2",
            params![&scoped_id, &legacy_id],
        )?;
        for sql in [
            "UPDATE media_file SET download_task_id = ?1 WHERE download_task_id = ?2",
            "UPDATE playback_checkpoint SET task_id = ?1 WHERE task_id = ?2",
            "UPDATE notification SET download_task_id = ?1 WHERE download_task_id = ?2",
        ] {
            stats.updated_references +=
                transaction.execute(sql, params![&scoped_id, &legacy_id])?;
        }
    }
    Ok(stats)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReleaseIdMigrationStats {
    updated_references: usize,
    updated_releases: usize,
    removed_releases: usize,
    cleared_cache: usize,
}

/// 修复历史空值和超长资源标识，并保持下载任务、单集偏好与资源记录的关联。
fn migrate_historical_release_ids(
    transaction: &Transaction<'_>,
) -> Result<ReleaseIdMigrationStats, StorageError> {
    let legacy_ids = {
        let mut statement = transaction.prepare(
            "SELECT release_id FROM download_task WHERE release_id IS NOT NULL
             UNION SELECT release_id FROM episode_preference WHERE release_id IS NOT NULL
             UNION SELECT id FROM release",
        )?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|value| value.trim().is_empty() || value.len() > MAX_RELEASE_ID_BYTES)
            .collect::<Vec<_>>();
        values
    };
    let source_ids = {
        let mut statement = transaction.prepare("SELECT id FROM release_source")?;
        let mut values = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values
    };
    let mut stats = ReleaseIdMigrationStats::default();

    for legacy_id in legacy_ids {
        let source_hint = transaction
            .query_row(
                "SELECT source_id FROM release WHERE id = ?1",
                [&legacy_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let replacement = repair_release_id(&legacy_id, source_hint.as_deref(), &source_ids);

        stats.updated_references += transaction.execute(
            "UPDATE download_task SET release_id = ?1 WHERE release_id = ?2",
            params![replacement.as_deref(), &legacy_id],
        )?;
        stats.updated_references += transaction.execute(
            "UPDATE episode_preference SET release_id = ?1 WHERE release_id = ?2",
            params![replacement.as_deref(), &legacy_id],
        )?;

        let Some(replacement) = replacement else {
            stats.removed_releases +=
                transaction.execute("DELETE FROM release WHERE id = ?1", [&legacy_id])?;
            continue;
        };
        let replacement_exists = transaction
            .query_row(
                "SELECT 1 FROM release WHERE id = ?1",
                [&replacement],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if replacement_exists {
            stats.removed_releases +=
                transaction.execute("DELETE FROM release WHERE id = ?1", [&legacy_id])?;
        } else {
            stats.updated_releases += transaction.execute(
                "UPDATE release SET id = ?1 WHERE id = ?2",
                params![&replacement, &legacy_id],
            )?;
        }
    }

    stats.cleared_cache = transaction.execute("DELETE FROM release_search_cache", [])?;
    Ok(stats)
}

/// 以来源原始身份生成与解析器一致的稳定 ID；无法还原来源时仍保证固定长度。
fn repair_release_id(
    legacy_id: &str,
    source_hint: Option<&str>,
    source_ids: &[String],
) -> Option<String> {
    if legacy_id.trim().is_empty() {
        return None;
    }
    if legacy_id.len() <= MAX_RELEASE_ID_BYTES {
        return Some(legacy_id.to_owned());
    }

    let source_and_identity = source_hint
        .and_then(|source_id| strip_release_source_prefix(legacy_id, source_id))
        .or_else(|| {
            source_ids
                .iter()
                .find_map(|source_id| strip_release_source_prefix(legacy_id, source_id))
        });
    let mut digest = Sha256::new();
    if let Some((source_id, identity)) = source_and_identity {
        digest.update(source_id.as_bytes());
        digest.update([0]);
        digest.update(identity.as_bytes());
    } else {
        digest.update(b"legacy-release-id");
        digest.update([0]);
        digest.update(legacy_id.as_bytes());
    }
    Some(format!("release:{:x}", digest.finalize()))
}

/// 从旧版 `sourceId:identity` 标识中剥离精确来源前缀。
fn strip_release_source_prefix<'legacy, 'source>(
    legacy_id: &'legacy str,
    source_id: &'source str,
) -> Option<(&'source str, &'legacy str)> {
    let identity = legacy_id.strip_prefix(source_id)?.strip_prefix(':')?;
    (!source_id.trim().is_empty() && !identity.trim().is_empty()).then_some((source_id, identity))
}

/// 空库首次启动时写入设置、首页空状态和默认下载源。
fn seed_database(transaction: &Transaction<'_>, seed: &StorageSeed) -> Result<(), StorageError> {
    let timestamp = now_iso();
    transaction.execute(
        "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('settings', ?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
        params![seed.settings.to_string(), &timestamp],
    )?;
    transaction.execute(
        "INSERT INTO app_state (key, value_json, updated_at) VALUES ('dashboard', ?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
        params![seed.dashboard.to_string(), &timestamp],
    )?;
    for source in &seed.release_sources {
        seed_release_source(transaction, source, &timestamp)?;
    }
    Ok(())
}

/// 写入一条默认下载源，保留旧库中已有的用户配置。
fn seed_release_source(
    transaction: &Transaction<'_>,
    source: &ReleaseSourceSeed,
    timestamp: &str,
) -> Result<(), StorageError> {
    let tags_json = serde_json::to_string(&source.tags).expect("string vector must serialize");
    transaction.execute(
        "INSERT OR IGNORE INTO release_source ( \
           id, name, kind, enabled, use_proxy, request_interval_ms, base_url, api_key, rss_url, \
           tags_json, created_at, updated_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            &source.id,
            &source.name,
            &source.kind,
            if source.enabled { 1_i64 } else { 0_i64 },
            if source.use_proxy { 1_i64 } else { 0_i64 },
            source.request_interval_ms,
            source.base_url.as_deref(),
            source.api_key.as_deref(),
            source.rss_url.as_deref(),
            tags_json,
            timestamp,
        ],
    )?;
    Ok(())
}

/// 幂等追加旧数据库缺失的列。
fn ensure_column(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StorageError> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|existing| existing == column) {
        transaction.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition};"))?;
    }
    Ok(())
}

/// 判断指定表是否存在。
fn table_exists(connection: &Connection, table: &str) -> Result<bool, StorageError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// 从 app_meta 读取并严格解析版本号。
fn read_version(connection: &Connection, key: &'static str) -> Result<Option<u32>, StorageError> {
    let value = connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    value
        .map(|raw| {
            raw.parse::<u32>()
                .map_err(|_| StorageError::InvalidVersionMetadata { key, value: raw })
        })
        .transpose()
}

/// 原子写入一项版本元数据。
fn set_meta(transaction: &Transaction<'_>, key: &str, value: &str) -> Result<(), StorageError> {
    transaction.execute(
        "INSERT INTO app_meta (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now_iso()],
    )?;
    Ok(())
}
