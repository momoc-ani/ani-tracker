use std::path::PathBuf;

use ani_contracts::{
    AppCommandError, DownloadServiceStatus, EmbeddedTorrentCoreStatus, QbittorrentManagedStatus,
    TorrentConnectionTestResult,
};
use ani_domain::{resolve_anime_download_path, DownloadTask, MyAnime, Release, TorrentEngineKind};
use ani_downloads::{
    AddTorrentOptions, DownloadAddRequest, DownloadServiceError, DownloadTaskContext,
};
use ani_sources::{classify_anime_release, AnimeReleaseCompatibility};
use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
#[cfg(mobile)]
use tauri_plugin_ani_mobile::AniMobileExt;
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::downloads::{release_download_context, release_download_source_url, AppDownloadState};
use crate::media::AppMediaState;

const DOWNLOAD_SERVICE_STATUS_CHANGED_EVENT: &str = "download-service-status-changed";

/// 通知应用壳重新读取默认下载服务状态。
pub(crate) fn emit_download_service_status_changed(app: &AppHandle) {
    if let Err(error) = app.emit(DOWNLOAD_SERVICE_STATUS_CHANGED_EVENT, ()) {
        log::warn!("发布下载服务状态事件失败 error={error}");
    }
}

/// 测试当前 qBittorrent WebUI 登录和任务读取。
#[tauri::command]
pub(crate) async fn test_qbittorrent(
    state: State<'_, AppDownloadState>,
) -> Result<TorrentConnectionTestResult, AppCommandError> {
    match state.test_qbittorrent().await {
        Ok(task_count) => Ok(TorrentConnectionTestResult {
            ok: true,
            message: "连接正常".to_owned(),
            task_count: Some(task_count),
        }),
        Err(error) => Ok(TorrentConnectionTestResult {
            ok: false,
            message: error,
            task_count: None,
        }),
    }
}

/// 读取当前默认下载引擎的统一健康状态。
#[tauri::command]
pub(crate) async fn get_download_service_status(
    state: State<'_, AppDownloadState>,
) -> Result<DownloadServiceStatus, AppCommandError> {
    Ok(state.download_service_status().await)
}

/// 读取托管 qBittorrent-nox 的进程状态。
#[tauri::command]
pub(crate) async fn get_qbittorrent_managed_status(
    state: State<'_, AppDownloadState>,
) -> Result<QbittorrentManagedStatus, AppCommandError> {
    state
        .managed_qbittorrent_status()
        .await
        .map_err(|error| runtime_error("读取托管 qBittorrent 状态", error))
}

/// 手动启动托管 qBittorrent-nox 并同步 WebUI 凭据。
#[tauri::command]
pub(crate) async fn start_qbittorrent_managed(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<QbittorrentManagedStatus, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取托管 qBittorrent 设置", error))?;
    let status = state
        .start_managed_qbittorrent(&settings)
        .await
        .map_err(|error| runtime_error("启动托管 qBittorrent", error))?;
    emit_download_service_status_changed(&app);
    Ok(status)
}

/// 手动停止托管 qBittorrent-nox。
#[tauri::command]
pub(crate) async fn stop_qbittorrent_managed(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<QbittorrentManagedStatus, AppCommandError> {
    let status = state
        .stop_managed_qbittorrent()
        .await
        .map_err(|error| runtime_error("停止托管 qBittorrent", error))?;
    emit_download_service_status_changed(&app);
    Ok(status)
}

/// 读取内置 torrent-core 的进程与协议状态。
#[tauri::command]
pub(crate) async fn get_embedded_torrent_status(
    state: State<'_, AppDownloadState>,
) -> Result<EmbeddedTorrentCoreStatus, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取内置下载设置", error))?;
    state
        .embedded_status(&settings)
        .await
        .map_err(|error| runtime_error("读取内置下载核心状态", error))
}

/// 手动启动内置 torrent-core。
#[tauri::command]
pub(crate) async fn start_embedded_torrent(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<EmbeddedTorrentCoreStatus, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取内置下载设置", error))?;
    state
        .start_embedded(&settings)
        .await
        .map_err(|error| runtime_error("启动内置下载核心", error))?;
    let status = state
        .embedded_status(&settings)
        .await
        .map_err(|error| runtime_error("读取内置下载核心状态", error))?;
    emit_download_service_status_changed(&app);
    Ok(status)
}

