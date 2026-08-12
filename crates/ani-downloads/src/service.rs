use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ani_domain::{DownloadStatus, DownloadTask, TorrentEngineKind};
use ani_repository::{DownloadRepository, RepositoryResult};

use crate::{AddTorrentOptions, DownloadEngineRegistry, DownloadServiceError, DownloadSource};

/// 供统一下载服务使用的线程安全持久化端口。
pub trait DownloadTaskStore: Send + Sync {
    /// 读取全部持久化任务。
    fn list_downloads(&self) -> RepositoryResult<Vec<DownloadTask>>;

    /// 幂等保存任务和文件快照。
    fn upsert_download_task(&self, task: &DownloadTask) -> RepositoryResult<Vec<DownloadTask>>;

    /// 删除业务任务记录，并按真实文件删除结果清理关联索引。
    fn remove_download_task(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> RepositoryResult<Vec<DownloadTask>>;
}

impl<T> DownloadTaskStore for T
where
    T: DownloadRepository + Send + Sync,
{
    fn list_downloads(&self) -> RepositoryResult<Vec<DownloadTask>> {
        DownloadRepository::list_downloads(self)
    }

    fn upsert_download_task(&self, task: &DownloadTask) -> RepositoryResult<Vec<DownloadTask>> {
        DownloadRepository::upsert_download_task(self, task)
    }

    fn remove_download_task(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> RepositoryResult<Vec<DownloadTask>> {
        DownloadRepository::remove_download_task(self, task_id, delete_files)
    }
}

/// 添加任务时由业务层附加的番剧、资源和媒体元数据。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DownloadTaskContext {
    pub name: Option<String>,
    pub release_id: Option<String>,
    pub anime_id: Option<String>,
    pub episode_id: Option<String>,
    pub anime_title: Option<String>,
    pub episode_no: Option<f64>,
    pub fansub_group_id: Option<String>,
    pub fansub_name: Option<String>,
    pub resolution: Option<String>,
    pub declared_video_codec: Option<String>,
    pub normalized_video_codec: Option<String>,
    pub bit_depth: Option<i64>,
    pub subtitle_languages: Vec<String>,
    pub subtitle: Option<String>,
}

/// 一次添加任务的完整引擎、来源、选项和业务上下文。
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadAddRequest {
    pub engine: TorrentEngineKind,
    pub source: DownloadSource,
    pub options: AddTorrentOptions,
    pub context: DownloadTaskContext,
}

/// 当前引擎刷新并恢复后的任务结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadRefreshResult {
    pub tasks: Vec<DownloadTask>,
}

/// 统一协调引擎路由、状态合并和 Repository 写入的任务服务。
pub struct DownloadTaskService {
    registry: Arc<DownloadEngineRegistry>,
    store: Arc<dyn DownloadTaskStore>,
}

impl DownloadTaskService {
    /// 创建不依赖 SQLite 或 Tauri 宿主的下载任务服务。
    pub fn new(registry: Arc<DownloadEngineRegistry>, store: Arc<dyn DownloadTaskStore>) -> Self {
        Self { registry, store }
    }

