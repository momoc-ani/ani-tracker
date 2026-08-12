use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ani_contracts::{AppCommandError, RemoteGatewayStatus, RemotePairingChallenge};
use ani_domain::{
    is_restricted_anime_content, AppSettings, Episode, EpisodePreference, MyAnime,
    ReleaseSourceConfig, ReportPlaybackProgressInput, SavePlaybackCheckpointInput,
    SetAnimeWatchProgressInput,
};
use ani_remote::{
    parse_trusted_origins, GatewayConfig, ImageCache, ImageCacheAsset, RemoteDeviceAuth,
    RemoteGateway, RemoteGatewayDependencies, RemoteMediaRepository, RemoteMediaSessionService,
    RemoteRpcHandler, RemoteRpcService, RemoteSecretStore, RemoteTlsCertificateStore,
    REMOTE_SECRET_PLACEHOLDER,
};
use ani_repository::prelude::*;
use ani_storage::Storage;
use async_trait::async_trait;
#[cfg(not(target_os = "macos"))]
use keyring::{Entry, Error as KeyringError};
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::sync::{OnceCell, RwLock};

use crate::downloads::AppDownloadState;
use crate::media::AppMediaState;

#[cfg(not(target_os = "macos"))]
const KEYRING_SERVICE: &str = "com.ani.tracker.remote";
#[cfg(not(target_os = "macos"))]
const KEYRING_ACCOUNT: &str = "remote-master-key-v1";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const FILE_MASTER_KEY: &str = "remote-master-key-v1.key";
#[cfg(target_os = "macos")]
const MACOS_REMOTE_SECRET_DIRECTORY: &str = "remote-secrets-file-v1";
const ENCRYPTED_FILE_HEADER: &[u8; 8] = b"ANIRSEC1";
const IMAGE_SIGNING_SECRET: &str = "image-signing-key-v1";

/// 使用平台主密钥加密任意长度远程秘密的文件安全存储。
struct PlatformRemoteSecretStore {
    directory: PathBuf,
    master_key: OnceCell<[u8; 32]>,
}

impl PlatformRemoteSecretStore {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            master_key: OnceCell::new(),
        }
    }

    async fn master_key(&self) -> Result<&[u8; 32], String> {
        let directory = self.directory.clone();
        self.master_key
            .get_or_try_init(|| async move { load_or_create_master_key(&directory).await })
            .await
    }

    fn file_path(&self, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        let name = digest
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<String>();
        self.directory.join(format!("{name}.secret"))
    }
}

#[async_trait]
impl RemoteSecretStore for PlatformRemoteSecretStore {
    /// 解密完整秘密文件，认证失败时拒绝返回任何部分内容。
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.file_path(key);
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("读取远程安全文件失败：{error}")),
        };
        if bytes.len() < ENCRYPTED_FILE_HEADER.len() + 12
            || &bytes[..ENCRYPTED_FILE_HEADER.len()] != ENCRYPTED_FILE_HEADER
        {
            return Err("远程安全文件格式无效".to_owned());
        }
        let nonce_offset = ENCRYPTED_FILE_HEADER.len();
        let nonce = Nonce::from_slice(&bytes[nonce_offset..nonce_offset + 12]);
        let cipher = Aes256Gcm::new_from_slice(self.master_key().await?)
            .map_err(|_| "远程安全存储主密钥无效".to_owned())?;
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &bytes[nonce_offset + 12..],
                    aad: key.as_bytes(),
                },
            )
            .map(Some)
            .map_err(|_| "远程安全文件认证失败".to_owned())
    }

    /// 使用随机 nonce 加密并原子替换秘密文件。
    async fn write(&self, key: &str, value: &[u8]) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(|error| format!("创建远程安全目录失败：{error}"))?;
        let mut nonce_bytes = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let cipher = Aes256Gcm::new_from_slice(self.master_key().await?)
            .map_err(|_| "远程安全存储主密钥无效".to_owned())?;
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: value,
                    aad: key.as_bytes(),
                },
            )
            .map_err(|_| "加密远程安全文件失败".to_owned())?;
        let mut payload = Vec::with_capacity(ENCRYPTED_FILE_HEADER.len() + 12 + encrypted.len());
        payload.extend_from_slice(ENCRYPTED_FILE_HEADER);
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&encrypted);
        write_atomic(&self.file_path(key), &payload).await
    }

    /// 删除指定秘密文件。
    async fn delete(&self, key: &str) -> Result<(), String> {
        match tokio::fs::remove_file(self.file_path(key)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除远程安全文件失败：{error}")),
        }
    }
}