/// 手动停止内置 torrent-core 并保存恢复数据。
#[tauri::command]
pub(crate) async fn stop_embedded_torrent(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<EmbeddedTorrentCoreStatus, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取内置下载设置", error))?;
    state
        .stop_embedded()
        .await
        .map_err(|error| runtime_error("停止内置下载核心", error))?;
    let status = state
        .embedded_status(&settings)
        .await
        .map_err(|error| runtime_error("读取内置下载核心状态", error))?;
    emit_download_service_status_changed(&app);
    Ok(status)
}

/// 先优雅停止再按当前设置重启内置 torrent-core。
#[tauri::command]
pub(crate) async fn restart_embedded_torrent(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<EmbeddedTorrentCoreStatus, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取内置下载设置", error))?;
    state
        .stop_embedded()
        .await
        .map_err(|error| runtime_error("停止内置下载核心", error))?;
    state
        .start_embedded(&settings)
        .await
        .map_err(|error| runtime_error("重启内置下载核心", error))?;
    let status = state
        .embedded_status(&settings)
        .await
        .map_err(|error| runtime_error("读取内置下载核心状态", error))?;
    emit_download_service_status_changed(&app);
    Ok(status)
}

/// 手动磁链或远程 torrent 添加输入。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddDownloadUrlInput {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    save_path: Option<String>,
    #[serde(default)]
    paused: bool,
}

/// 从资源搜索结果添加任务的完整业务关联输入。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddReleaseDownloadInput {
    release: Release,
    #[serde(default)]
    anime_id: Option<String>,
    #[serde(default)]
    episode_id: Option<String>,
    #[serde(default)]
    episode_no: Option<f64>,
    #[serde(default)]
    fansub_group_id: Option<String>,
    #[serde(default)]
    save_path: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    confirm_unknown_season: bool,
}

/// 读取全部本地下载任务快照。
#[tauri::command]
pub(crate) async fn list_downloads(
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("读取下载任务", error))?;
    let engine = state
        .default_engine(&settings)
        .map_err(|error| map_download_error("读取下载任务", error))?;
    state
        .service()
        .list_for_engine(&engine)
        .map_err(|error| map_download_error("读取下载任务", error))
}

/// 刷新当前默认引擎，其他引擎任务保留持久化快照。
#[tauri::command]
pub(crate) async fn refresh_downloads(
    state: State<'_, AppDownloadState>,
    media_state: State<'_, AppMediaState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error("刷新下载任务", error))?;
    let default_engine = state
        .default_engine(&settings)
        .map_err(|error| map_download_error("刷新下载任务", error))?;
    if default_engine == TorrentEngineKind::Qbittorrent {
        let managed = state
            .managed_qbittorrent_status()
            .await
            .map_err(|error| runtime_error("读取托管 qBittorrent 状态", error))?;
        if managed.enabled && !managed.running {
            log::debug!(
                "托管 qBittorrent 未运行，返回持久化任务快照 error={}",
                managed.last_error.as_deref().unwrap_or("进程未启动")
            );
            return state
                .service()
                .list_for_engine(&default_engine)
                .map_err(|error| map_download_error("刷新下载任务", error));
        }
    }
    let result = state
        .service()
        .refresh(default_engine)
        .await
        .map_err(|error| map_download_error("刷新下载任务", error))?;
    media_state.schedule_completed_scan(result.tasks.clone());
    Ok(result.tasks)
}

/// 暂停任务创建时所属的下载引擎。
#[tauri::command]
pub(crate) async fn pause_download(
    task_id: String,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    let engine = current_engine(&state, "暂停下载任务")?;
    state
        .service()
        .pause(&task_id, &engine)
        .await
        .map_err(|error| map_download_error("暂停下载任务", error))
}

/// 恢复任务创建时所属的下载引擎。
#[tauri::command]
pub(crate) async fn resume_download(
    task_id: String,
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    ensure_mobile_storage_available(&app)?;
    let engine = current_engine(&state, "恢复下载任务")?;
    state
        .service()
        .resume(&task_id, &engine)
        .await
        .map_err(|error| map_download_error("恢复下载任务", error))
}

