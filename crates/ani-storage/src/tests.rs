use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ani_domain::{
    AnimeAliasLanguage, AnimeDetailRefreshState, AnimeSeasonSyncState, AnimeSourceBinding,
    AnimeSourceBindingMatchMethod, AnimeSourceExclusion, AnimeSourceExclusionScope, AnimeStatus,
    DownloadStatus, DownloadTask, Episode, EpisodePreference, EpisodeStatus, MediaFile, MyAnime,
    NotificationKind, NotificationRecord, NotificationSeverity, PlaybackCheckpoint,
    ReleaseSearchResult, ReleaseSourceConfig, ReleaseSourceSyncState, ReportPlaybackProgressInput,
    RequestCircuitState, SavePlaybackCheckpointInput, SecretReference, SecretValue, SecureStore,
    SetAnimeWatchProgressInput, TorrentEngineKind, TorrentFile,
};
use ani_repository::{
    AnimeCatalogRepository, AnimeSourceBindingRepository, CachedReleaseQuery, DownloadRepository,
    MediaRepository, NotificationRepository, ReleaseCacheRepository, ReleaseSearchCacheEntry,
    ReleaseSourceRepository, RepositoryError, UnitOfWork, UnitOfWorkFactory,
};
use rusqlite::{params, Connection, OpenFlags};
use serde::Deserialize;
use serde_json::json;

use crate::{
    ReleaseSourceSeed, SecureStoreError, Storage, StorageError, StorageOptions, StorageSeed,
    APP_DATA_VERSION, SQLITE_SCHEMA_VERSION,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 验证 90% 完成边界以及完成状态不可被后续低进度覆盖。
#[test]
fn playback_checkpoint_completion_is_threshold_based_and_monotonic() {
    let directory = TestDirectory::new("checkpoint-completion");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("checkpoint database must initialize");
    let task_id = "checkpoint-threshold-task";
    storage
        .connection
        .execute(
            "INSERT INTO download_task (
               id, engine, name, status, progress, download_speed, upload_speed,
               save_path, created_at, updated_at
             ) VALUES (?1, 'embedded', 'checkpoint threshold', 'completed', 1, 0, 0,
               'C:/video', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z')",
            [task_id],
        )
        .expect("insert checkpoint download task");
    let repository = storage.repository();

    let below_threshold = repository
        .save_playback_checkpoint(&SavePlaybackCheckpointInput {
            task_id: task_id.to_owned(),
            file_index: Some(0),
            position_seconds: 89.99,
            duration_seconds: 100.0,
            completed: Some(false),
        })
        .expect("save checkpoint below threshold");
    assert!(!below_threshold.completed);

    let at_threshold = repository
        .save_playback_checkpoint(&SavePlaybackCheckpointInput {
            task_id: task_id.to_owned(),
            file_index: Some(0),
            position_seconds: 90.0,
            duration_seconds: 100.0,
            completed: Some(false),
        })
        .expect("save checkpoint at threshold");
    assert!(at_threshold.completed);

    let rewound = repository
        .save_playback_checkpoint(&SavePlaybackCheckpointInput {
            task_id: task_id.to_owned(),
            file_index: Some(0),
            position_seconds: 10.0,
            duration_seconds: 100.0,
            completed: Some(false),
        })
        .expect("save rewound checkpoint");
    assert!(rewound.completed);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContractFixture<T> {
    schema_version: u32,
    kind: String,
    payload: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct P3FollowingWriteModelFixture {
    my_anime: MyAnime,
    episode: Episode,
    preference: EpisodePreference,
    watch_progress_input: SetAnimeWatchProgressInput,
    report_playback_progress_input: ReportPlaybackProgressInput,
    save_playback_checkpoint_input: SavePlaybackCheckpointInput,
    checkpoint: PlaybackCheckpoint,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct P3SourceNetworkModelFixture {
    source: ReleaseSourceConfig,
    sync_state: ReleaseSourceSyncState,
    circuit_state: RequestCircuitState,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct P3SourceBindingModelFixture {
    binding: AnimeSourceBinding,
    exclusion: AnimeSourceExclusion,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct P3ReleaseSearchModelFixture {
    search_result: ReleaseSearchResult,
}

/// 验证新库在单次启动中完成建表、seed 和版本写入。
#[test]
fn initializes_new_database_with_seed() {
    let directory = TestDirectory::new("new");
    let options = test_options(&directory, "active.sqlite");
    let storage = Storage::open(options).expect("new database must initialize");

    assert!(storage.report().created);
    assert_eq!(storage.report().schema_version, SQLITE_SCHEMA_VERSION);
    assert_eq!(storage.report().app_data_version, APP_DATA_VERSION);
    assert_eq!(
        read_meta(&storage.connection, "schema_version"),
        SQLITE_SCHEMA_VERSION.to_string()
    );
    assert_eq!(read_meta(&storage.connection, "app_data_version"), "25");
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = 'settings'",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("seeded settings"),
        r#"{"appearance":{"mode":"system"}}"#
    );
    assert_eq!(
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM release_source", [], |row| row
                .get::<_, i64>(0))
            .expect("source count"),
        1
    );
    storage.verify().expect("new database integrity");
}

/// 验证设置补丁可持久化，同时不能覆盖宿主拥有的存储路径。
#[test]
fn updates_and_resets_settings_through_repository() {
    let directory = TestDirectory::new("settings-write");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open settings write database");
    let defaults = json!({
        "appearance": { "mode": "system" },
        "network": { "metadataProxy": { "mode": "system", "timeoutMs": 30000 } },
        "storage": { "databasePath": "host-owned.sqlite" }
    });
    let updated = storage
        .repository()
        .update_settings(
            &json!({
                "network": { "metadataProxy": { "mode": "manual", "url": "http://127.0.0.1:7890" } },
                "storage": { "databasePath": "untrusted.sqlite" }
            }),
            &defaults,
        )
        .expect("update settings");

    assert_eq!(updated["network"]["metadataProxy"]["mode"], "manual");
    assert_eq!(updated["storage"]["databasePath"], "host-owned.sqlite");
    let reset = storage
        .repository()
        .reset_settings(&defaults)
        .expect("reset settings");
    assert_eq!(reset, defaults);
}

/// 验证敏感设置和来源 API Key 仅在 SQLite 保存安全引用。
#[test]
fn stores_sensitive_fields_through_secure_store() {
    let directory = TestDirectory::new("secure-store");
    let mut storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open secure store database");
    let secure_store = Arc::new(MemorySecureStore::default());
    storage.set_secure_store(secure_store.clone());
    let defaults = json!({
        "download": { "qbittorrent": { "password": "" } },
        "appearance": { "mode": "system" }
    });
    let settings = storage
        .repository()
        .update_settings(
            &json!({ "download": { "qbittorrent": { "password": "qbit-secret" } } }),
            &defaults,
        )
        .expect("save secure settings");
    assert_eq!(
        settings["download"]["qbittorrent"]["password"],
        "qbit-secret"
    );
    let raw_settings: String = storage
        .connection
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'settings'",
            [],
            |row| row.get(0),
        )
        .expect("read raw settings");
    assert!(!raw_settings.contains("qbit-secret"));
    assert!(raw_settings.contains("secure-store:v1:settings.download.qbittorrent.password"));

    let mut source = storage
        .repository()
        .list_sources()
        .expect("list seeded sources")
        .remove(0);
    source.api_key = Some("source-secret".to_owned());
    let sources = storage
        .repository()
        .upsert_source(&source)
        .expect("save secure source");
    assert_eq!(sources[0].api_key.as_deref(), Some("source-secret"));
    let raw_api_key: String = storage
        .connection
        .query_row(
            "SELECT api_key FROM release_source WHERE id = 'anibt'",
            [],
            |row| row.get(0),
        )
        .expect("read raw source api key");
    assert_eq!(raw_api_key, "secure-store:v1:sources.anibt.api-key");
    assert_eq!(
        secure_store.read_text("settings.download.qbittorrent.password"),
        Some("qbit-secret".to_owned())
    );
    assert_eq!(
        secure_store.read_text("sources.anibt.api-key"),
        Some("source-secret".to_owned())
    );
}

/// 验证下载源采集间隔保存后重新打开数据库仍保持新值。
#[test]
fn persists_source_request_interval_across_restart() {
    let directory = TestDirectory::new("source-request-interval");
    {
        let storage = Storage::open(test_options(&directory, "active.sqlite"))
            .expect("open source interval database");
        let mut source = storage
            .repository()
            .list_sources()
            .expect("list seeded sources")
            .remove(0);
        source.request_interval_ms = 800;
        let saved = storage
            .repository()
            .upsert_source(&source)
            .expect("save source request interval");
        assert_eq!(saved[0].request_interval_ms, 800);
    }

    let reopened = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("reopen source interval database");
    assert_eq!(
        reopened
            .repository()
            .list_sources()
            .expect("list reopened sources")[0]
            .request_interval_ms,
        800
    );
}

/// 验证旧库升级前保留一致性备份，并执行结构与应用数据迁移。
#[test]
fn backs_up_and_migrates_legacy_versions() {
    let directory = TestDirectory::new("migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open legacy database");
    legacy
        .execute_batch(
            "ALTER TABLE anime_catalog DROP COLUMN detail_json;
             UPDATE app_meta SET value = '12' WHERE key = 'schema_version';
             UPDATE app_meta SET value = '21' WHERE key = 'app_data_version';",
        )
        .expect("downgrade schema fixture");
    insert_source(&legacy, "prowlarr", true);
    insert_source(&legacy, "anibt", true);
    drop(legacy);

    let storage = Storage::open(options).expect("legacy database must migrate");
    let backup_path = storage
        .report()
        .backup_path
        .clone()
        .expect("migration backup path");
    assert!(backup_path.is_file());
    assert!(column_exists(
        &storage.connection,
        "anime_catalog",
        "detail_json"
    ));
    assert_eq!(
        read_meta(&storage.connection, "schema_version"),
        SQLITE_SCHEMA_VERSION.to_string()
    );
    assert_eq!(read_meta(&storage.connection, "app_data_version"), "25");
    assert_eq!(source_count(&storage.connection, "prowlarr"), 0);
    assert_eq!(source_proxy(&storage.connection, "anibt"), 0);
    storage.verify().expect("migrated database integrity");

    let backup = open_read_only(&backup_path);
    assert_eq!(read_meta(&backup, "schema_version"), "12");
    assert_eq!(read_meta(&backup, "app_data_version"), "21");
    assert!(!column_exists(&backup, "anime_catalog", "detail_json"));
    assert_eq!(source_count(&backup, "prowlarr"), 1);
}

/// 验证版本 20 的媒体表会先补齐可用性字段，再创建目录索引。
#[test]
fn migrates_v20_media_availability_schema() {
    let directory = TestDirectory::new("media-availability-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open media migration fixture");
    legacy
        .execute_batch(
            "ALTER TABLE media_file RENAME TO media_file_v21;
             CREATE TABLE media_file (
               id TEXT PRIMARY KEY,
               anime_id TEXT NOT NULL,
               episode_id TEXT,
               download_task_id TEXT,
               file_path TEXT NOT NULL,
               file_name TEXT NOT NULL,
               size INTEGER NOT NULL,
               container TEXT,
               declared_video_codec TEXT,
               detected_video_codec TEXT,
               normalized_video_codec TEXT NOT NULL,
               resolution TEXT,
               bit_depth INTEGER,
               audio_codecs_json TEXT NOT NULL DEFAULT '[]',
               subtitle_tracks_json TEXT NOT NULL DEFAULT '[]',
               duration_seconds INTEGER,
               downloaded_at TEXT,
               probed_at TEXT
             );
             INSERT INTO media_file (
               id, anime_id, file_path, file_name, size, normalized_video_codec
             ) VALUES (
               'legacy-media', 'legacy-anime', '/media/legacy.mkv', 'legacy.mkv',
               1024, 'Unknown'
             );
             DROP TABLE media_file_v21;
             UPDATE app_meta SET value = '20' WHERE key = 'schema_version';",
        )
        .expect("downgrade media table to v20");
    drop(legacy);

    let storage = Storage::open(options).expect("v20 media table must migrate");
    for column in [
        "origin",
        "source_root",
        "fingerprint",
        "file_modified_at",
        "availability",
        "last_verified_at",
        "availability_error",
        "content_kind",
        "special_no",
    ] {
        assert!(column_exists(&storage.connection, "media_file", column));
    }
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT origin || ':' || availability FROM media_file WHERE id = 'legacy-media'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read migrated media defaults"),
        "download:available"
    );
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_media_file_source_root'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("read media source index"),
        1
    );
}

/// 验证版本 21 的单集媒体回填为正片，未关联媒体保持未知。
#[test]
fn migrates_v21_media_content_kind() {
    let directory = TestDirectory::new("media-content-kind-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open media content fixture");
    legacy
        .execute_batch(
            "INSERT INTO media_file (
               id, anime_id, episode_id, file_path, file_name, size, normalized_video_codec
             ) VALUES
               ('legacy-episode', 'anime-1', 'episode-1', '/media/e01.mkv', 'e01.mkv', 1, 'Unknown'),
               ('legacy-unknown', 'anime-1', NULL, '/media/unknown.mkv', 'unknown.mkv', 1, 'Unknown');
             UPDATE app_meta SET value = '21' WHERE key = 'schema_version';",
        )
        .expect("prepare v21 media content fixture");
    drop(legacy);

    let storage = Storage::open(options).expect("v21 media content must migrate");
    let kinds = storage
        .connection
        .prepare("SELECT id, content_kind FROM media_file ORDER BY id")
        .expect("prepare media kind query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query media kinds")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect media kinds");

    assert_eq!(
        kinds,
        vec![
            ("legacy-episode".to_owned(), "episode".to_owned()),
            ("legacy-unknown".to_owned(), "unknown".to_owned()),
        ]
    );
}

