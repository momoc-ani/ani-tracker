use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ani_domain::{DownloadStatus, DownloadTask, TorrentEngineKind, TorrentFile};
use ani_repository::RepositoryResult;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    AddTorrentOptions, DownloadAddRequest, DownloadEngine, DownloadEngineConfig,
    DownloadEngineError, DownloadEngineRegistry, DownloadEngineStatus, DownloadSource,
    DownloadTaskContext, DownloadTaskService, DownloadTaskStore, TorrentCoreEngine,
    TorrentCoreTransport,
};

#[derive(Default)]
struct MemoryStore {
    tasks: Mutex<Vec<DownloadTask>>,
}

impl MemoryStore {
    /// 创建包含给定任务的内存存储替身。
    fn with_tasks(tasks: Vec<DownloadTask>) -> Self {
        Self {
            tasks: Mutex::new(tasks),
        }
    }
}

impl DownloadTaskStore for MemoryStore {
    fn list_downloads(&self) -> RepositoryResult<Vec<DownloadTask>> {
        Ok(self.tasks.lock().expect("lock tasks").clone())
    }

    fn upsert_download_task(&self, task: &DownloadTask) -> RepositoryResult<Vec<DownloadTask>> {
        let mut tasks = self.tasks.lock().expect("lock tasks");
        tasks.retain(|item| item.id != task.id);
        tasks.insert(0, task.clone());
        Ok(tasks.clone())
    }

    fn remove_download_task(
        &self,
        task_id: &str,
        _delete_files: bool,
    ) -> RepositoryResult<Vec<DownloadTask>> {
        let mut tasks = self.tasks.lock().expect("lock tasks");
        tasks.retain(|task| task.id != task_id);
        Ok(tasks.clone())
    }
}

struct FakeEngine {
    kind: TorrentEngineKind,
    tasks: Mutex<Vec<DownloadTask>>,
    calls: Mutex<Vec<String>>,
    list_error: Option<DownloadEngineError>,
    remove_error: Option<DownloadEngineError>,
    shutdown_error: Option<DownloadEngineError>,
}

impl FakeEngine {
    /// 创建返回固定任务快照的下载引擎替身。
    fn new(kind: TorrentEngineKind, tasks: Vec<DownloadTask>) -> Self {
        Self {
            kind,
            tasks: Mutex::new(tasks),
            calls: Mutex::new(Vec::new()),
            list_error: None,
            remove_error: None,
            shutdown_error: None,
        }
    }

    /// 创建在刷新时返回错误的旧引擎替身。
    fn failing_list(kind: TorrentEngineKind) -> Self {
        Self {
            list_error: Some(DownloadEngineError::Unavailable("测试离线".to_owned())),
            ..Self::new(kind, Vec::new())
        }
    }

    /// 创建在删除时报告任务不存在的下载引擎替身。
    fn missing_remove(kind: TorrentEngineKind) -> Self {
        Self {
            remove_error: Some(DownloadEngineError::TaskNotFound("missing".to_owned())),
            ..Self::new(kind, Vec::new())
        }
    }

    /// 记录一次调用，供路由断言使用。
    fn record(&self, method: &str, task_id: Option<&str>) {
        self.calls.lock().expect("lock calls").push(match task_id {
            Some(task_id) => format!("{method}:{task_id}"),
            None => method.to_owned(),
        });
    }

    /// 返回已记录调用的快照。
    fn recorded(&self) -> Vec<String> {
        self.calls.lock().expect("lock calls").clone()
    }
}

#[async_trait]
impl DownloadEngine for FakeEngine {
    fn kind(&self) -> TorrentEngineKind {
        self.kind.clone()
    }

    async fn status(&self) -> Result<DownloadEngineStatus, DownloadEngineError> {
        Ok(DownloadEngineStatus {
            version: "test".to_owned(),
            task_count: self.tasks.lock().expect("lock tasks").len(),
            listen_port: Some(51413),
            network_policy_blocked: false,
        })
    }

    async fn configure(
        &self,
        _config: &DownloadEngineConfig,
    ) -> Result<DownloadEngineStatus, DownloadEngineError> {
        self.status().await
    }