/// 从引擎和 SQLite 删除任务，并按请求决定是否删除文件。
#[tauri::command]
pub(crate) async fn remove_download(
    task_id: String,
    delete_files: bool,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    let engine = current_engine(&state, "删除下载任务")?;
    state
        .remove_task(&task_id, delete_files, &engine)
        .await
        .map_err(|error| map_download_error("删除下载任务", error))
}

/// 更新任务内一组文件的 libtorrent/qBittorrent 优先级。
#[tauri::command]
pub(crate) async fn set_download_file_priority(
    task_id: String,
    file_indexes: Vec<i64>,
    priority: i64,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    let engine = current_engine(&state, "更新下载文件优先级")?;
    state
        .service()
        .set_file_priority(&task_id, &file_indexes, priority, &engine)
        .await
        .map_err(|error| map_download_error("更新下载文件优先级", error))
}

/// 通过磁链或远程 torrent 文件添加手动任务。
#[tauri::command]
pub(crate) async fn add_download_url(
    input: AddDownloadUrlInput,
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    ensure_mobile_storage_available(&app)?;
    let settings = state
        .settings()
        .map_err(|error| runtime_error("添加下载任务", error))?;
    let engine = state
        .default_engine(&settings)
        .map_err(|error| map_download_error("添加下载任务", error))?;
    let prepared = state
        .prepare_source(&input.url, &settings)
        .await
        .map_err(|error| runtime_error("准备下载资源", error))?;
    state
        .service()
        .add(DownloadAddRequest {
            engine,
            source: prepared.source(),
            options: AddTorrentOptions {
                save_path: resolve_save_path(input.save_path.as_deref(), &settings)?,
                paused: input.paused,
                ..AddTorrentOptions::default()
            },
            context: DownloadTaskContext {
                name: input.name,
                ..DownloadTaskContext::default()
            },
        })
        .await
        .map_err(|error| map_download_error("添加下载任务", error))
}

/// 通过系统文件选择器导入本地 torrent，并加入当前默认引擎。
#[tauri::command]
pub(crate) async fn import_torrent_file(
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<Option<Vec<DownloadTask>>, AppCommandError> {
    ensure_mobile_storage_available(&app)?;
    let dialog_app = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .set_title("导入 torrent 文件")
            .add_filter("BitTorrent 元信息", &["torrent"])
            .blocking_pick_file()
    })
    .await
    .map_err(|error| runtime_error("打开 torrent 文件选择器", error))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let (source_path, imported_document) = resolve_selected_document(&app, selected, "torrent")?;
    let settings = state
        .settings()
        .map_err(|error| runtime_error("导入 torrent 文件", error))?;
    let engine = state
        .default_engine(&settings)
        .map_err(|error| map_download_error("导入 torrent 文件", error))?;
    let prepared = state
        .prepare_local_torrent(&source_path)
        .await
        .map_err(|error| runtime_error("准备本地 torrent 文件", error))?;
    let name = source_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let tasks = state
        .service()
        .add(DownloadAddRequest {
            engine,
            source: prepared.source(),
            options: AddTorrentOptions {
                save_path: resolve_save_path(None, &settings)?,
                ..AddTorrentOptions::default()
            },
            context: DownloadTaskContext {
                name,
                ..DownloadTaskContext::default()
            },
        })
        .await
        .map_err(|error| map_download_error("导入 torrent 文件", error))?;
    drop(imported_document);
    log::info!("Tauri 本地 torrent 已导入 task_count={}", tasks.len());
    Ok(Some(tasks))
}