/// Tauri 生命周期内共享的桌面远程网关。
pub(crate) struct AppRemoteGatewayState {
    gateway: Option<Arc<RemoteGateway>>,
    image_cache: Option<Arc<ImageCache>>,
    startup_error: Arc<RwLock<Option<String>>>,
}

impl AppRemoteGatewayState {
    /// 装配平台安全存储、RPC、媒体、图片和 TLS 服务。
    pub(crate) async fn initialize(
        app: &AppHandle,
        storage: Arc<Mutex<Storage>>,
        platform_defaults: AppSettings,
        downloads: AppDownloadState,
        media: AppMediaState,
    ) -> Result<Self, String> {
        let settings = read_settings(&storage, &platform_defaults)?;
        let user_data = setting_path(&settings, "/storage/userDataDir")?;
        let cache_directory = setting_path(&settings, "/storage/cacheDir")?.join("images");
        let secret_store: Arc<dyn RemoteSecretStore> = Arc::new(PlatformRemoteSecretStore::new(
            remote_secret_directory(&user_data),
        ));
        let image_signing_secret =
            load_or_create_secret(Arc::clone(&secret_store), IMAGE_SIGNING_SECRET, 32).await?;
        let image_cache = Arc::new(ImageCache::new(cache_directory, image_signing_secret)?);
        let auth = Arc::new(RemoteDeviceAuth::new(Arc::clone(&secret_store)));
        let rpc = Arc::new(RemoteRpcService::new(Arc::new(TauriRemoteRpcHandler {
            app: app.clone(),
            storage: Arc::clone(&storage),
            downloads: downloads.clone(),
        })));
        let media_sessions = Arc::new(RemoteMediaSessionService::new(
            Arc::new(TauriRemoteMediaRepository {
                storage,
                platform_defaults,
            }),
            media.remote_media_tools()?,
            user_data.join("remote-media"),
        ));
        let tls_store = Arc::new(RemoteTlsCertificateStore::new(
            user_data.join("remote-tls"),
            secret_store,
        ));
        let renderer_directory = resolve_remote_renderer_directory(app);
        let trusted_origins_value = trusted_origins_value();
        let trusted_origins = parse_trusted_origins(trusted_origins_value.as_deref());
        let gateway = Arc::new(RemoteGateway::new(RemoteGatewayDependencies {
            auth,
            rpc,
            media: media_sessions,
            image_cache: Arc::clone(&image_cache),
            tls_store,
            renderer_directory,
            trusted_origins,
        }));
        let state = Self {
            gateway: Some(gateway),
            image_cache: Some(image_cache),
            startup_error: Arc::new(RwLock::new(None)),
        };
        if let Err(error) = state.apply_settings(&settings).await {
            log::error!("Tauri 远程网关启动失败，应用继续启动 error={error}");
        }
        Ok(state)
    }

    /// 创建不会阻止应用启动的停止状态，并保留初始化错误供设置页展示。
    pub(crate) fn unavailable(error: impl Into<String>) -> Self {
        Self {
            gateway: None,
            image_cache: None,
            startup_error: Arc::new(RwLock::new(Some(error.into()))),
        }
    }

    /// 应用设置中的局域网开关、端口和图片缓存目录。
    pub(crate) async fn apply_settings(&self, settings: &AppSettings) -> Result<(), String> {
        match self.apply_settings_inner(settings).await {
            Ok(()) => {
                *self.startup_error.write().await = None;
                Ok(())
            }
            Err(error) => {
                *self.startup_error.write().await = Some(error.clone());
                Err(error)
            }
        }
    }