    async fn add_magnet(
        &self,
        _url: &str,
        _options: &AddTorrentOptions,
    ) -> Result<DownloadTask, DownloadEngineError> {
        self.record("addMagnet", None);
        self.tasks
            .lock()
            .expect("lock tasks")
            .first()
            .cloned()
            .ok_or_else(|| DownloadEngineError::Protocol("缺少添加回执".to_owned()))
    }

    async fn add_torrent_file(
        &self,
        _file_path: &Path,
        _options: &AddTorrentOptions,
    ) -> Result<DownloadTask, DownloadEngineError> {
        self.record("addTorrentFile", None);
        self.tasks
            .lock()
            .expect("lock tasks")
            .first()
            .cloned()
            .ok_or_else(|| DownloadEngineError::Protocol("缺少添加回执".to_owned()))
    }

    async fn list_tasks(&self) -> Result<Vec<DownloadTask>, DownloadEngineError> {
        self.record("listTasks", None);
        if let Some(error) = self.list_error.clone() {
            return Err(error);
        }
        Ok(self.tasks.lock().expect("lock tasks").clone())
    }

    async fn get_task(&self, task_id: &str) -> Result<DownloadTask, DownloadEngineError> {
        self.tasks
            .lock()
            .expect("lock tasks")
            .iter()
            .find(|task| task.id == task_id || task.torrent_hash.as_deref() == Some(task_id))
            .cloned()
            .ok_or_else(|| DownloadEngineError::TaskNotFound(task_id.to_owned()))
    }

    async fn get_files(&self, task_id: &str) -> Result<Vec<TorrentFile>, DownloadEngineError> {
        Ok(self.get_task(task_id).await?.files)
    }

    async fn set_file_priority(
        &self,
        task_id: &str,
        _file_indexes: &[i64],
        _priority: i64,
    ) -> Result<(), DownloadEngineError> {
        self.record("setFilePriority", Some(task_id));
        Ok(())
    }

    async fn pause(&self, task_id: &str) -> Result<(), DownloadEngineError> {
        self.record("pause", Some(task_id));
        Ok(())
    }

    async fn resume(&self, task_id: &str) -> Result<(), DownloadEngineError> {
        self.record("resume", Some(task_id));
        Ok(())
    }