/// 将已选择资源加入下载引擎并持久化番剧和单集关联。
#[tauri::command]
pub(crate) async fn add_release_download(
    input: AddReleaseDownloadInput,
    app: AppHandle,
    state: State<'_, AppDownloadState>,
) -> Result<Vec<DownloadTask>, AppCommandError> {
    ensure_mobile_storage_available(&app)?;
    let settings = state
        .settings()
        .map_err(|error| runtime_error("添加资源下载", error))?;
    let engine = state
        .default_engine(&settings)
        .map_err(|error| map_download_error("添加资源下载", error))?;
    let anime_id = input
        .anime_id
        .clone()
        .or_else(|| input.release.anime_id.clone());
    let tracked_anime = match anime_id.as_deref() {
        Some(anime_id) => state
            .find_my_anime(anime_id)
            .map_err(|error| runtime_error("读取追番下载目录", error))?,
        None => None,
    };
    ensure_release_matches_tracked_anime(
        &input.release,
        tracked_anime.as_ref(),
        input.confirm_unknown_season,
    )?;
    let source_url =
        release_download_source_url(&input.release).map_err(|message| AppCommandError {
            code: "invalid_input".to_owned(),
            message,
        })?;
    let prepared = state
        .prepare_source(&source_url, &settings)
        .await
        .map_err(|error| runtime_error("准备资源下载", error))?;
    let episode_no = input.episode_no.or(input.release.episode_no);
    let fansub_group_id = input
        .fansub_group_id
        .or_else(|| input.release.fansub_group_id.clone());
    let save_path = resolve_release_save_path(
        input.save_path.as_deref(),
        &settings,
        tracked_anime.as_ref(),
    )?;
    let correlation_tag = build_correlation_tag(
        anime_id.as_deref(),
        input.episode_id.as_deref(),
        episode_no,
        &input.release.id,
    );
    let context = release_download_context(
        &input.release,
        anime_id,
        None,
        input.episode_id,
        episode_no,
        fansub_group_id,
    );
    state
        .service()
        .add(DownloadAddRequest {
            engine,
            source: prepared.source(),
            options: AddTorrentOptions {
                save_path,
                correlation_tag: Some(correlation_tag),
                paused: input.paused,
                ..AddTorrentOptions::default()
            },
            context,
        })
        .await
        .map_err(|error| map_download_error("添加资源下载", error))
}

/// 解析资源下载目录，显式目录优先于追番规则和全局默认目录。
fn resolve_release_save_path(
    requested: Option<&str>,
    settings: &Value,
    anime: Option<&MyAnime>,
) -> Result<String, AppCommandError> {
    if requested
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return resolve_save_path(requested, settings);
    }
    let resolved = resolve_anime_download_path(settings, anime);
    log::info!(
        "Tauri 资源下载目录已解析 anime_id={} save_path={}",
        anime.map(|item| item.anime.id.as_str()).unwrap_or("none"),
        resolved
    );
    resolve_save_path(Some(&resolved), settings)
}

/// 读取显式保存路径或回退到当前平台默认目录。
fn resolve_save_path(requested: Option<&str>, settings: &Value) -> Result<String, AppCommandError> {
    requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            settings
                .pointer("/download/defaultDownloadDir")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .map(str::to_owned)
        .ok_or_else(|| AppCommandError {
            code: "invalid_input".to_owned(),
            message: "添加下载任务失败：保存路径不能为空".to_owned(),
        })
}

/// 移动设备空间临界时拒绝新增写入，桌面端保持原有行为。
fn ensure_mobile_storage_available(app: &AppHandle) -> Result<(), AppCommandError> {
    #[cfg(mobile)]
    {
        let status = app
            .ani_mobile()
            .status()
            .map_err(|error| runtime_error("读取移动存储状态", error))?;
        if status.storage == "critical" {
            return Err(AppCommandError {
                code: "insufficient_storage".to_owned(),
                message: "可用存储空间不足 256 MiB，已暂停新增或恢复下载".to_owned(),
            });
        }
    }
    #[cfg(not(mobile))]
    let _ = app;
    Ok(())
}

/// 将文件选择结果转换为 Rust 可读取路径；Android content URI 先复制到私有缓存。
pub(super) fn resolve_selected_document(
    app: &AppHandle,
    selected: FilePath,
    kind: &str,
) -> Result<(PathBuf, Option<TemporaryImportedDocument>), AppCommandError> {
    #[cfg(not(mobile))]
    let _ = (app, kind);
    match selected {
        FilePath::Path(path) => {
            #[cfg(target_os = "ios")]
            {
                let url = url::Url::from_file_path(&path).map_err(|_| {
                    runtime_error("读取 iOS 系统文件选择结果", "路径无法转换为 file URL")
                })?;
                import_mobile_document(app, url.as_str(), kind)
            }
            #[cfg(not(target_os = "ios"))]
            Ok((path, None))
        }
        FilePath::Url(url) if url.scheme() == "file" => {
            #[cfg(target_os = "ios")]
            {
                import_mobile_document(app, url.as_str(), kind)
            }
            #[cfg(not(target_os = "ios"))]
            {
                url.to_file_path().map(|path| (path, None)).map_err(|_| {
                    runtime_error("读取系统文件选择结果", "file URL 无法转换为本地路径")
                })
            }
        }
        #[cfg(target_os = "android")]
        FilePath::Url(url) if url.scheme() == "content" => {
            let path = app
                .ani_mobile()
                .import_document(url.as_str(), kind)
                .map_err(|error| runtime_error("复制 Android 系统文档", error))?;
            Ok((path.clone(), Some(TemporaryImportedDocument(path))))
        }
        FilePath::Url(url) => Err(runtime_error(
            "读取系统文件选择结果",
            format!("不支持的文档协议：{}", url.scheme()),
        )),
    }
}

