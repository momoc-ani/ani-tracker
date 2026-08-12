use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ani_domain::{DownloadStatus, DownloadTask, TorrentEngineKind, TorrentFile};
use async_trait::async_trait;
use chrono::{SecondsFormat, TimeZone, Utc};
use data_encoding::BASE32_NOPAD;
use futures_util::future::try_join_all;
use reqwest::header::{COOKIE, SET_COOKIE};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::{
    AddTorrentOptions, DownloadEngine, DownloadEngineConfig, DownloadEngineError,
    DownloadEngineStatus,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONFIRMATION_INTERVAL: Duration = Duration::from_millis(250);
static CORRELATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// qBittorrent Web API 的可动态更新连接参数。
#[derive(Clone, PartialEq, Eq)]
pub struct QbittorrentConnectionConfig {
    pub base_url: String,
    pub username: String,
    pub password: Option<String>,
    pub request_timeout: Duration,
    pub confirmation_timeout: Duration,
    pub confirmation_interval: Duration,
}

impl QbittorrentConnectionConfig {
    /// 使用稳定超时创建 qBittorrent 连接参数。
    pub fn new(base_url: String, username: String, password: Option<String>) -> Self {
        Self {
            base_url,
            username,
            password,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            confirmation_timeout: DEFAULT_CONFIRMATION_TIMEOUT,
            confirmation_interval: DEFAULT_CONFIRMATION_INTERVAL,
        }
    }

    /// 校验协议、主机和轮询边界，并规范基础地址。
    fn normalized(&self) -> Result<Self, DownloadEngineError> {
        let mut url = Url::parse(self.base_url.trim()).map_err(|error| {
            DownloadEngineError::InvalidInput(format!("qBittorrent WebUI 地址无效：{error}"))
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(DownloadEngineError::InvalidInput(
                "qBittorrent WebUI 仅允许带主机的 HTTP 或 HTTPS 地址".to_owned(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(DownloadEngineError::InvalidInput(
                "qBittorrent 凭据必须通过独立设置提供".to_owned(),
            ));
        }
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
        let mut normalized = self.clone();
        normalized.base_url = url.to_string();
        normalized.username = normalized.username.trim().to_owned();
        normalized.password = normalized.password.filter(|value| !value.is_empty());
        normalized.request_timeout = normalized
            .request_timeout
            .clamp(Duration::from_secs(1), Duration::from_secs(60));
        normalized.confirmation_timeout = normalized
            .confirmation_timeout
            .clamp(Duration::from_millis(250), Duration::from_secs(60));
        normalized.confirmation_interval = normalized
            .confirmation_interval
            .clamp(Duration::from_millis(25), normalized.confirmation_timeout);
        Ok(normalized)
    }
}

#[derive(Clone)]
struct QbittorrentSession {
    config: QbittorrentConnectionConfig,
    cookie: Option<String>,
}

/// 将 qBittorrent Web API 适配为统一下载引擎端口。
pub struct QbittorrentEngine {
    client: reqwest::Client,
    connection: RwLock<QbittorrentConnectionConfig>,
    session: Mutex<Option<QbittorrentSession>>,
}

impl QbittorrentEngine {
    /// 创建支持外部或托管 WebUI 的 qBittorrent 引擎。
    pub fn new(config: QbittorrentConnectionConfig) -> Result<Self, DownloadEngineError> {
        let config = config.normalized()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(transport_error)?;
        Ok(Self {
            client,
            connection: RwLock::new(config),
            session: Mutex::new(None),
        })
    }

    /// 设置变化或托管端口变化后更新连接并清除旧会话。
    pub async fn update_connection(
        &self,
        config: QbittorrentConnectionConfig,
    ) -> Result<(), DownloadEngineError> {
        let config = config.normalized()?;
        let changed = *self.connection.read().await != config;
        if changed {
            *self.connection.write().await = config;
            *self.session.lock().await = None;
            log::info!("qBittorrent 连接设置已更新，旧会话已清除");
        }
        Ok(())
    }

    /// 托管进程首次启动后同步固定 WebUI 账号，并关闭 WebUI UPnP 暴露。
    pub async fn update_webui_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), DownloadEngineError> {
        let preferences = json!({
            "web_ui_username": username,
            "web_ui_password": password,
            "web_ui_address": "127.0.0.1",
            "web_ui_upnp": false,
            "bypass_local_auth": false,
            "web_ui_csrf_protection_enabled": true,
            "web_ui_host_header_validation_enabled": true
        });
        self.response_text(|client, connection| {
            Ok(client
                .post(endpoint(connection, "/api/v2/app/setPreferences")?)
                .timeout(connection.request_timeout)
                .form(&[("json", preferences.to_string())]))
        })
        .await?;
        *self.session.lock().await = None;
        Ok(())
    }

    /// 请求托管 qBittorrent 保存状态并退出。
    pub async fn shutdown_application(&self) -> Result<(), DownloadEngineError> {
        self.response_text(|client, connection| {
            Ok(client
                .post(endpoint(connection, "/api/v2/app/shutdown")?)
                .timeout(connection.request_timeout))
        })
        .await?;
        *self.session.lock().await = None;
        Ok(())
    }

    /// 返回当前连接参数快照，避免在网络等待期间持有读锁。
    async fn connection(&self) -> QbittorrentConnectionConfig {
        self.connection.read().await.clone()
    }

    /// 登录 WebUI 并缓存与当前连接参数绑定的 SID。
    async fn authenticated_session(&self) -> Result<QbittorrentSession, DownloadEngineError> {
        let config = self.connection().await;
        let mut session = self.session.lock().await;
        if let Some(current) = session.as_ref().filter(|item| item.config == config) {
            return Ok(current.clone());
        }
        let url = endpoint(&config, "/api/v2/auth/login")?;
        let response = self
            .client
            .post(url)
            .timeout(config.request_timeout)
            .form(&[
                ("username", config.username.as_str()),
                ("password", config.password.as_deref().unwrap_or_default()),
            ])
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let body = if status == StatusCode::NO_CONTENT {
            "Ok.".to_owned()
        } else {
            response.text().await.map_err(transport_error)?
        };
        if !status.is_success() || !body.trim().to_ascii_lowercase().starts_with("ok") {
            return Err(DownloadEngineError::Unavailable(format!(
                "qBittorrent 登录失败：HTTP {status}"
            )));
        }
        let authenticated = QbittorrentSession { config, cookie };
        *session = Some(authenticated.clone());
        Ok(authenticated)
    }

    /// 构建带当前会话、超时和固定 API 路径的请求。
    async fn send_authenticated<F>(&self, build: F) -> Result<Response, DownloadEngineError>
    where
        F: Fn(
            &reqwest::Client,
            &QbittorrentConnectionConfig,
        ) -> Result<RequestBuilder, DownloadEngineError>,
    {
        let mut session = self.authenticated_session().await?;
        let mut response = send_request(build(&self.client, &session.config)?, &session).await?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            *self.session.lock().await = None;
            session = self.authenticated_session().await?;
            response = send_request(build(&self.client, &session.config)?, &session).await?;
        }
        Ok(response)
    }

    /// 执行 API 请求并返回状态与正文，供幂等冲突处理保留协议信息。
    async fn response_status_text<F>(
        &self,
        build: F,
    ) -> Result<(StatusCode, String), DownloadEngineError>
    where
        F: Fn(
            &reqwest::Client,
            &QbittorrentConnectionConfig,
        ) -> Result<RequestBuilder, DownloadEngineError>,
    {
        let response = self.send_authenticated(build).await?;
        let status = response.status();
        let body = response.text().await.map_err(transport_error)?;
        Ok((status, body))
    }

    /// 执行 API 请求并要求成功状态，正文用于 JSON 或添加回执解析。
    async fn response_text<F>(&self, build: F) -> Result<String, DownloadEngineError>
    where
        F: Fn(
            &reqwest::Client,
            &QbittorrentConnectionConfig,
        ) -> Result<RequestBuilder, DownloadEngineError>,
    {
        let (status, body) = self.response_status_text(build).await?;
        if !status.is_success() {
            return Err(qbittorrent_status_error(status, &body));
        }
        Ok(body)
    }

    /// 读取不含文件详情的任务列表。
    async fn list_torrent_info(&self) -> Result<Vec<QbittorrentTorrentInfo>, DownloadEngineError> {
        let body = self
            .response_text(|client, config| {
                Ok(client
                    .get(endpoint(config, "/api/v2/torrents/info")?)
                    .timeout(config.request_timeout))
            })
            .await?;
        serde_json::from_str(&body).map_err(|error| {
            DownloadEngineError::Protocol(format!("qBittorrent 任务响应无效：{error}"))
        })
    }

    /// 在添加冲突时按磁链 BTIH 恢复 qBittorrent 已存在任务。
    async fn recover_existing_magnet(
        &self,
        magnet_url: &str,
    ) -> Result<Option<DownloadTask>, DownloadEngineError> {
        let Some(expected_hash) = magnet_btih_hash(magnet_url) else {
            return Ok(None);
        };
        let Some(torrent) = self
            .list_torrent_info()
            .await?
            .into_iter()
            .find(|torrent| torrent.hash.eq_ignore_ascii_case(&expected_hash))
        else {
            return Ok(None);
        };
        let files = self
            .get_files_internal(&torrent.hash)
            .await
            .unwrap_or_else(|error| {
                log::warn!(
                    "qBittorrent 已有任务文件读取失败，按空文件快照恢复 hash={} error={error}",
                    torrent.hash
                );
                Vec::new()
            });
        map_torrent(torrent, files).map(Some)
    }

    /// 读取单个任务文件并归一化选择状态。
    async fn get_files_internal(
        &self,
        task_id: &str,
    ) -> Result<Vec<TorrentFile>, DownloadEngineError> {
        let task_id = task_id.to_owned();
        let body = self
            .response_text(|client, config| {
                Ok(client
                    .get(endpoint(config, "/api/v2/torrents/files")?)
                    .timeout(config.request_timeout)
                    .query(&[("hash", task_id.as_str())]))
            })
            .await?;
        let files: Vec<QbittorrentTorrentFile> = serde_json::from_str(&body).map_err(|error| {
            DownloadEngineError::Protocol(format!("qBittorrent 文件响应无效：{error}"))
        })?;
        Ok(files
            .into_iter()
            .filter(|file| file.index >= 0)
            .map(|file| TorrentFile {
                id: format!("{task_id}:{}", file.index),
                index: file.index,
                name: file.name,
                episode_id: None,
                episode_no: None,
                size: file.size.max(0),
                progress: file.progress.clamp(0.0, 1.0),
                priority: file.priority.clamp(0, 7),
                selected: file.priority > 0,
            })
            .collect())
    }

    /// 轮询新增任务的真实哈希，避免持久化无法控制的占位记录。
    async fn confirm_added(
        &self,
        correlation_tag: &str,
        expected_ids: &[String],
    ) -> Result<DownloadTask, DownloadEngineError> {
        let config = self.connection().await;
        let expected = expected_ids
            .iter()
            .map(|value| value.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let deadline = tokio::time::Instant::now() + config.confirmation_timeout;
        loop {
            let torrents = self.list_torrent_info().await?;
            let matched = torrents
                .iter()
                .find(|item| expected.contains(&item.hash.to_ascii_lowercase()))
                .cloned()
                .or_else(|| {
                    torrents.into_iter().find(|item| {
                        item.tags
                            .split(',')
                            .map(str::trim)
                            .any(|tag| tag == correlation_tag)
                    })
                });
            if let Some(torrent) = matched {
                let files = self
                    .get_files_internal(&torrent.hash)
                    .await
                    .unwrap_or_else(|error| {
                        log::warn!(
                            "qBittorrent 新任务文件尚未就绪 hash={} error={error}",
                            torrent.hash
                        );
                        Vec::new()
                    });
                return map_torrent(torrent, files);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DownloadEngineError::Unavailable(format!(
                    "qBittorrent 未在 {}ms 内确认新增任务",
                    config.confirmation_timeout.as_millis()
                )));
            }
            tokio::time::sleep(config.confirmation_interval).await;
        }
    }

    /// 提交磁链或 torrent 文件后解析 Enhanced 版回执并确认任务。
    async fn finish_add(
        &self,
        response: String,
        correlation_tag: &str,
        options: &AddTorrentOptions,
    ) -> Result<DownloadTask, DownloadEngineError> {
        let expected_ids = parse_add_response(&response)?;
        let mut task = self.confirm_added(correlation_tag, &expected_ids).await?;
        if let Some(selected) = options.selected_file_indexes.as_deref() {
            let selected_set = selected.iter().copied().collect::<HashSet<_>>();
            let skipped = task
                .files
                .iter()
                .map(|file| file.index)
                .filter(|index| !selected_set.contains(index))
                .collect::<Vec<_>>();
            if !skipped.is_empty() {
                self.set_file_priority(&task.id, &skipped, 0).await?;
            }
            if !selected.is_empty() {
                self.set_file_priority(&task.id, selected, 1).await?;
            }
            task.files = self.get_files_internal(&task.id).await?;
        }
        Ok(task)
    }

    /// 调用 qBittorrent 5 动作端点并在 404 时回退到 qBittorrent 4。
    async fn torrent_action(
        &self,
        task_id: &str,
        current_path: &'static str,
        legacy_path: &'static str,
    ) -> Result<(), DownloadEngineError> {
        let task_id = task_id.to_owned();
        let response = self
            .send_authenticated(|client, config| {
                Ok(client
                    .post(endpoint(config, current_path)?)
                    .timeout(config.request_timeout)
                    .form(&[("hashes", task_id.as_str())]))
            })
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            self.response_text(|client, config| {
                Ok(client
                    .post(endpoint(config, legacy_path)?)
                    .timeout(config.request_timeout)
                    .form(&[("hashes", task_id.as_str())]))
            })
            .await?;
            return Ok(());
        }
        let status = response.status();
        if !status.is_success() {
            return Err(DownloadEngineError::Protocol(format!(
                "qBittorrent 任务操作失败：HTTP {status}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl DownloadEngine for QbittorrentEngine {
    fn kind(&self) -> TorrentEngineKind {
        TorrentEngineKind::Qbittorrent
    }

    async fn status(&self) -> Result<DownloadEngineStatus, DownloadEngineError> {
        let version = self
            .response_text(|client, config| {
                Ok(client
                    .get(endpoint(config, "/api/v2/app/version")?)
                    .timeout(config.request_timeout))
            })
            .await?;
        Ok(DownloadEngineStatus {
            version: version.trim().to_owned(),
            task_count: self.list_torrent_info().await?.len(),
            listen_port: None,
            network_policy_blocked: false,
        })
    }

    async fn configure(
        &self,
        config: &DownloadEngineConfig,
    ) -> Result<DownloadEngineStatus, DownloadEngineError> {
        for (path, value) in [
            (
                "/api/v2/transfer/setDownloadLimit",
                u64::from(config.max_download_speed) * 1024,
            ),
            (
                "/api/v2/transfer/setUploadLimit",
                u64::from(config.max_upload_speed) * 1024,
            ),
        ] {
            self.response_text(|client, connection| {
                Ok(client
                    .post(endpoint(connection, path)?)
                    .timeout(connection.request_timeout)
                    .form(&[("limit", value.to_string())]))
            })
            .await?;
        }
        let limits = &config.seeding_limits;
        let ratio_limit = if !limits.enabled {
            0.0
        } else if limits.ratio_enabled {
            limits.ratio_limit.max(0.1)
        } else {
            -1.0
        };
        let time_limit = if limits.enabled && limits.time_enabled {
            i64::from(limits.time_limit_minutes.max(1))
        } else {
            -1
        };
        let preferences = json!({
            "max_ratio_enabled": ratio_limit >= 0.0,
            "max_ratio": ratio_limit,
            "max_seeding_time_enabled": time_limit >= 0,
            "max_seeding_time": time_limit,
            "max_ratio_act": 0
        });
        self.response_text(|client, connection| {
            Ok(client
                .post(endpoint(connection, "/api/v2/app/setPreferences")?)
                .timeout(connection.request_timeout)
                .form(&[("json", preferences.to_string())]))
        })
        .await?;
        self.status().await
    }

    async fn add_magnet(
        &self,
        url: &str,
        options: &AddTorrentOptions,
    ) -> Result<DownloadTask, DownloadEngineError> {
        if !url.trim().to_ascii_lowercase().starts_with("magnet:?") {
            return Err(DownloadEngineError::InvalidInput(
                "qBittorrent 仅接受 magnet:? 磁链".to_owned(),
            ));
        }
        let correlation_tag = options
            .correlation_tag
            .clone()
            .unwrap_or_else(create_correlation_tag);
        let url = url.to_owned();
        let save_path = options.save_path.clone();
        let paused = options.paused.to_string();
        let tag = correlation_tag.clone();
        let (status, response) = self
            .response_status_text(|client, config| {
                Ok(client
                    .post(endpoint(config, "/api/v2/torrents/add")?)
                    .timeout(config.request_timeout)
                    .form(&[
                        ("urls", url.as_str()),
                        ("savepath", save_path.as_str()),
                        ("paused", paused.as_str()),
                        ("tags", tag.as_str()),
                    ]))
            })
            .await?;
        if status == StatusCode::CONFLICT {
            match self.recover_existing_magnet(&url).await {
                Ok(Some(task)) => {
                    log::info!(
                        "qBittorrent 磁链已存在，按 BTIH 幂等恢复：hash={}, tag={correlation_tag}",
                        task.torrent_hash.as_deref().unwrap_or(task.id.as_str())
                    );
                    return Ok(task);
                }
                Ok(None) => {}
                Err(error) => log::warn!(
                    "qBittorrent 添加冲突后恢复已有任务失败：tag={correlation_tag}, error={error}"
                ),
            }
        }
        if !status.is_success() {
            return Err(qbittorrent_status_error(status, &response));
        }
        log::info!("qBittorrent 磁链已提交 tag={correlation_tag}");
        self.finish_add(response, &correlation_tag, options).await
    }

    async fn add_torrent_file(
        &self,
        file_path: &Path,
        options: &AddTorrentOptions,
    ) -> Result<DownloadTask, DownloadEngineError> {
        let bytes = tokio::fs::read(file_path).await.map_err(transport_error)?;
        let file_name = file_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("download.torrent")
            .to_owned();
        let correlation_tag = options
            .correlation_tag
            .clone()
            .unwrap_or_else(create_correlation_tag);
        let save_path = options.save_path.clone();
        let paused = options.paused.to_string();
        let tag = correlation_tag.clone();
        let response = self
            .response_text(|client, config| {
                let part = reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name(file_name.clone())
                    .mime_str("application/x-bittorrent")
                    .map_err(|error| DownloadEngineError::InvalidInput(error.to_string()))?;
                let form = reqwest::multipart::Form::new()
                    .part("torrents", part)
                    .text("savepath", save_path.clone())
                    .text("paused", paused.clone())
                    .text("tags", tag.clone());
                Ok(client
                    .post(endpoint(config, "/api/v2/torrents/add")?)
                    .timeout(config.request_timeout)
                    .multipart(form))
            })
            .await?;
        log::info!("qBittorrent torrent 文件已提交 tag={correlation_tag}");
        self.finish_add(response, &correlation_tag, options).await
    }

    async fn list_tasks(&self) -> Result<Vec<DownloadTask>, DownloadEngineError> {
        let torrents = self.list_torrent_info().await?;
        try_join_all(torrents.into_iter().map(|torrent| async move {
            let files = self.get_files_internal(&torrent.hash).await?;
            map_torrent(torrent, files)
        }))
        .await
    }

    async fn get_task(&self, task_id: &str) -> Result<DownloadTask, DownloadEngineError> {
        let task_id = task_id.to_owned();
        let body = self
            .response_text(|client, config| {
                Ok(client
                    .get(endpoint(config, "/api/v2/torrents/info")?)
                    .timeout(config.request_timeout)
                    .query(&[("hashes", task_id.as_str())]))
            })
            .await?;
        let torrent = serde_json::from_str::<Vec<QbittorrentTorrentInfo>>(&body)
            .map_err(|error| DownloadEngineError::Protocol(error.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| DownloadEngineError::TaskNotFound(task_id.clone()))?;
        let files = self.get_files_internal(&torrent.hash).await?;
        map_torrent(torrent, files)
    }

    async fn get_files(&self, task_id: &str) -> Result<Vec<TorrentFile>, DownloadEngineError> {
        self.get_files_internal(task_id).await
    }

    async fn set_file_priority(
        &self,
        task_id: &str,
        file_indexes: &[i64],
        priority: i64,
    ) -> Result<(), DownloadEngineError> {
        let task_id = task_id.to_owned();
        let indexes = file_indexes
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join("|");
        let priority = priority.to_string();
        self.response_text(|client, config| {
            Ok(client
                .post(endpoint(config, "/api/v2/torrents/filePrio")?)
                .timeout(config.request_timeout)
                .form(&[
                    ("hash", task_id.as_str()),
                    ("id", indexes.as_str()),
                    ("priority", priority.as_str()),
                ]))
        })
        .await?;
        Ok(())
    }

    async fn pause(&self, task_id: &str) -> Result<(), DownloadEngineError> {
        self.torrent_action(task_id, "/api/v2/torrents/stop", "/api/v2/torrents/pause")
            .await
    }

    async fn resume(&self, task_id: &str) -> Result<(), DownloadEngineError> {
        self.torrent_action(task_id, "/api/v2/torrents/start", "/api/v2/torrents/resume")
            .await
    }

    async fn remove(&self, task_id: &str, delete_files: bool) -> Result<(), DownloadEngineError> {
        let task_id = task_id.to_owned();
        let delete_files = delete_files.to_string();
        self.response_text(|client, config| {
            Ok(client
                .post(endpoint(config, "/api/v2/torrents/delete")?)
                .timeout(config.request_timeout)
                .form(&[
                    ("hashes", task_id.as_str()),
                    ("deleteFiles", delete_files.as_str()),
                ]))
        })
        .await?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), DownloadEngineError> {
        *self.session.lock().await = None;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct QbittorrentTorrentInfo {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    dlspeed: i64,
    #[serde(default)]
    upspeed: i64,
    #[serde(default)]
    eta: i64,
    #[serde(default)]
    save_path: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    added_on: i64,
    #[serde(default)]
    completion_on: i64,
}

#[derive(Debug, Deserialize)]
struct QbittorrentTorrentFile {
    index: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    priority: i64,
}

#[derive(Debug, Default, Deserialize)]
struct EnhancedAddResponse {
    #[serde(default)]
    added_torrent_ids: Vec<String>,
    #[serde(default)]
    success_count: u64,
    #[serde(default)]
    pending_count: u64,
}

fn map_torrent(
    torrent: QbittorrentTorrentInfo,
    files: Vec<TorrentFile>,
) -> Result<DownloadTask, DownloadEngineError> {
    if torrent.hash.trim().is_empty() {
        return Err(DownloadEngineError::Protocol(
            "qBittorrent 任务缺少 hash".to_owned(),
        ));
    }
    let id = torrent.hash.to_ascii_lowercase();
    let status = map_qbittorrent_status(&torrent.state);
    Ok(DownloadTask {
        id: id.clone(),
        release_id: None,
        anime_id: None,
        episode_id: None,
        anime_title: None,
        episode_no: None,
        fansub_group_id: None,
        fansub_name: None,
        resolution: None,
        declared_video_codec: None,
        normalized_video_codec: None,
        bit_depth: None,
        subtitle_languages: Vec::new(),
        subtitle: None,
        correlation_tag: extract_correlation_tag(&torrent.tags),
        engine: TorrentEngineKind::Qbittorrent,
        torrent_hash: Some(id),
        name: if torrent.name.trim().is_empty() {
            torrent.hash
        } else {
            torrent.name
        },
        status,
        progress: torrent.progress.clamp(0.0, 1.0),
        download_speed: torrent.dlspeed.max(0),
        upload_speed: torrent.upspeed.max(0),
        eta_seconds: (torrent.eta > 0 && torrent.eta < 8_640_000).then_some(torrent.eta),
        save_path: torrent.save_path,
        files,
        created_at: epoch_to_iso(torrent.added_on).unwrap_or_else(now_iso),
        completed_at: epoch_to_iso(torrent.completion_on),
    })
}

fn map_qbittorrent_status(state: &str) -> DownloadStatus {
    match state {
        "metaDL" | "forcedMetaDL" => DownloadStatus::FetchingMetadata,
        "downloading" | "forcedDL" => DownloadStatus::Downloading,
        "stalledDL" => DownloadStatus::Stalled,
        "queuedDL" => DownloadStatus::Queued,
        "pausedDL" | "stoppedDL" => DownloadStatus::Paused,
        "checkingDL" | "checkingUP" | "checkingResumeData" => DownloadStatus::Checking,
        "moving" => DownloadStatus::Moving,
        "uploading" | "forcedUP" | "stalledUP" => DownloadStatus::Seeding,
        "queuedUP" | "pausedUP" | "stoppedUP" => DownloadStatus::Completed,
        "missingFiles" => DownloadStatus::MissingFiles,
        "error" | "unknown" => DownloadStatus::Error,
        _ => DownloadStatus::Queued,
    }
}

fn parse_add_response(body: &str) -> Result<Vec<String>, DownloadEngineError> {
    let body = body.trim();
    if body.is_empty() || body.to_ascii_lowercase().starts_with("ok") {
        return Ok(Vec::new());
    }
    let response: EnhancedAddResponse = serde_json::from_str(body)
        .map_err(|_| DownloadEngineError::Protocol(format!("qBittorrent 添加任务失败：{body}")))?;
    if response.success_count == 0
        && response.pending_count == 0
        && response.added_torrent_ids.is_empty()
    {
        return Err(DownloadEngineError::Protocol(format!(
            "qBittorrent 添加任务失败：{body}"
        )));
    }
    Ok(response.added_torrent_ids)
}

/// 从磁链提取并规范为 qBittorrent 使用的 40 位十六进制 BTIH。
fn magnet_btih_hash(magnet_url: &str) -> Option<String> {
    let url = Url::parse(magnet_url.trim()).ok()?;
    url.query_pairs().find_map(|(key, value)| {
        if !key.eq_ignore_ascii_case("xt") {
            return None;
        }
        let value = value.as_ref();
        if !value
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
        {
            return None;
        }
        let raw = value.get(9..)?;
        if raw.len() == 40 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(raw.to_ascii_lowercase());
        }
        if raw.len() != 32 {
            return None;
        }
        let bytes = BASE32_NOPAD
            .decode(raw.to_ascii_uppercase().as_bytes())
            .ok()?;
        if bytes.len() != 20 {
            return None;
        }
        let mut hash = String::with_capacity(40);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            hash.push(HEX[(byte >> 4) as usize] as char);
            hash.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Some(hash)
    })
}

fn extract_correlation_tag(tags: &str) -> Option<String> {
    tags.split(',')
        .map(str::trim)
        .find(|tag| tag.starts_with("ani:") || tag.starts_with("ani-tracker-"))
        .map(str::to_owned)
}

fn create_correlation_tag() -> String {
    let sequence = CORRELATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "ani-tracker-rust-{}-{}-{sequence}",
        std::process::id(),
        Utc::now().timestamp_millis()
    )
}

fn endpoint(config: &QbittorrentConnectionConfig, path: &str) -> Result<Url, DownloadEngineError> {
    Url::parse(&config.base_url)
        .and_then(|base| base.join(path))
        .map_err(|error| DownloadEngineError::InvalidInput(error.to_string()))
}

async fn send_request(
    request: RequestBuilder,
    session: &QbittorrentSession,
) -> Result<Response, DownloadEngineError> {
    let request = match session.cookie.as_deref() {
        Some(cookie) => request.header(COOKIE, cookie),
        None => request,
    };
    request.send().await.map_err(transport_error)
}

fn epoch_to_iso(value: i64) -> Option<String> {
    (value > 0)
        .then(|| Utc.timestamp_opt(value, 0).single())
        .flatten()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn transport_error(error: impl std::fmt::Display) -> DownloadEngineError {
    DownloadEngineError::Transport(error.to_string())
}

fn qbittorrent_status_error(status: StatusCode, body: &str) -> DownloadEngineError {
    DownloadEngineError::Protocol(format!(
        "qBittorrent 请求失败：HTTP {status} {}",
        body.trim()
    ))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// 验证 qBittorrent 状态和时间字段映射保持 Electron 语义。
    #[test]
    fn maps_qbittorrent_task_snapshot() {
        let task = map_torrent(
            QbittorrentTorrentInfo {
                hash: "ABCDEF".to_owned(),
                name: "Episode".to_owned(),
                state: "stoppedUP".to_owned(),
                progress: 1.0,
                dlspeed: 0,
                upspeed: 0,
                eta: 8_640_000,
                save_path: "C:/Downloads".to_owned(),
                tags: "other,ani:anime-1:1:release-1".to_owned(),
                added_on: 1_700_000_000,
                completion_on: 1_700_000_100,
            },
            Vec::new(),
        )
        .expect("map qBittorrent task");

        assert_eq!(task.id, "abcdef");
        assert_eq!(task.status, DownloadStatus::Completed);
        assert_eq!(
            task.correlation_tag.as_deref(),
            Some("ani:anime-1:1:release-1")
        );
        assert!(task.eta_seconds.is_none());
        assert!(task.completed_at.is_some());
    }

    /// 验证经典版与 Enhanced 版添加回执均可接受，失败正文被拒绝。
    #[test]
    fn parses_qbittorrent_add_responses() {
        assert!(parse_add_response("Ok.").expect("classic add").is_empty());
        assert_eq!(
            parse_add_response(
                r#"{"added_torrent_ids":["abc"],"success_count":1,"pending_count":0}"#
            )
            .expect("enhanced add"),
            vec!["abc"]
        );
        assert!(parse_add_response("Fails.").is_err());
    }

    /// 验证十六进制和 Base32 磁链均规范为相同 BTIH。
    #[test]
    fn parses_magnet_btih_hashes() {
        let bytes = [
            0x54, 0x48, 0xae, 0x0e, 0xd3, 0x69, 0x12, 0xeb, 0x0d, 0xfb, 0xa5, 0x3c, 0x3e, 0x49,
            0x5b, 0x99, 0x88, 0x84, 0x1e, 0x68,
        ];
        let expected = "5448ae0ed36912eb0dfba53c3e495b9988841e68";
        assert_eq!(
            magnet_btih_hash(&format!(
                "magnet:?xt=URN:BTIH:{}",
                expected.to_ascii_uppercase()
            ))
            .as_deref(),
            Some(expected)
        );
        assert_eq!(
            magnet_btih_hash(&format!(
                "magnet:?xt=urn:btih:{}",
                BASE32_NOPAD.encode(&bytes)
            ))
            .as_deref(),
            Some(expected)
        );
    }

    /// 验证连接地址不会携带凭据、查询参数或非 HTTP 协议。
    #[test]
    fn validates_qbittorrent_connection() {
        let normalized = QbittorrentConnectionConfig::new(
            "http://127.0.0.1:18080/path?secret=1".to_owned(),
            " admin ".to_owned(),
            Some(String::new()),
        )
        .normalized()
        .expect("normalize qBittorrent config");
        assert_eq!(normalized.base_url, "http://127.0.0.1:18080/");
        assert_eq!(normalized.username, "admin");
        assert!(normalized.password.is_none());
        assert!(QbittorrentConnectionConfig::new(
            "file:///tmp/qbit".to_owned(),
            String::new(),
            None
        )
        .normalized()
        .is_err());
    }

    /// 验证登录 Cookie、经典添加回执、标签确认和文件列表形成完整任务。
    #[tokio::test]
    async fn adds_magnet_through_qbittorrent_web_api() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind qBittorrent mock");
        let address = listener.local_addr().expect("read mock address");
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.expect("accept qBittorrent request");
                let request = read_http_request(&mut stream).await;
                let first_line = request.lines().next().unwrap_or_default();
                let (body, extra_headers) = if first_line.contains("/api/v2/auth/login") {
                    assert!(request.contains("username=admin"));
                    ("Ok.".to_owned(), "Set-Cookie: SID=test-session\r\n")
                } else if first_line.contains("/api/v2/torrents/add") {
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("cookie: sid=test-session"));
                    assert!(request.contains("tags=ani%3Atest"));
                    assert!(request.contains(
                        "urls=magnet%3A%3Fxt%3Durn%3Abtih%3A5448ae0ed36912eb0dfba53c3e495b9988841e68%26dn%3DEpisode"
                    ));
                    ("Ok.".to_owned(), "")
                } else if first_line.contains("/api/v2/torrents/info") {
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("cookie: sid=test-session"));
                    (
                        r#"[{"hash":"ABC","name":"Episode","state":"downloading","progress":0.25,"dlspeed":1024,"upspeed":16,"eta":60,"save_path":"C:/Downloads","tags":"ani:test","added_on":1700000000,"completion_on":0}]"#.to_owned(),
                        "",
                    )
                } else if first_line.contains("/api/v2/torrents/files") {
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("cookie: sid=test-session"));
                    (
                        r#"[{"index":0,"name":"episode.mkv","size":4096,"progress":0.25,"priority":1}]"#.to_owned(),
                        "",
                    )
                } else {
                    panic!("unexpected qBittorrent request: {first_line}");
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write qBittorrent response");
            }
        });
        let engine = QbittorrentEngine::new(QbittorrentConnectionConfig::new(
            format!("http://{address}"),
            "admin".to_owned(),
            Some("secret".to_owned()),
        ))
        .expect("create qBittorrent engine");

        let task = engine
            .add_magnet(
                "magnet:?xt=urn:btih:5448ae0ed36912eb0dfba53c3e495b9988841e68&dn=Episode",
                &AddTorrentOptions {
                    save_path: "C:/Downloads".to_owned(),
                    correlation_tag: Some("ani:test".to_owned()),
                    ..AddTorrentOptions::default()
                },
            )
            .await
            .expect("add qBittorrent magnet");

        assert_eq!(task.id, "abc");
        assert_eq!(task.files.len(), 1);
        assert_eq!(task.correlation_tag.as_deref(), Some("ani:test"));
        server.await.expect("finish qBittorrent mock");
    }

    /// 验证相同 BTIH 的 409 冲突会恢复已有任务而不是上报失败。
    #[tokio::test]
    async fn recovers_existing_magnet_after_conflict() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind qBittorrent conflict mock");
        let address = listener.local_addr().expect("read conflict mock address");
        let expected_hash = "5448ae0ed36912eb0dfba53c3e495b9988841e68";
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.expect("accept conflict request");
                let request = read_http_request(&mut stream).await;
                let first_line = request.lines().next().unwrap_or_default();
                let (status, body, extra_headers) = if first_line.contains("/api/v2/auth/login") {
                    (
                        "200 OK",
                        "Ok.".to_owned(),
                        "Set-Cookie: SID=conflict-session\r\n",
                    )
                } else if first_line.contains("/api/v2/torrents/add") {
                    assert!(request.contains(expected_hash));
                    ("409 Conflict", "Conflict".to_owned(), "")
                } else if first_line.contains("/api/v2/torrents/info") {
                    (
                        "200 OK",
                        format!(
                            r#"[{{"hash":"{expected_hash}","name":"Existing Episode","state":"stoppedUP","progress":1.0,"dlspeed":0,"upspeed":0,"eta":0,"save_path":"C:/Downloads","tags":"ani:old","added_on":1700000000,"completion_on":1700000100}}]"#
                        ),
                        "",
                    )
                } else if first_line.contains("/api/v2/torrents/files") {
                    (
                            "200 OK",
                            r#"[{"index":0,"name":"episode.mkv","size":4096,"progress":1.0,"priority":1}]"#
                                .to_owned(),
                            "",
                        )
                } else {
                    panic!("unexpected qBittorrent conflict request: {first_line}");
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write conflict response");
            }
        });
        let engine = QbittorrentEngine::new(QbittorrentConnectionConfig::new(
            format!("http://{address}"),
            "admin".to_owned(),
            Some("secret".to_owned()),
        ))
        .expect("create conflict qBittorrent engine");

        let task = engine
            .add_magnet(
                &format!("magnet:?xt=urn:btih:{expected_hash}&dn=Existing%20Episode"),
                &AddTorrentOptions {
                    save_path: "C:/Downloads".to_owned(),
                    correlation_tag: Some("ani:new".to_owned()),
                    ..AddTorrentOptions::default()
                },
            )
            .await
            .expect("recover existing qBittorrent magnet");

        assert_eq!(task.id, expected_hash);
        assert_eq!(task.status, DownloadStatus::Completed);
        assert_eq!(task.files.len(), 1);
        server.await.expect("finish qBittorrent conflict mock");
    }

    /// 读取一条带 Content-Length 的测试 HTTP 请求。
    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.expect("read HTTP request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            let Some(header_end) = bytes.windows(4).position(|item| item == b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .or_else(|| line.strip_prefix("Content-Length:"))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if bytes.len() >= header_end + content_length {
                break;
            }
        }
        String::from_utf8(bytes).expect("UTF-8 HTTP request")
    }
}