    /// 读取持久化下载任务，供 Tauri 首屏和下载页使用。
    pub fn list(&self) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        Ok(self.store.list_downloads()?)
    }

    /// 读取指定下载引擎的持久化任务快照。
    pub fn list_for_engine(
        &self,
        engine: &TorrentEngineKind,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        Ok(filter_tasks_by_engine(self.store.list_downloads()?, engine))
    }

    /// 读取任务创建时所属的下载引擎，供宿主准备对应引擎生命周期。
    pub fn task_engine(&self, task_id: &str) -> Result<TorrentEngineKind, DownloadServiceError> {
        Ok(self.require_task(task_id)?.engine)
    }

    /// 按单集标识或番剧集数读取当前持久化任务，供提交前幂等复查。
    pub fn find_episode_download(
        &self,
        anime_id: &str,
        episode_id: &str,
        episode_no: f64,
    ) -> Result<Option<DownloadTask>, DownloadServiceError> {
        Ok(self.store.list_downloads()?.into_iter().find(|task| {
            task.anime_id.as_deref() == Some(anime_id)
                && (task.episode_id.as_deref() == Some(episode_id)
                    || task
                        .episode_no
                        .is_some_and(|number| (number - episode_no).abs() < 1e-9))
        }))
    }

    /// 通过指定引擎添加任务，附加业务元数据后原子持久化。
    pub async fn add(
        &self,
        request: DownloadAddRequest,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        validate_add_request(&request)?;
        let engine = self.registry.require(&request.engine)?;
        let task = match &request.source {
            DownloadSource::Magnet(url) => {
                engine
                    .add_magnet(url, &request.options)
                    .await
                    .map_err(|error| {
                        DownloadServiceError::engine(request.engine.clone(), "addMagnet", error)
                    })?
            }
            DownloadSource::TorrentFile(path) => engine
                .add_torrent_file(path, &request.options)
                .await
                .map_err(|error| {
                    DownloadServiceError::engine(request.engine.clone(), "addTorrentFile", error)
                })?,
        };
        let task = scope_engine_task(apply_add_context(task, &request));
        let tasks = filter_tasks_by_engine(self.store.upsert_download_task(&task)?, &task.engine);
        log::info!(
            "下载任务已加入统一服务：task_id={}, engine={:?}",
            task.id,
            task.engine
        );
        Ok(tasks)
    }

    /// 仅刷新当前引擎，其他引擎任务保留 SQLite 快照等待切回恢复。
    pub async fn refresh(
        &self,
        default_engine: TorrentEngineKind,
    ) -> Result<DownloadRefreshResult, DownloadServiceError> {
        let existing = self.store.list_downloads()?;
        let engine = self.registry.require(&default_engine)?;
        let engine_tasks = engine.list_tasks().await.map_err(|error| {
            DownloadServiceError::engine(default_engine.clone(), "listTasks", error)
        })?;
        self.merge_engine_snapshot(&existing, default_engine.clone(), engine_tasks)?;
        let tasks = filter_tasks_by_engine(self.store.list_downloads()?, &default_engine);
        log::info!(
            "当前下载引擎任务已恢复：engine={:?}, task_count={}",
            default_engine,
            tasks.len()
        );
        Ok(DownloadRefreshResult { tasks })
    }

    /// 暂停任务原属引擎，并立即持久化暂停状态。
    pub async fn pause(
        &self,
        task_id: &str,
        active_engine: &TorrentEngineKind,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        let mut task = self.require_active_task(task_id, active_engine)?;
        let engine = self.registry.require(&task.engine)?;
        engine
            .pause(engine_task_id(&task))
            .await
            .map_err(|error| DownloadServiceError::engine(task.engine.clone(), "pause", error))?;
        task.status = DownloadStatus::Paused;
        task.download_speed = 0;
        task.upload_speed = 0;
        Ok(filter_tasks_by_engine(
            self.store.upsert_download_task(&task)?,
            &task.engine,
        ))
    }

    /// 恢复任务原属引擎，并立即持久化活动状态。
    pub async fn resume(
        &self,
        task_id: &str,
        active_engine: &TorrentEngineKind,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        let mut task = self.require_active_task(task_id, active_engine)?;
        let engine = self.registry.require(&task.engine)?;
        engine
            .resume(engine_task_id(&task))
            .await
            .map_err(|error| DownloadServiceError::engine(task.engine.clone(), "resume", error))?;
        task.status = if task.is_completed() {
            DownloadStatus::Seeding
        } else {
            DownloadStatus::Downloading
        };
        Ok(filter_tasks_by_engine(
            self.store.upsert_download_task(&task)?,
            &task.engine,
        ))
    }

    /// 先从任务原属引擎移除，再删除本地业务记录。
    pub async fn remove(
        &self,
        task_id: &str,
        delete_files: bool,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        let task = self.require_task(task_id)?;
        let engine = self.registry.require(&task.engine)?;
        match engine.remove(engine_task_id(&task), delete_files).await {
            Ok(()) => {}
            Err(crate::DownloadEngineError::TaskNotFound(_)) if !delete_files => {
                log::warn!(
                    "下载引擎任务已不存在，继续清理本地快照：task_id={}, engine={:?}",
                    task.id,
                    task.engine
                );
            }
            Err(error) => {
                return Err(DownloadServiceError::engine(
                    task.engine.clone(),
                    "remove",
                    error,
                ));
            }
        }
        let tasks = filter_tasks_by_engine(
            self.store.remove_download_task(&task.id, delete_files)?,
            &task.engine,
        );
        log::info!(
            "下载任务已从原属引擎移除：task_id={}, engine={:?}, delete_files={delete_files}",
            task.id,
            task.engine
        );
        Ok(tasks)
    }

    /// 更新文件优先级并同步本地选择状态。
    pub async fn set_file_priority(
        &self,
        task_id: &str,
        file_indexes: &[i64],
        priority: i64,
        active_engine: &TorrentEngineKind,
    ) -> Result<Vec<DownloadTask>, DownloadServiceError> {
        validate_file_priority(file_indexes, priority)?;
        let mut task = self.require_active_task(task_id, active_engine)?;
        let known_indexes = task
            .files
            .iter()
            .map(|file| file.index)
            .collect::<HashSet<_>>();
        if let Some(index) = file_indexes
            .iter()
            .find(|index| !known_indexes.contains(index))
        {
            return Err(DownloadServiceError::invalid(
                "fileIndexes",
                format!("任务中不存在文件索引 {index}"),
            ));
        }
        let engine = self.registry.require(&task.engine)?;
        engine
            .set_file_priority(engine_task_id(&task), file_indexes, priority)
            .await
            .map_err(|error| {
                DownloadServiceError::engine(task.engine.clone(), "setFilePriority", error)
            })?;
        for file in &mut task.files {
            if file_indexes.contains(&file.index) {
                file.priority = priority;
                file.selected = priority > 0;
            }
        }
        Ok(filter_tasks_by_engine(
            self.store.upsert_download_task(&task)?,
            &task.engine,
        ))
    }

    /// 通过应用内唯一标识读取任务，禁止跨引擎使用原始 hash 匹配。
    fn require_task(&self, task_id: &str) -> Result<DownloadTask, DownloadServiceError> {
        self.store
            .list_downloads()?
            .into_iter()
            .find(|task| task.id == task_id)
            .ok_or_else(|| DownloadServiceError::TaskNotFound(task_id.to_owned()))
    }

    /// 校验任务属于当前引擎，阻止旧页面重新唤起已切走的引擎。
    fn require_active_task(
        &self,
        task_id: &str,
        active_engine: &TorrentEngineKind,
    ) -> Result<DownloadTask, DownloadServiceError> {
        let task = self.require_task(task_id)?;
        if &task.engine != active_engine {
            return Err(DownloadServiceError::invalid(
                "taskId",
                format!("任务属于 {}，请切回该下载引擎后重试", task.engine.as_key()),
            ));
        }
        Ok(task)
    }

    /// 合并引擎动态字段和本地业务关联，替换首次占位任务。
    fn merge_engine_snapshot(
        &self,
        existing: &[DownloadTask],
        kind: TorrentEngineKind,
        engine_tasks: Vec<DownloadTask>,
    ) -> Result<(), DownloadServiceError> {
        let mut current = existing.to_vec();
        for task in engine_tasks {
            let mut task = task;
            task.engine = kind.clone();
            task = scope_engine_task(task);
            let matched = find_existing_task(&current, &task);
            if let Some(stored) = matched.as_ref() {
                task = merge_download_task(stored, task);
            }
            self.store.upsert_download_task(&task)?;
            if let Some(stored) = matched.filter(|stored| stored.id != task.id) {
                self.store.remove_download_task(&stored.id, false)?;
                current.retain(|item| item.id != stored.id);
            }
            current.retain(|item| item.id != task.id);
            current.push(task);
        }
        Ok(())
    }
}