/// 通过移动原生插件复制安全作用域文档，并在命令完成时清理副本。
#[cfg(mobile)]
fn import_mobile_document(
    app: &AppHandle,
    uri: &str,
    kind: &str,
) -> Result<(PathBuf, Option<TemporaryImportedDocument>), AppCommandError> {
    let path = app
        .ani_mobile()
        .import_document(uri, kind)
        .map_err(|error| runtime_error("复制移动系统文档", error))?;
    Ok((path.clone(), Some(TemporaryImportedDocument(path))))
}

/// 在命令结束时清理从移动系统文档提供器复制的临时文件。
pub(super) struct TemporaryImportedDocument(PathBuf);

impl Drop for TemporaryImportedDocument {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "清理移动导入文档失败 path={} error={error}",
                    self.0.display()
                );
            }
        }
    }
}

/// 为首次占位任务与引擎真实哈希建立稳定弱关联。
fn build_correlation_tag(
    anime_id: Option<&str>,
    episode_id: Option<&str>,
    episode_no: Option<f64>,
    release_id: &str,
) -> String {
    format!(
        "ani:{}:{}:{}",
        anime_id.unwrap_or("manual"),
        episode_id
            .map(str::to_owned)
            .or_else(|| episode_no.map(|value| value.to_string()))
            .unwrap_or_else(|| "unknown".to_owned()),
        release_id
    )
}

/// 阻止季度不兼容资源，并仅允许人工确认一条未知季度资源。
fn ensure_release_matches_tracked_anime(
    release: &Release,
    tracked_anime: Option<&MyAnime>,
    confirm_unknown_season: bool,
) -> Result<(), AppCommandError> {
    let Some(tracked_anime) = tracked_anime else {
        return Ok(());
    };
    let compatibility = classify_anime_release(release, &tracked_anime.anime);
    match compatibility {
        AnimeReleaseCompatibility::Current => return Ok(()),
        AnimeReleaseCompatibility::Other if confirm_unknown_season => {
            log::info!(
                "Tauri 单条未知季度资源已人工确认 anime_id={} release_id={}",
                tracked_anime.anime.id,
                release.id
            );
            return Ok(());
        }
        AnimeReleaseCompatibility::Other | AnimeReleaseCompatibility::Mismatch => {}
    }

    log::warn!(
        "Tauri 资源下载季度门禁拒绝 anime_id={} release_id={} compatibility={compatibility:?}",
        tracked_anime.anime.id,
        release.id
    );
    let reason = match compatibility {
        AnimeReleaseCompatibility::Other => "资源未标明当前季度",
        AnimeReleaseCompatibility::Mismatch => "资源季度与当前追番不一致",
        AnimeReleaseCompatibility::Current => unreachable!(),
    };
    Err(AppCommandError {
        code: "invalid_input".to_owned(),
        message: format!("{reason}，已阻止关联到「{}」", tracked_anime.anime.title),
    })
}

/// 将下载服务错误映射为 Renderer 可处理的稳定命令错误。
fn map_download_error(action: &str, error: DownloadServiceError) -> AppCommandError {
    log::error!("Tauri 下载命令失败 action={action} error={error}");
    let code = match &error {
        DownloadServiceError::InvalidInput { .. } => "invalid_input",
        DownloadServiceError::TaskNotFound(_) => "record_not_found",
        DownloadServiceError::EngineNotRegistered(_) | DownloadServiceError::DuplicateEngine(_) => {
            "download_engine_unavailable"
        }
        DownloadServiceError::Engine { .. } => "download_engine_failed",
        DownloadServiceError::Repository(_) => "storage_operation_failed",
    };
    AppCommandError {
        code: code.to_owned(),
        message: format!("{action}失败：{error}"),
    }
}

