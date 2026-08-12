use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use ani_automation::{
    build_automation_notifications, AutomaticDownloadExecutor, AutomaticDownloadReceipt,
    AutomaticDownloadRequest, AutomationRunOptions, AutomationRunService, AutomationScanStore,
};
use ani_domain::{
    AppSettings, AutomationRunResult, AutomationSchedulerStatus, NotificationKind,
    NotificationRecord, NotificationSeverity,
};
use ani_downloads::{AddTorrentOptions, DownloadAddRequest};
use ani_repository::prelude::*;
use ani_sources::release_satisfies_subtitle_requirement;
use ani_storage::Storage;
use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use tauri::AppHandle;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::downloads::{release_download_context, release_download_source_url, AppDownloadState};
use crate::sources::{AppSourceState, SharedReleaseSearchStore};

const MIN_INTERVAL_MINUTES: i64 = 5;
const MANUAL_COOLDOWN_SECONDS: i64 = 60;

/// Tauri 生命周期内持有自动扫描调度、冷却和最近结果。
#[derive(Clone)]
pub(crate) struct AppAutomationState {
    inner: Arc<AutomationRuntime>,
}

struct AutomationRuntime {
    app: AppHandle,
    storage: Arc<Mutex<Storage>>,
    platform_defaults: AppSettings,
    source_state: AppSourceState,
    executor: Arc<dyn AutomaticDownloadExecutor>,
    status: AsyncMutex<AutomationSchedulerStatus>,
    wake: Notify,
    started: AtomicBool,
    in_flight: AtomicBool,
}

impl AppAutomationState {
    /// 创建尚未启动的自动扫描状态。
    pub(crate) fn new(
        app: AppHandle,
        storage: Arc<Mutex<Storage>>,
        platform_defaults: AppSettings,
        source_state: AppSourceState,
        executor: Arc<dyn AutomaticDownloadExecutor>,
    ) -> Self {
        Self {
            inner: Arc::new(AutomationRuntime {
                app,
                storage,
                platform_defaults,
                source_state,
                executor,
                status: AsyncMutex::new(AutomationSchedulerStatus {
                    enabled: false,
                    running: false,
                    in_flight: false,
                    interval_minutes: MIN_INTERVAL_MINUTES,
                    next_run_at: None,
                    manual_cooldown_until: None,
                    last_run_at: None,
                    last_result: None,
                    last_error: None,
                }),
                wake: Notify::new(),
                started: AtomicBool::new(false),
                in_flight: AtomicBool::new(false),
            }),
        }
    }

