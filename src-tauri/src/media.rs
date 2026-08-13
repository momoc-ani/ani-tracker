use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(desktop)]
use std::time::Duration;

use ani_contracts::{DesktopMediaToolsStatus, MediaToolStatus};
use ani_domain::{AppSettings, DownloadTask, MediaFile, ReportPlaybackProgressInput, TorrentFile};
use ani_media::{DownloadMediaScanner, MediaScanResult};
#[cfg(desktop)]
use ani_media::{FfprobeMediaProbe, MediaProbe, MediaProbeContext};
#[cfg(desktop)]
use ani_remote::RemoteMediaTools;
use ani_repository::{
    DownloadRepository, MediaRepository, PlaybackRepository, RepositoryError, RepositoryResult,
    SettingsRepository,
};
use ani_storage::Storage;
use serde_json::Value;
use tauri::{AppHandle, Manager};
#[cfg(desktop)]
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;

use crate::sources::AppSourceState;

mod local_import;

/// 将应用 SQLite 单写者适配为媒体 Repository 端口。
struct SharedMediaRepository {
    storage: Arc<Mutex<Storage>>,
}

impl SharedMediaRepository {
    /// 在短临界区内执行媒体 Repository 操作。
    fn with_repository<T>(
        &self,
        operation: impl FnOnce(&dyn MediaRepository) -> RepositoryResult<T>,
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

impl MediaRepository for SharedMediaRepository {
    fn list_media_files(&self) -> RepositoryResult<Vec<MediaFile>> {
        self.with_repository(|repository| repository.list_media_files())
    }

    fn upsert_media_files(&self, media_files: &[MediaFile]) -> RepositoryResult<Vec<MediaFile>> {
        self.with_repository(|repository| repository.upsert_media_files(media_files))
    }

    /// 在共享 SQLite 临界区内批量删除媒体记录。
    fn remove_media_files(&self, media_file_ids: &[String]) -> RepositoryResult<Vec<MediaFile>> {
        self.with_repository(|repository| repository.remove_media_files(media_file_ids))
    }

    /// 在共享 SQLite 临界区内清理无引用的导入番剧。
    fn cleanup_orphaned_imported_anime(
        &self,
        anime_ids: &[String],
    ) -> RepositoryResult<Vec<String>> {
        self.with_repository(|repository| repository.cleanup_orphaned_imported_anime(anime_ids))
    }
}

/// Tauri 生命周期内共享的媒体扫描、工具解析和自动关联状态。
#[derive(Clone)]
pub(crate) struct AppMediaState {
    app: AppHandle,
    storage: Arc<Mutex<Storage>>,
    repository: Arc<SharedMediaRepository>,
    platform_defaults: AppSettings,
    resource_roots: Arc<Vec<PathBuf>>,
    in_flight_task_ids: Arc<AsyncMutex<HashSet<String>>>,
    source_state: AppSourceState,
    local_import: Arc<local_import::LocalMediaRuntime>,
}

impl AppMediaState {
    /// 从应用资源目录、构建输出和源码资源创建媒体服务。
    pub(crate) fn new(
        app: &AppHandle,
        storage: Arc<Mutex<Storage>>,
        platform_defaults: AppSettings,
        source_state: AppSourceState,
    ) -> Self {
        let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut roots = Vec::new();
        if let Ok(resource_directory) = app.path().resource_dir() {
            roots.push(resource_directory.join("ffmpeg"));
        }
        roots.extend([current.join("out/ffmpeg"), current.join("resources/ffmpeg")]);
        roots.dedup();
        Self {
            app: app.clone(),
            repository: Arc::new(SharedMediaRepository {
                storage: Arc::clone(&storage),
            }),
            storage,
            platform_defaults,
            resource_roots: Arc::new(roots),
            in_flight_task_ids: Arc::new(AsyncMutex::new(HashSet::new())),
            source_state,
            local_import: Arc::new(local_import::LocalMediaRuntime::new()),
        }
    }

    /// 探测一个已授权的本地媒体文件，供原地导入流程复用。
    #[cfg(desktop)]
    pub(super) async fn probe_local_file(
        &self,
        file_path: &Path,
        context: &MediaProbeContext,
    ) -> Result<MediaFile, String> {
        let settings = self.settings()?;
        let timeout = setting_u64(&settings, "/media/ffprobeTimeoutSeconds", 20).clamp(1, 60);
        let probe = FfprobeMediaProbe::new(
            self.ffprobe_commands(&settings),
            Duration::from_secs(timeout),
        )
        .map_err(|error| error.to_string())?;
        probe
            .probe(file_path, context)
            .await
            .map_err(|error| error.to_string())
    }

    /// 读取全部已登记媒体文件。
    pub(crate) fn list_media_files(&self) -> Result<Vec<MediaFile>, String> {
        self.repository
            .list_media_files()
            .map_err(|error| error.to_string())
    }

    /// 校验 Renderer 请求的媒体路径仅来自已登记媒体或应用下载目录。
    pub(crate) fn authorize_media_path(&self, requested: &str) -> Result<PathBuf, String> {
        let candidate = crate::path_utils::canonicalize(requested)
            .map_err(|error| format!("媒体文件不存在：{requested}（{error}）"))?;
        if !candidate.is_file() {
            return Err(format!("媒体路径不是普通文件：{}", candidate.display()));
        }
        let registered = self
            .repository
            .list_media_files()
            .map_err(|error| format!("读取媒体登记失败：{error}"))?
            .into_iter()
            .any(|media| path_key(Path::new(&media.file_path)) == path_key(&candidate));
        if registered || self.is_in_download_directory(&candidate)? {
            return Ok(candidate);
        }
        if self.is_in_download_task(&candidate)? {
            log::info!(
                "Tauri 外部媒体路径通过下载任务授权 path={}",
                candidate.display()
            );
            return Ok(candidate);
        }
        log::warn!(
            "Tauri 外部媒体路径授权拒绝 requested={} canonical={}",
            requested,
            candidate.display()
        );
        Err("媒体路径不属于 Ani Tracker 下载目录或媒体登记".to_owned())
    }

    /// 将外部播放器百分比映射到下载任务并回写已看状态。
    pub(crate) fn report_external_playback_progress(
        &self,
        file_path: &Path,
        percent: f64,
    ) -> Result<bool, String> {
        self.report_external_playback_progress_with_title(file_path, None, percent)
    }

    /// 将外部播放器当前媒体标题解析为唯一文件，并回写已看状态。
    pub(crate) fn report_external_playback_progress_with_title(
        &self,
        file_path: &Path,
        media_title: Option<&str>,
        percent: f64,
    ) -> Result<bool, String> {
        if !percent.is_finite() || percent < 90.0 {
            return Ok(false);
        }
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("回写外部播放器进度失败：{error}"))?;
        let repository = storage.repository();
        let media_files = repository
            .list_media_files()
            .map_err(|error| format!("读取媒体登记失败：{error}"))?;
        let downloads = repository
            .list_downloads()
            .map_err(|error| format!("读取下载任务失败：{error}"))?;
        let resolved_path =
            resolve_external_media_path(file_path, media_title, &media_files, &downloads);
        let target_key = path_key(&resolved_path);
        let media = media_files
            .iter()
            .find(|media| path_key(Path::new(&media.file_path)) == target_key);
        let task = media
            .and_then(|media| media.download_task_id.as_deref())
            .and_then(|task_id| downloads.iter().find(|task| task.id == task_id))
            .or_else(|| {
                downloads.iter().find(|task| {
                    task.files.iter().any(|file| {
                        path_key(&Path::new(&task.save_path).join(&file.name)) == target_key
                    })
                })
            })
            .ok_or_else(|| "外部播放器媒体没有关联下载任务".to_owned())?;
        let file_index = task
            .files
            .iter()
            .find(|file| path_key(&Path::new(&task.save_path).join(&file.name)) == target_key)
            .map(|file| file.index);
        let updated = repository
            .report_playback_progress(&ReportPlaybackProgressInput {
                task_id: task.id.clone(),
                file_index,
                percent,
            })
            .map_err(|error| format!("回写外部播放器进度失败：{error}"))?;
        if updated {
            log::info!(
                "Tauri 外部播放器进度已回写 task_id={} file_index={file_index:?} percent={percent:.2}",
                task.id
            );
        }
        Ok(updated)
    }

    /// 手动扫描指定下载任务并原子写入成功结果。
    pub(crate) async fn scan_download_task(
        &self,
        task_id: &str,
    ) -> Result<MediaScanResult, String> {
        let task = self
            .with_download_repository(|repository| {
                repository
                    .list_downloads()?
                    .into_iter()
                    .find(|task| task.id == task_id)
                    .ok_or_else(|| RepositoryError::RecordNotFound {
                        entity: "downloadTask".to_owned(),
                        id: task_id.to_owned(),
                    })
            })
            .map_err(|error| error.to_string())?;
        let scanner = self.scanner()?;
        let result = scanner.scan_task(&task).await;
        if !result.media_files.is_empty() {
            self.repository
                .upsert_media_files(&result.media_files)
                .map_err(|error| error.to_string())?;
        }
        log_scan_result(&result, "manual");
        Ok(result)
    }

    /// 异步触发下载完成媒体关联，不阻塞下载列表刷新。
    pub(crate) fn schedule_completed_scan(&self, tasks: Vec<DownloadTask>) {
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = state.scan_completed_tasks(tasks).await {
                log::warn!("Tauri 下载完成媒体自动关联失败 error={error}");
            }
        });
    }

    /// 扫描尚未完整入库的已完成任务，并隔离单任务错误。
    async fn scan_completed_tasks(&self, tasks: Vec<DownloadTask>) -> Result<(), String> {
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = tasks;
            return Ok(());
        }
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        {
            let scanner = self.scanner()?;
            let existing = self
                .repository
                .list_media_files()
                .map_err(|error| error.to_string())?;
            let existing_paths = existing
                .iter()
                .map(|media| path_key(Path::new(&media.file_path)))
                .collect::<HashSet<_>>();
            for task in tasks.into_iter().filter(DownloadTask::is_completed) {
                let candidates = scanner.candidate_paths(&task);
                if candidates.is_empty()
                    || candidates
                        .iter()
                        .all(|candidate| existing_paths.contains(&path_key(candidate)))
                    || !self.claim_task(&task.id).await
                {
                    continue;
                }
                let result = scanner.scan_task(&task).await;
                if !result.media_files.is_empty() {
                    if let Err(error) = self.repository.upsert_media_files(&result.media_files) {
                        log::warn!(
                            "Tauri 下载完成媒体写入失败 task_id={} error={error}",
                            task.id
                        );
                    }
                }
                log_scan_result(&result, "automatic");
                self.release_task(&task.id).await;
            }
            Ok(())
        }
    }

    /// 检查 FFprobe 与 FFmpeg 的当前解析路径和版本。
    pub(crate) async fn media_tools_status(&self) -> DesktopMediaToolsStatus {
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            return DesktopMediaToolsStatus {
                ffprobe: unavailable_mobile_tool(),
                ffmpeg: unavailable_mobile_tool(),
            };
        }
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        {
            let settings = match self.settings() {
                Ok(settings) => settings,
                Err(error) => {
                    return DesktopMediaToolsStatus {
                        ffprobe: unavailable_tool(error.clone()),
                        ffmpeg: unavailable_tool(error),
                    }
                }
            };
            let ffprobe_commands = self.ffprobe_commands(&settings);
            let ffmpeg_commands = self.ffmpeg_commands(&settings);
            let (ffprobe, ffmpeg) = tokio::join!(
                inspect_media_tool(ffprobe_commands),
                inspect_media_tool(ffmpeg_commands)
            );
            DesktopMediaToolsStatus { ffprobe, ffmpeg }
        }
    }

    /// 返回远程媒体会话使用的 FFprobe/FFmpeg 受控路径。
    #[cfg(desktop)]
    pub(crate) fn remote_media_tools(&self) -> Result<RemoteMediaTools, String> {
        let settings = self.settings()?;
        let timeout = setting_u64(&settings, "/media/ffprobeTimeoutSeconds", 20).clamp(1, 60);
        let ffprobe_paths = self.ffprobe_commands(&settings);
        let ffmpeg_path = self
            .ffmpeg_commands(&settings)
            .into_iter()
            .next()
            .ok_or_else(|| "没有可用的 FFmpeg 命令".to_owned())?;
        let rife_sidecar_root = resolve_model_sidecar_root(&self.app);
        Ok(RemoteMediaTools {
            ffprobe_paths,
            ffmpeg_path,
            timeout: Duration::from_secs(timeout),
            rife_sidecar_root,
            rife_available_vram_bytes: configured_vram_bytes(&settings),
        })
    }

    fn scanner(&self) -> Result<DownloadMediaScanner, String> {
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            return Err("移动端媒体解析将在 libVLC 插件阶段装配".to_owned());
        }
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        {
            let settings = self.settings()?;
            let timeout = setting_u64(&settings, "/media/ffprobeTimeoutSeconds", 20).clamp(1, 60);
            let extensions = settings
                .pointer("/media/videoExtensions")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let probe = FfprobeMediaProbe::new(
                self.ffprobe_commands(&settings),
                Duration::from_secs(timeout),
            )
            .map_err(|error| error.to_string())?;
            Ok(DownloadMediaScanner::new(Arc::new(probe), &extensions))
        }
    }

    fn settings(&self) -> Result<AppSettings, String> {
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("读取媒体设置失败：{error}"))?;
        storage
            .repository()
            .get_settings(&self.platform_defaults)
            .map_err(|error| format!("读取媒体设置失败：{error}"))
    }

    /// 判断媒体是否位于持久化下载根目录或临时下载目录内。
    fn is_in_download_directory(&self, candidate: &Path) -> Result<bool, String> {
        let settings = self.settings()?;
        for pointer in [
            "/download/defaultDownloadDir",
            "/download/temporaryDownloadDir",
        ] {
            let Some(root) = settings.pointer(pointer).and_then(Value::as_str) else {
                continue;
            };
            let Ok(root) = crate::path_utils::canonicalize(root) else {
                continue;
            };
            if candidate.starts_with(root) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 判断媒体是否精确对应某个下载任务的文件清单。
    fn is_in_download_task(&self, candidate: &Path) -> Result<bool, String> {
        let downloads = self
            .with_download_repository(|repository| repository.list_downloads())
            .map_err(|error| format!("读取下载任务失败：{error}"))?;
        Ok(is_download_task_file_path(candidate, &downloads))
    }

    fn with_download_repository<T>(
        &self,
        operation: impl FnOnce(&dyn DownloadRepository) -> RepositoryResult<T>,
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

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    fn ffprobe_commands(&self, settings: &AppSettings) -> Vec<PathBuf> {
        let configured = settings
            .pointer("/media/ffprobePath")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ffprobe");
        let bundled = resolve_bundled_media_binary("ffprobe", &self.resource_roots);
        let default = is_default_tool_command(configured, "ffprobe");
        unique_paths(if default {
            vec![bundled, Some(PathBuf::from(configured))]
        } else {
            vec![Some(PathBuf::from(configured)), bundled]
        })
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    fn ffmpeg_commands(&self, settings: &AppSettings) -> Vec<PathBuf> {
        let bundled = resolve_bundled_media_binary("ffmpeg", &self.resource_roots);
        let configured_probe = settings
            .pointer("/media/ffprobePath")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("ffprobe");
        let sibling = Path::new(configured_probe).parent().map(|directory| {
            directory.join(if cfg!(target_os = "windows") {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            })
        });
        unique_paths(vec![
            bundled,
            sibling,
            Some(PathBuf::from(if cfg!(target_os = "windows") {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            })),
        ])
    }

    async fn claim_task(&self, task_id: &str) -> bool {
        self.in_flight_task_ids
            .lock()
            .await
            .insert(task_id.to_owned())
    }

    async fn release_task(&self, task_id: &str) {
        self.in_flight_task_ids.lock().await.remove(task_id);
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
async fn inspect_media_tool(commands: Vec<PathBuf>) -> MediaToolStatus {
    let mut last_error = None;
    for command_path in commands {
        let mut command = Command::new(&command_path);
        command.arg("-version").kill_on_drop(true);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.as_std_mut().creation_flags(0x0800_0000);
        }
        match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                let text = if output.stdout.is_empty() {
                    String::from_utf8_lossy(&output.stderr)
                } else {
                    String::from_utf8_lossy(&output.stdout)
                };
                return MediaToolStatus {
                    available: true,
                    command: Some(command_path.to_string_lossy().into_owned()),
                    version: text.lines().next().map(str::trim).map(str::to_owned),
                    error: None,
                };
            }
            Ok(Ok(output)) => {
                last_error = Some(format!("退出状态 {}", output.status));
            }
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("版本检查超时".to_owned()),
        }
    }
    unavailable_tool(last_error.unwrap_or_else(|| "没有可用命令".to_owned()))
}

fn unavailable_tool(error: String) -> MediaToolStatus {
    MediaToolStatus {
        available: false,
        command: None,
        version: None,
        error: Some(error),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn unavailable_mobile_tool() -> MediaToolStatus {
    unavailable_tool("移动端不包含 FFmpeg/FFprobe".to_owned())
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn resolve_bundled_media_binary(tool: &str, roots: &[PathBuf]) -> Option<PathBuf> {
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
        format!("{tool}.exe")
    } else {
        tool.to_owned()
    };
    for root in roots {
        for directory in [format!("{platform}-{arch}"), platform.to_owned()] {
            let candidate = root.join(directory).join(&binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn is_default_tool_command(command: &str, tool: &str) -> bool {
    command.eq_ignore_ascii_case(tool)
        || (cfg!(target_os = "windows") && command.eq_ignore_ascii_case(&format!("{tool}.exe")))
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
fn unique_paths(groups: Vec<Option<PathBuf>>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    groups
        .into_iter()
        .flatten()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn path_key(path: &Path) -> String {
    let path =
        crate::path_utils::canonicalize(path).unwrap_or_else(|_| crate::path_utils::simplify(path));
    let value = path.to_string_lossy().into_owned();
    if cfg!(target_os = "windows") {
        value.to_lowercase()
    } else {
        value
    }
}

/// 解析下载任务中的文件路径，并阻止相对路径逃逸保存目录。
fn resolve_download_task_file_path(task: &DownloadTask, file: &TorrentFile) -> Option<PathBuf> {
    let file_path = Path::new(&file.name);
    if file_path.is_absolute() {
        return crate::path_utils::canonicalize(file_path).ok();
    }
    let root = crate::path_utils::canonicalize(&task.save_path).ok()?;
    let resolved = crate::path_utils::canonicalize(root.join(file_path)).ok()?;
    resolved.starts_with(&root).then_some(resolved)
}

/// 判断候选路径是否精确对应任一下载任务文件。
fn is_download_task_file_path(candidate: &Path, tasks: &[DownloadTask]) -> bool {
    let candidate_key = path_key(candidate);
    tasks.iter().any(|task| {
        task.files.iter().any(|file| {
            resolve_download_task_file_path(task, file)
                .is_some_and(|path| path_key(&path) == candidate_key)
        })
    })
}

/// 仅在播放器标题能唯一对应已登记媒体时切换回写目标。
fn resolve_external_media_path(
    initial_path: &Path,
    media_title: Option<&str>,
    media_files: &[MediaFile],
    downloads: &[DownloadTask],
) -> PathBuf {
    let Some(title) = media_title.filter(|value| !value.trim().is_empty()) else {
        return initial_path.to_owned();
    };
    let mut candidates = Vec::new();
    for media in media_files {
        if external_media_title_matches(title, &media.file_name) {
            candidates.push(PathBuf::from(&media.file_path));
        }
    }
    for task in downloads {
        for file in &task.files {
            if external_media_title_matches(title, &file.name) {
                candidates.push(Path::new(&task.save_path).join(&file.name));
            }
        }
    }
    let mut seen = HashSet::new();
    candidates.retain(|path| seen.insert(path_key(path)));
    if candidates.len() == 1 {
        return candidates.remove(0);
    }
    initial_path.to_owned()
}

/// 比较播放器标题与媒体文件名，兼容播放器省略扩展名的情况。
fn external_media_title_matches(title: &str, file_name: &str) -> bool {
    let title = normalize_external_media_label(title);
    let file_name = normalize_external_media_label(file_name);
    if title.is_empty() || file_name.is_empty() {
        return false;
    }
    title == file_name
        || Path::new(&title)
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy() == file_name)
        || Path::new(&file_name)
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy() == title)
}

/// 归一化播放器标题中的目录分隔符与大小写。
fn normalize_external_media_label(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn setting_u64(settings: &AppSettings, pointer: &str, fallback: u64) -> u64 {
    settings
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or(fallback)
}

#[cfg(desktop)]
fn configured_vram_bytes(settings: &AppSettings) -> u64 {
    setting_u64(settings, "/media/rifeAvailableVramBytes", 0)
}

#[cfg(desktop)]
fn resolve_model_sidecar_root(app: &AppHandle) -> Option<PathBuf> {
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
    let mut roots = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        roots.push(resources.join("model-sidecar"));
    }
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    roots.extend([
        current.join("out/model-sidecar"),
        current.join("resources/model-sidecar"),
    ]);
    #[cfg(debug_assertions)]
    if let Some(workspace) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        roots.push(workspace.join("out/model-sidecar"));
    }
    roots
        .into_iter()
        .map(|root| root.join(format!("{platform}-{arch}")))
        .find(|path| path.is_dir())
}

fn log_scan_result(result: &MediaScanResult, mode: &str) {
    log::info!(
        "Tauri 媒体扫描完成 mode={mode} task_id={} media_files={} skipped_files={} errors={}",
        result.task_id,
        result.media_files.len(),
        result.skipped_files.len(),
        result.errors.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证媒体资源路径优先解析当前平台架构目录。
    #[test]
    fn resolves_platform_media_binary() {
        let root = std::env::temp_dir().join(format!("ani-media-resolver-{}", std::process::id()));
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
        let binary = root
            .join(format!("{platform}-{arch}"))
            .join(if cfg!(target_os = "windows") {
                "ffprobe.exe"
            } else {
                "ffprobe"
            });
        std::fs::create_dir_all(binary.parent().expect("binary parent"))
            .expect("create media resolver directory");
        std::fs::write(&binary, b"test").expect("write media resolver binary");

        assert_eq!(
            resolve_bundled_media_binary("ffprobe", std::slice::from_ref(&root)),
            Some(binary)
        );

        std::fs::remove_dir_all(root).expect("remove media resolver directory");
    }

    /// 验证外部播放器标题匹配文件名和省略扩展名的情况。
    #[test]
    fn matches_external_media_titles() {
        assert!(external_media_title_matches(
            "D:\\Anime\\Episode 02.mkv",
            "Episode 02.mkv"
        ));
        assert!(external_media_title_matches("Episode 02", "Episode 02.mkv"));
        assert!(!external_media_title_matches(
            "Episode 03",
            "Episode 02.mkv"
        ));
    }

    /// 验证自定义下载目录中的任务文件可以通过精确路径授权。
    #[test]
    fn authorizes_exact_download_task_file() {
        let directory = test_directory("exact");
        let media_path = directory.join("episode.mkv");
        let other_path = directory.join("unlisted.mkv");
        std::fs::write(&media_path, b"media").expect("write media");
        std::fs::write(&other_path, b"other").expect("write other");
        let task = test_download_task(&directory, "episode.mkv");

        assert!(is_download_task_file_path(
            &media_path,
            std::slice::from_ref(&task)
        ));
        assert!(!is_download_task_file_path(&other_path, &[task]));
        std::fs::remove_dir_all(directory).expect("remove download directory");
    }

    /// 验证任务文件中的父级跳转不会授权保存目录之外的文件。
    #[test]
    fn rejects_download_task_path_escape() {
        let container = test_directory("escape");
        let directory = container.join("download");
        let outside_path = container.join("outside.mkv");
        std::fs::create_dir_all(&directory).expect("create download directory");
        std::fs::write(&outside_path, b"outside").expect("write outside media");
        let task = test_download_task(&directory, "../outside.mkv");

        assert!(!is_download_task_file_path(&outside_path, &[task]));
        std::fs::remove_dir_all(container).expect("remove escape directory");
    }

    /// 创建隔离的标准库临时测试目录。
    fn test_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "ani-media-path-auth-{}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create test directory");
        directory
    }

    /// 构造路径授权测试使用的最小下载任务。
    fn test_download_task(directory: &Path, file_name: &str) -> DownloadTask {
        DownloadTask {
            id: "download-path-test".to_owned(),
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
            correlation_tag: None,
            engine: ani_domain::TorrentEngineKind::Embedded,
            torrent_hash: None,
            name: "路径授权测试".to_owned(),
            status: ani_domain::DownloadStatus::Completed,
            progress: 1.0,
            download_speed: 0,
            upload_speed: 0,
            eta_seconds: None,
            save_path: directory.to_string_lossy().into_owned(),
            files: vec![TorrentFile {
                id: "file-path-test".to_owned(),
                index: 0,
                name: file_name.to_owned(),
                episode_id: None,
                episode_no: None,
                size: 5,
                progress: 1.0,
                priority: 1,
                selected: true,
            }],
            created_at: "2026-08-10T00:00:00Z".to_owned(),
            completed_at: Some("2026-08-10T00:00:00Z".to_owned()),
        }
    }
}