    async fn remove(&self, task_id: &str, delete_files: bool) -> Result<(), DownloadEngineError> {
        self.record(
            if delete_files {
                "removeFiles"
            } else {
                "remove"
            },
            Some(task_id),
        );
        match self.remove_error.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn shutdown(&self) -> Result<(), DownloadEngineError> {
        self.record("shutdown", None);
        match self.shutdown_error.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

struct RecordingCoreTransport {
    responses: HashMap<String, Value>,
    calls: Mutex<Vec<(String, Value)>>,
    shutdown_count: Mutex<usize>,
}

impl RecordingCoreTransport {
    /// 创建返回固定协议结果的内存 transport。
    fn new(responses: HashMap<String, Value>) -> Self {
        Self {
            responses,
            calls: Mutex::new(Vec::new()),
            shutdown_count: Mutex::new(0),
        }
    }
}

#[async_trait]
impl TorrentCoreTransport for RecordingCoreTransport {
    async fn execute(&self, method: &str, params: Value) -> Result<Value, DownloadEngineError> {
        self.calls
            .lock()
            .expect("lock core calls")
            .push((method.to_owned(), params));
        Ok(self.responses.get(method).cloned().unwrap_or(Value::Null))
    }

    async fn shutdown(&self) -> Result<(), DownloadEngineError> {
        *self.shutdown_count.lock().expect("lock shutdown count") += 1;
        Ok(())
    }
}

/// 验证添加任务时业务关联覆盖引擎动态快照并进入持久化端口。
#[tokio::test]
async fn adds_associated_task_through_selected_engine() {
    let raw = task("raw-id", TorrentEngineKind::Embedded, Some("hash-add"));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, vec![raw]));
    let mut registry = DownloadEngineRegistry::new();
    registry
        .register(embedded.clone())
        .expect("register embedded");
    let store = Arc::new(MemoryStore::default());
    let service = DownloadTaskService::new(Arc::new(registry), store.clone());

    let tasks = service
        .add(DownloadAddRequest {
            engine: TorrentEngineKind::Embedded,
            source: DownloadSource::Magnet("magnet:?xt=urn:btih:hash-add".to_owned()),
            options: AddTorrentOptions {
                save_path: "C:/Downloads".to_owned(),
                correlation_tag: Some("auto-1".to_owned()),
                ..AddTorrentOptions::default()
            },
            context: DownloadTaskContext {
                name: Some("测试任务".to_owned()),
                release_id: Some("release-1".to_owned()),
                anime_id: Some("anime-1".to_owned()),
                episode_id: Some("episode-1".to_owned()),
                anime_title: Some("测试番剧".to_owned()),
                episode_no: Some(1.0),
                resolution: Some("1080p".to_owned()),
                subtitle_languages: vec!["chs".to_owned()],
                ..DownloadTaskContext::default()
            },
        })
        .await
        .expect("add associated task");

    assert_eq!(tasks[0].anime_id.as_deref(), Some("anime-1"));
    assert_eq!(tasks[0].id, "embedded:raw-id");
    assert_eq!(tasks[0].files[0].id, "embedded:raw-id:0");
    assert_eq!(tasks[0].name, "测试任务");
    assert_eq!(tasks[0].release_id.as_deref(), Some("release-1"));
    assert_eq!(tasks[0].correlation_tag.as_deref(), Some("auto-1"));
    assert_eq!(embedded.recorded(), vec!["addMagnet"]);
}

/// 验证提交前复查可识别缺少单集标识的历史任务。
#[test]
fn finds_existing_episode_download_by_episode_number() {
    let mut existing = stored_task(
        "existing-episode",
        TorrentEngineKind::Qbittorrent,
        Some("existing-hash"),
    );
    existing.anime_id = Some("anime-1".to_owned());
    existing.episode_id = None;
    existing.episode_no = Some(1.0);
    let store = Arc::new(MemoryStore::with_tasks(vec![existing.clone()]));
    let service = DownloadTaskService::new(Arc::new(DownloadEngineRegistry::new()), store);

    let matched = service
        .find_episode_download("anime-1", "episode-late-1", 1.0)
        .expect("find episode download")
        .expect("existing episode task");
    assert_eq!(matched.id, existing.id);
    assert!(service
        .find_episode_download("anime-1", "episode-late-2", 2.0)
        .expect("find different episode")
        .is_none());
}

/// 验证暂停、恢复、优先级和删除始终路由到任务创建时的引擎。
#[tokio::test]
async fn routes_task_controls_to_original_engine() {
    let existing = stored_task("qb-task", TorrentEngineKind::Qbittorrent, Some("qb-hash"));
    let store = Arc::new(MemoryStore::with_tasks(vec![existing]));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, Vec::new()));
    let qbittorrent = Arc::new(FakeEngine::new(TorrentEngineKind::Qbittorrent, Vec::new()));
    let mut registry = DownloadEngineRegistry::new();
    registry
        .register(embedded.clone())
        .expect("register embedded");
    registry
        .register(qbittorrent.clone())
        .expect("register qbittorrent");
    let service = DownloadTaskService::new(Arc::new(registry), store);

    let paused = service
        .pause("qbittorrent:qb-task", &TorrentEngineKind::Qbittorrent)
        .await
        .expect("pause task");
    assert_eq!(paused[0].status, DownloadStatus::Paused);
    let prioritized = service
        .set_file_priority(
            "qbittorrent:qb-task",
            &[0],
            0,
            &TorrentEngineKind::Qbittorrent,
        )
        .await
        .expect("set priority");
    assert!(!prioritized[0].files[0].selected);
    let resumed = service
        .resume("qbittorrent:qb-task", &TorrentEngineKind::Qbittorrent)
        .await
        .expect("resume task");
    assert_eq!(resumed[0].status, DownloadStatus::Downloading);
    assert!(service
        .remove("qbittorrent:qb-task", true)
        .await
        .expect("remove task")
        .is_empty());

    assert!(embedded.recorded().is_empty());
    assert_eq!(
        qbittorrent.recorded(),
        vec![
            "pause:qb-hash",
            "setFilePriority:qb-hash",
            "resume:qb-hash",
            "removeFiles:qb-hash"
        ]
    );
}