    /// 校验并应用一次远程网关配置，错误由外层统一记录到状态。
    async fn apply_settings_inner(&self, settings: &AppSettings) -> Result<(), String> {
        let Some(gateway) = self.gateway.as_ref() else {
            return Err(self
                .startup_error
                .read()
                .await
                .clone()
                .unwrap_or_else(|| "远程网关未完成初始化".to_owned()));
        };
        let lan_enabled = settings
            .pointer("/network/remoteAccess/lanEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let port = settings
            .pointer("/network/remoteAccess/port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(18_083);
        let config = GatewayConfig::new(lan_enabled, port).map_err(|error| error.to_string())?;
        let cache_directory = setting_path(settings, "/storage/cacheDir")?.join("images");
        let image_cache = self
            .image_cache
            .as_ref()
            .ok_or_else(|| "远程图片缓存未完成初始化".to_owned())?;
        image_cache.set_cache_directory(cache_directory).await;
        gateway
            .apply_config(config)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// 返回当前网关状态。
    pub(crate) async fn status(&self) -> RemoteGatewayStatus {
        let startup_error = self.startup_error.read().await.clone();
        if let Some(gateway) = self.gateway.as_ref() {
            let mut status = gateway.status().await;
            if startup_error.is_some() {
                status.last_error = startup_error;
            }
            return status;
        }
        RemoteGatewayStatus {
            running: false,
            host: "127.0.0.1".to_owned(),
            port: 18_083,
            protocol: "http".to_owned(),
            lan_enabled: false,
            base_url: "http://127.0.0.1:18083".to_owned(),
            addresses: Vec::new(),
            devices: Vec::new(),
            certificate: None,
            last_error: startup_error.or_else(|| Some("远程网关未完成初始化".to_owned())),
        }
    }

    /// 创建一次性远程配对码。
    pub(crate) async fn create_pairing_code(&self) -> Result<RemotePairingChallenge, String> {
        self.gateway
            .as_ref()
            .ok_or_else(|| "远程网关未完成初始化".to_owned())?
            .create_pairing_code()
            .await
            .map_err(|error| error.to_string())
    }

    /// 吊销设备并返回最新状态。
    pub(crate) async fn revoke_device(
        &self,
        device_id: &str,
    ) -> Result<RemoteGatewayStatus, String> {
        self.gateway
            .as_ref()
            .ok_or_else(|| "远程网关未完成初始化".to_owned())?
            .revoke_device(device_id)
            .await
            .map_err(|error| error.to_string())
    }

    /// 读取本地 Renderer 所需图片，命中缓存或按安全策略下载。
    pub(crate) async fn load_image_asset(
        &self,
        source_url: &str,
    ) -> Result<ImageCacheAsset, String> {
        self.image_cache
            .as_ref()
            .ok_or_else(|| "远程图片缓存未完成初始化".to_owned())?
            .get(source_url)
            .await
            .map_err(|error| error.to_string())
    }

    /// 删除本地 Renderer 解码失败对应的图片缓存。
    pub(crate) async fn invalidate_image_asset(&self, source_url: &str) -> Result<(), String> {
        self.image_cache
            .as_ref()
            .ok_or_else(|| "远程图片缓存未完成初始化".to_owned())?
            .invalidate(source_url)
            .await
            .map_err(|error| error.to_string())
    }

    /// 应用退出时停止监听和媒体进程。
    pub(crate) async fn shutdown(&self) {
        if let Some(gateway) = self.gateway.as_ref() {
            gateway.stop().await;
        }
    }
}

/// 运行时环境优先，缺失时使用构建阶段从 .env 注入的值。
fn trusted_origins_value() -> Option<String> {
    resolve_trusted_origins_value(
        std::env::var("ANI_TRUSTED_ORIGINS").ok(),
        option_env!("ANI_TRUSTED_ORIGINS"),
    )
}

/// 合并运行时与构建时可信来源，空白值按未配置处理。
fn resolve_trusted_origins_value(
    runtime: Option<String>,
    compiled: Option<&str>,
) -> Option<String> {
    runtime
        .or_else(|| compiled.map(str::to_owned))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 设置更新后异步重配远程网关，不让远程能力错误回滚其他设置。
pub(crate) async fn apply_settings(app: &AppHandle, settings: &AppSettings) {
    if let Some(state) = app.try_state::<AppRemoteGatewayState>() {
        if let Err(error) = state.apply_settings(settings).await {
            log::error!("应用远程网关设置失败 error={error}");
        }
    }
}

struct TauriRemoteRpcHandler {
    app: AppHandle,
    storage: Arc<Mutex<Storage>>,
    downloads: AppDownloadState,
}

#[async_trait]
impl RemoteRpcHandler for TauriRemoteRpcHandler {
    /// 将白名单方法映射到与 Tauri commands 相同的 Rust 服务和 Repository。
    async fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match method {
            "getDashboard" => {
                self.query(|repository| {
                    repository
                        .get_dashboard()
                        .map(crate::commands::data::filter_missing_dashboard_media)
                })
                .await
            }
            "listNotifications" => {
                self.query(|repository| repository.list_notifications())
                    .await
            }
            "getUnreadNotificationCount" => {
                self.query(|repository| repository.get_unread_notification_count())
                    .await
            }
            "markNotificationRead" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.mark_notification_read(&id))
                    .await
            }
            "markAllNotificationsRead" => {
                self.query(|repository| repository.mark_all_notifications_read())
                    .await
            }
            "listMyAnime" => self.query(|repository| repository.list_my_anime()).await,
            "upsertMyAnime" => {
                let mut item: MyAnime = value_arg(&args, 0)?;
                self.query(move |repository| {
                    item.download_dir = repository
                        .list_my_anime()?
                        .into_iter()
                        .find(|current| current.id == item.id)
                        .and_then(|current| current.download_dir);
                    repository.upsert_my_anime(item)
                })
                .await
            }
            "followBangumiAnime" => {
                let item: MyAnime = value_arg(&args, 0)?;
                command_value(
                    crate::commands::data::follow_bangumi_anime(
                        item,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "removeMyAnime" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.remove_my_anime(&id))
                    .await
            }
            "listMyAnimeWatchProgress" => {
                self.query(|repository| repository.list_my_anime_watch_progress())
                    .await
            }
            "setAnimeWatchProgress" => {
                let input: SetAnimeWatchProgressInput = value_arg(&args, 0)?;
                self.query(move |repository| repository.set_anime_watch_progress(&input))
                    .await
            }
            "reportPlaybackProgress" => {
                let input: ReportPlaybackProgressInput = value_arg(&args, 0)?;
                self.query(move |repository| repository.report_playback_progress(&input))
                    .await
            }
            "savePlaybackCheckpoint" => {
                let input: SavePlaybackCheckpointInput = value_arg(&args, 0)?;
                self.query(move |repository| repository.save_playback_checkpoint(&input))
                    .await
            }
            "listAnimeCatalog" => {
                let year = args.first().and_then(Value::as_i64);
                let month = args.get(1).and_then(Value::as_i64);
                self.query(move |repository| {
                    let mut items = repository.list_anime_catalog(year, month)?;
                    items.retain(|item| !is_restricted_anime_content(item));
                    Ok(items)
                })
                .await
            }
            "getAnimeDetail" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.get_anime_detail(&id))
                    .await
            }
            "searchAnimeCatalog" => {
                let keyword = string_arg(&args, 0)?;
                command_value(
                    crate::commands::data::search_anime_catalog(
                        keyword,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "browseBangumiAnime" => {
                let query = value_arg(&args, 0)?;
                command_value(
                    crate::commands::data::browse_bangumi_anime(
                        query,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "listFansubs" => {
                let anime_id = args.first().and_then(Value::as_str).map(str::to_owned);
                self.query(move |repository| repository.list_fansubs(anime_id.as_deref()))
                    .await
            }
            "listEpisodes" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.list_episodes(&id))
                    .await
            }
            "upsertEpisode" => {
                let episode: Episode = value_arg(&args, 0)?;
                self.query(move |repository| repository.upsert_episode(&episode))
                    .await
            }
            "listEpisodePreferences" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.list_episode_preferences(&id))
                    .await
            }
            "upsertEpisodePreference" => {
                let preference: EpisodePreference = value_arg(&args, 0)?;
                self.query(move |repository| repository.upsert_episode_preference(&preference))
                    .await
            }
            "removeEpisodePreference" => {
                let id = string_arg(&args, 0)?;
                self.query(move |repository| repository.remove_episode_preference(&id))
                    .await
            }
            "previewEpisodeReleases" => {
                let anime_id = string_arg(&args, 0)?;
                let episode_id = string_arg(&args, 1)?;
                command_value(
                    crate::commands::sources::preview_episode_releases(
                        anime_id,
                        episode_id,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "searchReleases" => {
                let query = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::search_releases(
                        query,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "searchAnimeReleases" => {
                let query = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::search_anime_releases(
                        query,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "searchRssSubscriptionReleases" => {
                let query = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::search_rss_subscription_releases(
                        query,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "getAnimeSourceBindingState" => {
                let anime_id = string_arg(&args, 0)?;
                let discover_candidates = args.get(1).and_then(Value::as_bool);
                command_value(
                    crate::commands::sources::get_anime_source_binding_state(
                        anime_id,
                        discover_candidates,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "confirmAnimeSourceBinding" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::confirm_anime_source_binding(
                        input,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "reportAnimeSourceCandidateMismatch" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::report_anime_source_candidate_mismatch(
                        input,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "removeAnimeSourceCandidateMismatch" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::remove_anime_source_candidate_mismatch(
                        input,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "setAnimeSourceExcluded" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::sources::set_anime_source_excluded(
                        input,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "removeAnimeSourceBinding" => {
                let anime_id = string_arg(&args, 0)?;
                let source_id = string_arg(&args, 1)?;
                command_value(
                    crate::commands::sources::remove_anime_source_binding(
                        anime_id,
                        source_id,
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "listDownloads" => {
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(
                    self.downloads
                        .service()
                        .list_for_engine(&engine)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            "refreshDownloads" => {
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                if engine == ani_domain::TorrentEngineKind::Qbittorrent {
                    let managed = self.downloads.managed_qbittorrent_status().await?;
                    if managed.enabled && !managed.running {
                        log::debug!(
                            "Rust 远程下载刷新跳过：托管 qBittorrent 未运行，返回持久化快照"
                        );
                        return serde_json::to_value(
                            self.downloads
                                .service()
                                .list_for_engine(&engine)
                                .map_err(|error| error.to_string())?,
                        )
                        .map_err(|error| error.to_string());
                    }
                }
                let result = self
                    .downloads
                    .service()
                    .refresh(engine)
                    .await
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(result.tasks).map_err(|error| error.to_string())
            }
            "pauseDownload" => {
                let id = string_arg(&args, 0)?;
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(
                    self.downloads
                        .service()
                        .pause(&id, &engine)
                        .await
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            "resumeDownload" => {
                let id = string_arg(&args, 0)?;
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(
                    self.downloads
                        .service()
                        .resume(&id, &engine)
                        .await
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            "removeDownload" => {
                let id = string_arg(&args, 0)?;
                let delete_files: bool = value_arg(&args, 1)?;
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(
                    self.downloads
                        .remove_task(&id, delete_files, &engine)
                        .await
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            "setDownloadFilePriority" => {
                let id = string_arg(&args, 0)?;
                let file_indexes: Vec<i64> = value_arg(&args, 1)?;
                let priority = args
                    .get(2)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| "远程文件优先级参数缺失".to_owned())?;
                let settings = self.downloads.settings()?;
                let engine = self
                    .downloads
                    .default_engine(&settings)
                    .map_err(|error| error.to_string())?;
                serde_json::to_value(
                    self.downloads
                        .service()
                        .set_file_priority(&id, &file_indexes, priority, &engine)
                        .await
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())
            }
            "addDownloadUrl" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::downloads::add_download_url(
                        input,
                        self.app.clone(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "addReleaseDownload" => {
                let input = value_arg(&args, 0)?;
                command_value(
                    crate::commands::downloads::add_release_download(
                        input,
                        self.app.clone(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "listSources" => self.query(|repository| repository.list_sources()).await,
            "setSourceEnabled" => {
                let source_id = string_arg(&args, 0)?;
                let enabled = args
                    .get(1)
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "远程下载源状态参数缺失".to_owned())?;
                self.query(move |repository| repository.set_source_enabled(&source_id, enabled))
                    .await
            }
            "upsertSource" => {
                let mut source: ReleaseSourceConfig = value_arg(&args, 0)?;
                self.query(move |repository| {
                    let incoming_secret = source.api_key.as_deref().map(str::trim);
                    if incoming_secret.is_none_or(|secret| {
                        secret.is_empty() || secret == REMOTE_SECRET_PLACEHOLDER
                    }) {
                        source.api_key = repository
                            .list_sources()?
                            .into_iter()
                            .find(|current| current.id == source.id)
                            .and_then(|current| current.api_key);
                    }
                    repository.upsert_source(&source)
                })
                .await
            }
            "getSourceSyncStatus" => command_value(
                crate::commands::source_sync::get_source_sync_status(self.app.state()).await,
            ),
            "getSettings" => {
                let defaults = self.app.state::<crate::storage::AppStorageState>();
                let platform_defaults = defaults.platform_defaults().clone();
                self.query(move |repository| repository.get_settings(&platform_defaults))
                    .await
            }
            "updateSettings" => {
                let mut patch = args
                    .first()
                    .cloned()
                    .ok_or_else(|| "远程设置参数缺失".to_owned())?;
                if patch
                    .pointer("/download/qbittorrent/password")
                    .and_then(Value::as_str)
                    == Some(REMOTE_SECRET_PLACEHOLDER)
                {
                    let defaults = self.app.state::<crate::storage::AppStorageState>();
                    let platform_defaults = defaults.platform_defaults().clone();
                    let current = self
                        .query(move |repository| repository.get_settings(&platform_defaults))
                        .await?;
                    restore_remote_settings_secrets(&mut patch, &current);
                }
                command_value(
                    crate::commands::data::update_settings(
                        patch,
                        self.app.clone(),
                        self.app.state(),
                        self.app.state(),
                        self.app.state(),
                        self.app.state(),
                    )
                    .await,
                )
            }
            "testQbittorrent" => {
                command_value(crate::commands::downloads::test_qbittorrent(self.app.state()).await)
            }
            "getAutomationSchedulerStatus" => command_value(
                crate::commands::automation::get_automation_scheduler_status(self.app.state())
                    .await,
            ),
            "getQbittorrentManagedStatus" => command_value(
                crate::commands::downloads::get_qbittorrent_managed_status(self.app.state()).await,
            ),
            "startQbittorrentManaged" => command_value(
                crate::commands::downloads::start_qbittorrent_managed(
                    self.app.clone(),
                    self.app.state(),
                )
                .await,
            ),
            "stopQbittorrentManaged" => command_value(
                crate::commands::downloads::stop_qbittorrent_managed(
                    self.app.clone(),
                    self.app.state(),
                )
                .await,
            ),
            "getEmbeddedTorrentStatus" => command_value(
                crate::commands::downloads::get_embedded_torrent_status(self.app.state()).await,
            ),
            "startEmbeddedTorrent" => command_value(
                crate::commands::downloads::start_embedded_torrent(
                    self.app.clone(),
                    self.app.state(),
                )
                .await,
            ),
            "stopEmbeddedTorrent" => command_value(
                crate::commands::downloads::stop_embedded_torrent(
                    self.app.clone(),
                    self.app.state(),
                )
                .await,
            ),
            "restartEmbeddedTorrent" => command_value(
                crate::commands::downloads::restart_embedded_torrent(
                    self.app.clone(),
                    self.app.state(),
                )
                .await,
            ),
            _ => Err("远程方法未装配".to_owned()),
        }
    }
}

impl TauriRemoteRpcHandler {
    async fn query<T, F>(&self, operation: F) -> Result<Value, String>
    where
        T: serde::Serialize + Send + 'static,
        F: FnOnce(&ani_storage::SqliteRepository<'_>) -> ani_repository::RepositoryResult<T>
            + Send
            + 'static,
    {
        let storage = Arc::clone(&self.storage);
        tauri::async_runtime::spawn_blocking(move || {
            let storage = storage.lock().map_err(|error| error.to_string())?;
            operation(&storage.repository()).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
    }
}

/// 将远程脱敏占位值替换为 SQLite 中现有秘密，避免无意覆盖密码。
fn restore_remote_settings_secrets(patch: &mut Value, current: &Value) {
    let Some(password) = patch.pointer_mut("/download/qbittorrent/password") else {
        return;
    };
    if password.as_str() != Some(REMOTE_SECRET_PLACEHOLDER) {
        return;
    }
    if let Some(current_password) = current.pointer("/download/qbittorrent/password") {
        *password = current_password.clone();
        log::debug!("Rust 远程设置保留现有 qBittorrent 密码");
    }
}

/** 将 Tauri 命令结果转换为远程 RPC 的 JSON 返回值。 */
fn command_value<T: serde::Serialize>(result: Result<T, AppCommandError>) -> Result<Value, String> {
    result
        .map_err(|error| format!("Tauri command failed code={}", error.code))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
}

struct TauriRemoteMediaRepository {
    storage: Arc<Mutex<Storage>>,
    platform_defaults: AppSettings,
}

#[async_trait]
impl RemoteMediaRepository for TauriRemoteMediaRepository {
    async fn get_download_task(
        &self,
        task_id: &str,
    ) -> Result<Option<ani_domain::DownloadTask>, String> {
        let task_id = task_id.to_owned();
        self.query(move |repository| {
            Ok(repository
                .list_downloads()?
                .into_iter()
                .find(|task| task.id == task_id))
        })
        .await
    }

    async fn list_media_files(&self) -> Result<Vec<ani_domain::MediaFile>, String> {
        self.query(|repository| repository.list_media_files()).await
    }

    async fn get_settings(&self) -> Result<AppSettings, String> {
        let defaults = self.platform_defaults.clone();
        self.query(move |repository| repository.get_settings(&defaults))
            .await
    }

    async fn get_playback_checkpoint(
        &self,
        task_id: &str,
        file_index: Option<i64>,
    ) -> Result<Option<ani_domain::PlaybackCheckpoint>, String> {
        let task_id = task_id.to_owned();
        self.query(move |repository| repository.get_playback_checkpoint(&task_id, file_index))
            .await
    }
}

impl TauriRemoteMediaRepository {
    async fn query<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&ani_storage::SqliteRepository<'_>) -> ani_repository::RepositoryResult<T>
            + Send
            + 'static,
    {
        let storage = Arc::clone(&self.storage);
        tauri::async_runtime::spawn_blocking(move || {
            let storage = storage.lock().map_err(|error| error.to_string())?;
            operation(&storage.repository()).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

/// macOS 使用应用数据文件保存主密钥，避免应用启动触发 Keychain 授权。
#[cfg(target_os = "macos")]
async fn load_or_create_master_key(directory: &Path) -> Result<[u8; 32], String> {
    let directory = directory.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        load_or_create_file_master_key(&directory, "macOS")
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 从平台凭据库读取或创建远程访问主密钥。
#[cfg(not(target_os = "macos"))]
async fn load_or_create_master_key(_directory: &Path) -> Result<[u8; 32], String> {
    #[cfg(target_os = "linux")]
    let directory = _directory.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        if is_windows_subsystem_for_linux() {
            return load_or_create_file_master_key(&directory, "WSL");
        }
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .map_err(|error| format!("打开系统凭据库失败：{error}"))?;
        match entry.get_secret() {
            Ok(bytes) => <[u8; 32]>::try_from(bytes)
                .map_err(|_| "系统凭据库中的远程主密钥长度无效".to_owned()),
            Err(KeyringError::NoEntry) => {
                let mut key = [0_u8; 32];
                OsRng.fill_bytes(&mut key);
                entry
                    .set_secret(&key)
                    .map_err(|error| format!("保存远程主密钥失败：{error}"))?;
                Ok(key)
            }
            Err(error) => Err(format!("读取远程主密钥失败：{error}")),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 在应用数据目录中读取或创建仅当前用户可读的随机主密钥。
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn load_or_create_file_master_key(directory: &Path, platform: &str) -> Result<[u8; 32], String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("创建 {platform} 远程安全目录失败：{error}"))?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("限制 {platform} 远程安全目录权限失败：{error}"))?;
    let path = directory.join(FILE_MASTER_KEY);
    match std::fs::read(&path) {
        Ok(bytes) => {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("限制 {platform} 远程主密钥权限失败：{error}"))?;
            return <[u8; 32]>::try_from(bytes)
                .map_err(|_| format!("{platform} 远程主密钥长度无效"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("读取 {platform} 远程主密钥失败：{error}")),
    }

    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("创建 {platform} 远程主密钥失败：{error}"))?;
    file.write_all(&key)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("写入 {platform} 远程主密钥失败：{error}"))?;
    log::info!(
        "{platform} 使用应用数据文件保存远程主密钥 path={}",
        path.display()
    );
    Ok(key)
}

/// 判断当前 Linux 进程是否运行在 Windows Subsystem for Linux 中。
#[cfg(target_os = "linux")]
fn is_windows_subsystem_for_linux() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

async fn load_or_create_secret(
    store: Arc<dyn RemoteSecretStore>,
    key: &str,
    size: usize,
) -> Result<Vec<u8>, String> {
    if let Some(value) = store.read(key).await? {
        if value.len() == size {
            return Ok(value);
        }
        return Err(format!("远程秘密 {key} 长度无效"));
    }
    let mut value = vec![0_u8; size];
    OsRng.fill_bytes(&mut value);
    store.write(key, &value).await?;
    Ok(value)
}

fn read_settings(
    storage: &Arc<Mutex<Storage>>,
    defaults: &AppSettings,
) -> Result<AppSettings, String> {
    storage
        .lock()
        .map_err(|error| error.to_string())?
        .repository()
        .get_settings(defaults)
        .map_err(|error| error.to_string())
}

fn setting_path(settings: &AppSettings, pointer: &str) -> Result<PathBuf, String> {
    settings
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("设置路径缺失：{pointer}"))
}

/// 返回当前桌面平台的远程安全数据目录。
fn remote_secret_directory(user_data: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return user_data.join(MACOS_REMOTE_SECRET_DIRECTORY);

    #[cfg(not(target_os = "macos"))]
    user_data.join("remote-secrets")
}

/// 解析远程 PWA 目录，开发态使用工作区产物，发布态使用打包资源。
fn resolve_remote_renderer_directory(app: &AppHandle) -> PathBuf {
    let bundled_directory = app
        .path()
        .resource_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("remote-pwa");
    #[cfg(debug_assertions)]
    let development_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|workspace| workspace.join(".tauri-remote-pwa"));
    #[cfg(not(debug_assertions))]
    let development_directory = None;
    let directory = select_remote_renderer_directory(development_directory, bundled_directory);
    if directory.join("index.html").is_file() {
        log::info!("远程 PWA 资源目录已解析 path={}", directory.display());
    } else {
        log::warn!(
            "远程 PWA 资源目录缺少 index.html path={}",
            directory.display()
        );
    }
    directory
}

/// 开发产物有效时优先选用，否则回退到 Tauri 打包资源目录。
fn select_remote_renderer_directory(
    development_directory: Option<PathBuf>,
    bundled_directory: PathBuf,
) -> PathBuf {
    development_directory
        .filter(|directory| directory.join("index.html").is_file())
        .unwrap_or(bundled_directory)
}

async fn write_atomic(path: &Path, value: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    tokio::fs::write(&temporary, value)
        .await
        .map_err(|error| format!("写入远程安全临时文件失败：{error}"))?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|error| format!("限制远程安全文件权限失败：{error}"))?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            tokio::fs::remove_file(path)
                .await
                .map_err(|remove_error| format!("替换远程安全文件失败：{remove_error}"))?;
            tokio::fs::rename(&temporary, path)
                .await
                .map_err(|rename_error| format!("替换远程安全文件失败：{rename_error}"))?;
        } else {
            return Err(format!("保存远程安全文件失败：{error}"));
        }
    }
    Ok(())
}

fn string_arg(args: &[Value], index: usize) -> Result<String, String> {
    args.get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "远程字符串参数缺失".to_owned())
}

fn value_arg<T: serde::de::DeserializeOwned>(args: &[Value], index: usize) -> Result<T, String> {
    serde_json::from_value(
        args.get(index)
            .cloned()
            .ok_or_else(|| "远程对象参数缺失".to_owned())?,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 macOS 使用独立版本目录，不再读取旧 Keychain 密文。
    #[test]
    fn resolves_platform_remote_secret_directory() {
        let root = PathBuf::from("/tmp/ani-user-data");
        let directory = remote_secret_directory(&root);
        #[cfg(target_os = "macos")]
        assert_eq!(directory, root.join(MACOS_REMOTE_SECRET_DIRECTORY));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(directory, root.join("remote-secrets"));
    }

    /// 验证运行时配置覆盖构建值，缺失时才使用 .env 编译回退。
    #[test]
    fn resolves_runtime_and_compiled_trusted_origins() {
        assert_eq!(
            resolve_trusted_origins_value(
                Some(" https://runtime.example ".to_owned()),
                Some("https://compiled.example"),
            ),
            Some("https://runtime.example".to_owned())
        );
        assert_eq!(
            resolve_trusted_origins_value(None, Some(" https://compiled.example ")),
            Some("https://compiled.example".to_owned())
        );
        assert_eq!(resolve_trusted_origins_value(None, Some("  ")), None);
    }

    /// 验证远程回传脱敏密码时继续使用宿主现有秘密。
    #[test]
    fn preserves_qbittorrent_password_placeholder() {
        let mut patch = serde_json::json!({
            "download": { "qbittorrent": { "password": REMOTE_SECRET_PLACEHOLDER } }
        });
        let current = serde_json::json!({
            "download": { "qbittorrent": { "password": "existing-secret" } }
        });

        restore_remote_settings_secrets(&mut patch, &current);

        assert_eq!(
            patch.pointer("/download/qbittorrent/password"),
            Some(&Value::String("existing-secret".to_owned()))
        );
    }

    /// 验证开发产物存在时优先使用工作区 PWA 目录。
    #[test]
    fn prefers_development_remote_renderer_directory() {
        let root = std::env::temp_dir().join(format!(
            "ani-remote-renderer-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let development = root.join("development");
        let bundled = root.join("bundled");
        std::fs::create_dir_all(&development).expect("create development renderer directory");
        std::fs::write(development.join("index.html"), "renderer")
            .expect("write development renderer index");

        assert_eq!(
            select_remote_renderer_directory(Some(development.clone()), bundled),
            development
        );
        std::fs::remove_dir_all(root).expect("remove remote renderer test directory");
    }

    /// 验证开发产物缺失时回退到打包 PWA 目录。
    #[test]
    fn falls_back_to_bundled_remote_renderer_directory() {
        let development = PathBuf::from("/missing/development/remote-pwa");
        let bundled = PathBuf::from("/application/resources/remote-pwa");

        assert_eq!(
            select_remote_renderer_directory(Some(development), bundled.clone()),
            bundled
        );
    }

    /// 验证 macOS 文件主密钥可复用且权限仅允许当前用户访问。
    #[cfg(target_os = "macos")]
    #[test]
    fn persists_macos_file_master_key_with_private_permissions() {
        let directory = std::env::temp_dir().join(format!(
            "ani-remote-key-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let first = load_or_create_file_master_key(&directory, "macOS")
            .expect("create macOS remote master key");
        let second = load_or_create_file_master_key(&directory, "macOS")
            .expect("reload macOS remote master key");
        assert_eq!(first, second);
        let mode = std::fs::metadata(directory.join(FILE_MASTER_KEY))
            .expect("read macOS remote master key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(directory).expect("remove macOS remote key test directory");
    }
}
