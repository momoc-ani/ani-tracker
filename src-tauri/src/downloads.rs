use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ani_contracts::{
    DownloadServiceMode, DownloadServiceState, DownloadServiceStatus, EmbeddedTorrentCoreStatus,
    QbittorrentManagedStatus,
};
use ani_domain::{
    AppSettings, DownloadTask, Episode, MyAnime, Release, SubtitleLanguage, SubtitlePreference,
    TorrentEngineKind, TorrentFile,
};
use ani_downloads::{
    DownloadEngine, DownloadEngineConfig, DownloadEngineRegistry, DownloadServiceError,
    DownloadSource, DownloadTaskContext, DownloadTaskService, DownloadTaskStore,
    QbittorrentConnectionConfig, QbittorrentEngine, SeedingLimits, TorrentCoreEngine,
};
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
use ani_downloads::{ProcessTorrentCoreTransport, TorrentCoreProcessOptions};
use ani_repository::{
    AnimeTrackingRepository, DownloadRepository, RepositoryError, RepositoryResult,
    SettingsRepository,
};
use ani_sources::{parse_release_title, HttpMethod, NativeHttpClient, NativeHttpRequest};
use ani_storage::Storage;
use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tauri::AppHandle;
#[cfg(desktop)]
use tauri::Manager;
#[cfg(any(target_os = "android", target_os = "ios"))]
use tauri_plugin_ani_torrent::{AniTorrentExt, MobileTorrentCoreTransport};
use tokio::sync::Mutex as AsyncMutex;

use crate::qbittorrent_managed::{managed_credentials, AppManagedQbittorrentState};
use crate::sources::native_http_config;

static TORRENT_IMPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_TORRENT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAGNET_PARAMETER_PREFIXES: [&str; 9] = [
    "dn=", "xl=", "tr=", "ws=", "as=", "xs=", "kt=", "so=", "x.pe=",
];

#[derive(Default)]
struct EmbeddedLifecycle {
    last_started_at: Option<String>,
    last_stopped_at: Option<String>,
    last_error: Option<String>,
}

/// 将 Tauri 的 SQLite 单写者适配为线程安全下载任务存储端口。
struct SharedDownloadTaskStore {
    storage: Arc<Mutex<Storage>>,
}

trait DownloadAssociationRepository: DownloadRepository + AnimeTrackingRepository {}

impl<T> DownloadAssociationRepository for T where T: DownloadRepository + AnimeTrackingRepository {}

impl SharedDownloadTaskStore {
    /// 创建复用应用 SQLite 连接的下载存储适配器。
    fn new(storage: Arc<Mutex<Storage>>) -> Self {
        Self { storage }
    }