fn validate_add_request(request: &DownloadAddRequest) -> Result<(), DownloadServiceError> {
    if request.options.save_path.trim().is_empty() {
        return Err(DownloadServiceError::invalid(
            "savePath",
            "下载保存路径不能为空",
        ));
    }
    match &request.source {
        DownloadSource::Magnet(url) if !url.trim().to_ascii_lowercase().starts_with("magnet:?") => {
            return Err(DownloadServiceError::invalid("url", "仅接受 magnet:? 磁链"));
        }
        DownloadSource::TorrentFile(path) if path.as_os_str().is_empty() => {
            return Err(DownloadServiceError::invalid(
                "filePath",
                "torrent 文件路径不能为空",
            ));
        }
        _ => {}
    }
    if let Some(indexes) = request.options.selected_file_indexes.as_deref() {
        if indexes.iter().any(|index| *index < 0) {
            return Err(DownloadServiceError::invalid(
                "selectedFileIndexes",
                "文件索引不能为负数",
            ));
        }
    }
    Ok(())
}

fn validate_file_priority(file_indexes: &[i64], priority: i64) -> Result<(), DownloadServiceError> {
    if file_indexes.is_empty() {
        return Err(DownloadServiceError::invalid(
            "fileIndexes",
            "至少选择一个文件",
        ));
    }
    if file_indexes.iter().any(|index| *index < 0) {
        return Err(DownloadServiceError::invalid(
            "fileIndexes",
            "文件索引不能为负数",
        ));
    }
    if !(0..=7).contains(&priority) {
        return Err(DownloadServiceError::invalid(
            "priority",
            "文件优先级必须在 0 到 7 之间",
        ));
    }
    Ok(())
}