/// 验证删除不受当前默认引擎限制，并按任务自身引擎调用原始 hash。
#[tokio::test]
async fn removes_task_through_its_original_engine() {
    let embedded_task = stored_task(
        "embedded-task",
        TorrentEngineKind::Embedded,
        Some("embedded-hash"),
    );
    let qbittorrent_task = stored_task(
        "qbittorrent-task",
        TorrentEngineKind::Qbittorrent,
        Some("qbittorrent-hash"),
    );
    let store = Arc::new(MemoryStore::with_tasks(vec![
        embedded_task,
        qbittorrent_task,
    ]));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, Vec::new()));
    let qbittorrent = Arc::new(FakeEngine::new(TorrentEngineKind::Qbittorrent, Vec::new()));
    let mut registry = DownloadEngineRegistry::new();
    registry
        .register(embedded.clone())
        .expect("register embedded");
    registry
        .register(qbittorrent.clone())
        .expect("register qbittorrent");
    let service = DownloadTaskService::new(Arc::new(registry), store.clone());

    assert_eq!(
        service
            .task_engine("embedded:embedded-task")
            .expect("read task engine"),
        TorrentEngineKind::Embedded
    );
    assert!(service
        .remove("embedded:embedded-task", false)
        .await
        .expect("remove embedded task while another engine is active")
        .is_empty());

    assert_eq!(embedded.recorded(), vec!["remove:embedded-hash"]);
    assert!(qbittorrent.recorded().is_empty());
    assert_eq!(
        store.list_downloads().expect("list remaining tasks")[0].id,
        "qbittorrent:qbittorrent-task"
    );
}

/// 验证刷新真实哈希任务时合并占位任务业务元数据并移除旧标识。
#[tokio::test]
async fn merges_engine_snapshot_with_pending_task() {
    let mut pending = stored_task("pending-task", TorrentEngineKind::Embedded, None);
    pending.correlation_tag = Some("auto-correlation".to_owned());
    pending.anime_id = Some("anime-1".to_owned());
    pending.episode_id = Some("episode-1".to_owned());
    pending.files[0].episode_id = Some("episode-1".to_owned());
    let mut actual = task(
        "actual-hash",
        TorrentEngineKind::Embedded,
        Some("actual-hash"),
    );
    actual.correlation_tag = Some("auto-correlation".to_owned());
    actual.progress = 0.5;
    actual.files[0].progress = 0.5;
    let store = Arc::new(MemoryStore::with_tasks(vec![pending]));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, vec![actual]));
    let mut registry = DownloadEngineRegistry::new();
    registry.register(embedded).expect("register embedded");
    let service = DownloadTaskService::new(Arc::new(registry), store);

    let result = service
        .refresh(TorrentEngineKind::Embedded)
        .await
        .expect("refresh embedded");
    assert_eq!(result.tasks.len(), 1);
    assert_eq!(result.tasks[0].id, "embedded:actual-hash");
    assert_eq!(result.tasks[0].anime_id.as_deref(), Some("anime-1"));
    assert_eq!(
        result.tasks[0].files[0].episode_id.as_deref(),
        Some("episode-1")
    );
}

/// 验证引擎空保存路径不会覆盖本地已持久化的有效目录。
#[tokio::test]
async fn preserves_stored_save_path_when_engine_snapshot_is_empty() {
    let stored = stored_task(
        "existing-task",
        TorrentEngineKind::Embedded,
        Some("existing-hash"),
    );
    let mut snapshot = task(
        "existing-hash",
        TorrentEngineKind::Embedded,
        Some("existing-hash"),
    );
    snapshot.save_path.clear();
    let store = Arc::new(MemoryStore::with_tasks(vec![stored]));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, vec![snapshot]));
    let mut registry = DownloadEngineRegistry::new();
    registry.register(embedded).expect("register embedded");
    let service = DownloadTaskService::new(Arc::new(registry), store);

    let result = service
        .refresh(TorrentEngineKind::Embedded)
        .await
        .expect("refresh embedded");

    assert_eq!(result.tasks[0].save_path, "C:/Downloads");
}