/// 验证版本 23 会修复历史超长资源标识，并保留任务与单集偏好的稳定关联。
#[test]
fn migrates_historical_oversized_release_ids() {
    let directory = TestDirectory::new("oversized-release-id-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy_id = format!("anibt:{}", "oversized-identity-".repeat(16));
    let legacy = Connection::open(&database_path).expect("open oversized release fixture");
    legacy
        .execute(
            "UPDATE app_meta SET value = '22' WHERE key = 'app_data_version'",
            [],
        )
        .expect("downgrade app data version");
    legacy
        .execute_batch(
            "INSERT INTO anime_catalog (
               id, title, premiere_year, premiere_month, created_at, updated_at
             ) VALUES (
               'anime-release-id', '资源标识迁移番', 2026, 7,
               '2026-07-26T00:00:00.000Z', '2026-07-26T00:00:00.000Z'
             );
             INSERT INTO episode (
               id, anime_id, episode_no, status, created_at, updated_at
             ) VALUES (
               'episode-release-id', 'anime-release-id', 1, 'aired',
               '2026-07-26T00:00:00.000Z', '2026-07-26T00:00:00.000Z'
             );",
        )
        .expect("insert release relation fixture");
    legacy
        .execute(
            "INSERT INTO release (
               id, title, source_id, source_name, published_at
             ) VALUES (?1, '超长标识资源', 'anibt', 'AniBT', '2026-07-26T00:00:00.000Z')",
            [&legacy_id],
        )
        .expect("insert oversized release");
    legacy
        .execute(
            "INSERT INTO episode_preference (
               id, anime_id, episode_id, release_id, updated_at
             ) VALUES (
               'preference-release-id', 'anime-release-id', 'episode-release-id', ?1,
               '2026-07-26T00:00:00.000Z'
             )",
            [&legacy_id],
        )
        .expect("insert oversized episode preference");
    legacy
        .execute(
            "INSERT INTO download_task (
               id, release_id, engine, name, status, progress, download_speed, upload_speed,
               save_path, created_at, updated_at
             ) VALUES (
               'download-release-id', ?1, 'embedded', '超长标识任务', 'completed', 1, 0, 0,
               'C:/video', '2026-07-26T00:00:00.000Z', '2026-07-26T00:00:00.000Z'
             )",
            [&legacy_id],
        )
        .expect("insert oversized download task");
    legacy
        .execute_batch(
            "INSERT INTO download_task (
               id, release_id, engine, name, status, progress, download_speed, upload_speed,
               save_path, created_at, updated_at
             ) VALUES (
               'download-empty-release-id', '   ', 'embedded', '空标识任务', 'paused', 0, 0, 0,
               'C:/video', '2026-07-26T00:00:00.000Z', '2026-07-26T00:00:00.000Z'
             );
             INSERT INTO release_search_cache (cache_key, result_json, expires_at, updated_at)
             VALUES (
               'legacy-cache', '[]', '2026-07-27T00:00:00.000Z', '2026-07-26T00:00:00.000Z'
             );",
        )
        .expect("insert invalid release references and cache");
    drop(legacy);

    let storage = Storage::open(options).expect("oversized release ids must migrate");
    let migrated_task_id = storage
        .connection
        .query_row(
            "SELECT release_id FROM download_task WHERE id = 'embedded:download-release-id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read migrated task release id");
    let migrated_preference_id = storage
        .connection
        .query_row(
            "SELECT release_id FROM episode_preference WHERE id = 'preference-release-id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("read migrated preference release id");

    assert_eq!(read_meta(&storage.connection, "app_data_version"), "25");
    assert_eq!(migrated_task_id, migrated_preference_id);
    assert!(migrated_task_id.starts_with("release:"));
    assert!(migrated_task_id.len() <= 200);
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT release_id FROM download_task WHERE id = 'embedded:download-empty-release-id'",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("read cleared empty release id"),
        None
    );
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM release WHERE id = ?1",
                [&migrated_task_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count migrated release"),
        1
    );
    assert_eq!(
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM release_search_cache", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count cleared release cache"),
        0
    );

    let migrated_task = DownloadRepository::list_downloads(&storage.repository())
        .expect("list migrated downloads")
        .into_iter()
        .find(|task| task.id == "embedded:download-release-id")
        .expect("migrated download task");
    DownloadRepository::upsert_download_task(&storage.repository(), &migrated_task)
        .expect("migrated task must pass write validation");
}

/// 验证版本 24 为下载任务及全部关联记录补充引擎命名空间。
#[test]
fn migrates_download_task_engine_identity() {
    let directory = TestDirectory::new("download-engine-id-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open download migration fixture");
    legacy
        .execute(
            "UPDATE app_meta SET value = '23' WHERE key = 'app_data_version'",
            [],
        )
        .expect("downgrade app data version");
    legacy
        .execute_batch(
            "INSERT INTO download_task (
               id, engine, torrent_hash, name, status, progress, download_speed, upload_speed,
               save_path, created_at, updated_at
             ) VALUES (
               'shared-hash', 'qbittorrent', 'shared-hash', '引擎迁移任务', 'paused',
               0.5, 0, 0, 'C:/video', '2026-07-27T00:00:00.000Z',
               '2026-07-27T00:00:00.000Z'
             );
             INSERT INTO torrent_file (
               id, download_task_id, file_index, name, size, progress, priority, selected
             ) VALUES ('shared-hash:0', 'shared-hash', 0, 'episode.mkv', 1024, 0.5, 1, 1);
             INSERT INTO media_file (
               id, anime_id, download_task_id, file_path, file_name, size,
               normalized_video_codec, audio_codecs_json, subtitle_tracks_json
             ) VALUES (
               'engine-media', 'engine-anime', 'shared-hash', 'C:/video/episode.mkv',
               'episode.mkv', 1024, 'H.265/HEVC', '[]', '[]'
             );
             INSERT INTO playback_checkpoint (
               task_id, file_index, position_seconds, duration_seconds, completed,
               watched_reported, updated_at
             ) VALUES ('shared-hash', 0, 10, 100, 0, 0, '2026-07-27T00:00:00.000Z');
             INSERT INTO notification (
               id, kind, title, body, severity, download_task_id, created_at
             ) VALUES (
               'engine-notification', 'download', '下载任务', '等待恢复', 'info',
               'shared-hash', '2026-07-27T00:00:00.000Z'
             );",
        )
        .expect("insert legacy download relations");
    drop(legacy);

    let storage = Storage::open(options).expect("download identities must migrate");
    let scoped_id = "qbittorrent:shared-hash";
    assert_eq!(read_meta(&storage.connection, "app_data_version"), "25");
    for (table, column) in [
        ("download_task", "id"),
        ("torrent_file", "download_task_id"),
        ("media_file", "download_task_id"),
        ("playback_checkpoint", "task_id"),
        ("notification", "download_task_id"),
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
        assert_eq!(
            storage
                .connection
                .query_row(&sql, [scoped_id], |row| row.get::<_, i64>(0))
                .expect("count migrated download relation"),
            1,
            "{table}.{column} must reference scoped task id"
        );
    }
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT id FROM torrent_file WHERE download_task_id = ?1",
                [scoped_id],
                |row| row.get::<_, String>(0),
            )
            .expect("read migrated torrent file id"),
        "qbittorrent:shared-hash:0"
    );
    storage.verify().expect("migrated database integrity");
}

/// 验证版本 25 将旧版默认元数据超时升级为 30 秒。
#[test]
fn migrates_legacy_metadata_proxy_timeout_default() {
    let directory = TestDirectory::new("metadata-timeout-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open metadata timeout fixture");
    legacy
        .execute(
            "UPDATE app_meta SET value = '24' WHERE key = 'app_data_version'",
            [],
        )
        .expect("downgrade app data version");
    legacy
        .execute(
            "UPDATE app_settings SET value_json = ?1 WHERE key = 'settings'",
            [json!({
                "network": { "metadataProxy": { "mode": "system", "timeoutMs": 15_000 } }
            })
            .to_string()],
        )
        .expect("write legacy metadata timeout");
    drop(legacy);

    let storage = Storage::open(options).expect("metadata timeout must migrate");
    let settings: serde_json::Value = storage
        .connection
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'settings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| serde_json::from_str(&value).expect("parse migrated settings"))
        .expect("read migrated settings");
    assert_eq!(settings["network"]["metadataProxy"]["timeoutMs"], 30_000);
    assert_eq!(read_meta(&storage.connection, "app_data_version"), "25");
}

/// 验证版本 25 不覆盖用户主动配置的元数据超时。
#[test]
fn preserves_custom_metadata_proxy_timeout_during_migration() {
    let directory = TestDirectory::new("custom-metadata-timeout-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open custom timeout fixture");
    legacy
        .execute(
            "UPDATE app_meta SET value = '24' WHERE key = 'app_data_version'",
            [],
        )
        .expect("downgrade app data version");
    legacy
        .execute(
            "UPDATE app_settings SET value_json = ?1 WHERE key = 'settings'",
            [json!({
                "network": { "metadataProxy": { "mode": "manual", "timeoutMs": 23_000 } }
            })
            .to_string()],
        )
        .expect("write custom metadata timeout");
    drop(legacy);

    let storage = Storage::open(options).expect("custom timeout migration must succeed");
    let settings: serde_json::Value = storage
        .connection
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'settings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| serde_json::from_str(&value).expect("parse preserved settings"))
        .expect("read preserved settings");
    assert_eq!(settings["network"]["metadataProxy"]["timeoutMs"], 23_000);
}

/// 验证 Tauri 首启只复制 Electron 数据库，不修改或删除源文件。
#[test]
fn copies_legacy_database_without_modifying_source() {
    let directory = TestDirectory::new("copy");
    let source_options = test_options(&directory, "electron.sqlite");
    let source_path = source_options.database_path.clone();
    drop(Storage::open(source_options).expect("create electron database"));

    let target_path = directory.path().join("tauri").join("ani-tracker.sqlite");
    let options = StorageOptions {
        database_path: target_path.clone(),
        backup_directory: directory.path().join("backups"),
        legacy_database_paths: vec![source_path.clone()],
        seed: StorageSeed::default(),
    };
    let storage = Storage::open(options).expect("copy legacy database");

    assert_eq!(
        storage.report().copied_from.as_deref(),
        Some(source_path.as_path())
    );
    assert!(storage
        .report()
        .backup_path
        .as_ref()
        .is_some_and(|path| path.is_file()));
    assert!(source_path.is_file());
    assert!(target_path.is_file());
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = 'settings'",
                [],
                |row| row.get::<_, String>(0)
            )
            .expect("copied settings"),
        r#"{"appearance":{"mode":"system"}}"#
    );
    open_read_only(&source_path)
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map(|result| assert_eq!(result, "ok"))
        .expect("source database remains valid");
}