    /// 在短临界区内执行 Repository 操作。
    fn with_repository<T>(
        &self,
        operation: impl FnOnce(&dyn DownloadAssociationRepository) -> RepositoryResult<T>,
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

impl DownloadTaskStore for SharedDownloadTaskStore {
    fn list_downloads(&self) -> RepositoryResult<Vec<DownloadTask>> {
        self.with_repository(|repository| {
            let tasks = repository.list_downloads()?;
            let mut episodes_by_anime = BTreeMap::new();
            for anime_id in tasks.iter().filter_map(|task| task.anime_id.as_deref()) {
                if !episodes_by_anime.contains_key(anime_id) {
                    episodes_by_anime
                        .insert(anime_id.to_owned(), repository.list_episodes(anime_id)?);
                }
            }
            let mut changed = false;
            for task in &tasks {
                let Some(anime_id) = task.anime_id.as_deref() else {
                    continue;
                };
                let Some(episodes) = episodes_by_anime.get(anime_id) else {
                    continue;
                };
                let mut linked = task.clone();
                let count = associate_torrent_files(
                    &mut linked.files,
                    linked.episode_id.as_deref(),
                    linked.episode_no,
                    episodes,
                );
                if count == 0 {
                    continue;
                }
                repository.upsert_download_task(&linked)?;
                changed = true;
                log::info!(
                    "Tauri 历史合集文件关联已补齐 task_id={} linked_files={}",
                    linked.id,
                    count
                );
            }
            if changed {
                repository.list_downloads()
            } else {
                Ok(tasks)
            }
        })
    }

    fn upsert_download_task(&self, task: &DownloadTask) -> RepositoryResult<Vec<DownloadTask>> {
        self.with_repository(|repository| {
            let mut linked = task.clone();
            if let Some(anime_id) = linked.anime_id.as_deref() {
                let episodes = repository.list_episodes(anime_id)?;
                let count = associate_torrent_files(
                    &mut linked.files,
                    linked.episode_id.as_deref(),
                    linked.episode_no,
                    &episodes,
                );
                if count > 0 {
                    log::info!(
                        "Tauri 下载文件单集关联已补齐 task_id={} linked_files={}",
                        linked.id,
                        count
                    );
                }
            }
            repository.upsert_download_task(&linked)
        })
    }

    fn remove_download_task(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> RepositoryResult<Vec<DownloadTask>> {
        self.with_repository(|repository| repository.remove_download_task(task_id, delete_files))
    }
}

/// 从合集视频文件名识别集数，并绑定到追番已有单集。
fn associate_torrent_files(
    files: &mut [TorrentFile],
    task_episode_id: Option<&str>,
    task_episode_no: Option<f64>,
    episodes: &[Episode],
) -> usize {
    let video_file_count = files
        .iter()
        .filter(|file| is_video_file_name(&file.name))
        .count();
    let mut linked_count = 0;
    for file in files {
        if !is_video_file_name(&file.name) {
            continue;
        }
        let existing_episode = file
            .episode_id
            .as_deref()
            .and_then(|id| episodes.iter().find(|episode| episode.id == id))
            .or_else(|| {
                file.episode_no
                    .and_then(|number| find_episode_by_number(episodes, number))
            });
        let candidate = existing_episode.or_else(|| {
            let number = if video_file_count == 1 {
                task_episode_no.or_else(|| parse_file_episode_no(&file.name))
            } else {
                parse_file_episode_no(&file.name)
            }?;
            find_episode_by_number(episodes, number)
        });
        let candidate = candidate.or_else(|| {
            (video_file_count == 1)
                .then_some(task_episode_id)
                .flatten()
                .and_then(|id| episodes.iter().find(|episode| episode.id == id))
        });
        let Some(episode) = candidate else {
            continue;
        };
        if file.episode_id.as_deref() == Some(episode.id.as_str())
            && file.episode_no == Some(episode.episode_no)
        {
            continue;
        }
        file.episode_id = Some(episode.id.clone());
        file.episode_no = Some(episode.episode_no);
        linked_count += 1;
    }
    linked_count
}

/// 从发布式文件名或纯数字文件名读取单集编号。
fn parse_file_episode_no(file_name: &str) -> Option<f64> {
    let base_name = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name)
        .trim();
    parse_release_title(base_name, &[])
        .episode_no
        .or_else(|| {
            let digit_end = base_name
                .char_indices()
                .take_while(|(_, character)| character.is_ascii_digit() || *character == '.')
                .map(|(index, character)| index + character.len_utf8())
                .last()?;
            let suffix = &base_name[digit_end..];
            if suffix.chars().next().is_some_and(|character| {
                !character.is_whitespace() && !matches!(character, '-' | '_' | 'v' | 'V')
            }) {
                return None;
            }
            base_name[..digit_end]
                .trim_end_matches('.')
                .parse::<f64>()
                .ok()
        })
        .filter(|number| number.is_finite() && *number > 0.0)
}

/// 判断文件名是否属于可建立播放关联的视频文件。
fn is_video_file_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    [".mkv", ".mp4", ".avi", ".mov", ".webm", ".m4v", ".ts"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

fn find_episode_by_number(episodes: &[Episode], number: f64) -> Option<&Episode> {
    episodes
        .iter()
        .find(|episode| (episode.episode_no - number).abs() < f64::EPSILON)
}

/// Tauri 生命周期内共享下载服务、引擎注册表和临时种子目录。
#[derive(Clone)]
pub(crate) struct AppDownloadState {
    service: Arc<DownloadTaskService>,
    registry: Arc<DownloadEngineRegistry>,
    qbittorrent: Arc<QbittorrentEngine>,
    managed_qbittorrent: AppManagedQbittorrentState,
    embedded_lifecycle: Arc<Mutex<EmbeddedLifecycle>>,
    embedded_operation: Arc<AsyncMutex<()>>,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    embedded_transport: Arc<ProcessTorrentCoreTransport>,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    embedded_binary_path: PathBuf,
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    embedded_data_directory: PathBuf,
    #[cfg(any(target_os = "android", target_os = "ios"))]
    mobile_transport: Arc<MobileTorrentCoreTransport>,
    storage: Arc<Mutex<Storage>>,
    platform_defaults: AppSettings,
    torrent_import_directory: PathBuf,
}

impl AppDownloadState {
    /// 创建 Tauri 下载状态并在桌面注册 torrent-core sidecar。
    pub(crate) fn new(
        app: &AppHandle,
        storage: Arc<Mutex<Storage>>,
        platform_defaults: AppSettings,
    ) -> Result<Self, String> {
        let mut registry = DownloadEngineRegistry::new();
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        let (embedded_transport, embedded_binary_path, embedded_data_directory) = {
            let binary_path = resolve_torrent_core_binary(app);
            let data_directory =
                setting_path(&platform_defaults, "/storage/userDataDir")?.join("torrent-core");
            let transport = Arc::new(ProcessTorrentCoreTransport::new(
                TorrentCoreProcessOptions::new(binary_path.clone(), data_directory.clone()),
            ));
            registry
                .register(Arc::new(TorrentCoreEngine::new(transport.clone())))
                .map_err(|error| error.to_string())?;
            log::info!(
                "Tauri torrent-core transport 已装配 binary={}",
                binary_path.display()
            );
            (transport, binary_path, data_directory)
        };
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let mobile_transport = {
            let transport = Arc::new(app.ani_torrent().transport());
            registry
                .register(Arc::new(TorrentCoreEngine::new(transport.clone())))
                .map_err(|error| error.to_string())?;
            log::info!(
                "Tauri 移动 torrent-core transport 已装配 platform={}",
                std::env::consts::OS
            );
            transport
        };
        let qbittorrent = Arc::new(
            QbittorrentEngine::new(qbittorrent_connection_config(
                &platform_defaults,
                None,
                false,
            ))
            .map_err(|error| error.to_string())?,
        );
        registry
            .register(qbittorrent.clone())
            .map_err(|error| error.to_string())?;
        let registry = Arc::new(registry);
        let store = Arc::new(SharedDownloadTaskStore::new(Arc::clone(&storage)));
        let service = Arc::new(DownloadTaskService::new(Arc::clone(&registry), store));
        let torrent_import_directory =
            setting_path(&platform_defaults, "/storage/cacheDir")?.join("torrent-imports");
        Ok(Self {
            service,
            registry,
            qbittorrent,
            managed_qbittorrent: AppManagedQbittorrentState::new(app),
            embedded_lifecycle: Arc::new(Mutex::new(EmbeddedLifecycle::default())),
            embedded_operation: Arc::new(AsyncMutex::new(())),
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            embedded_transport,
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            embedded_binary_path,
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            embedded_data_directory,
            #[cfg(any(target_os = "android", target_os = "ios"))]
            mobile_transport,
            storage,
            platform_defaults,
            torrent_import_directory,
        })
    }

    /// 返回 commands 和自动扫描共用的任务服务。
    pub(crate) fn service(&self) -> &Arc<DownloadTaskService> {
        &self.service
    }

    /// 从 SQLite 读取当前下载设置。
    pub(crate) fn settings(&self) -> Result<AppSettings, String> {
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("读取下载设置失败：{error}"))?;
        storage
            .repository()
            .get_settings(&self.platform_defaults)
            .map_err(|error| format!("读取下载设置失败：{error}"))
    }

    /// 按番剧目录 ID 读取追番配置，供下载路径规则解析使用。
    pub(crate) fn find_my_anime(&self, anime_id: &str) -> Result<Option<MyAnime>, String> {
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("读取追番下载目录失败：{error}"))?;
        storage
            .repository()
            .list_my_anime()
            .map_err(|error| format!("读取追番下载目录失败：{error}"))
            .map(|items| items.into_iter().find(|item| item.anime.id == anime_id))
    }

    /// 读取当前设置选择的默认下载引擎。
    pub(crate) fn default_engine(
        &self,
        settings: &AppSettings,
    ) -> Result<TorrentEngineKind, DownloadServiceError> {
        #[cfg(mobile)]
        {
            let _ = settings;
            return Ok(TorrentEngineKind::Embedded);
        }
        #[cfg(desktop)]
        match settings
            .pointer("/download/defaultTorrentEngine")
            .and_then(Value::as_str)
        {
            Some("embedded") | None
                if settings
                    .pointer("/download/embedded/enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true) =>
            {
                Ok(TorrentEngineKind::Embedded)
            }
            Some("embedded") | None => Err(DownloadServiceError::InvalidInput {
                field: "defaultTorrentEngine",
                message: "内置下载引擎已停用".to_owned(),
            }),
            Some("qbittorrent") => Ok(TorrentEngineKind::Qbittorrent),
            Some(value) => Err(DownloadServiceError::InvalidInput {
                field: "defaultTorrentEngine",
                message: format!("未知下载引擎：{value}"),
            }),
        }
    }

    /// 启动或刷新内置核心配置；配置失败不阻止 Tauri 首屏启动。
    pub(crate) fn start(&self) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            let settings = match state.settings() {
                Ok(settings) => settings,
                Err(error) => {
                    log::error!("Tauri 下载服务读取启动设置失败：{error}");
                    return;
                }
            };
            if let Err(error) = state.refresh_from_settings(&settings).await {
                log::error!("Tauri 下载引擎启动失败：{error}");
            }
        });
    }

    /// 设置变化后切换默认引擎并同步托管进程和传输参数。
    pub(crate) async fn refresh_from_settings(&self, settings: &AppSettings) -> Result<(), String> {
        let embedded_enabled = settings
            .pointer("/download/embedded/enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let default_engine = self
            .default_engine(settings)
            .map_err(|error| error.to_string())?;
        if default_engine == TorrentEngineKind::Embedded && embedded_enabled {
            self.start_embedded(settings).await?;
        } else {
            self.stop_embedded().await?;
        }
        if default_engine == TorrentEngineKind::Qbittorrent {
            if AppManagedQbittorrentState::should_auto_start(settings) {
                self.start_managed_qbittorrent(settings).await?;
            } else {
                self.managed_qbittorrent
                    .stop(settings, Some(&self.qbittorrent))
                    .await;
                self.configure_qbittorrent(settings, false).await?;
            }
        } else {
            self.managed_qbittorrent
                .stop(settings, Some(&self.qbittorrent))
                .await;
            self.qbittorrent
                .shutdown()
                .await
                .map_err(|error| error.to_string())?;
        }
        let restored = self
            .service
            .refresh(default_engine.clone())
            .await
            .map_err(|error| format!("恢复当前下载引擎任务失败：{error}"))?;
        log::info!(
            "Tauri 下载引擎切换完成：engine={:?}, restored_tasks={}",
            default_engine,
            restored.tasks.len()
        );
        Ok(())
    }

    /// 测试当前外部或托管 qBittorrent 连接并返回任务数量。
    pub(crate) async fn test_qbittorrent(&self) -> Result<usize, String> {
        let settings = self.settings()?;
        let managed = AppManagedQbittorrentState::is_managed_enabled(&settings)
            && self.managed_qbittorrent.status(&settings).await.running;
        self.configure_qbittorrent(&settings, managed).await?;
        self.qbittorrent
            .status()
            .await
            .map(|status| status.task_count)
            .map_err(|error| error.to_string())
    }

    /// 返回应用壳使用的当前默认下载服务健康状态。
    pub(crate) async fn download_service_status(&self) -> DownloadServiceStatus {
        let settings = match self.settings() {
            Ok(settings) => settings,
            Err(error) => return download_service_error(DownloadServiceMode::Embedded, error),
        };
        match self.default_engine(&settings) {
            Ok(TorrentEngineKind::Embedded) => match self.embedded_status(&settings).await {
                Ok(status) if status.last_error.is_some() => download_service_error(
                    DownloadServiceMode::Embedded,
                    status.last_error.unwrap_or_default(),
                ),
                Ok(status) if status.network_policy_blocked == Some(true) => {
                    DownloadServiceStatus {
                        mode: DownloadServiceMode::Embedded,
                        state: DownloadServiceState::Idle,
                        message: "移动网络下载已关闭，等待 Wi-Fi".to_owned(),
                        task_count: status.task_count,
                    }
                }
                Ok(status) if status.running => DownloadServiceStatus {
                    mode: DownloadServiceMode::Embedded,
                    state: DownloadServiceState::Online,
                    message: "内置下载引擎运行中".to_owned(),
                    task_count: status.task_count,
                },
                Ok(_) => DownloadServiceStatus {
                    mode: DownloadServiceMode::Embedded,
                    state: DownloadServiceState::Idle,
                    message: "内置下载引擎未启动".to_owned(),
                    task_count: None,
                },
                Err(error) => download_service_error(DownloadServiceMode::Embedded, error),
            },
            Ok(TorrentEngineKind::Qbittorrent) => {
                let managed = self.managed_qbittorrent.status(&settings).await;
                if managed.enabled {
                    if let Some(error) = managed.last_error {
                        return download_service_error(DownloadServiceMode::Managed, error);
                    }
                    return DownloadServiceStatus {
                        mode: DownloadServiceMode::Managed,
                        state: if managed.running {
                            DownloadServiceState::Online
                        } else {
                            DownloadServiceState::Idle
                        },
                        message: if managed.running {
                            "qBittorrent-nox 运行中".to_owned()
                        } else {
                            "qBittorrent-nox 未运行".to_owned()
                        },
                        task_count: None,
                    };
                }
                match self.qbittorrent.status().await {
                    Ok(status) => DownloadServiceStatus {
                        mode: DownloadServiceMode::External,
                        state: DownloadServiceState::Online,
                        message: "外部 qBittorrent 已连接".to_owned(),
                        task_count: Some(status.task_count),
                    },
                    Err(error) => {
                        download_service_error(DownloadServiceMode::External, error.to_string())
                    }
                }
            }
            Err(error) => download_service_error(DownloadServiceMode::Embedded, error.to_string()),
        }
    }

    /// 读取托管 qBittorrent 进程状态。
    pub(crate) async fn managed_qbittorrent_status(
        &self,
    ) -> Result<QbittorrentManagedStatus, String> {
        let settings = self.settings()?;
        Ok(self.managed_qbittorrent.status(&settings).await)
    }

    /// 手动启动托管进程、同步首次凭据并应用下载设置。
    pub(crate) async fn start_managed_qbittorrent(
        &self,
        settings: &AppSettings,
    ) -> Result<QbittorrentManagedStatus, String> {
        let status = self.managed_qbittorrent.start(settings).await?;
        if let Err(error) = self.configure_qbittorrent(settings, status.running).await {
            self.managed_qbittorrent.stop(settings, None).await;
            return Err(error);
        }
        Ok(self.managed_qbittorrent.status(settings).await)
    }

    /// 手动停止托管进程并清除 WebUI 会话。
    pub(crate) async fn stop_managed_qbittorrent(
        &self,
    ) -> Result<QbittorrentManagedStatus, String> {
        let settings = self.settings()?;
        let status = self
            .managed_qbittorrent
            .stop(&settings, Some(&self.qbittorrent))
            .await;
        self.qbittorrent
            .shutdown()
            .await
            .map_err(|error| error.to_string())?;
        Ok(status)
    }

    /// 按任务原属引擎删除，并在需要时临时唤起已停用的内置核心。
    pub(crate) async fn remove_task(
        &self,
        task_id: &str,
        delete_files: bool,
        visible_engine: &TorrentEngineKind,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        let task_engine = self.service.task_engine(task_id)?;
        let removed_engine_tasks = if task_engine == TorrentEngineKind::Embedded {
            let _operation = self.embedded_operation.lock().await;
            let settings = self
                .settings()
                .map_err(|error| embedded_remove_error("readSettingsForRemove", error))?;
            let embedded_running = self
                .embedded_status(&settings)
                .await
                .map_err(|error| embedded_remove_error("readStatusForRemove", error))?
                .running;
            let temporarily_started =
                should_temporarily_start_embedded(&task_engine, embedded_running);
            if temporarily_started {
                log::info!("删除历史任务前临时启动内置核心：task_id={task_id}");
                if let Err(error) = self.start_embedded_unlocked(&settings).await {
                    if let Err(stop_error) = self.stop_embedded_unlocked().await {
                        log::error!(
                            "内置核心临时启动失败后恢复停用失败：task_id={task_id}, error={stop_error}"
                        );
                    }
                    return Err(embedded_remove_error("startForRemove", error));
                }
            }

            let removal = self.service.remove(task_id, delete_files).await;
            if temporarily_started {
                match self.stop_embedded_unlocked().await {
                    Ok(()) => log::info!("历史任务删除后内置核心已恢复停用：task_id={task_id}"),
                    Err(stop_error) => {
                        log::error!(
                            "历史任务删除后内置核心恢复停用失败：task_id={task_id}, error={stop_error}"
                        );
                        if removal.is_ok() {
                            return Err(embedded_remove_error("restoreAfterRemove", stop_error));
                        }
                    }
                }
            }
            removal?
        } else {
            self.service.remove(task_id, delete_files).await?
        };

        if &task_engine == visible_engine {
            Ok(removed_engine_tasks)
        } else {
            self.service.list_for_engine(visible_engine)
        }
    }

    /// 启动或重配桌面 torrent-core，并记录生命周期结果。
    pub(crate) async fn start_embedded(&self, settings: &AppSettings) -> Result<(), String> {
        let _operation = self.embedded_operation.lock().await;
        self.start_embedded_unlocked(settings).await
    }

    /// 在持有内置核心操作锁时启动或重配核心。
    async fn start_embedded_unlocked(&self, settings: &AppSettings) -> Result<(), String> {
        let result = async {
            let engine = self
                .registry
                .require(&TorrentEngineKind::Embedded)
                .map_err(|error| error.to_string())?;
            engine
                .configure(&embedded_engine_config(settings))
                .await
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(())
        }
        .await;
        if let Ok(mut lifecycle) = self.embedded_lifecycle.lock() {
            match &result {
                Ok(()) => {
                    lifecycle.last_started_at = Some(now_iso());
                    lifecycle.last_error = None;
                }
                Err(error) => lifecycle.last_error = Some(error.clone()),
            }
        }
        result
    }

    /// 请求 torrent-core 保存恢复数据并停止。
    pub(crate) async fn stop_embedded(&self) -> Result<(), String> {
        let _operation = self.embedded_operation.lock().await;
        self.stop_embedded_unlocked().await
    }

    /// 在持有内置核心操作锁时保存恢复数据并停止核心。
    async fn stop_embedded_unlocked(&self) -> Result<(), String> {
        if let Ok(engine) = self.registry.require(&TorrentEngineKind::Embedded) {
            engine.shutdown().await.map_err(|error| error.to_string())?;
            if let Ok(mut lifecycle) = self.embedded_lifecycle.lock() {
                lifecycle.last_stopped_at = Some(now_iso());
            }
        }
        Ok(())
    }

    /// 读取内置核心进程与协议状态，未运行时不会隐式启动。
    pub(crate) async fn embedded_status(
        &self,
        settings: &AppSettings,
    ) -> Result<EmbeddedTorrentCoreStatus, String> {
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        {
            let pid = self
                .embedded_transport
                .process_id()
                .await
                .map_err(|error| error.to_string())?;
            let protocol = if pid.is_some() {
                Some(
                    self.registry
                        .require(&TorrentEngineKind::Embedded)
                        .map_err(|error| error.to_string())?
                        .status()
                        .await
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            let lifecycle = self
                .embedded_lifecycle
                .lock()
                .map_err(|error| error.to_string())?;
            Ok(EmbeddedTorrentCoreStatus {
                enabled: settings
                    .pointer("/download/embedded/enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                running: pid.is_some(),
                platform: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                binary_path: Some(self.embedded_binary_path.to_string_lossy().into_owned()),
                data_dir: Some(self.embedded_data_directory.to_string_lossy().into_owned()),
                pid,
                foreground_service: None,
                version: protocol.as_ref().map(|value| value.version.clone()),
                task_count: protocol.as_ref().map(|value| value.task_count),
                listen_port: protocol.as_ref().and_then(|value| value.listen_port),
                network_policy_blocked: protocol.as_ref().map(|value| value.network_policy_blocked),
                last_started_at: lifecycle.last_started_at.clone(),
                last_stopped_at: lifecycle.last_stopped_at.clone(),
                last_error: lifecycle.last_error.clone(),
            })
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let native = self
                .mobile_transport
                .native_status()
                .await
                .map_err(|error| error.to_string())?;
            let protocol = if native.running {
                Some(
                    self.registry
                        .require(&TorrentEngineKind::Embedded)
                        .map_err(|error| error.to_string())?
                        .status()
                        .await
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            let lifecycle = self
                .embedded_lifecycle
                .lock()
                .map_err(|error| error.to_string())?;
            Ok(EmbeddedTorrentCoreStatus {
                enabled: settings
                    .pointer("/download/embedded/enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                running: native.running,
                platform: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                binary_path: None,
                data_dir: native.data_directory,
                pid: None,
                foreground_service: Some(native.foreground_service),
                version: protocol.as_ref().map(|value| value.version.clone()),
                task_count: protocol.as_ref().map(|value| value.task_count),
                listen_port: protocol.as_ref().and_then(|value| value.listen_port),
                network_policy_blocked: protocol.as_ref().map(|value| value.network_policy_blocked),
                last_started_at: lifecycle.last_started_at.clone(),
                last_stopped_at: lifecycle.last_stopped_at.clone(),
                last_error: lifecycle.last_error.clone(),
            })
        }
    }

    /// 为外部或托管 WebUI 更新连接、首次凭据和传输限制。
    async fn configure_qbittorrent(
        &self,
        settings: &AppSettings,
        managed_running: bool,
    ) -> Result<(), String> {
        let base_url = self.managed_qbittorrent.runtime_base_url(settings).await;
        let desired =
            qbittorrent_connection_config(settings, Some(base_url.clone()), managed_running);
        self.qbittorrent
            .update_connection(desired.clone())
            .await
            .map_err(|error| error.to_string())?;
        let config = qbittorrent_engine_config(settings);
        match self.qbittorrent.configure(&config).await {
            Ok(_) => Ok(()),
            Err(initial_error) if managed_running => {
                let temporary_password = self
                    .managed_qbittorrent
                    .temporary_password()
                    .await
                    .ok_or_else(|| initial_error.to_string())?;
                let bootstrap = QbittorrentEngine::new(QbittorrentConnectionConfig::new(
                    base_url,
                    "admin".to_owned(),
                    Some(temporary_password),
                ))
                .map_err(|error| error.to_string())?;
                let (username, password) = managed_credentials(settings);
                bootstrap
                    .update_webui_credentials(&username, &password)
                    .await
                    .map_err(|error| format!("同步托管 qBittorrent 凭据失败：{error}"))?;
                self.qbittorrent
                    .update_connection(desired)
                    .await
                    .map_err(|error| error.to_string())?;
                self.qbittorrent
                    .configure(&config)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// 下载远程 torrent 到受限临时目录，磁链直接返回。
    pub(crate) async fn prepare_source(
        &self,
        url: &str,
        settings: &AppSettings,
    ) -> Result<PreparedDownloadSource, String> {
        let url = url.trim();
        if url.to_ascii_lowercase().starts_with("magnet:?") {
            return Ok(PreparedDownloadSource {
                source: DownloadSource::Magnet(url.to_owned()),
                temporary_file: None,
            });
        }
        let parsed = url::Url::parse(url).map_err(|error| format!("种子地址无效：{error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("种子地址仅允许 magnet、HTTP 或 HTTPS".to_owned());
        }
        let client = NativeHttpClient::new(native_http_config(settings))
            .map_err(|error| format!("创建种子下载连接失败：{error}"))?;
        let response = client
            .execute(NativeHttpRequest {
                source_id: "torrent-import".to_owned(),
                method: HttpMethod::Get,
                url: parsed.to_string(),
                headers: BTreeMap::new(),
                body: None,
                request_interval_ms: 0,
            })
            .await
            .map_err(|error| format!("下载 torrent 文件失败：{error}"))?;
        if !(200..300).contains(&response.status) {
            return Err(format!("下载 torrent 文件失败：HTTP {}", response.status));
        }
        if response.body.first() != Some(&b'd') {
            return Err("远程响应不是有效的 torrent 元信息".to_owned());
        }
        let path = self.next_torrent_import_path().await?;
        tokio::fs::write(&path, response.body)
            .await
            .map_err(|error| format!("写入种子临时文件失败：{error}"))?;
        Ok(PreparedDownloadSource {
            source: DownloadSource::TorrentFile(path.clone()),
            temporary_file: Some(path),
        })
    }

    /// 校验用户选择的本地 torrent，并复制到应用私有临时目录。
    pub(crate) async fn prepare_local_torrent(
        &self,
        source: &Path,
    ) -> Result<PreparedDownloadSource, String> {
        let metadata = tokio::fs::metadata(source)
            .await
            .map_err(|error| format!("读取本地种子文件失败：{error}"))?;
        if !metadata.is_file() {
            return Err("所选 torrent 不是普通文件".to_owned());
        }
        if metadata.len() == 0 || metadata.len() > MAX_TORRENT_FILE_BYTES {
            return Err("torrent 文件为空或超过 32 MiB 限制".to_owned());
        }
        let bytes = tokio::fs::read(source)
            .await
            .map_err(|error| format!("读取本地种子文件失败：{error}"))?;
        if bytes.first() != Some(&b'd') {
            return Err("所选文件不是有效的 torrent 元信息".to_owned());
        }
        let path = self.next_torrent_import_path().await?;
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|error| format!("复制本地种子文件失败：{error}"))?;
        Ok(PreparedDownloadSource {
            source: DownloadSource::TorrentFile(path.clone()),
            temporary_file: Some(path),
        })
    }

    /// 在应用私有缓存中分配不冲突的 torrent 临时路径。
    async fn next_torrent_import_path(&self) -> Result<PathBuf, String> {
        tokio::fs::create_dir_all(&self.torrent_import_directory)
            .await
            .map_err(|error| format!("创建种子临时目录失败：{error}"))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = TORRENT_IMPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .torrent_import_directory
            .join(format!("import-{timestamp}-{sequence}.torrent")))
    }

    /// 请求全部已注册引擎保存状态并关闭。
    pub(crate) async fn shutdown(&self) {
        if let Ok(settings) = self.settings() {
            self.managed_qbittorrent
                .stop(&settings, Some(&self.qbittorrent))
                .await;
        }
        for (kind, error) in self.registry.shutdown_all().await {
            log::error!("Tauri 下载引擎关闭失败 engine={kind:?} error={error}");
        }
    }
}

/// 判断删除任务是否需要临时启动当前未运行的内置核心。
fn should_temporarily_start_embedded(
    task_engine: &TorrentEngineKind,
    embedded_running: bool,
) -> bool {
    task_engine == &TorrentEngineKind::Embedded && !embedded_running
}

/// 将宿主侧内置核心生命周期错误映射到稳定下载错误模型。
fn embedded_remove_error(
    operation: &'static str,
    error: impl Into<String>,
) -> DownloadServiceError {
    DownloadServiceError::Engine {
        engine: TorrentEngineKind::Embedded,
        operation,
        source: ani_downloads::DownloadEngineError::Unavailable(error.into()),
    }
}

/// 保证远程 torrent 临时文件在添加完成后清理。
pub(crate) struct PreparedDownloadSource {
    source: DownloadSource,
    temporary_file: Option<PathBuf>,
}

impl PreparedDownloadSource {
    /// 返回可交给统一任务服务的来源快照。
    pub(crate) fn source(&self) -> DownloadSource {
        self.source.clone()
    }
}

impl Drop for PreparedDownloadSource {
    fn drop(&mut self) {
        if let Some(path) = self.temporary_file.take() {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("清理种子临时文件失败 path={} error={error}", path.display());
                }
            }
        }
    }
}

/// 将资源元数据和明确业务关联转换为统一下载上下文。
pub(crate) fn release_download_context(
    release: &Release,
    anime_id: Option<String>,
    anime_title: Option<String>,
    episode_id: Option<String>,
    episode_no: Option<f64>,
    fansub_group_id: Option<String>,
) -> DownloadTaskContext {
    DownloadTaskContext {
        name: Some(release.title.clone()),
        release_id: Some(release.id.clone()),
        anime_id,
        episode_id,
        anime_title,
        episode_no,
        fansub_group_id,
        fansub_name: release.fansub_name.clone(),
        resolution: release
            .resolution
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        declared_video_codec: release.declared_video_codec.clone(),
        normalized_video_codec: release
            .normalized_video_codec
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        bit_depth: release.bit_depth,
        subtitle_languages: release
            .subtitle_languages
            .iter()
            .map(subtitle_language_value)
            .map(str::to_owned)
            .collect(),
        subtitle: release
            .subtitle
            .as_ref()
            .map(subtitle_preference_value)
            .map(str::to_owned),
    }
}

fn subtitle_language_value(value: &SubtitleLanguage) -> &'static str {
    match value {
        SubtitleLanguage::Chs => "chs",
        SubtitleLanguage::Cht => "cht",
        SubtitleLanguage::Jpn => "jpn",
        SubtitleLanguage::Eng => "eng",
    }
}

fn subtitle_preference_value(value: &SubtitlePreference) -> &'static str {
    match value {
        SubtitlePreference::Chs => "chs",
        SubtitlePreference::Cht => "cht",
        SubtitlePreference::Jpn => "jpn",
        SubtitlePreference::Eng => "eng",
        SubtitlePreference::Multi => "multi",
    }
}

/// 将版本化设置解析为 torrent-core 运行配置。
fn embedded_engine_config(settings: &AppSettings) -> DownloadEngineConfig {
    let download = settings.pointer("/download");
    let embedded = settings.pointer("/download/embedded");
    let seeding = embedded.and_then(|value| value.get("seedingLimits"));
    DownloadEngineConfig {
        listen_port: setting_u64(embedded, "listenPort", 51_413, 1_024, 65_535) as u16,
        dht_enabled: setting_bool(embedded, "dhtEnabled", true),
        upnp_enabled: setting_bool(embedded, "upnpEnabled", true),
        max_active_downloads: setting_u64(embedded, "maxActiveDownloads", 3, 1, 100) as u32,
        max_download_speed: setting_u64(embedded, "maxDownloadSpeed", 0, 0, u32::MAX as u64) as u32,
        max_upload_speed: setting_u64(embedded, "maxUploadSpeed", 0, 0, u32::MAX as u64) as u32,
        allow_metered_downloads: setting_bool(download, "allowMeteredDownloads", !cfg!(mobile)),
        seeding_limits: SeedingLimits {
            enabled: setting_bool(seeding, "enabled", false),
            ratio_enabled: setting_bool(seeding, "ratioEnabled", false),
            ratio_limit: seeding
                .and_then(|value| value.get("ratioLimit"))
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(0.1),
            time_enabled: setting_bool(seeding, "timeEnabled", false),
            time_limit_minutes: setting_u64(seeding, "timeLimitMinutes", 120, 1, u32::MAX as u64)
                as u32,
        },
    }
}

/// 将 qBittorrent 设置解析为 WebUI 连接参数。
fn qbittorrent_connection_config(
    settings: &AppSettings,
    base_url: Option<String>,
    managed: bool,
) -> QbittorrentConnectionConfig {
    let (username, password) = if managed {
        let (username, password) = managed_credentials(settings);
        (username, Some(password))
    } else {
        (
            settings
                .pointer("/download/qbittorrent/username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            settings
                .pointer("/download/qbittorrent/password")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
    };
    QbittorrentConnectionConfig::new(
        base_url.unwrap_or_else(|| {
            settings
                .pointer("/download/qbittorrent/baseUrl")
                .and_then(Value::as_str)
                .unwrap_or("http://127.0.0.1:18080")
                .to_owned()
        }),
        username,
        password,
    )
}

fn download_service_error(
    mode: DownloadServiceMode,
    message: impl Into<String>,
) -> DownloadServiceStatus {
    DownloadServiceStatus {
        mode,
        state: DownloadServiceState::Error,
        message: message.into(),
        task_count: None,
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// 将 qBittorrent KiB/s 和做种设置映射到统一引擎配置。
fn qbittorrent_engine_config(settings: &AppSettings) -> DownloadEngineConfig {
    let qbittorrent = settings.pointer("/download/qbittorrent");
    let seeding = qbittorrent.and_then(|value| value.get("seedingLimits"));
    DownloadEngineConfig {
        listen_port: 0,
        dht_enabled: false,
        upnp_enabled: false,
        max_active_downloads: 1,
        max_download_speed: setting_u64(qbittorrent, "downloadLimitKiBps", 0, 0, u32::MAX as u64)
            as u32,
        max_upload_speed: setting_u64(qbittorrent, "uploadLimitKiBps", 0, 0, u32::MAX as u64)
            as u32,
        allow_metered_downloads: true,
        seeding_limits: SeedingLimits {
            enabled: setting_bool(seeding, "enabled", false),
            ratio_enabled: setting_bool(seeding, "ratioEnabled", false),
            ratio_limit: seeding
                .and_then(|value| value.get("ratioLimit"))
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(0.1),
            time_enabled: setting_bool(seeding, "timeEnabled", false),
            time_limit_minutes: setting_u64(seeding, "timeLimitMinutes", 120, 1, u32::MAX as u64)
                as u32,
        },
    }
}

fn setting_bool(parent: Option<&Value>, key: &str, fallback: bool) -> bool {
    parent
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

fn setting_u64(parent: Option<&Value>, key: &str, fallback: u64, min: u64, max: u64) -> u64 {
    parent
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(fallback)
        .clamp(min, max)
}

fn setting_path(settings: &AppSettings, pointer: &str) -> Result<PathBuf, String> {
    settings
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("平台设置缺少路径：{pointer}"))
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn resolve_torrent_core_binary(app: &AppHandle) -> PathBuf {
    if let Some(path) = std::env::var_os("ANI_TORRENT_CORE_PATH") {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
    }
    let platform = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        value => value,
    };
    let binary_name = if cfg!(target_os = "windows") {
        "torrent-core.exe"
    } else {
        "torrent-core"
    };
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut roots = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        roots.push(resources.join("torrent-core"));
    }
    #[cfg(debug_assertions)]
    if let Some(workspace) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        // Tauri dev 会从 src-tauri 启动宿主，显式加入工作区根目录下的已整理资源。
        roots.extend([
            workspace.join("out/torrent-core"),
            workspace.join("resources/torrent-core"),
            workspace.join("native/torrent-core/build/portable-release"),
        ]);
    }
    roots.extend([
        current.join("out/torrent-core"),
        current.join("resources/torrent-core"),
        current.join("native/torrent-core/build/release"),
        current.join("native/torrent-core/build/Release"),
        current.join("native/torrent-core/build/portable-release"),
    ]);
    for root in &roots {
        for candidate in [
            root.join(format!("{platform}-{arch}")).join(binary_name),
            root.join(platform).join(binary_name),
            root.join(binary_name),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    roots[0]
        .join(format!("{platform}-{arch}"))
        .join(binary_name)
}

/// 返回可下载的资源地址，并兼容实体分隔符曾被吞掉的历史磁链。
pub(crate) fn release_download_source_url(release: &Release) -> Result<String, String> {
    if let Some(magnet) = release
        .magnet_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if has_valid_magnet_info_hash(magnet) {
            return Ok(magnet.to_owned());
        }
        let info_hash = release
            .info_hash
            .as_deref()
            .and_then(normalize_info_hash)
            .or_else(|| embedded_magnet_info_hash(magnet))
            .ok_or_else(|| "添加资源下载失败：磁链中的 info-hash 无效".to_owned())?;
        let repaired = restore_magnet_parameter_separators(magnet, &info_hash)
            .filter(|value| has_valid_magnet_info_hash(value))
            .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{info_hash}"));
        log::warn!(
            "Tauri 历史磁链已在下载边界修复 release_id={} info_hash={}",
            release.id,
            info_hash
        );
        return Ok(repaired);
    }
    release
        .torrent_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "添加资源下载失败：资源没有磁链或 torrent 地址".to_owned())
}

/// 判断磁链是否包含独立且格式正确的 BT info-hash 参数。
fn has_valid_magnet_info_hash(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme().eq_ignore_ascii_case("magnet")
            && url.query_pairs().any(|(key, value)| {
                key.eq_ignore_ascii_case("xt")
                    && value
                        .to_ascii_lowercase()
                        .strip_prefix("urn:btih:")
                        .and_then(normalize_info_hash)
                        .is_some()
            })
    })
}

/// 规范十六进制或 Base32 BT info-hash。
fn normalize_info_hash(value: &str) -> Option<String> {
    let value = value.trim();
    let valid_hex = value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    let valid_base32 = value.len() == 32
        && value.bytes().all(|byte| {
            byte.to_ascii_uppercase().is_ascii_uppercase() || (b'2'..=b'7').contains(&byte)
        });
    (valid_hex || valid_base32).then(|| value.to_ascii_lowercase())
}

/// 从即使参数已粘连的 magnet xt 中提取十六进制或 Base32 hash。
fn embedded_magnet_info_hash(value: &str) -> Option<String> {
    let lowercase = value.to_ascii_lowercase();
    let marker = "xt=urn:btih:";
    let start = lowercase.find(marker)? + marker.len();
    [40, 32].into_iter().find_map(|length| {
        value
            .get(start..start + length)
            .and_then(normalize_info_hash)
    })
}

/// 为旧版 XML 解析器吞掉的 magnet 查询参数恢复 `&` 分隔符。
fn restore_magnet_parameter_separators(value: &str, info_hash: &str) -> Option<String> {
    let lowercase = value.to_ascii_lowercase();
    let marker = "xt=urn:btih:";
    let hash_start = lowercase.find(marker)? + marker.len();
    let hash_end = hash_start + info_hash.len();
    let embedded_hash = normalize_info_hash(value.get(hash_start..hash_end)?)?;
    if embedded_hash != info_hash {
        return None;
    }
    let mut suffix = value.get(hash_end..)?;
    let mut repaired = value.get(..hash_end)?.to_owned();
    if suffix.is_empty() {
        return Some(repaired);
    }

    while !suffix.is_empty() {
        let prefix = MAGNET_PARAMETER_PREFIXES
            .iter()
            .find(|prefix| suffix.starts_with(**prefix))?;
        let value_start = prefix.len();
        let next = MAGNET_PARAMETER_PREFIXES
            .iter()
            .filter_map(|candidate| {
                suffix[value_start..]
                    .find(candidate)
                    .map(|offset| value_start + offset)
            })
            .min()
            .unwrap_or(suffix.len());
        repaired.push('&');
        repaired.push_str(&suffix[..next]);
        suffix = &suffix[next..];
    }
    Some(repaired)
}

#[cfg(test)]
mod tests {
    use ani_domain::EpisodeStatus;
    use serde_json::json;

    use super::*;

    /// 验证下载设置被边界化后映射到核心配置。
    #[test]
    fn maps_download_engine_settings() {
        let config = embedded_engine_config(&json!({
            "download": {
                "embedded": {
                    "listenPort": 1,
                    "maxActiveDownloads": 0,
                    "maxDownloadSpeed": 512,
                    "seedingLimits": {
                        "enabled": true,
                        "ratioLimit": 0,
                        "timeLimitMinutes": 0
                    }
                }
            }
        }));
        assert_eq!(config.listen_port, 1_024);
        assert_eq!(config.max_active_downloads, 1);
        assert_eq!(config.max_download_speed, 512);
        assert!(config.seeding_limits.enabled);
        assert_eq!(config.seeding_limits.ratio_limit, 0.1);
        assert_eq!(config.seeding_limits.time_limit_minutes, 1);
    }

    /// 验证 qBittorrent 限速和做种设置使用独立配置分支。
    #[test]
    fn maps_qbittorrent_engine_settings() {
        let config = qbittorrent_engine_config(&json!({
            "download": {
                "qbittorrent": {
                    "downloadLimitKiBps": 512,
                    "uploadLimitKiBps": 128,
                    "seedingLimits": {
                        "enabled": true,
                        "ratioEnabled": true,
                        "ratioLimit": 1.5,
                        "timeEnabled": true,
                        "timeLimitMinutes": 90
                    }
                }
            }
        }));
        assert_eq!(config.max_download_speed, 512);
        assert_eq!(config.max_upload_speed, 128);
        assert_eq!(config.seeding_limits.ratio_limit, 1.5);
        assert_eq!(config.seeding_limits.time_limit_minutes, 90);
    }

    /// 验证仅删除已停用内置核心的历史任务时执行临时启停。
    #[test]
    fn decides_temporary_embedded_lifecycle_for_removal() {
        assert!(should_temporarily_start_embedded(
            &TorrentEngineKind::Embedded,
            false
        ));
        assert!(!should_temporarily_start_embedded(
            &TorrentEngineKind::Embedded,
            true
        ));
        assert!(!should_temporarily_start_embedded(
            &TorrentEngineKind::Qbittorrent,
            false
        ));
    }

    /// 验证历史 AniBT 磁链恢复 dn、xl 和 tracker 参数分隔符。
    #[test]
    fn repairs_legacy_anibt_magnet_before_download() {
        let release = test_release(
            "magnet:?xt=urn:btih:5448ae0ed36912eb0dfba53c3e495b9988841e68dn=%5BNix-Raws%5D%20Episode%2001xl=1479404657tr=https%3A%2F%2Ftracker.anibt.net%2Fannounce",
            Some("5448ae0ed36912eb0dfba53c3e495b9988841e68"),
        );

        assert_eq!(
            release_download_source_url(&release).expect("repair historical magnet"),
            "magnet:?xt=urn:btih:5448ae0ed36912eb0dfba53c3e495b9988841e68&dn=%5BNix-Raws%5D%20Episode%2001&xl=1479404657&tr=https%3A%2F%2Ftracker.anibt.net%2Fannounce"
        );
    }

    /// 验证格式正确的磁链不会在下载边界被重新编码。
    #[test]
    fn preserves_valid_release_magnet() {
        let magnet = "magnet:?xt=urn:btih:5448ae0ed36912eb0dfba53c3e495b9988841e68&dn=Episode";
        let release = test_release(magnet, None);
        assert_eq!(
            release_download_source_url(&release).expect("preserve valid magnet"),
            magnet
        );
    }

    /// 验证旧缓存中的 Base32 磁链也能从 xt 参数恢复。
    #[test]
    fn repairs_legacy_base32_magnet_without_cached_hash() {
        let release = test_release(
            "magnet:?xt=urn:btih:AERUKZ4JVPG66AJDIVTYTK6LWO5FADP5dn=Episode",
            None,
        );

        assert_eq!(
            release_download_source_url(&release).expect("repair Base32 magnet"),
            "magnet:?xt=urn:btih:AERUKZ4JVPG66AJDIVTYTK6LWO5FADP5&dn=Episode"
        );
    }

    /// 验证合集视频文件按文件名关联单集，非视频文件不会污染下载状态。
    #[test]
    fn associates_collection_video_files_with_episodes() {
        let episodes = vec![
            test_episode("episode-1", 1.0),
            test_episode("episode-2", 2.0),
        ];
        let mut files = vec![
            test_torrent_file(0, "Season/[Group] Anime - 01 [1080p].mkv"),
            test_torrent_file(1, "Season/02.mkv"),
            test_torrent_file(2, "Season/cover.jpg"),
        ];

        assert_eq!(
            associate_torrent_files(&mut files, None, None, &episodes),
            2
        );
        assert_eq!(files[0].episode_id.as_deref(), Some("episode-1"));
        assert_eq!(files[0].episode_no, Some(1.0));
        assert_eq!(files[1].episode_id.as_deref(), Some("episode-2"));
        assert_eq!(files[2].episode_id, None);
        assert_eq!(
            associate_torrent_files(&mut files, None, None, &episodes),
            0
        );
    }

    /// 验证父目录合集范围不会覆盖乱序视频文件自身的集数。
    #[test]
    fn associates_out_of_order_collection_files_by_basename_episode() {
        let episodes = (1..=12)
            .map(|episode_no| test_episode(&format!("episode-{episode_no}"), f64::from(episode_no)))
            .collect::<Vec<_>>();
        let episode_order = [12, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 1];
        let mut files = episode_order
            .iter()
            .enumerate()
            .map(|(file_index, episode_no)| {
                test_torrent_file(
                    file_index as i64,
                    &format!(
                        "[CheeseAni] KimiSen Season Ⅱ [1-12][CR-WebRip 1080p HEVC AAC][简繁内封]/[CheeseAni] KimiSen Season Ⅱ [{episode_no:02}][CR-WebRip 1080p HEVC AAC][简繁内封].mkv"
                    ),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            associate_torrent_files(&mut files, None, None, &episodes),
            12
        );
        for (file, episode_no) in files.iter().zip(episode_order) {
            let episode_id = format!("episode-{episode_no}");
            assert_eq!(file.episode_id.as_deref(), Some(episode_id.as_str()));
            assert_eq!(file.episode_no, Some(f64::from(episode_no)));
        }
    }

    fn test_episode(id: &str, episode_no: f64) -> Episode {
        Episode {
            id: id.to_owned(),
            anime_id: "anime-1".to_owned(),
            episode_no,
            title: None,
            air_time: None,
            status: EpisodeStatus::Aired,
        }
    }

    fn test_torrent_file(index: i64, name: &str) -> TorrentFile {
        TorrentFile {
            id: format!("file-{index}"),
            index,
            name: name.to_owned(),
            episode_id: None,
            episode_no: None,
            size: 1_024,
            progress: 1.0,
            priority: 1,
            selected: true,
        }
    }

    fn test_release(magnet_url: &str, info_hash: Option<&str>) -> Release {
        serde_json::from_value(json!({
            "id": "anibt:release-1",
            "title": "[Nix-Raws] Episode 01",
            "sourceId": "anibt",
            "sourceName": "AniBT",
            "magnetUrl": magnet_url,
            "infoHash": info_hash,
            "size": 1479404657,
            "publishedAt": "2026-07-26T00:00:00.000Z"
        }))
        .expect("build test release")
    }
}