/// 验证刷新当前引擎不会唤起历史引擎，历史快照仍可切回读取。
#[tokio::test]
async fn isolates_inactive_engine_refresh_failure() {
    let old = stored_task(
        "old-qb-task",
        TorrentEngineKind::Qbittorrent,
        Some("old-qb-hash"),
    );
    let current = task(
        "embedded-task",
        TorrentEngineKind::Embedded,
        Some("embedded-hash"),
    );
    let store = Arc::new(MemoryStore::with_tasks(vec![old]));
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, vec![current]));
    let qbittorrent = Arc::new(FakeEngine::failing_list(TorrentEngineKind::Qbittorrent));
    let mut registry = DownloadEngineRegistry::new();
    registry.register(embedded).expect("register embedded");
    registry
        .register(qbittorrent.clone())
        .expect("register qbittorrent");
    let service = DownloadTaskService::new(Arc::new(registry), store);

    let result = service
        .refresh(TorrentEngineKind::Embedded)
        .await
        .expect("refresh with old engine failure");
    assert_eq!(result.tasks.len(), 1);
    assert_eq!(result.tasks[0].id, "embedded:embedded-task");
    assert!(qbittorrent.recorded().is_empty());
    let old_tasks = service
        .list_for_engine(&TorrentEngineKind::Qbittorrent)
        .expect("list inactive engine snapshot");
    assert_eq!(old_tasks.len(), 1);
    assert_eq!(old_tasks[0].id, "qbittorrent:old-qb-task");
}

/// 验证同一 torrent hash 在两个引擎中独立保存并可分别恢复控制。
#[tokio::test]
async fn isolates_same_hash_across_engine_switches() {
    let embedded = Arc::new(FakeEngine::new(
        TorrentEngineKind::Embedded,
        vec![task(
            "shared-hash",
            TorrentEngineKind::Embedded,
            Some("shared-hash"),
        )],
    ));
    let qbittorrent = Arc::new(FakeEngine::new(
        TorrentEngineKind::Qbittorrent,
        vec![task(
            "shared-hash",
            TorrentEngineKind::Qbittorrent,
            Some("shared-hash"),
        )],
    ));
    let mut registry = DownloadEngineRegistry::new();
    registry
        .register(embedded.clone())
        .expect("register embedded");
    registry
        .register(qbittorrent.clone())
        .expect("register qbittorrent");
    let service = DownloadTaskService::new(Arc::new(registry), Arc::new(MemoryStore::default()));

    let embedded_tasks = service
        .refresh(TorrentEngineKind::Embedded)
        .await
        .expect("restore embedded tasks")
        .tasks;
    let qbittorrent_tasks = service
        .refresh(TorrentEngineKind::Qbittorrent)
        .await
        .expect("restore qbittorrent tasks")
        .tasks;
    assert_eq!(embedded_tasks[0].id, "embedded:shared-hash");
    assert_eq!(qbittorrent_tasks[0].id, "qbittorrent:shared-hash");
    assert_eq!(service.list().expect("list all tasks").len(), 2);

    service
        .pause("embedded:shared-hash", &TorrentEngineKind::Embedded)
        .await
        .expect("pause embedded task");
    service
        .pause("qbittorrent:shared-hash", &TorrentEngineKind::Qbittorrent)
        .await
        .expect("pause qbittorrent task");
    assert!(embedded
        .recorded()
        .contains(&"pause:shared-hash".to_owned()));
    assert!(qbittorrent
        .recorded()
        .contains(&"pause:shared-hash".to_owned()));
    let calls_before = qbittorrent.recorded();
    assert!(service
        .resume("qbittorrent:shared-hash", &TorrentEngineKind::Embedded)
        .await
        .is_err());
    assert_eq!(qbittorrent.recorded(), calls_before);
}