/// 验证手动备份包含 WAL 数据，并可恢复为当前活动连接。
#[test]
fn exports_and_restores_manual_backup() {
    let directory = TestDirectory::new("manual-backup");
    let mut storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("open active database");
    let defaults = json!({ "appearance": { "mode": "system" } });
    storage
        .repository()
        .update_settings(&json!({ "appearance": { "mode": "dark" } }), &defaults)
        .expect("write backup settings");
    let backup = storage
        .create_manual_backup()
        .expect("create manual backup");

    storage
        .repository()
        .update_settings(&json!({ "appearance": { "mode": "light" } }), &defaults)
        .expect("write active settings");
    let rollback = storage
        .restore_from(&backup)
        .expect("restore manual backup");

    assert!(rollback.is_file());
    assert_eq!(
        storage
            .repository()
            .get_settings(&defaults)
            .expect("read restored settings")["appearance"]["mode"],
        "dark"
    );
    let rollback_storage = Storage::open(StorageOptions {
        database_path: rollback,
        backup_directory: directory.path().join("rollback-backups"),
        legacy_database_paths: Vec::new(),
        seed: StorageSeed::default(),
    })
    .expect("open rollback snapshot");
    assert_eq!(
        rollback_storage
            .repository()
            .get_settings(&defaults)
            .expect("read rollback settings")["appearance"]["mode"],
        "light"
    );
}

/// 验证导出快照可独立打开，并拒绝当前活动数据库作为目标或来源。
#[test]
fn exports_consistent_snapshot_and_rejects_active_database_path() {
    let directory = TestDirectory::new("manual-export");
    let mut storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("open active database");
    let exported = directory.path().join("exported.sqlite");
    storage.export_to(&exported).expect("export database");
    Storage::open(StorageOptions {
        database_path: exported,
        backup_directory: directory.path().join("export-backups"),
        legacy_database_paths: Vec::new(),
        seed: StorageSeed::default(),
    })
    .expect("open exported database")
    .verify()
    .expect("verify exported database");

    assert!(matches!(
        storage.export_to(storage.database_path()),
        Err(StorageError::InvalidInput {
            field: "backupPath",
            ..
        })
    ));
    let active_path = storage.database_path().to_path_buf();
    assert!(matches!(
        storage.restore_from(&active_path),
        Err(StorageError::InvalidInput {
            field: "backupPath",
            ..
        })
    ));
}

/// 验证损坏备份在写入活动连接前被拒绝，现有数据保持不变。
#[test]
fn rejects_corrupt_manual_restore_without_mutating_active_database() {
    let directory = TestDirectory::new("manual-restore-corrupt");
    let mut storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("open active database");
    let corrupt = directory.path().join("corrupt.sqlite");
    fs::write(&corrupt, b"not-a-sqlite-database").expect("write corrupt backup");

    assert!(storage.restore_from(&corrupt).is_err());
    storage.verify().expect("active database remains valid");
    assert_eq!(
        read_meta(&storage.connection, "schema_version"),
        SQLITE_SCHEMA_VERSION.to_string()
    );
}

/// 验证迁移事务失败后恢复原始版本和表结构。
#[test]
fn restores_backup_when_migration_fails() {
    let directory = TestDirectory::new("rollback");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open migration failure fixture");
    legacy
        .execute_batch(
            "ALTER TABLE torrent_file DROP COLUMN episode_no;
             UPDATE app_meta SET value = '17' WHERE key = 'schema_version';
             CREATE TRIGGER reject_schema_version_update
             BEFORE UPDATE OF value ON app_meta
             WHEN OLD.key = 'schema_version'
             BEGIN SELECT RAISE(ABORT, 'forced migration failure'); END;",
        )
        .expect("prepare migration failure fixture");
    drop(legacy);

    let error = Storage::open(options)
        .err()
        .expect("migration must fail and return an error");
    assert!(matches!(error, StorageError::MigrationRolledBack { .. }));

    let restored = open_read_only(&database_path);
    assert_eq!(read_meta(&restored, "schema_version"), "17");
    assert!(!column_exists(&restored, "torrent_file", "episode_no"));
    assert_eq!(
        restored
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'reject_schema_version_update'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("restored trigger"),
        1
    );
}

/// 验证损坏库阻止启动，且不会被 seed 覆盖。
#[test]
fn rejects_corrupt_database_without_overwrite() {
    let directory = TestDirectory::new("corrupt");
    let options = test_options(&directory, "active.sqlite");
    let corrupt_bytes = b"not a sqlite database";
    fs::write(&options.database_path, corrupt_bytes).expect("write corrupt fixture");

    let error = Storage::open(options.clone())
        .err()
        .expect("corrupt database must fail");
    assert!(matches!(error, StorageError::CorruptDatabase { .. }));
    assert_eq!(
        fs::read(&options.database_path).expect("read untouched corrupt fixture"),
        corrupt_bytes
    );
    assert_eq!(
        fs::read_dir(&options.backup_directory)
            .expect("read backup directory")
            .count(),
        0
    );
}

/// 验证 P2 首批设置、通知、追番和首页查询使用同一 SQLite 数据源。
#[test]
fn reads_p2_business_views_from_sqlite() {
    let directory = TestDirectory::new("p2-views");
    let options = test_options(&directory, "active.sqlite");
    let storage = Storage::open(options).expect("create p2 view database");
    insert_p2_read_model_fixture(&storage.connection);
    let repository = storage.repository();

    let settings = repository
        .get_settings(&json!({
            "appearance": { "mode": "system", "themePackId": "default" },
            "network": { "metadataProxy": { "mode": "off", "timeoutMs": 30000 } },
            "storage": { "databasePath": "C:/tauri/ani-tracker.sqlite", "cacheDir": "C:/tauri/cache" },
            "players": [
                { "id": "built-in", "name": "内置播放器", "executablePath": "", "argumentTemplate": "{file}" },
                { "id": "system", "name": "系统播放器", "executablePath": "", "argumentTemplate": "{file}" }
            ]
        }))
        .expect("read merged settings");
    assert_eq!(settings["appearance"]["mode"], "dark");
    assert_eq!(settings["appearance"]["themePackId"], "default");
    assert_eq!(settings["network"]["metadataProxy"]["timeoutMs"], 30_000);
    assert_eq!(
        settings["storage"]["databasePath"],
        "C:/tauri/ani-tracker.sqlite"
    );
    assert_eq!(settings["storage"]["cacheDir"], "C:/tauri/cache");
    assert_eq!(settings["players"][0]["executablePath"], "C:/VLC/vlc.exe");
    assert_eq!(settings["players"][0]["argumentTemplate"], "{file}");
    assert_eq!(settings["players"][1]["id"], "system");

    let notifications = repository.list_notifications().expect("list notifications");
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[0].id, "notification-new");
    assert_eq!(
        repository
            .get_unread_notification_count()
            .expect("unread count"),
        1
    );

    let followed = repository.list_my_anime().expect("list my anime");
    assert_eq!(followed.len(), 1);
    assert_eq!(followed[0].anime.title, "测试番剧");
    assert_eq!(followed[0].anime.aliases[0].alias, "测试别名");
    assert_eq!(followed[0].preferred_subtitle_languages, ["chs", "cht"]);
    assert_eq!(followed[0].rss_subscriptions.len(), 1);
    assert_eq!(
        followed[0].rss_subscriptions[0].preferred_subtitle_languages,
        ["cht"]
    );

    let dashboard = repository.get_dashboard().expect("read dashboard");
    assert_eq!(dashboard.daily_reminder.total, 3);
    assert_eq!(dashboard.daily_reminder.aired, 1);
    assert_eq!(dashboard.daily_reminder.downloading, 1);
    assert_eq!(dashboard.daily_reminder.downloaded, 1);
    assert_eq!(dashboard.today_episodes.len(), 3);
    assert_eq!(dashboard.pending_actions.len(), 1);
    assert_eq!(dashboard.pending_actions[0].episode_no, Some(1.0));
    assert_eq!(dashboard.active_downloads.len(), 1);
    assert_eq!(dashboard.active_downloads[0].id, "download-active");
    assert_eq!(dashboard.recent_completed.len(), 1);
    assert_eq!(dashboard.weekly_schedule[0].day, "周一");
    assert_eq!(dashboard.source_health[0].status, "warning");
}

/// 验证首页追番视图会使用已确认来源绑定的中文标题，但不写回目录别名。
#[test]
fn uses_confirmed_binding_chinese_title_for_followed_anime() {
    let directory = TestDirectory::new("confirmed-binding-title");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create confirmed binding title database");
    let anime_id = "bangumi-565701";
    let now = "2026-08-06T05:46:30.000Z";
    let japanese_title = "片田舎の剣聖、剣士になる 第二季";
    let chinese_title = "乡下大叔成为剑圣 第二季";
    storage
        .connection
        .execute(
            "INSERT INTO anime_catalog (
               id, title, original_title, premiere_year, premiere_month,
               external_ids_json, detail_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 2026, 7, '{\"bangumi\":\"565701\"}', '{}', ?4, ?4)",
            params![anime_id, japanese_title, japanese_title, now],
        )
        .expect("insert japanese anime");
    storage
        .connection
        .execute(
            "INSERT INTO my_anime (
               id, anime_id, status, auto_download, preferred_subtitle_languages_json,
               added_at, updated_at
             ) VALUES ('my-anime-confirmed-binding-title', ?1, 'watching', 0, '[]', ?2, ?2)",
            params![anime_id, now],
        )
        .expect("insert followed anime");

    let repository = storage.repository();
    let binding = AnimeSourceBinding {
        id: "source-binding:bangumi-565701:anibt".to_owned(),
        anime_id: anime_id.to_owned(),
        source_id: "anibt".to_owned(),
        source_anime_id: "565701".to_owned(),
        source_anime_title: Some(chinese_title.to_owned()),
        source_url: Some("https://anibt.example/subject/565701".to_owned()),
        match_method: AnimeSourceBindingMatchMethod::ExternalId,
        confidence: 1.0,
        confirmed: true,
        created_at: now.to_owned(),
        updated_at: now.to_owned(),
    };
    AnimeSourceBindingRepository::upsert_anime_source_binding(&repository, &binding)
        .expect("save confirmed source binding");

    let followed = repository.list_my_anime().expect("list followed anime");
    assert_eq!(followed.len(), 1);
    assert_eq!(followed[0].anime.title, japanese_title);
    assert_eq!(followed[0].anime.aliases.len(), 1);
    assert_eq!(followed[0].anime.aliases[0].alias, chinese_title);
    assert_eq!(
        followed[0].anime.aliases[0].language,
        AnimeAliasLanguage::Zh
    );
    assert_eq!(followed[0].anime.aliases[0].priority, 80);
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM anime_alias WHERE anime_id = ?1",
                [anime_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count persisted aliases"),
        0
    );

    let mut unconfirmed = binding;
    unconfirmed.confirmed = false;
    unconfirmed.updated_at = "2026-08-06T05:47:30.000Z".to_owned();
    AnimeSourceBindingRepository::upsert_anime_source_binding(&repository, &unconfirmed)
        .expect("cancel source binding confirmation");
    let followed = repository.list_my_anime().expect("list after cancellation");
    assert!(followed[0].anime.aliases.is_empty());
}