fn apply_add_context(mut task: DownloadTask, request: &DownloadAddRequest) -> DownloadTask {
    let context = &request.context;
    task.engine = request.engine.clone();
    if let Some(name) = context
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        task.name = name.to_owned();
    }
    task.release_id = context.release_id.clone();
    task.anime_id = context.anime_id.clone();
    task.episode_id = context.episode_id.clone();
    task.anime_title = context.anime_title.clone();
    task.episode_no = context.episode_no;
    task.fansub_group_id = context.fansub_group_id.clone();
    task.fansub_name = context.fansub_name.clone();
    task.resolution = context.resolution.clone();
    task.declared_video_codec = context.declared_video_codec.clone();
    task.normalized_video_codec = context.normalized_video_codec.clone();
    task.bit_depth = context.bit_depth;
    task.subtitle_languages = context.subtitle_languages.clone();
    task.subtitle = context.subtitle.clone();
    task.correlation_tag = request
        .options
        .correlation_tag
        .clone()
        .or(task.correlation_tag);
    if task.save_path.trim().is_empty() {
        task.save_path = request.options.save_path.clone();
    }
    task
}

fn find_existing_task(existing: &[DownloadTask], candidate: &DownloadTask) -> Option<DownloadTask> {
    let same_engine = existing
        .iter()
        .filter(|task| task.engine == candidate.engine)
        .collect::<Vec<_>>();
    if let Some(task) = same_engine.iter().find(|task| task.id == candidate.id) {
        return Some((**task).clone());
    }
    if let Some(hash) = candidate.torrent_hash.as_deref() {
        if let Some(task) = same_engine
            .iter()
            .find(|task| task.torrent_hash.as_deref() == Some(hash))
        {
            return Some((**task).clone());
        }
    }
    let tag = candidate.correlation_tag.as_deref()?;
    let matches = same_engine
        .into_iter()
        .filter(|task| {
            task.correlation_tag.as_deref() == Some(tag)
                && (task.torrent_hash.is_none() || candidate.torrent_hash.is_none())
        })
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0].clone())
}

fn merge_download_task(stored: &DownloadTask, mut engine: DownloadTask) -> DownloadTask {
    let links = stored
        .files
        .iter()
        .map(|file| (file.index, (file.episode_id.clone(), file.episode_no)))
        .collect::<HashMap<_, _>>();
    for file in &mut engine.files {
        if let Some((episode_id, episode_no)) = links.get(&file.index) {
            file.episode_id = episode_id.clone();
            file.episode_no = *episode_no;
        }
    }
    engine.release_id = stored.release_id.clone();
    engine.anime_id = stored.anime_id.clone();
    engine.episode_id = stored.episode_id.clone();
    engine.anime_title = stored.anime_title.clone();
    engine.episode_no = stored.episode_no;
    engine.fansub_group_id = stored.fansub_group_id.clone();
    engine.fansub_name = stored.fansub_name.clone();
    engine.resolution = stored.resolution.clone();
    engine.declared_video_codec = stored.declared_video_codec.clone();
    engine.normalized_video_codec = stored.normalized_video_codec.clone();
    engine.bit_depth = stored.bit_depth;
    engine.subtitle_languages = stored.subtitle_languages.clone();
    engine.subtitle = stored.subtitle.clone();
    engine.correlation_tag = engine
        .correlation_tag
        .or_else(|| stored.correlation_tag.clone());
    if engine.save_path.trim().is_empty() {
        log::warn!(
            "下载引擎返回空保存路径，保留持久化路径：task_id={}, engine={:?}",
            stored.id,
            stored.engine
        );
        engine.save_path = stored.save_path.clone();
    }
    engine.created_at = stored.created_at.clone();
    engine.completed_at = engine.completed_at.or_else(|| stored.completed_at.clone());
    engine
}

fn engine_task_id(task: &DownloadTask) -> &str {
    task.torrent_hash
        .as_deref()
        .unwrap_or_else(|| task.engine.unscoped_task_id(&task.id))
}

/// 将引擎快照转换为不会与其他引擎冲突的应用任务身份。
fn scope_engine_task(mut task: DownloadTask) -> DownloadTask {
    task.id = task.engine.scope_task_id(&task.id);
    for file in &mut task.files {
        file.id = format!("{}:{}", task.id, file.index);
    }
    task
}

/// 过滤指定引擎任务并保留 Repository 的稳定排序。
fn filter_tasks_by_engine(
    tasks: Vec<DownloadTask>,
    engine: &TorrentEngineKind,
) -> Vec<DownloadTask> {
    tasks
        .into_iter()
        .filter(|task| &task.engine == engine)
        .collect()
}