/// 验证引擎任务已消失时仅允许保留文件地移除本地快照。
#[tokio::test]
async fn removes_missing_engine_task_without_deleting_files() {
    let existing = stored_task(
        "missing-task",
        TorrentEngineKind::Embedded,
        Some("missing-hash"),
    );
    let engine = Arc::new(FakeEngine::missing_remove(TorrentEngineKind::Embedded));
    let mut registry = DownloadEngineRegistry::new();
    registry.register(engine).expect("register embedded");
    let store = Arc::new(MemoryStore::with_tasks(vec![existing.clone()]));
    let service = DownloadTaskService::new(Arc::new(registry), store);

    assert!(service
        .remove("embedded:missing-task", false)
        .await
        .expect("remove stale local snapshot")
        .is_empty());

    let engine = Arc::new(FakeEngine::missing_remove(TorrentEngineKind::Embedded));
    let mut registry = DownloadEngineRegistry::new();
    registry.register(engine).expect("register embedded");
    let store = Arc::new(MemoryStore::with_tasks(vec![existing]));
    let service = DownloadTaskService::new(Arc::new(registry), store.clone());
    assert!(service.remove("embedded:missing-task", true).await.is_err());
    assert_eq!(store.list_downloads().expect("task remains").len(), 1);
}

/// 验证未注册引擎和重复注册以稳定错误返回。
#[test]
fn rejects_duplicate_and_missing_engines() {
    let embedded = Arc::new(FakeEngine::new(TorrentEngineKind::Embedded, Vec::new()));
    let mut registry = DownloadEngineRegistry::new();
    registry
        .register(embedded.clone())
        .expect("register embedded");
    assert!(registry.register(embedded).is_err());
    assert!(registry.require(&TorrentEngineKind::Qbittorrent).is_err());
    assert_eq!(registry.kinds(), vec![TorrentEngineKind::Embedded]);
}

/// 验证 property_tree 字符串叶子可映射为领域任务和配置协议。
#[tokio::test]
async fn maps_torrent_core_protocol_and_options() {
    let core_task = json!({
        "id": "core-hash",
        "torrentHash": "core-hash",
        "correlationTag": "core-tag",
        "name": "Core Task",
        "status": "downloading",
        "progress": "0.25",
        "downloadSpeed": "2048",
        "uploadSpeed": "128",
        "etaSeconds": "60",
        "savePath": "C:/Downloads",
        "createdAt": "2026-07-25T00:00:00.000Z",
        "files": [{
            "index": "0",
            "name": "episode.mkv",
            "size": "4096",
            "progress": "0.5",
            "priority": "7",
            "selected": "true"
        }]
    });
    let mut waiting_core_task = core_task.clone();
    waiting_core_task["status"] = json!("waiting_network");
    waiting_core_task["downloadSpeed"] = json!("0");
    waiting_core_task["uploadSpeed"] = json!("0");
    let transport = Arc::new(RecordingCoreTransport::new(HashMap::from([
        (
            "status".to_owned(),
            json!({ "version": "2.1.0", "taskCount": "1", "listenPort": "51413" }),
        ),
        (
            "configure".to_owned(),
            json!({ "version": "2.1.0", "taskCount": "1", "listenPort": "51515" }),
        ),
        ("addMagnet".to_owned(), core_task.clone()),
        (
            "listTasks".to_owned(),
            json!({ "tasks": [waiting_core_task] }),
        ),
        ("remove".to_owned(), json!({})),
    ])));
    let engine = TorrentCoreEngine::new(transport.clone());

    let status = engine.status().await.expect("read core status");
    assert_eq!(status.task_count, 1);
    assert_eq!(status.listen_port, Some(51413));
    let configured = engine
        .configure(&DownloadEngineConfig {
            listen_port: 51515,
            dht_enabled: true,
            upnp_enabled: false,
            max_active_downloads: 2,
            max_download_speed: 1024,
            max_upload_speed: 256,
            allow_metered_downloads: false,
            seeding_limits: Default::default(),
        })
        .await
        .expect("configure core");
    assert_eq!(configured.listen_port, Some(51515));
    let added = engine
        .add_magnet(
            "magnet:?xt=urn:btih:core-hash",
            &AddTorrentOptions {
                save_path: "C:/Downloads".to_owned(),
                selected_file_indexes: Some(vec![0]),
                correlation_tag: Some("core-tag".to_owned()),
                paused: true,
                ..AddTorrentOptions::default()
            },
        )
        .await
        .expect("add core magnet");
    assert_eq!(added.progress, 0.25);
    assert_eq!(added.files[0].priority, 7);
    assert_eq!(added.files[0].id, "core-hash:0");
    let listed = engine.list_tasks().await.expect("list core tasks");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, DownloadStatus::WaitingNetwork);
    assert_eq!(listed[0].download_speed, 0);
    assert_eq!(listed[0].upload_speed, 0);
    engine
        .remove("core-hash", true)
        .await
        .expect("remove core task");
    engine.shutdown().await.expect("shutdown core");

    let calls = transport.calls.lock().expect("lock core calls");
    let configure = calls
        .iter()
        .find(|(method, _)| method == "configure")
        .expect("configure call");
    assert_eq!(configure.1["maxActiveDownloads"], 2);
    assert_eq!(configure.1["allowMeteredDownloads"], false);
    let add = calls
        .iter()
        .find(|(method, _)| method == "addMagnet")
        .expect("add call");
    assert_eq!(add.1["selectedFileIndexes"], json!([0]));
    assert_eq!(add.1["paused"], true);
    assert_eq!(add.1["savePath"], "C:/Downloads");
    let remove = calls
        .iter()
        .find(|(method, _)| method == "remove")
        .expect("remove call");
    assert_eq!(remove.1["taskId"], "core-hash");
    assert_eq!(remove.1["deleteFiles"], true);
    assert_eq!(*transport.shutdown_count.lock().expect("shutdown count"), 1);
}