/// 验证 P3 追番、单集、偏好、观看进度和续播写入形成完整事务闭环。
#[test]
fn writes_p3_following_business_transactionally() {
    let directory = TestDirectory::new("p3-following");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create p3 following database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.kind, "p3-following-write-model");
    let repository = storage.repository();

    let followed = repository
        .upsert_my_anime(fixture.payload.my_anime.clone())
        .expect("save my anime");
    assert_eq!(followed.len(), 1);
    assert_eq!(followed[0].anime.aliases[0].id, "anime-p3-1-alias-1");
    assert_eq!(followed[0].rss_subscriptions.len(), 1);

    repository
        .upsert_episode(&fixture.payload.episode)
        .expect("save episode one");
    let episode_two = Episode {
        id: "episode-anime-p3-1-2".to_owned(),
        episode_no: 2.0,
        title: Some("第二集".to_owned()),
        ..fixture.payload.episode.clone()
    };
    repository
        .upsert_episode(&episode_two)
        .expect("save episode two");
    let preferences = repository
        .upsert_episode_preference(&fixture.payload.preference)
        .expect("save preference");
    assert_eq!(preferences.len(), 1);
    assert_eq!(preferences[0], fixture.payload.preference);

    let progress = repository
        .set_anime_watch_progress(&fixture.payload.watch_progress_input)
        .expect("set watch progress");
    assert_eq!(progress.watched_episode_count, 1);
    assert_eq!(progress.total_episode_count, 12);
    assert_eq!(
        repository
            .list_episodes("anime-p3-1")
            .expect("list episodes")[1]
            .status,
        ani_domain::EpisodeStatus::Aired
    );

    insert_p3_playback_download(&storage.connection);
    assert!(repository
        .report_playback_progress(&fixture.payload.report_playback_progress_input)
        .expect("report playback progress"));
    assert_eq!(
        repository
            .list_episodes("anime-p3-1")
            .expect("list watched episodes")[1]
            .status,
        ani_domain::EpisodeStatus::Watched
    );
    repository
        .upsert_episode(&episode_two)
        .expect("reset episode two for checkpoint assertion");
    let checkpoint = repository
        .save_playback_checkpoint(&fixture.payload.save_playback_checkpoint_input)
        .expect("save playback checkpoint");
    assert_eq!(checkpoint.task_id, fixture.payload.checkpoint.task_id);
    assert!(checkpoint.watched_reported);
    assert_eq!(
        repository
            .get_playback_checkpoint("download-p3-1", Some(0))
            .expect("read checkpoint")
            .expect("checkpoint exists")
            .position_seconds,
        1_380.0
    );

    assert!(repository
        .remove_episode_preference("episode-anime-p3-1-1")
        .expect("remove preference")
        .is_empty());
    let mut completed = fixture.payload.my_anime.clone();
    completed.status = AnimeStatus::Completed;
    completed.auto_download = true;
    assert!(
        !repository
            .upsert_my_anime(completed)
            .expect("save completed my anime")[0]
            .auto_download
    );
    assert!(repository
        .remove_my_anime("my-anime-p3-1")
        .expect("remove my anime")
        .is_empty());
    assert!(repository
        .list_episodes("anime-p3-1")
        .expect("episodes removed")
        .is_empty());
}

/// 验证追番复合写入失败时番剧目录和追番记录均不落库。
#[test]
fn rolls_back_failed_p3_following_write() {
    let directory = TestDirectory::new("p3-following-rollback");
    let storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("create rollback database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let mut invalid = fixture.payload.my_anime;
    invalid.default_fansub_group_id = Some("missing-fansub".to_owned());

    assert!(storage.repository().upsert_my_anime(invalid).is_err());
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM anime_catalog WHERE id = 'anime-p3-1'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("catalog rollback count"),
        0
    );
    assert_eq!(
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM my_anime", [], |row| row
                .get::<_, i64>(0))
            .expect("my anime rollback count"),
        0
    );
}

/// 验证下载任务和文件快照完整往返，并按文件进度同步关联单集。
#[test]
fn writes_download_snapshot_and_syncs_episode_statuses() {
    let directory = TestDirectory::new("p4-download-write");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create p4 download database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    repository
        .upsert_my_anime(fixture.payload.my_anime.clone())
        .expect("save p4 anime");
    repository
        .upsert_episode(&fixture.payload.episode)
        .expect("save p4 episode one");
    let episode_two = Episode {
        id: "episode-anime-p3-1-2".to_owned(),
        episode_no: 2.0,
        title: Some("第二集".to_owned()),
        ..fixture.payload.episode
    };
    repository
        .upsert_episode(&episode_two)
        .expect("save p4 episode two");

    let task = p4_download_task();
    let saved = DownloadRepository::upsert_download_task(&repository, &task)
        .expect("save p4 download snapshot");
    assert_eq!(saved, vec![task.clone()]);
    assert_eq!(
        repository
            .list_episodes("anime-p3-1")
            .expect("list synced episodes")
            .into_iter()
            .map(|episode| episode.status)
            .collect::<Vec<_>>(),
        vec![EpisodeStatus::Downloading, EpisodeStatus::Downloaded]
    );

    let mut completed = task.clone();
    completed.status = DownloadStatus::Completed;
    completed.progress = 1.0;
    completed.created_at = "2099-01-01T00:00:00.000Z".to_owned();
    completed.completed_at = Some("2026-07-25T01:00:00.000Z".to_owned());
    for file in &mut completed.files {
        file.progress = 1.0;
    }
    let completed_snapshot = DownloadRepository::upsert_download_task(&repository, &completed)
        .expect("complete p4 download snapshot");
    assert_eq!(completed_snapshot[0].created_at, task.created_at);
    assert_eq!(completed_snapshot[0].completed_at, completed.completed_at);
    assert!(repository
        .list_episodes("anime-p3-1")
        .expect("list completed episodes")
        .iter()
        .all(|episode| episode.status == EpisodeStatus::Downloaded));
}

/// 验证下载任务早于单集创建时会回填关联并同步完成状态。
#[test]
fn backfills_download_link_when_episode_is_created_later() {
    let directory = TestDirectory::new("p4-download-late-episode-link");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create late episode link database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    repository
        .upsert_my_anime(fixture.payload.my_anime.clone())
        .expect("save late-link anime");

    let mut task = p4_download_task();
    task.episode_id = None;
    task.episode_no = Some(fixture.payload.episode.episode_no);
    task.status = DownloadStatus::Completed;
    task.progress = 1.0;
    task.completed_at = Some("2026-07-25T01:00:00.000Z".to_owned());
    task.files.truncate(1);
    task.files[0].episode_id = None;
    task.files[0].episode_no = Some(fixture.payload.episode.episode_no);
    task.files[0].progress = 1.0;
    DownloadRepository::upsert_download_task(&repository, &task).expect("save task before episode");

    repository
        .upsert_episode(&fixture.payload.episode)
        .expect("save episode after task");

    let saved_task = DownloadRepository::list_downloads(&repository)
        .expect("list backfilled task")
        .into_iter()
        .find(|item| item.id == task.id)
        .expect("find backfilled task");
    assert_eq!(
        saved_task.episode_id.as_deref(),
        Some(fixture.payload.episode.id.as_str())
    );
    assert_eq!(
        saved_task.files[0].episode_id.as_deref(),
        Some(fixture.payload.episode.id.as_str())
    );
    assert_eq!(
        repository
            .list_episodes(&fixture.payload.episode.anime_id)
            .expect("list linked episode")[0]
            .status,
        EpisodeStatus::Downloaded
    );
}

/// 验证删除下载任务会恢复单集状态，且外键失败不会留下半条任务。
#[test]
fn removes_download_snapshot_and_rolls_back_invalid_files() {
    let directory = TestDirectory::new("p4-download-remove");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create p4 download removal database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    repository
        .upsert_my_anime(fixture.payload.my_anime.clone())
        .expect("save removal anime");
    repository
        .upsert_episode(&fixture.payload.episode)
        .expect("save removal episode one");
    repository
        .upsert_episode(&Episode {
            id: "episode-anime-p3-1-2".to_owned(),
            episode_no: 2.0,
            ..fixture.payload.episode
        })
        .expect("save removal episode two");
    let task = p4_download_task();
    DownloadRepository::upsert_download_task(&repository, &task).expect("save removable task");
    MediaRepository::upsert_media_files(
        &repository,
        &[MediaFile {
            id: "p4-removal-media".to_owned(),
            anime_id: "anime-p3-1".to_owned(),
            episode_id: Some("episode-anime-p3-1-1".to_owned()),
            download_task_id: Some(task.id.clone()),
            content_kind: ani_domain::MediaContentKind::Episode,
            special_no: None,
            file_path: "C:/video/episode-1.mkv".to_owned(),
            file_name: "episode-1.mkv".to_owned(),
            size: 1024,
            container: Some("mkv".to_owned()),
            declared_video_codec: Some("HEVC".to_owned()),
            detected_video_codec: Some("hevc".to_owned()),
            normalized_video_codec: "H.265/HEVC".to_owned(),
            resolution: Some("1920x1080".to_owned()),
            bit_depth: Some(10),
            audio_codecs: vec!["AAC".to_owned()],
            subtitle_tracks: vec!["chi / ASS".to_owned()],
            duration_seconds: Some(1440),
            downloaded_at: Some("2026-07-25T00:00:00.000Z".to_owned()),
            probed_at: Some("2026-07-25T00:01:00.000Z".to_owned()),
            origin: Default::default(),
            source_root: None,
            fingerprint: None,
            file_modified_at: None,
            availability: Default::default(),
            last_verified_at: None,
            availability_error: None,
        }],
    )
    .expect("save removable media");
    repository
        .save_playback_checkpoint(&SavePlaybackCheckpointInput {
            task_id: task.id.clone(),
            file_index: Some(0),
            position_seconds: 10.0,
            duration_seconds: 100.0,
            completed: None,
        })
        .expect("save removable checkpoint");

    let remaining = DownloadRepository::remove_download_task(&repository, &task.id, false)
        .expect("remove task by scoped id");
    assert!(remaining.is_empty());
    assert_eq!(
        MediaRepository::list_media_files(&repository)
            .expect("list preserved media")
            .len(),
        1
    );
    assert!(repository
        .get_playback_checkpoint(&task.id, Some(0))
        .expect("read removed checkpoint")
        .is_none());
    assert!(repository
        .list_episodes("anime-p3-1")
        .expect("list restored episodes")
        .iter()
        .all(|episode| episode.status == EpisodeStatus::Aired));

    DownloadRepository::upsert_download_task(&repository, &task).expect("restore removable task");
    DownloadRepository::remove_download_task(&repository, &task.id, true)
        .expect("remove task and media");
    assert!(MediaRepository::list_media_files(&repository)
        .expect("list deleted media")
        .is_empty());

    let mut invalid = p4_download_task();
    invalid.id = "p4-invalid-task".to_owned();
    invalid.files[0].episode_id = Some("missing-episode".to_owned());
    assert!(DownloadRepository::upsert_download_task(&repository, &invalid).is_err());
    assert!(DownloadRepository::list_downloads(&repository)
        .expect("list downloads after rollback")
        .is_empty());
}

/// 验证媒体记录完整往返，并按文件路径移除旧标识。
#[test]
fn upserts_media_files_and_deduplicates_paths() {
    let directory = TestDirectory::new("p4-media-write");
    let storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("create media database");
    let repository = storage.repository();
    let mut media = MediaFile {
        id: "media-old".to_owned(),
        anime_id: "anime-1".to_owned(),
        episode_id: Some("episode-1".to_owned()),
        download_task_id: Some("download-1".to_owned()),
        content_kind: ani_domain::MediaContentKind::Episode,
        special_no: None,
        file_path: "C:/Anime/episode-1.mkv".to_owned(),
        file_name: "episode-1.mkv".to_owned(),
        size: 1024,
        container: Some("mkv".to_owned()),
        declared_video_codec: Some("HEVC".to_owned()),
        detected_video_codec: Some("hevc".to_owned()),
        normalized_video_codec: "H.265/HEVC".to_owned(),
        resolution: Some("1920x1080".to_owned()),
        bit_depth: Some(10),
        audio_codecs: vec!["AAC".to_owned()],
        subtitle_tracks: vec!["chi / ASS".to_owned()],
        duration_seconds: Some(1440),
        downloaded_at: Some("2026-07-25T00:00:00.000Z".to_owned()),
        probed_at: Some("2026-07-25T00:01:00.000Z".to_owned()),
        origin: Default::default(),
        source_root: None,
        fingerprint: None,
        file_modified_at: None,
        availability: Default::default(),
        last_verified_at: None,
        availability_error: None,
    };
    let first = MediaRepository::upsert_media_files(&repository, &[media.clone()])
        .expect("write first media");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].audio_codecs, ["AAC"]);

    media.id = "media-new".to_owned();
    media.duration_seconds = Some(1500);
    let replaced =
        MediaRepository::upsert_media_files(&repository, &[media]).expect("replace media by path");

    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].id, "media-new");
    assert_eq!(replaced[0].duration_seconds, Some(1500));

    let mut special = replaced[0].clone();
    special.id = "media-special".to_owned();
    special.episode_id = None;
    special.file_path = "C:/Anime/SP/episode-sp1.mkv".to_owned();
    special.file_name = "episode-sp1.mkv".to_owned();
    special.content_kind = ani_domain::MediaContentKind::Special;
    special.special_no = Some("SP01".to_owned());
    let with_special =
        MediaRepository::upsert_media_files(&repository, &[special]).expect("save special media");
    let saved_special = with_special
        .iter()
        .find(|item| item.id == "media-special")
        .expect("read special media");
    assert_eq!(
        saved_special.content_kind,
        ani_domain::MediaContentKind::Special
    );
    assert_eq!(saved_special.special_no.as_deref(), Some("SP01"));
    assert_eq!(saved_special.episode_id, None);

    let remaining = MediaRepository::remove_media_files(
        &repository,
        &["media-new".to_owned(), "media-special".to_owned()],
    )
    .expect("remove media by id");
    assert!(remaining.is_empty());
}

