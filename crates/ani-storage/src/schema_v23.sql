CREATE TABLE IF NOT EXISTS app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS app_settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS app_state (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS anime_catalog (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  original_title TEXT,
  premiere_date TEXT,
  premiere_year INTEGER NOT NULL,
  premiere_month INTEGER NOT NULL,
  season TEXT,
  summary TEXT,
  cover_url TEXT,
  rating_score REAL,
  rating_count INTEGER,
  rating_source TEXT,
  external_ids_json TEXT NOT NULL DEFAULT '{}',
  detail_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_anime_catalog_premiere_month
  ON anime_catalog (premiere_year, premiere_month);

CREATE TABLE IF NOT EXISTS anime_season_sync_state (
  year INTEGER NOT NULL,
  season TEXT NOT NULL CHECK(season IN ('winter', 'spring', 'summer', 'fall')),
  last_attempt_at TEXT,
  last_successful_sync_at TEXT,
  completed_at TEXT,
  last_anilist_error TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (year, season)
);

CREATE TABLE IF NOT EXISTS anime_detail_refresh_state (
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  provider TEXT NOT NULL CHECK(provider IN ('bangumi', 'mikan')),
  external_id TEXT NOT NULL,
  slot_day INTEGER NOT NULL CHECK(slot_day BETWEEN 0 AND 6),
  last_completed_cycle INTEGER,
  last_attempt_at TEXT,
  last_success_at TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0,
  next_retry_at TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (anime_id, provider)
);

CREATE INDEX IF NOT EXISTS idx_anime_detail_refresh_due
  ON anime_detail_refresh_state (slot_day, last_completed_cycle, next_retry_at);

CREATE TABLE IF NOT EXISTS anime_alias (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  language TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_anime_alias_alias ON anime_alias (alias);

CREATE TABLE IF NOT EXISTS fansub_group (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  aliases_json TEXT NOT NULL DEFAULT '[]',
  source_ids_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS anime_fansub_group (
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  fansub_group_id TEXT NOT NULL REFERENCES fansub_group(id) ON DELETE CASCADE,
  first_seen_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  PRIMARY KEY (anime_id, fansub_group_id)
);

CREATE INDEX IF NOT EXISTS idx_anime_fansub_group_anime
  ON anime_fansub_group (anime_id, last_seen_at DESC);

CREATE TABLE IF NOT EXISTS my_anime (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  default_fansub_group_id TEXT REFERENCES fansub_group(id) ON DELETE SET NULL,
  auto_download INTEGER NOT NULL DEFAULT 0,
  download_dir TEXT,
  preferred_resolution TEXT,
  preferred_codec TEXT,
  preferred_subtitle TEXT,
  preferred_subtitle_languages_json TEXT NOT NULL DEFAULT '[]',
  preferred_bit_depth INTEGER,
  added_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_my_anime_status ON my_anime (status);

CREATE TABLE IF NOT EXISTS my_anime_rss_subscription (
  id TEXT PRIMARY KEY,
  my_anime_id TEXT NOT NULL REFERENCES my_anime(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  preferred_subtitle TEXT,
  preferred_subtitle_languages_json TEXT NOT NULL DEFAULT '[]',
  refresh_interval_minutes INTEGER,
  last_fetched_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_my_anime_rss_subscription_my_anime
  ON my_anime_rss_subscription (my_anime_id);

CREATE TABLE IF NOT EXISTS anime_source_binding (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  source_id TEXT NOT NULL REFERENCES release_source(id) ON DELETE CASCADE,
  source_anime_id TEXT NOT NULL,
  source_anime_title TEXT,
  source_url TEXT,
  match_method TEXT NOT NULL,
  confidence REAL NOT NULL DEFAULT 0,
  confirmed INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(anime_id, source_id)
);

CREATE INDEX IF NOT EXISTS idx_anime_source_binding_source
  ON anime_source_binding (source_id, source_anime_id);

CREATE TABLE IF NOT EXISTS anime_source_exclusion (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  source_id TEXT NOT NULL REFERENCES release_source(id) ON DELETE CASCADE,
  scope TEXT NOT NULL CHECK(scope IN ('candidate', 'source')),
  source_anime_id TEXT NOT NULL DEFAULT '',
  source_anime_title TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(anime_id, source_id, source_anime_id)
);

CREATE INDEX IF NOT EXISTS idx_anime_source_exclusion_lookup
  ON anime_source_exclusion (anime_id, source_id, scope);

CREATE TABLE IF NOT EXISTS episode (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  episode_no REAL NOT NULL,
  title TEXT,
  air_time TEXT,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(anime_id, episode_no)
);

CREATE TABLE IF NOT EXISTS episode_preference (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL REFERENCES anime_catalog(id) ON DELETE CASCADE,
  episode_id TEXT NOT NULL REFERENCES episode(id) ON DELETE CASCADE,
  fansub_group_id TEXT REFERENCES fansub_group(id) ON DELETE SET NULL,
  release_id TEXT,
  is_manual_override INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  UNIQUE(episode_id)
);

CREATE TABLE IF NOT EXISTS release_source (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  use_proxy INTEGER NOT NULL DEFAULT 0,
  request_interval_ms INTEGER NOT NULL DEFAULT 1000,
  base_url TEXT,
  api_key TEXT,
  rss_url TEXT,
  tags_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS release_source_sync_state (
  source_id TEXT PRIMARY KEY REFERENCES release_source(id) ON DELETE CASCADE,
  request_host TEXT,
  last_request_at TEXT,
  request_failure_count INTEGER NOT NULL DEFAULT 0,
  backoff_until TEXT,
  last_sync_attempt_at TEXT,
  last_successful_sync_at TEXT,
  last_sync_error TEXT,
  etag TEXT,
  last_modified TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS request_circuit_state (
  circuit_key TEXT PRIMARY KEY,
  circuit_group TEXT NOT NULL,
  request_host TEXT,
  last_request_at TEXT,
  failure_count INTEGER NOT NULL DEFAULT 0,
  backoff_until TEXT,
  network_context TEXT,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_request_circuit_group_host
  ON request_circuit_state (circuit_group, request_host);

CREATE TABLE IF NOT EXISTS release (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  anime_id TEXT REFERENCES anime_catalog(id) ON DELETE SET NULL,
  episode_no REAL,
  fansub_group_id TEXT REFERENCES fansub_group(id) ON DELETE SET NULL,
  source_id TEXT NOT NULL REFERENCES release_source(id) ON DELETE CASCADE,
  source_name TEXT NOT NULL,
  magnet_url TEXT,
  torrent_url TEXT,
  info_hash TEXT,
  size INTEGER,
  resolution TEXT,
  declared_video_codec TEXT,
  normalized_video_codec TEXT,
  bit_depth INTEGER,
  subtitle TEXT,
  subtitle_languages_json TEXT NOT NULL DEFAULT '[]',
  published_at TEXT NOT NULL,
  seeders INTEGER,
  raw_json TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_release_lookup
  ON release (anime_id, episode_no, fansub_group_id, published_at);

CREATE INDEX IF NOT EXISTS idx_release_anime_source
  ON release (anime_id, source_id, published_at DESC);

CREATE TABLE IF NOT EXISTS release_search_cache (
  cache_key TEXT PRIMARY KEY,
  result_json TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_release_search_cache_expires_at
  ON release_search_cache (expires_at);

CREATE TABLE IF NOT EXISTS download_task (
  id TEXT PRIMARY KEY,
  release_id TEXT,
  anime_id TEXT,
  episode_id TEXT,
  anime_title TEXT,
  episode_no REAL,
  fansub_group_id TEXT,
  fansub_name TEXT,
  resolution TEXT,
  declared_video_codec TEXT,
  normalized_video_codec TEXT,
  bit_depth INTEGER,
  subtitle_languages_json TEXT NOT NULL DEFAULT '[]',
  subtitle TEXT,
  correlation_tag TEXT,
  engine TEXT NOT NULL,
  torrent_hash TEXT,
  name TEXT NOT NULL,
  status TEXT NOT NULL,
  progress REAL NOT NULL DEFAULT 0,
  download_speed INTEGER NOT NULL DEFAULT 0,
  upload_speed INTEGER NOT NULL DEFAULT 0,
  eta_seconds INTEGER,
  save_path TEXT NOT NULL,
  created_at TEXT NOT NULL,
  completed_at TEXT,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_download_task_status ON download_task (status);
CREATE INDEX IF NOT EXISTS idx_download_task_torrent_hash ON download_task (torrent_hash);
CREATE INDEX IF NOT EXISTS idx_download_task_correlation_tag ON download_task (correlation_tag);

CREATE TABLE IF NOT EXISTS torrent_file (
  id TEXT PRIMARY KEY,
  download_task_id TEXT NOT NULL REFERENCES download_task(id) ON DELETE CASCADE,
  file_index INTEGER NOT NULL,
  name TEXT NOT NULL,
  episode_id TEXT REFERENCES episode(id) ON DELETE SET NULL,
  episode_no REAL,
  size INTEGER NOT NULL,
  progress REAL NOT NULL DEFAULT 0,
  priority INTEGER NOT NULL DEFAULT 0,
  selected INTEGER NOT NULL DEFAULT 1,
  UNIQUE(download_task_id, file_index)
);

CREATE TABLE IF NOT EXISTS media_file (
  id TEXT PRIMARY KEY,
  anime_id TEXT NOT NULL,
  episode_id TEXT,
  download_task_id TEXT,
  content_kind TEXT NOT NULL DEFAULT 'unknown',
  special_no TEXT,
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
  probed_at TEXT,
  origin TEXT NOT NULL DEFAULT 'download',
  source_root TEXT,
  fingerprint TEXT,
  file_modified_at TEXT,
  availability TEXT NOT NULL DEFAULT 'available',
  last_verified_at TEXT,
  availability_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_media_file_anime_episode
  ON media_file (anime_id, episode_id);

CREATE TABLE IF NOT EXISTS playback_checkpoint (
  task_id TEXT NOT NULL REFERENCES download_task(id) ON DELETE CASCADE,
  file_index INTEGER NOT NULL DEFAULT -1,
  position_seconds REAL NOT NULL DEFAULT 0,
  duration_seconds REAL NOT NULL DEFAULT 0,
  completed INTEGER NOT NULL DEFAULT 0,
  watched_reported INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (task_id, file_index)
);

CREATE INDEX IF NOT EXISTS idx_playback_checkpoint_updated_at
  ON playback_checkpoint (updated_at DESC);

CREATE TABLE IF NOT EXISTS notification (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  severity TEXT NOT NULL,
  anime_id TEXT,
  episode_id TEXT,
  download_task_id TEXT,
  created_at TEXT NOT NULL,
  read_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_notification_created_at ON notification (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notification_unread ON notification (read_at, created_at DESC);