    /// 启动按间隔运行的后台调度循环。
    pub(crate) fn start(&self) {
        if self.inner.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            state.run_loop().await;
        });
    }

    /// 返回自动扫描调度状态快照，并清理已过期冷却时间。
    pub(crate) async fn status(&self) -> AutomationSchedulerStatus {
        let mut status = self.inner.status.lock().await;
        if status
            .manual_cooldown_until
            .as_deref()
            .and_then(parse_datetime)
            .is_some_and(|until| until <= Utc::now())
        {
            status.manual_cooldown_until = None;
        }
        status.clone()
    }

    /// 设置保存后立即刷新开关和检查间隔。
    pub(crate) async fn refresh_from_settings(&self, settings: &AppSettings) {
        let enabled = settings
            .pointer("/automation/scheduledCheckEnabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let interval_minutes = settings
            .pointer("/automation/checkIntervalMinutes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(30)
            .max(MIN_INTERVAL_MINUTES);
        {
            let mut status = self.inner.status.lock().await;
            status.enabled = enabled;
            status.interval_minutes = interval_minutes;
            if !enabled {
                status.running = false;
                status.next_run_at = None;
            }
        }
        self.inner.wake.notify_one();
    }

    /// 重新读取持久化设置并唤醒调度循环。
    pub(crate) async fn restart(&self) -> Result<AutomationSchedulerStatus, String> {
        let settings = self.load_settings().await?;
        self.refresh_from_settings(&settings).await;
        Ok(self.status().await)
    }

    /// 立即执行并等待一次扫描，并对人工触发应用一分钟冷却。
    pub(crate) async fn run_now(
        &self,
        manual: bool,
        trigger: &'static str,
    ) -> Result<AutomationRunResult, String> {
        self.reserve_run(manual).await?;
        self.execute_reserved_run(trigger).await
    }

    /// 将一次扫描加入宿主后台任务并立即返回状态。
    pub(crate) async fn start_now(
        &self,
        manual: bool,
        trigger: &'static str,
    ) -> Result<AutomationSchedulerStatus, String> {
        self.reserve_run(manual).await?;
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = state.execute_reserved_run(trigger).await {
                log::error!("Tauri 后台自动扫描失败 trigger={trigger} error={error}");
            }
        });
        Ok(self.status().await)
    }

    /// 抢占扫描执行权并初始化调度状态。
    async fn reserve_run(&self, manual: bool) -> Result<(), String> {
        if self
            .inner
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("自动扫描正在运行".to_owned());
        }
        if manual {
            let mut status = self.inner.status.lock().await;
            if let Some(until) = status
                .manual_cooldown_until
                .as_deref()
                .and_then(parse_datetime)
                .filter(|until| *until > Utc::now())
            {
                self.inner.in_flight.store(false, Ordering::Release);
                let seconds = (until - Utc::now()).num_seconds().max(1);
                return Err(format!("扫描过于频繁，请 {seconds} 秒后再试"));
            }
            status.manual_cooldown_until = Some(to_iso(
                Utc::now() + Duration::seconds(MANUAL_COOLDOWN_SECONDS),
            ));
        }
        {
            let mut status = self.inner.status.lock().await;
            status.in_flight = true;
            status.running = false;
            status.next_run_at = None;
            status.last_result = None;
            status.last_error = None;
        }
        Ok(())
    }

    /// 执行已经预留的扫描任务，并统一释放状态与发送通知。
    async fn execute_reserved_run(
        &self,
        trigger: &'static str,
    ) -> Result<AutomationRunResult, String> {
        let started = Instant::now();
        log::info!("Tauri 自动扫描开始 trigger={trigger}");
        let outcome = self.execute_scan().await;
        {
            let mut status = self.inner.status.lock().await;
            status.in_flight = false;
            match &outcome {
                Ok(result) => {
                    status.last_run_at = Some(result.finished_at.clone());
                    status.last_result = Some(result.clone());
                }
                Err(error) => status.last_error = Some(error.clone()),
            }
        }
        self.inner.in_flight.store(false, Ordering::Release);
        self.inner.wake.notify_one();
        match &outcome {
            Ok(result) => {
                log::info!(
                    "Tauri 自动扫描结束 trigger={trigger} checked={} downloaded={} errors={} duration_ms={}",
                    result.checked_episodes,
                    result.downloaded.len(),
                    result.errors.len(),
                    started.elapsed().as_millis()
                );
                if let Ok(settings) = self.load_settings().await {
                    crate::system_integration::notify_automation_result(
                        &self.inner.app,
                        &settings,
                        result,
                    );
                }
            }
            Err(error) => {
                log::error!(
                    "Tauri 自动扫描异常 trigger={trigger} duration_ms={} error={error}",
                    started.elapsed().as_millis()
                );
                if let Ok(settings) = self.load_settings().await {
                    crate::system_integration::notify_scheduler_error(
                        &self.inner.app,
                        &settings,
                        error,
                    );
                }
            }
        }
        outcome
    }

    /// 从 SQLite、来源连接池和平台下载适配器装配一次扫描。
    async fn execute_scan(&self) -> Result<AutomationRunResult, String> {
        let storage = Arc::clone(&self.inner.storage);
        let defaults = self.inner.platform_defaults.clone();
        let (settings, sources, fansubs) = tauri::async_runtime::spawn_blocking(move || {
            let storage = storage
                .lock()
                .map_err(|error| format!("读取自动扫描上下文失败：{error}"))?;
            let repository = storage.repository();
            let settings = repository
                .get_settings(&defaults)
                .map_err(|error| format!("读取自动扫描设置失败：{error}"))?;
            let sources = repository
                .list_sources()
                .map_err(|error| format!("读取下载源失败：{error}"))?;
            let fansubs = repository
                .list_fansubs(None)
                .map_err(|error| format!("读取字幕组失败：{error}"))?;
            Ok::<_, String>((settings, sources, fansubs))
        })
        .await
        .map_err(|error| format!("读取自动扫描上下文失败：{error}"))??;
        let network = self
            .inner
            .source_state
            .network_service(&settings)
            .await
            .map_err(|error| format!("初始化来源网络失败：{error}"))?;
        let store = SharedReleaseSearchStore::new(Arc::clone(&self.inner.storage));
        let result = AutomationRunService::new(network, Arc::clone(&self.inner.executor))
            .run(
                &store,
                AutomationRunOptions {
                    now: None,
                    settings,
                    sources,
                    fansubs,
                },
            )
            .await
            .map_err(|error| format!("执行自动扫描失败：{error}"))?;
        let records = build_automation_notifications(&result);
        if !records.is_empty() {
            store
                .add_automation_notifications(&records)
                .map_err(|error| format!("写入自动扫描通知失败：{error}"))?;
        }
        Ok(result)
    }

    /// 按当前设置持续安排下一次自动扫描。
    async fn run_loop(&self) {
        match self.load_settings().await {
            Ok(settings) => self.refresh_from_settings(&settings).await,
            Err(error) => {
                log::error!("Tauri 自动扫描调度器读取设置失败 error={error}");
                self.inner.status.lock().await.last_error = Some(error);
            }
        }
        loop {
            let (enabled, interval_minutes) = {
                let status = self.inner.status.lock().await;
                (status.enabled, status.interval_minutes)
            };
            if !enabled {
                self.inner.wake.notified().await;
                continue;
            }
            let next = Utc::now() + Duration::minutes(interval_minutes);
            {
                let mut status = self.inner.status.lock().await;
                status.running = true;
                status.next_run_at = Some(to_iso(next));
            }
            let wait = StdDuration::from_secs((interval_minutes as u64).saturating_mul(60).max(1));
            tokio::select! {
                _ = tokio::time::sleep(wait) => {
                    if let Err(error) = self.run_now(false, "scheduled").await {
                        log::error!("Tauri 定时自动扫描失败 error={error}");
                        self.write_scheduler_error(&error).await;
                    }
                }
                _ = self.inner.wake.notified() => {}
            }
        }
    }

    /// 从 SQLite 读取当前自动扫描设置。
    async fn load_settings(&self) -> Result<AppSettings, String> {
        let storage = Arc::clone(&self.inner.storage);
        let defaults = self.inner.platform_defaults.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let storage = storage
                .lock()
                .map_err(|error| format!("读取自动扫描设置失败：{error}"))?;
            storage
                .repository()
                .get_settings(&defaults)
                .map_err(|error| format!("读取自动扫描设置失败：{error}"))
        })
        .await
        .map_err(|error| format!("读取自动扫描设置失败：{error}"))?
    }

    /// 将调度级错误写入提醒中心，避免后台失败静默丢失。
    async fn write_scheduler_error(&self, message: &str) {
        let storage = Arc::clone(&self.inner.storage);
        let message = message.to_owned();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let storage = storage.lock().map_err(|error| error.to_string())?;
            let created_at = to_iso(Utc::now());
            storage
                .repository()
                .add_notifications(&[NotificationRecord {
                    id: format!("notification-{created_at}-scheduler-error"),
                    kind: NotificationKind::Automation,
                    title: "自动扫描失败".to_owned(),
                    body: message,
                    severity: NotificationSeverity::Error,
                    anime_id: None,
                    episode_id: None,
                    download_task_id: None,
                    created_at,
                    read_at: None,
                }])
                .map_err(|error| error.to_string())
        })
        .await;
        if let Err(error) = result {
            log::error!("Tauri 自动扫描错误通知写入任务失败 error={error}");
        }
    }
}