/// 验证删除旁车媒体后，仅在单集失去全部媒体时恢复其状态。
#[test]
fn removing_media_restores_only_orphaned_episode_status() {
    let directory = TestDirectory::new("p4-media-remove");
    let storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("create media database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    repository
        .upsert_my_anime(fixture.payload.my_anime)
        .expect("save media anime");
    let mut episode = fixture.payload.episode;
    episode.status = EpisodeStatus::Downloaded;
    repository
        .upsert_episode(&episode)
        .expect("save downloaded episode");
    let media = MediaFile {
        id: "media-apple-double".to_owned(),
        anime_id: episode.anime_id.clone(),
        episode_id: Some(episode.id.clone()),
        download_task_id: None,
        content_kind: ani_domain::MediaContentKind::Episode,
        special_no: None,
        file_path: "/media/._episode.mkv".to_owned(),
        file_name: "._episode.mkv".to_owned(),
        size: 1024,
        container: Some("mkv".to_owned()),
        declared_video_codec: None,
        detected_video_codec: None,
        normalized_video_codec: "unknown".to_owned(),
        resolution: None,
        bit_depth: None,
        audio_codecs: Vec::new(),
        subtitle_tracks: Vec::new(),
        duration_seconds: None,
        downloaded_at: None,
        probed_at: None,
        origin: Default::default(),
        source_root: Some("/media".to_owned()),
        fingerprint: None,
        file_modified_at: None,
        availability: Default::default(),
        last_verified_at: None,
        availability_error: None,
    };
    let mut real_media = media.clone();
    real_media.id = "media-real".to_owned();
    real_media.file_path = "/media/episode.mkv".to_owned();
    real_media.file_name = "episode.mkv".to_owned();
    MediaRepository::upsert_media_files(&repository, &[media, real_media])
        .expect("save paired media");

    MediaRepository::remove_media_files(&repository, &["media-apple-double".to_owned()])
        .expect("remove AppleDouble media");
    assert_eq!(
        repository
            .list_episodes(&episode.anime_id)
            .expect("list episode with real media")[0]
            .status,
        EpisodeStatus::Downloaded
    );

    MediaRepository::remove_media_files(&repository, &["media-real".to_owned()])
        .expect("remove final media");
    assert_eq!(
        repository
            .list_episodes(&episode.anime_id)
            .expect("list orphaned episode")[0]
            .status,
        EpisodeStatus::Aired
    );
}

/// 验证媒体重绑恢复旧单集，并只清理未被用户维护的空导入番剧。
#[test]
fn rebinds_media_and_cleans_only_untouched_imported_anime() {
    let directory = TestDirectory::new("p4-media-rebind");
    let storage =
        Storage::open(test_options(&directory, "active.sqlite")).expect("create media database");
    let timestamp = "2026-08-02T00:00:00.000Z";
    for (anime_id, title) in [
        ("local-old", "错误导入番剧"),
        ("local-protected", "用户维护番剧"),
        ("anime-target", "正确番剧"),
    ] {
        storage
            .connection
            .execute(
                "INSERT INTO anime_catalog (
                   id, title, premiere_year, premiere_month, external_ids_json,
                   detail_json, created_at, updated_at
                 ) VALUES (?1, ?2, 2026, 8, ?3, '{}', ?4, ?4)",
                params![
                    anime_id,
                    title,
                    if anime_id.starts_with("local-") {
                        r#"{"localImport":true}"#
                    } else {
                        "{}"
                    },
                    timestamp
                ],
            )
            .expect("insert rebind anime");
    }
    for (item_id, anime_id, status) in [
        ("my-local-old", "local-old", "planned"),
        ("my-local-protected", "local-protected", "watching"),
    ] {
        storage
            .connection
            .execute(
                "INSERT INTO my_anime (
                   id, anime_id, status, auto_download, preferred_resolution,
                   preferred_codec, preferred_subtitle,
                   preferred_subtitle_languages_json, preferred_bit_depth,
                   added_at, updated_at
                 ) VALUES (?1, ?2, ?3, 0, '1080p', 'H.265/HEVC', 'chs',
                   '[\"chs\"]', 10, ?4, ?4)",
                params![item_id, anime_id, status, timestamp],
            )
            .expect("insert rebind tracking");
    }
    for (episode_id, anime_id) in [
        ("episode-old-1", "local-old"),
        ("episode-target-1", "anime-target"),
    ] {
        storage
            .connection
            .execute(
                "INSERT INTO episode (
                   id, anime_id, episode_no, status, created_at, updated_at
                 ) VALUES (?1, ?2, 1, 'downloaded', ?3, ?3)",
                params![episode_id, anime_id, timestamp],
            )
            .expect("insert rebind episode");
    }
    let repository = storage.repository();
    let mut media = MediaFile {
        id: "media-rebind".to_owned(),
        anime_id: "local-old".to_owned(),
        episode_id: Some("episode-old-1".to_owned()),
        download_task_id: None,
        content_kind: ani_domain::MediaContentKind::Episode,
        special_no: None,
        file_path: "/media/episode-1.mkv".to_owned(),
        file_name: "episode-1.mkv".to_owned(),
        size: 1024,
        container: Some("mkv".to_owned()),
        declared_video_codec: None,
        detected_video_codec: None,
        normalized_video_codec: "unknown".to_owned(),
        resolution: None,
        bit_depth: None,
        audio_codecs: Vec::new(),
        subtitle_tracks: Vec::new(),
        duration_seconds: None,
        downloaded_at: None,
        probed_at: None,
        origin: ani_domain::MediaOrigin::Imported,
        source_root: Some("/media".to_owned()),
        fingerprint: None,
        file_modified_at: None,
        availability: Default::default(),
        last_verified_at: None,
        availability_error: None,
    };
    MediaRepository::upsert_media_files(&repository, &[media.clone()])
        .expect("save old media association");
    media.anime_id = "anime-target".to_owned();
    media.episode_id = Some("episode-target-1".to_owned());
    MediaRepository::upsert_media_files(&repository, &[media]).expect("rebind media association");

    assert_eq!(
        repository
            .list_episodes("local-old")
            .expect("list restored old episode")[0]
            .status,
        EpisodeStatus::Aired
    );
    let removed = MediaRepository::cleanup_orphaned_imported_anime(
        &repository,
        &["local-old".to_owned(), "local-protected".to_owned()],
    )
    .expect("cleanup orphaned imported anime");
    assert_eq!(removed, ["local-old"]);
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM anime_catalog WHERE id = 'local-old'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count removed local anime"),
        0
    );
    assert_eq!(
        storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM anime_catalog WHERE id = 'local-protected'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count protected local anime"),
        1
    );
}

/// 验证番剧目录合并、搜索、月份替换和详情聚合保持业务引用。
#[test]
fn reads_and_replaces_p3_anime_catalog() {
    let directory = TestDirectory::new("p3-catalog");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create p3 catalog database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    repository
        .upsert_my_anime(fixture.payload.my_anime.clone())
        .expect("save referenced anime");

    let mut old_cache = fixture.payload.my_anime.anime.clone();
    old_cache.id = "anime-p3-old-cache".to_owned();
    old_cache.title = "待替换缓存番剧".to_owned();
    old_cache.original_title = Some("Old Cache Anime".to_owned());
    old_cache.external_ids = json!({ "bangumi": "old-cache" });
    old_cache.aliases[0].alias = "Old Cache Alias".to_owned();
    let initial = repository
        .upsert_anime_catalog(&[old_cache])
        .expect("save old catalog cache");
    assert_eq!(initial.added_count, 1);
    assert_eq!(
        repository
            .list_anime_catalog(Some(2026), Some(7))
            .expect("list july catalog")
            .len(),
        2
    );
    assert_eq!(
        repository
            .search_anime_catalog("p3 alias")
            .expect("search alias")
            .items[0]
            .id,
        "anime-p3-1"
    );

    let mut refreshed = fixture.payload.my_anime.anime.clone();
    refreshed.id = "provider-replacement-id".to_owned();
    refreshed.title = "P3 刷新番剧".to_owned();
    refreshed.aliases[0].alias = "Refreshed Alias".to_owned();
    refreshed.detail = Some(json!({
        "episodeCount": 12,
        "refreshedAt": chrono::Utc::now().to_rfc3339()
    }));
    let replaced = repository
        .replace_anime_catalog_month(2026, 7, &[refreshed])
        .expect("replace july catalog");
    assert_eq!(replaced.existing_count, 1);
    let july = repository
        .list_anime_catalog(Some(2026), Some(7))
        .expect("list replaced july catalog");
    assert_eq!(july.len(), 1);
    assert_eq!(july[0].id, "anime-p3-1");
    assert_eq!(july[0].title, "P3 刷新番剧");
    assert!(july[0]
        .aliases
        .iter()
        .any(|alias| alias.alias == "Refreshed Alias"));

    let detail = repository
        .get_anime_detail("anime-p3-1")
        .expect("read local anime detail");
    assert!(detail.my_anime.is_some());
    assert!(!detail.stale);
    assert!(detail.partial_errors.is_empty());
}

/// 验证增量采集的空字段不会覆盖已有目录数据。
#[test]
fn preserves_existing_catalog_fields_for_empty_incremental_values() {
    let directory = TestDirectory::new("incremental-catalog-fields");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create incremental catalog database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    let mut existing = fixture.payload.my_anime.anime;
    existing.id = "incremental-anime-1".to_owned();
    existing.title = "旧标题".to_owned();
    existing.original_title = Some("Existing Original".to_owned());
    existing.premiere_date = Some("2026-07-03".to_owned());
    existing.premiere_year = 2026;
    existing.premiere_month = 7;
    existing.season = Some("summer".to_owned());
    existing.summary = Some("已有简介".to_owned());
    existing.cover_url = Some("https://example.com/existing.jpg".to_owned());
    existing.external_ids = json!({"bangumi": "101", "anilist": "202"});
    existing.detail = Some(json!({
        "episodeCount": 12,
        "genres": ["动画"],
        "broadcast": {"weekday": 5, "time": "22:15"},
        "staff": [{"name": "已有职员", "role": "导演"}]
    }));
    repository
        .upsert_anime_catalog(&[existing.clone()])
        .expect("save existing catalog");

    let mut incoming = existing.clone();
    incoming.title = "有效新标题".to_owned();
    incoming.original_title = Some(" ".to_owned());
    incoming.aliases.clear();
    incoming.premiere_date = None;
    incoming.premiere_year = 2000;
    incoming.premiere_month = 1;
    incoming.season = Some(String::new());
    incoming.summary = Some(String::new());
    incoming.cover_url = Some(" ".to_owned());
    incoming.rating = None;
    incoming.external_ids = json!({"bangumi": "", "anilist": null, "mikan": "303"});
    incoming.detail = Some(json!({
        "episodeCount": null,
        "genres": [],
        "broadcast": {"time": ""},
        "staff": [{}],
        "durationMinutes": 24
    }));
    let persisted = repository
        .upsert_anime_catalog(&[incoming])
        .expect("merge incremental catalog");
    assert_eq!(persisted.added_count, 0);
    assert_eq!(persisted.existing_count, 1);

    let merged = repository
        .get_anime_catalog_by_id("incremental-anime-1")
        .expect("read merged catalog")
        .expect("merged catalog exists");
    assert_eq!(merged.title, "有效新标题");
    assert_eq!(merged.original_title.as_deref(), Some("Existing Original"));
    assert_eq!(merged.premiere_date.as_deref(), Some("2026-07-03"));
    assert_eq!(merged.premiere_year, 2026);
    assert_eq!(merged.premiere_month, 7);
    assert_eq!(merged.season.as_deref(), Some("summer"));
    assert_eq!(merged.summary.as_deref(), Some("已有简介"));
    assert_eq!(
        merged.cover_url.as_deref(),
        Some("https://example.com/existing.jpg")
    );
    assert_eq!(merged.external_ids["bangumi"], "101");
    assert_eq!(merged.external_ids["anilist"], "202");
    assert_eq!(merged.external_ids["mikan"], "303");
    let detail = merged.detail.expect("merged detail");
    assert_eq!(detail["episodeCount"], 12);
    assert_eq!(detail["genres"], json!(["动画"]));
    assert_eq!(detail["broadcast"]["weekday"], 5);
    assert_eq!(detail["broadcast"]["time"], "22:15");
    assert_eq!(detail["staff"][0]["name"], "已有职员");
    assert_eq!(detail["durationMinutes"], 24);
}