/// 读取命令执行时的当前下载引擎，隔离切换前页面残留操作。
fn current_engine(
    state: &AppDownloadState,
    action: &str,
) -> Result<ani_domain::TorrentEngineKind, AppCommandError> {
    let settings = state
        .settings()
        .map_err(|error| runtime_error(action, error))?;
    state
        .default_engine(&settings)
        .map_err(|error| map_download_error(action, error))
}

fn runtime_error(action: &str, error: impl std::fmt::Display) -> AppCommandError {
    log::error!("Tauri 下载运行时失败 action={action} error={error}");
    AppCommandError {
        code: "download_runtime_failed".to_owned(),
        message: format!("{action}失败：{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证资源关联标签在相同输入下稳定。
    #[test]
    fn builds_stable_download_correlation_tag() {
        assert_eq!(
            build_correlation_tag(Some("anime-1"), None, Some(2.0), "release-1"),
            "ani:anime-1:2:release-1"
        );
    }

    /// 验证后端下载命令拒绝续作中未标季数的资源。
    #[test]
    fn rejects_unmarked_release_for_tracked_sequel() {
        let tracked_anime = tracked_anime("地狱模式 第二季", "Hell Mode 2nd Season");
        let release = release_with_season(None);

        let error = ensure_release_matches_tracked_anime(&release, Some(&tracked_anime), false)
            .expect_err("unmarked sequel release must be rejected");

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("资源未标明当前季度"));
    }

    /// 验证后端下载命令接受明确标注当前季的资源。
    #[test]
    fn accepts_marked_release_for_tracked_sequel() {
        let tracked_anime = tracked_anime("地狱模式 第二季", "Hell Mode 2nd Season");
        let release = release_with_season(Some(2));

        assert!(
            ensure_release_matches_tracked_anime(&release, Some(&tracked_anime), false).is_ok()
        );
    }

    /// 验证人工确认只放行当前这一条未知季度资源。
    #[test]
    fn accepts_confirmed_unknown_season_release() {
        let tracked_anime = tracked_anime("地狱模式 第二季", "Hell Mode 2nd Season");
        let release = release_with_season(None);

        assert!(ensure_release_matches_tracked_anime(&release, Some(&tracked_anime), true).is_ok());
    }

    /// 验证明确季度不匹配的资源即使携带确认标志也会被拒绝。
    #[test]
    fn rejects_mismatched_release_even_when_confirmed() {
        let tracked_anime = tracked_anime("地狱模式 第二季", "Hell Mode 2nd Season");
        let release = release_with_season(Some(1));

        let error = ensure_release_matches_tracked_anime(&release, Some(&tracked_anime), true)
            .expect_err("explicit season mismatch must stay blocked");

        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("资源季度与当前追番不一致"));
    }

    /// 创建下载季度门禁测试所需的追番记录。
    fn tracked_anime(title: &str, original_title: &str) -> MyAnime {
        serde_json::from_value(serde_json::json!({
            "id": "my-anime-hell-mode-2",
            "anime": {
                "id": "anime-hell-mode-2",
                "title": title,
                "originalTitle": original_title,
                "aliases": [],
                "premiereYear": 2026,
                "premiereMonth": 7,
                "externalIds": {}
            },
            "status": "watching",
            "autoDownload": true,
            "addedAt": "2026-08-03T00:00:00.000Z",
            "updatedAt": "2026-08-03T00:00:00.000Z"
        }))
        .expect("decode tracked anime")
    }

    /// 创建带可选季数标记的下载资源。
    fn release_with_season(series_season_no: Option<i64>) -> Release {
        let mut release: Release = serde_json::from_value(serde_json::json!({
            "id": "nyaa:https://nyaa.si/view/2081273",
            "title": "[LoliHouse] 地狱模式～喜欢速通游戏的玩家在废设定异世界无双～ / Hell Mode - 08 [WebRip 1080p HEVC-10bit AAC][简繁内封字幕]",
            "episodeNo": 8,
            "contentKind": "episode",
            "sourceId": "nyaa",
            "sourceName": "Nyaa",
            "magnetUrl": "magnet:?xt=urn:btih:365cc4e818e5c9073fe53c72fa2319dad3708b5d",
            "publishedAt": "2026-08-03T00:00:00.000Z"
        }))
        .expect("decode release");
        release.series_season_no = series_season_no;
        release
    }
}