/// 验证 property_tree 空节点不会让空任务和文件列表解析失败。
#[tokio::test]
async fn accepts_property_tree_empty_arrays() {
    let transport = Arc::new(RecordingCoreTransport::new(HashMap::from([
        ("listTasks".to_owned(), json!({ "tasks": "" })),
        ("getFiles".to_owned(), json!({ "files": "" })),
    ])));
    let engine = TorrentCoreEngine::new(transport);

    assert!(engine
        .list_tasks()
        .await
        .expect("list empty core tasks")
        .is_empty());
    assert!(engine
        .get_files("core-hash")
        .await
        .expect("list empty core files")
        .is_empty());
}

/// 验证缺失桌面 sidecar 时返回可诊断的引擎不可用错误。
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn reports_missing_torrent_core_binary() {
    use std::path::PathBuf;

    use crate::{ProcessTorrentCoreTransport, TorrentCoreProcessOptions};

    let transport = Arc::new(ProcessTorrentCoreTransport::new(
        TorrentCoreProcessOptions::new(
            PathBuf::from("missing-torrent-core-binary"),
            PathBuf::from("missing-torrent-core-data"),
        ),
    ));
    let error = TorrentCoreEngine::new(transport)
        .status()
        .await
        .expect_err("missing sidecar must fail");
    assert!(matches!(error, DownloadEngineError::Unavailable(_)));
}

/// 创建统一任务服务测试使用的任务快照。
fn task(id: &str, engine: TorrentEngineKind, hash: Option<&str>) -> DownloadTask {
    DownloadTask {
        id: id.to_owned(),
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
        engine,
        torrent_hash: hash.map(str::to_owned),
        name: id.to_owned(),
        status: DownloadStatus::Downloading,
        progress: 0.1,
        download_speed: 100,
        upload_speed: 10,
        eta_seconds: Some(60),
        save_path: "C:/Downloads".to_owned(),
        files: vec![TorrentFile {
            id: format!("{id}:0"),
            index: 0,
            name: "episode.mkv".to_owned(),
            episode_id: None,
            episode_no: None,
            size: 1024,
            progress: 0.1,
            priority: 1,
            selected: true,
        }],
        created_at: "2026-07-25T00:00:00.000Z".to_owned(),
        completed_at: None,
    }
}

/// 创建已经过应用引擎命名空间处理的持久化任务。
fn stored_task(id: &str, engine: TorrentEngineKind, hash: Option<&str>) -> DownloadTask {
    let mut task = task(id, engine, hash);
    task.id = task.engine.scope_task_id(&task.id);
    for file in &mut task.files {
        file.id = format!("{}:{}", task.id, file.index);
    }
    task
}