/// 验证仅刷新易变时间戳时不会重写未变化目录行。
#[test]
fn skips_unchanged_catalog_rows_during_incremental_upsert() {
    let directory = TestDirectory::new("incremental-catalog-unchanged");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create unchanged catalog database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode following fixture");
    let repository = storage.repository();
    let mut anime = fixture.payload.my_anime.anime;
    anime.id = "incremental-unchanged-1".to_owned();
    anime.detail = Some(json!({"episodeCount": 12, "refreshedAt": "2026-07-20T00:00:00.000Z"}));
    repository
        .upsert_anime_catalog(&[anime.clone()])
        .expect("save initial anime");
    storage
        .connection
        .execute(
            "UPDATE anime_catalog SET updated_at = '2026-07-01T00:00:00.000Z' WHERE id = ?1",
            [&anime.id],
        )
        .expect("pin update timestamp");

    anime.detail.as_mut().expect("detail")["refreshedAt"] = json!("2026-07-28T00:00:00.000Z");
    repository
        .upsert_anime_catalog(&[anime])
        .expect("upsert unchanged anime");
    let updated_at: String = storage
        .connection
        .query_row(
            "SELECT updated_at FROM anime_catalog WHERE id = 'incremental-unchanged-1'",
            [],
            |row| row.get(0),
        )
        .expect("read update timestamp");
    assert_eq!(updated_at, "2026-07-01T00:00:00.000Z");
}

/// 验证纯增量写入不会删除同月未返回的旧目录。
#[test]
fn retains_unreturned_catalog_entries_during_incremental_upsert() {
    let directory = TestDirectory::new("incremental-catalog-retention");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create incremental retention database");
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode p3 following fixture");
    let repository = storage.repository();
    let mut old = fixture.payload.my_anime.anime.clone();
    old.id = "incremental-old".to_owned();
    old.title = "本轮未返回番剧".to_owned();
    old.original_title = Some("Unreturned Anime".to_owned());
    old.external_ids = json!({"bangumi": ""});
    let mut incoming = fixture.payload.my_anime.anime;
    incoming.id = "incremental-new".to_owned();
    incoming.title = "本轮新增番剧".to_owned();
    incoming.original_title = Some("New Anime".to_owned());
    incoming.external_ids = json!({"bangumi": ""});
    repository
        .upsert_anime_catalog(&[old])
        .expect("save old catalog");
    repository
        .upsert_anime_catalog(&[incoming])
        .expect("save incremental catalog");

    let july = repository
        .list_anime_catalog(Some(2026), Some(7))
        .expect("list incremental catalog");
    assert!(july.iter().any(|anime| anime.id == "incremental-old"));
    assert!(july.iter().any(|anime| anime.id == "incremental-new"));
}

/// 验证月度替换会清理历史遗留的未引用同名重复目录。
#[test]
fn removes_unreferenced_duplicate_catalog_entries_on_month_replace() {
    let directory = TestDirectory::new("duplicate-season-catalog");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("create duplicate catalog database");
    storage
        .connection
        .execute_batch(
            "INSERT INTO anime_catalog (
               id, title, premiere_year, premiere_month, external_ids_json, detail_json,
               created_at, updated_at
             ) VALUES
               ('anilist-199111', '重复季度番剧', 2026, 7, '{\"anilist\":\"199111\"}', '{}',
                '2026-07-28T00:00:00.000Z', '2026-07-28T00:00:00.000Z'),
               ('mikan-4014', '重复季度番剧', 2026, 7, '{\"mikan\":\"4014\"}', '{}',
                '2026-07-28T00:00:00.000Z', '2026-07-28T00:00:00.000Z');",
        )
        .expect("insert duplicate catalog rows");
    let repository = storage.repository();
    let mut refreshed = repository
        .get_anime_catalog_by_id("anilist-199111")
        .expect("read incoming catalog")
        .expect("incoming catalog exists");
    refreshed.external_ids = json!({ "anilist": "199111", "mikan": "4014" });
    refreshed.cover_url = Some("https://example.com/cover.jpg".to_owned());

    let replaced = repository
        .replace_anime_catalog_month(2026, 7, &[refreshed])
        .expect("replace duplicate month");
    assert_eq!(replaced.added_count, 0);
    assert_eq!(replaced.existing_count, 1);
    let items = repository
        .list_anime_catalog(Some(2026), Some(7))
        .expect("list deduplicated month");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "anilist-199111");
    assert_eq!(items[0].external_ids["mikan"], "4014");
}

/// 验证季度完成标记和 AniList 错误可跨重启持久化。
#[test]
fn persists_anime_season_sync_state() {
    let directory = TestDirectory::new("anime-season-sync-state");
    let options = test_options(&directory, "active.sqlite");
    let storage = Storage::open(options.clone()).expect("create season state database");
    let repository = storage.repository();
    repository
        .upsert_anime_season_sync_state(&AnimeSeasonSyncState {
            year: 2026,
            season: "summer".to_owned(),
            last_attempt_at: Some("2026-07-28T06:00:00.000Z".to_owned()),
            last_successful_sync_at: None,
            completed_at: None,
            last_anilist_error: Some("anilist: timeout".to_owned()),
        })
        .expect("save failed season state");
    drop(storage);

    let reopened = Storage::open(options).expect("reopen season state database");
    let state = reopened
        .repository()
        .get_anime_season_sync_state(2026, "summer")
        .expect("read season state")
        .expect("saved season state");
    assert_eq!(state.completed_at, None);
    assert_eq!(
        state.last_anilist_error.as_deref(),
        Some("anilist: timeout")
    );
}

/// 验证详情来源分片和周期完成状态可跨重启持久化。
#[test]
fn persists_anime_detail_refresh_state() {
    let directory = TestDirectory::new("anime-detail-refresh-state");
    let options = test_options(&directory, "active.sqlite");
    let storage = Storage::open(options.clone()).expect("create detail state database");
    let repository = storage.repository();
    let fixture: ContractFixture<P3FollowingWriteModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-following-write-model.v1.json"
        )))
        .expect("decode following fixture");
    repository
        .upsert_anime_catalog(&[fixture.payload.my_anime.anime])
        .expect("save state anime");
    repository
        .upsert_anime_detail_refresh_states(&[AnimeDetailRefreshState {
            anime_id: "anime-p3-1".to_owned(),
            provider: "bangumi".to_owned(),
            external_id: "424242".to_owned(),
            slot_day: 3,
            last_completed_cycle: Some(2951),
            last_attempt_at: Some("2026-07-28T06:00:00.000Z".to_owned()),
            last_success_at: Some("2026-07-28T06:00:00.000Z".to_owned()),
            failure_count: 0,
            next_retry_at: None,
        }])
        .expect("save detail refresh state");
    drop(storage);

    let reopened = Storage::open(options).expect("reopen detail state database");
    let states = reopened
        .repository()
        .list_anime_detail_refresh_states()
        .expect("read detail refresh states");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].provider, "bangumi");
    assert_eq!(states[0].slot_day, 3);
    assert_eq!(states[0].last_completed_cycle, Some(2951));
}

/// 验证公共工作单元能回滚或提交复用事务的 Repository 写入。
#[test]
fn exposes_atomic_repository_unit_of_work() {
    let directory = TestDirectory::new("repository-unit-of-work");
    let mut storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open repository unit of work database");
    let cache_key = "source-search-unit-of-work";
    let now = "2026-07-25T00:00:00.000Z";
    let entry = ReleaseSearchCacheEntry {
        result: json!({ "items": [{ "title": "事务缓存" }] }),
        expires_at: "2026-07-26T00:00:00.000Z".to_owned(),
    };

    let work = storage
        .begin_unit_of_work()
        .expect("begin rollback unit of work");
    {
        let repositories = work.repositories();
        ReleaseSourceRepository::upsert_release_search_cache(&repositories, cache_key, &entry)
            .expect("write cache inside rollback unit of work");
        assert!(
            ReleaseSourceRepository::get_release_search_cache(&repositories, cache_key, now)
                .expect("read uncommitted cache")
                .is_some()
        );
    }
    work.rollback().expect("rollback unit of work");
    assert!(ReleaseSourceRepository::get_release_search_cache(
        &storage.repository(),
        cache_key,
        now
    )
    .expect("read cache after rollback")
    .is_none());

    let work = storage
        .begin_unit_of_work()
        .expect("begin commit unit of work");
    {
        let repositories = work.repositories();
        ReleaseSourceRepository::upsert_release_search_cache(&repositories, cache_key, &entry)
            .expect("write cache inside commit unit of work");
    }
    work.commit().expect("commit unit of work");
    assert!(ReleaseSourceRepository::get_release_search_cache(
        &storage.repository(),
        cache_key,
        now
    )
    .expect("read cache after commit")
    .is_some());
}

/// 验证来源配置、同步游标、熔断和搜索缓存均通过 SQLite 适配器持久化。
#[test]
fn persists_p3_source_network_state() {
    let directory = TestDirectory::new("p3-source-network");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open p3 source network database");
    let fixture: ContractFixture<P3SourceNetworkModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-source-network-model.v1.json"
        )))
        .expect("decode p3 source network fixture");
    let repository = storage.repository();

    let sources = repository
        .upsert_source(&fixture.payload.source)
        .expect("save source config");
    assert!(sources
        .iter()
        .any(|source| source.id == fixture.payload.source.id));
    repository
        .upsert_source_sync_state(&fixture.payload.sync_state)
        .expect("save source sync state");
    assert_eq!(
        repository
            .list_source_sync_states()
            .expect("list source sync states")
            .into_iter()
            .find(|state| state.source_id == fixture.payload.source.id)
            .expect("saved source sync state")
            .request_failure_count,
        2
    );
    repository
        .upsert_request_circuit_state(&fixture.payload.circuit_state)
        .expect("save request circuit state");
    let circuit = repository
        .get_request_circuit_state(&fixture.payload.circuit_state.key)
        .expect("read request circuit state")
        .expect("saved request circuit state");
    assert_eq!(circuit.failure_count, 2);
    assert_eq!(circuit.network_context.as_deref(), Some("fixture-network"));
    repository
        .clear_request_circuit_state(&fixture.payload.circuit_state.key)
        .expect("clear request circuit state");
    assert!(repository
        .get_request_circuit_state(&fixture.payload.circuit_state.key)
        .expect("read cleared request circuit state")
        .is_none());
}