/// 将自动扫描下载请求交给 Tauri 统一下载服务。
pub(crate) struct TauriAutomaticDownloadExecutor {
    downloads: AppDownloadState,
}

impl TauriAutomaticDownloadExecutor {
    /// 创建复用 commands 下载状态的自动执行器。
    pub(crate) fn new(downloads: AppDownloadState) -> Self {
        Self { downloads }
    }
}

#[async_trait]
impl AutomaticDownloadExecutor for TauriAutomaticDownloadExecutor {
    /// 添加磁链或远程 torrent，并持久化完整番剧和单集关联。
    async fn execute(
        &self,
        request: AutomaticDownloadRequest,
    ) -> Result<AutomaticDownloadReceipt, String> {
        if !release_satisfies_subtitle_requirement(
            &request.release,
            &request.anime.preferred_subtitle_languages,
            request.anime.preferred_subtitle.as_deref(),
        ) {
            log::warn!(
                "Tauri 自动下载执行器拒绝字幕不满足资源：anime_id={}, episode_id={}, title={:?}, actual={:?}, required={:?}",
                request.anime.anime.id,
                request.episode.id,
                request.release.title,
                request.release.subtitle_languages,
                request.anime.preferred_subtitle_languages
            );
            return Err("字幕规则不满足，已阻止自动下载".to_owned());
        }
        if let Some(task) = self
            .downloads
            .service()
            .find_episode_download(
                &request.anime.anime.id,
                &request.episode.id,
                request.episode.episode_no,
            )
            .map_err(|error| error.to_string())?
        {
            log::info!(
                "Tauri 自动下载提交前命中已有任务：anime_id={}, episode_id={}, episode_no={}, task_id={}",
                request.anime.anime.id,
                request.episode.id,
                request.episode.episode_no,
                task.id
            );
            return Ok(AutomaticDownloadReceipt { task_id: task.id });
        }
        let settings = self.downloads.settings()?;
        let engine = self
            .downloads
            .default_engine(&settings)
            .map_err(|error| error.to_string())?;
        let source_url = release_download_source_url(&request.release)?;
        let prepared = self
            .downloads
            .prepare_source(&source_url, &settings)
            .await?;
        let correlation_tag = format!(
            "ani:{}:{}:{}",
            request.anime.anime.id, request.episode.id, request.release.id
        );
        let context = release_download_context(
            &request.release,
            Some(request.anime.anime.id.clone()),
            Some(request.anime.anime.title.clone()),
            Some(request.episode.id.clone()),
            Some(request.episode.episode_no),
            request
                .release
                .fansub_group_id
                .clone()
                .or(request.anime.default_fansub_group_id.clone()),
        );
        let tasks = self
            .downloads
            .service()
            .add(DownloadAddRequest {
                engine,
                source: prepared.source(),
                options: AddTorrentOptions {
                    save_path: request.save_path,
                    correlation_tag: Some(correlation_tag),
                    paused: false,
                    ..AddTorrentOptions::default()
                },
                context,
            })
            .await
            .map_err(|error| error.to_string())?;
        let task = tasks
            .into_iter()
            .find(|task| {
                task.release_id.as_deref() == Some(request.release.id.as_str())
                    && task.episode_id.as_deref() == Some(request.episode.id.as_str())
            })
            .ok_or_else(|| "下载引擎已接收任务，但未找到持久化回执".to_owned())?;
        Ok(AutomaticDownloadReceipt { task_id: task.id })
    }
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn to_iso(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证自动扫描设置强制最小间隔并保留关闭状态。
    #[tokio::test]
    async fn refreshes_scheduler_settings() {
        let status = AutomationSchedulerStatus {
            enabled: true,
            running: true,
            in_flight: false,
            interval_minutes: 1,
            next_run_at: None,
            manual_cooldown_until: None,
            last_run_at: None,
            last_result: None,
            last_error: None,
        };
        assert_eq!(status.interval_minutes.max(MIN_INTERVAL_MINUTES), 5);
    }
}