/// 验证 v22 升级会补齐网络上下文列并清除范围未知的旧熔断状态。
#[test]
fn migrates_v22_request_circuit_context() {
    let directory = TestDirectory::new("request-circuit-context-migration");
    let options = test_options(&directory, "active.sqlite");
    let database_path = options.database_path.clone();
    drop(Storage::open(options.clone()).expect("create current database"));

    let legacy = Connection::open(&database_path).expect("open legacy circuit database");
    legacy
        .execute_batch(
            "ALTER TABLE request_circuit_state DROP COLUMN network_context;
             UPDATE app_meta SET value = '22' WHERE key = 'schema_version';
             INSERT INTO request_circuit_state (
               circuit_key, circuit_group, request_host, last_request_at,
               failure_count, backoff_until, updated_at
             ) VALUES (
               'release-source:metadata-bangumi', 'release-source', 'api.bgm.tv',
               '2026-08-06T05:46:30.000Z', 3, '2099-01-01T00:00:00.000Z',
               '2026-08-06T05:46:30.000Z'
             );",
        )
        .expect("prepare v22 circuit database");
    drop(legacy);

    let storage = Storage::open(options).expect("migrate v22 circuit database");

    assert!(column_exists(
        &storage.connection,
        "request_circuit_state",
        "network_context"
    ));
    assert_eq!(
        storage
            .connection
            .query_row("SELECT COUNT(*) FROM request_circuit_state", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count migrated circuit states"),
        0
    );
    assert_eq!(
        read_meta(&storage.connection, "schema_version"),
        SQLITE_SCHEMA_VERSION.to_string()
    );
}

/// 验证来源绑定和排除记录通过公共 Repository 端口完整持久化并校验输入。
#[test]
fn persists_p3_source_bindings_and_exclusions() {
    let directory = TestDirectory::new("p3-source-binding");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open p3 source binding database");
    let fixture: ContractFixture<P3SourceBindingModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-source-binding-model.v1.json"
        )))
        .expect("decode p3 source binding fixture");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.kind, "p3-source-binding-model");
    insert_source(
        &storage.connection,
        &fixture.payload.binding.source_id,
        false,
    );
    insert_source_binding_anime(&storage.connection, &fixture.payload.binding.anime_id);
    let repository = storage.repository();

    let bindings = AnimeSourceBindingRepository::upsert_anime_source_binding(
        &repository,
        &fixture.payload.binding,
    )
    .expect("save source binding");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0], fixture.payload.binding);

    let mut replacement = fixture.payload.binding.clone();
    replacement.id = "replacement-binding-id".to_owned();
    replacement.source_anime_id = "528829".to_owned();
    replacement.match_method = AnimeSourceBindingMatchMethod::Manual;
    replacement.confidence = 0.75;
    replacement.updated_at = "2026-07-25T00:10:00.000Z".to_owned();
    let bindings =
        AnimeSourceBindingRepository::upsert_anime_source_binding(&repository, &replacement)
            .expect("replace source binding");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].source_anime_id, "528829");
    assert_eq!(bindings[0].created_at, fixture.payload.binding.created_at);

    let mut invalid_binding = replacement.clone();
    invalid_binding.source_url = Some("file:///private/source".to_owned());
    assert!(matches!(
        AnimeSourceBindingRepository::upsert_anime_source_binding(&repository, &invalid_binding),
        Err(RepositoryError::InvalidInput { .. })
    ));

    let exclusions = AnimeSourceBindingRepository::upsert_anime_source_exclusion(
        &repository,
        &fixture.payload.exclusion,
    )
    .expect("save candidate exclusion");
    assert_eq!(exclusions, vec![fixture.payload.exclusion.clone()]);

    let mut source_exclusion = fixture.payload.exclusion.clone();
    source_exclusion.id = "source-exclusion-all".to_owned();
    source_exclusion.scope = AnimeSourceExclusionScope::Source;
    source_exclusion.source_anime_id = None;
    source_exclusion.source_anime_title = None;
    let exclusions =
        AnimeSourceBindingRepository::upsert_anime_source_exclusion(&repository, &source_exclusion)
            .expect("save source exclusion");
    assert_eq!(exclusions.len(), 2);

    let mut invalid_exclusion = fixture.payload.exclusion.clone();
    invalid_exclusion.source_anime_id = None;
    assert!(matches!(
        AnimeSourceBindingRepository::upsert_anime_source_exclusion(
            &repository,
            &invalid_exclusion
        ),
        Err(RepositoryError::InvalidInput { .. })
    ));

    let exclusions = AnimeSourceBindingRepository::remove_anime_source_exclusion(
        &repository,
        &fixture.payload.exclusion.anime_id,
        &fixture.payload.exclusion.source_id,
        fixture.payload.exclusion.source_anime_id.as_deref(),
    )
    .expect("remove candidate exclusion");
    assert_eq!(exclusions, vec![source_exclusion]);
    let exclusions = AnimeSourceBindingRepository::remove_anime_source_exclusion(
        &repository,
        &fixture.payload.exclusion.anime_id,
        &fixture.payload.exclusion.source_id,
        None,
    )
    .expect("remove source exclusion");
    assert!(exclusions.is_empty());

    let bindings = AnimeSourceBindingRepository::remove_anime_source_binding(
        &repository,
        &replacement.anime_id,
        &replacement.source_id,
    )
    .expect("remove source binding");
    assert!(bindings.is_empty());
}

/// 验证原始资源缓存与动态字幕组观察通过公共 Repository 端口持久化。
#[test]
fn persists_p3_release_cache_and_observed_fansubs() {
    let directory = TestDirectory::new("p3-release-cache");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open p3 release cache database");
    let fixture: ContractFixture<P3ReleaseSearchModelFixture> =
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/contracts/p3-release-search-model.v1.json"
        )))
        .expect("decode p3 release search fixture");
    let release = fixture.payload.search_result.releases[0].clone();
    insert_source(&storage.connection, &release.source_id, false);
    insert_source_binding_anime(
        &storage.connection,
        release.anime_id.as_deref().expect("fixture anime id"),
    );
    let repository = storage.repository();

    let mut alias_release = release.clone();
    alias_release.fansub_name = Some("契约字幕组别名".to_owned());
    alias_release.source_id = "other-contract".to_owned();
    let observed = AnimeCatalogRepository::observe_anime_fansubs(
        &repository,
        release.anime_id.as_deref().expect("fixture anime id"),
        &[release.clone(), alias_release],
    )
    .expect("observe release fansubs");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].id, "fansub-contract");
    assert_eq!(observed[0].name, "契约字幕组");
    assert_eq!(observed[0].aliases, vec!["契约字幕组别名"]);
    assert_eq!(
        observed[0].source_ids,
        vec!["other-contract", "rss-contract"]
    );

    assert_eq!(
        ReleaseCacheRepository::upsert_cached_releases(&repository, std::slice::from_ref(&release))
            .expect("save first cached release"),
        1
    );
    assert_eq!(
        ReleaseCacheRepository::upsert_cached_releases(&repository, std::slice::from_ref(&release))
            .expect("save duplicate cached release"),
        0
    );
    let query = CachedReleaseQuery {
        source_ids: Some(vec![release.source_id.clone()]),
        anime_id: release.anime_id.clone(),
        limit: Some(10),
    };
    assert_eq!(
        ReleaseCacheRepository::list_cached_releases(&repository, &query)
            .expect("list cached release"),
        vec![release.clone()]
    );

    let mut refreshed = release.clone();
    refreshed.anime_id = None;
    refreshed.seeders = Some(48);
    ReleaseCacheRepository::upsert_cached_releases(&repository, &[refreshed])
        .expect("refresh cached release without anime id");
    let cached = ReleaseCacheRepository::list_cached_releases(&repository, &query)
        .expect("list refreshed cached release");
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].anime_id, release.anime_id);
    assert_eq!(cached[0].seeders, Some(48));
    assert!(ReleaseCacheRepository::list_cached_releases(
        &repository,
        &CachedReleaseQuery {
            source_ids: Some(Vec::new()),
            ..CachedReleaseQuery::default()
        }
    )
    .expect("list empty source cache")
    .is_empty());

    assert_eq!(
        ReleaseCacheRepository::prune_cached_releases(&repository, "2026-07-26T00:00:00.000Z")
            .expect("prune cached releases"),
        1
    );
    assert!(
        ReleaseCacheRepository::list_cached_releases(&repository, &query)
            .expect("list pruned cache")
            .is_empty()
    );
}

/// 验证公共通知写入端口增量保存来源同步提醒并保留已读状态。
#[test]
fn persists_notifications_through_repository_port() {
    let directory = TestDirectory::new("notification-write");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open notification database");
    let repository = storage.repository();
    let mut record = NotificationRecord {
        id: "source-sync-contract".to_owned(),
        kind: NotificationKind::System,
        title: "来源同步失败".to_owned(),
        body: "失败来源：契约来源".to_owned(),
        severity: NotificationSeverity::Warning,
        anime_id: None,
        episode_id: None,
        download_task_id: None,
        created_at: "2026-07-25T01:00:00.000Z".to_owned(),
        read_at: Some("2026-07-25T02:00:00.000Z".to_owned()),
    };
    let saved =
        NotificationRepository::add_notifications(&repository, std::slice::from_ref(&record))
            .expect("save source sync notification");
    assert_eq!(saved, vec![record.clone()]);
    record.body = "更新后的失败原因".to_owned();
    record.read_at = None;
    let updated = NotificationRepository::add_notifications(&repository, &[record])
        .expect("update source sync notification");
    assert_eq!(updated[0].body, "更新后的失败原因");
    assert_eq!(
        updated[0].read_at.as_deref(),
        Some("2026-07-25T02:00:00.000Z")
    );
    assert_eq!(
        NotificationRepository::get_unread_notification_count(&repository)
            .expect("count unread notifications"),
        0
    );
}

/// 验证公共通知端口支持单条已读、全部已读和清空操作。
#[test]
fn mutates_notifications_through_repository_port() {
    let directory = TestDirectory::new("notification-mutations");
    let storage = Storage::open(test_options(&directory, "active.sqlite"))
        .expect("open notification database");
    let repository = storage.repository();
    let records = ["notification-a", "notification-b"].map(|id| NotificationRecord {
        id: id.to_owned(),
        kind: NotificationKind::System,
        title: "系统提醒".to_owned(),
        body: "测试通知状态".to_owned(),
        severity: NotificationSeverity::Info,
        anime_id: None,
        episode_id: None,
        download_task_id: None,
        created_at: format!(
            "2026-07-25T01:00:0{}.000Z",
            if id.ends_with('a') { 1 } else { 2 }
        ),
        read_at: None,
    });
    NotificationRepository::add_notifications(&repository, &records).expect("save notifications");

    let marked = NotificationRepository::mark_notification_read(&repository, "notification-a")
        .expect("mark notification read");
    assert!(marked
        .iter()
        .find(|record| record.id == "notification-a")
        .and_then(|record| record.read_at.as_ref())
        .is_some());
    assert_eq!(
        NotificationRepository::get_unread_notification_count(&repository)
            .expect("count one unread notification"),
        1
    );

    let all_read = NotificationRepository::mark_all_notifications_read(&repository)
        .expect("mark all notifications read");
    assert!(all_read.iter().all(|record| record.read_at.is_some()));
    assert!(NotificationRepository::clear_notifications(&repository)
        .expect("clear notifications")
        .is_empty());
    assert!(NotificationRepository::list_notifications(&repository)
        .expect("list cleared notifications")
        .is_empty());
}

/// 创建包含固定设置和下载源的测试启动参数。
fn test_options(directory: &TestDirectory, database_name: &str) -> StorageOptions {
    StorageOptions {
        database_path: directory.path().join(database_name),
        backup_directory: directory.path().join("backups"),
        legacy_database_paths: Vec::new(),
        seed: StorageSeed {
            settings: json!({ "appearance": { "mode": "system" } }),
            dashboard: json!({ "todayEpisodes": [] }),
            release_sources: vec![ReleaseSourceSeed {
                id: "anibt".to_owned(),
                name: "AniBT".to_owned(),
                kind: "site_adapter".to_owned(),
                enabled: true,
                use_proxy: false,
                request_interval_ms: 1_500,
                base_url: Some("https://anibt.example".to_owned()),
                api_key: None,
                rss_url: None,
                tags: vec!["anime".to_owned()],
            }],
        },
    }
}

/// 写入覆盖 P2 首批查询的固定业务样本。
fn insert_p2_read_model_fixture(connection: &Connection) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    connection
        .execute(
            "UPDATE app_settings SET value_json = ?1 WHERE key = 'settings'",
            [r#"{"appearance":{"mode":"dark"},"players":[{"id":"built-in","executablePath":"C:/VLC/vlc.exe"}]}"#],
        )
        .expect("patch stored settings");
    connection
        .execute(
            "UPDATE app_state SET value_json = ?1 WHERE key = 'dashboard'",
            [r#"{"weeklySchedule":[{"day":"周一","items":[]}],"sourceHealth":[{"sourceId":"anibt","name":"AniBT","status":"ok"}]}"#],
        )
        .expect("patch stored dashboard");
    connection
        .execute(
            "UPDATE release_source SET enabled = 0 WHERE id = 'anibt'",
            [],
        )
        .expect("disable fixture source");
    connection
        .execute(
            "INSERT INTO fansub_group (id, name, aliases_json, source_ids_json, created_at, updated_at)
             VALUES ('fansub-1', '测试字幕组', '[]', '[]', ?1, ?1)",
            [&now],
        )
        .expect("insert fansub");
    connection
        .execute(
            r#"INSERT INTO anime_catalog (
               id, title, original_title, premiere_year, premiere_month, external_ids_json,
               detail_json, created_at, updated_at
             ) VALUES ('anime-1', '测试番剧', 'Test Anime', 2026, 7, '{"bangumi":"1"}',
               '{"episodeCount":12}', ?1, ?1)"#,
            [&now],
        )
        .expect("insert anime");
    connection
        .execute(
            "INSERT INTO anime_alias (id, anime_id, alias, language, priority)
             VALUES ('alias-1', 'anime-1', '测试别名', 'zh', 10)",
            [],
        )
        .expect("insert alias");
    connection
        .execute(
            "INSERT INTO my_anime (
               id, anime_id, status, default_fansub_group_id, auto_download,
               preferred_subtitle, preferred_subtitle_languages_json, added_at, updated_at
             ) VALUES ('my-anime-1', 'anime-1', 'watching', 'fansub-1', 1,
               'multi', '[]', ?1, ?1)",
            [&now],
        )
        .expect("insert my anime");
    connection
        .execute(
            "INSERT INTO my_anime_rss_subscription (
               id, my_anime_id, name, url, enabled, preferred_subtitle,
               preferred_subtitle_languages_json, created_at, updated_at
             ) VALUES ('rss-1', 'my-anime-1', '测试 RSS', 'https://example.test/rss', 1,
               'cht', '[]', ?1, ?1)",
            [&now],
        )
        .expect("insert rss subscription");

    for (episode_no, status) in [(1_i64, "aired"), (2_i64, "aired"), (3_i64, "aired")] {
        connection
            .execute(
                "INSERT INTO episode (
                   id, anime_id, episode_no, air_time, status, created_at, updated_at
                 ) VALUES (?1, 'anime-1', ?2, ?3, ?4, ?3, ?3)",
                params![format!("episode-{episode_no}"), episode_no, &now, status],
            )
            .expect("insert episode");
    }
    insert_download(
        connection,
        "download-active",
        "episode-2",
        2,
        "downloading",
        0.5,
        &now,
    );
    insert_download(
        connection,
        "download-completed",
        "episode-3",
        3,
        "completed",
        1.0,
        &now,
    );
    connection
        .execute(
            r#"INSERT INTO media_file (
               id, anime_id, episode_id, download_task_id, file_path, file_name, size,
               normalized_video_codec, audio_codecs_json, subtitle_tracks_json, downloaded_at
             ) VALUES ('media-1', 'anime-1', 'episode-3', 'download-completed',
               'C:/video/3.mkv', '3.mkv', 1024, 'H.265/HEVC', '["aac"]', '["ass"]', ?1)"#,
            [&now],
        )
        .expect("insert media file");
    connection
        .execute(
            "INSERT INTO notification (id, kind, title, body, severity, created_at, read_at)
             VALUES ('notification-old', 'system', '已读', '旧通知', 'info',
               '2026-07-01T00:00:00.000Z', '2026-07-01T01:00:00.000Z')",
            [],
        )
        .expect("insert old notification");
    connection
        .execute(
            "INSERT INTO notification (id, kind, title, body, severity, created_at)
             VALUES ('notification-new', 'download', '下载完成', '第 3 集', 'success',
               '2026-07-02T00:00:00.000Z')",
            [],
        )
        .expect("insert unread notification");
}

/// 创建覆盖任务元数据和文件级单集关联的 P4 下载快照。
fn p4_download_task() -> DownloadTask {
    DownloadTask {
        id: "embedded:p4-download-task".to_owned(),
        release_id: None,
        anime_id: Some("anime-p3-1".to_owned()),
        episode_id: None,
        anime_title: Some("P3 契约番剧".to_owned()),
        episode_no: None,
        fansub_group_id: None,
        fansub_name: None,
        resolution: Some("1080p".to_owned()),
        declared_video_codec: Some("HEVC".to_owned()),
        normalized_video_codec: Some("H.265/HEVC".to_owned()),
        bit_depth: Some(10),
        subtitle_languages: vec!["chs".to_owned(), "cht".to_owned()],
        subtitle: Some("multi".to_owned()),
        correlation_tag: Some("p4-contract".to_owned()),
        engine: TorrentEngineKind::Embedded,
        torrent_hash: Some("p4-hash".to_owned()),
        name: "P4 batch".to_owned(),
        status: DownloadStatus::Downloading,
        progress: 0.5,
        download_speed: 1024,
        upload_speed: 128,
        eta_seconds: Some(60),
        save_path: "C:/video".to_owned(),
        files: vec![
            TorrentFile {
                id: "embedded:p4-download-task:0".to_owned(),
                index: 0,
                name: "episode-1.mkv".to_owned(),
                episode_id: Some("episode-anime-p3-1-1".to_owned()),
                episode_no: Some(1.0),
                size: 1024,
                progress: 0.5,
                priority: 1,
                selected: true,
            },
            TorrentFile {
                id: "embedded:p4-download-task:1".to_owned(),
                index: 1,
                name: "episode-2.mkv".to_owned(),
                episode_id: Some("episode-anime-p3-1-2".to_owned()),
                episode_no: Some(2.0),
                size: 2048,
                progress: 1.0,
                priority: 7,
                selected: true,
            },
        ],
        created_at: "2026-07-25T00:00:00.000Z".to_owned(),
        completed_at: None,
    }
}

/// 写入使用文件级单集关联的 P3 播放任务。
fn insert_p3_playback_download(connection: &Connection) {
    let timestamp = "2026-07-25T00:00:00.000Z";
    connection
        .execute(
            "INSERT INTO download_task (
               id, anime_id, anime_title, engine, name, status, progress,
               download_speed, upload_speed, save_path, created_at, updated_at
             ) VALUES (
               'download-p3-1', 'anime-p3-1', 'P3 契约番剧', 'embedded',
               'P3 batch', 'downloading', 0.5, 1024, 0, 'C:/video', ?1, ?1
             )",
            [timestamp],
        )
        .expect("insert p3 download task");
    connection
        .execute(
            "INSERT INTO torrent_file (
               id, download_task_id, file_index, name, episode_id, episode_no,
               size, progress, priority, selected
             ) VALUES (
               'torrent-file-p3-1', 'download-p3-1', 0, 'episode-2.mkv',
               'episode-anime-p3-1-2', 2, 1024, 0.5, 1, 1
             )",
            [],
        )
        .expect("insert p3 torrent file");
}

/// 写入首页下载状态测试记录。
fn insert_download(
    connection: &Connection,
    id: &str,
    episode_id: &str,
    episode_no: i64,
    status: &str,
    progress: f64,
    timestamp: &str,
) {
    connection
        .execute(
            "INSERT INTO download_task (
               id, anime_id, episode_id, anime_title, episode_no, fansub_group_id, fansub_name,
               engine, name, status, progress, download_speed, upload_speed, save_path,
               created_at, updated_at
             ) VALUES (?1, 'anime-1', ?2, '测试番剧', ?3, 'fansub-1', '测试字幕组',
               'embedded', ?1, ?4, ?5, 1024, 0, 'C:/video', ?6, ?6)",
            params![id, episode_id, episode_no, status, progress, timestamp],
        )
        .expect("insert download task");
}

/// 插入旧版下载源测试记录。
fn insert_source(connection: &Connection, id: &str, use_proxy: bool) {
    connection
        .execute(
            "INSERT OR REPLACE INTO release_source (
               id, name, kind, enabled, use_proxy, request_interval_ms, tags_json, created_at, updated_at
             ) VALUES (?1, ?1, 'manual', 1, ?2, 1000, '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            params![id, if use_proxy { 1_i64 } else { 0_i64 }],
        )
        .expect("insert source fixture");
}

/// 写入来源绑定外键依赖的最小番剧目录记录。
fn insert_source_binding_anime(connection: &Connection, anime_id: &str) {
    connection
        .execute(
            "INSERT INTO anime_catalog (
               id, title, premiere_year, premiere_month, external_ids_json, created_at, updated_at
             ) VALUES (?1, '来源绑定契约番', 2026, 7, '{}',
               '2026-07-25T00:00:00.000Z', '2026-07-25T00:00:00.000Z')",
            [anime_id],
        )
        .expect("insert source binding anime fixture");
}

/// 读取一项数据库版本元数据。
fn read_meta(connection: &Connection, key: &str) -> String {
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .expect("read app_meta value")
}

/// 判断表中是否存在指定列。
fn column_exists(connection: &Connection, table: &str, column: &str) -> bool {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table_info");
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query table_info")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect table_info")
        .iter()
        .any(|name| name == column)
}

/// 统计指定下载源记录。
fn source_count(connection: &Connection, source_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM release_source WHERE id = ?1",
            [source_id],
            |row| row.get(0),
        )
        .expect("source count")
}

/// 读取指定下载源代理标记。
fn source_proxy(connection: &Connection, source_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT use_proxy FROM release_source WHERE id = ?1",
            [source_id],
            |row| row.get(0),
        )
        .expect("source proxy")
}

/// 以只读模式打开测试数据库。
fn open_read_only(path: &Path) -> Connection {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open read-only database")
}

/// 测试专用内存安全存储，模拟 Keystore/Keychain 的引用读写。
#[derive(Default)]
struct MemorySecureStore {
    values: Mutex<HashMap<SecretReference, SecretValue>>,
}

impl MemorySecureStore {
    /// 读取 UTF-8 文本，便于断言真实值没有落入 SQLite。
    fn read_text(&self, key: &str) -> Option<String> {
        let reference = SecretReference {
            namespace: "ani-tracker".to_owned(),
            key: key.to_owned(),
        };
        self.values
            .lock()
            .expect("lock memory secure store")
            .get(&reference)
            .map(|value| String::from_utf8_lossy(value.expose()).into_owned())
    }
}

impl SecureStore for MemorySecureStore {
    type Error = SecureStoreError;

    /// 读取内存中的敏感值。
    fn read_secret(&self, reference: &SecretReference) -> Result<Option<SecretValue>, Self::Error> {
        Ok(self
            .values
            .lock()
            .map_err(|error| SecureStoreError(error.to_string()))?
            .get(reference)
            .cloned())
    }

    /// 写入内存中的敏感值。
    fn write_secret(
        &self,
        reference: &SecretReference,
        value: &SecretValue,
    ) -> Result<(), Self::Error> {
        self.values
            .lock()
            .map_err(|error| SecureStoreError(error.to_string()))?
            .insert(reference.clone(), value.clone());
        Ok(())
    }

    /// 删除内存中的敏感值。
    fn delete_secret(&self, reference: &SecretReference) -> Result<(), Self::Error> {
        self.values
            .lock()
            .map_err(|error| SecureStoreError(error.to_string()))?
            .remove(reference);
        Ok(())
    }
}

/// 自动清理的独立测试目录。
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    /// 创建不与并行测试冲突的临时目录。
    fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ani-storage-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self { path }
    }

    /// 返回测试目录路径。
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    /// 测试结束后删除精确创建的临时目录。
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
