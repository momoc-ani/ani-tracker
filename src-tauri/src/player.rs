use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(desktop)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ani_contracts::{
    DesktopPlaybackSessionInput, DesktopPlayerWindowDragInput, DesktopPlayerWindowInput,
    PlaybackSession, PlaybackSubtitle, PlayerAvailability, PlayerBackend, PlayerCapabilities,
    PlayerCommand, PlayerCommandAction, PlayerCommandResult, PlayerHostPlatform, PlayerMediaMode,
    PlayerSnapshot, PlayerSubtitleType,
};
#[cfg(mobile)]
use ani_contracts::{PlayerMediaSource, PlayerSubtitleSource};
use ani_domain::{DownloadTask, TorrentFile};
use ani_media::player::PlayerService;
#[cfg(mobile)]
use ani_repository::AnimeTrackingRepository;
use ani_repository::{DownloadRepository, MediaRepository, PlaybackRepository};
use ani_storage::Storage;
use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSWindow, NSWindowOrderingMode};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSPoint, NSRect, NSSize};
#[cfg(desktop)]
use tauri::window::{Color, WindowBuilder};
#[cfg(target_os = "macos")]
use tauri::LogicalPosition;
#[cfg(desktop)]
use tauri::Manager;
#[cfg(any(test, all(desktop, not(target_os = "macos"))))]
use tauri::PhysicalPosition;
#[cfg(target_os = "macos")]
use tauri::WebviewWindow;
use tauri::{AppHandle, Emitter, Runtime, Window, WindowEvent};
#[cfg(desktop)]
use tauri::{PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_ani_player::AniPlayerExt;
#[cfg(desktop)]
use tauri_plugin_ani_player::{DesktopVideoTarget, DesktopWindowController};
use tokio::sync::RwLock;

pub(crate) const PLAYER_CONTROL_WINDOW_LABEL: &str = "ani-player-controls";
const PLAYER_VIDEO_WINDOW_LABEL: &str = "ani-player-video";
pub(crate) const PLAYER_SNAPSHOT_EVENT: &str = "player-snapshot";
const PLAYER_WINDOW_WIDTH: f64 = 1120.0;
const PLAYER_WINDOW_HEIGHT: f64 = 630.0;
const SESSION_TTL_HOURS: i64 = 4;
const PLAYER_SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(15);
const PLAYER_SERVICE_READY_POLL_INTERVAL: Duration = Duration::from_millis(25);
const PLAYER_BOUNDS_SYNC_DELAY: Duration = Duration::from_millis(32);

#[derive(Clone)]
struct ResolvedPlaybackSession {
    public: PlaybackSession,
    media_path: PathBuf,
    subtitle_paths: HashMap<String, PathBuf>,
    expires_at: SystemTime,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy)]
struct DesktopWindowDragState {
    pointer_start_x: f64,
    pointer_start_y: f64,
    window_start_x: f64,
    window_start_y: f64,
}

#[cfg(desktop)]
#[derive(Debug, Clone, Copy)]
enum PlayerBoundsSyncSide {
    Video,
    Controls,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
struct MacOSWindowFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg(target_os = "macos")]
impl From<NSRect> for MacOSWindowFrame {
    fn from(frame: NSRect) -> Self {
        Self {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        }
    }
}

#[cfg(target_os = "macos")]
impl MacOSWindowFrame {
    /// 转换为 AppKit 使用的窗口边界。
    fn into_ns_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width, self.height),
        )
    }
}

/// Tauri 生命周期内共享的播放窗口、受控会话和平台 transport。
#[derive(Clone)]
pub(crate) struct AppPlayerState {
    app: AppHandle,
    storage: Arc<Mutex<Storage>>,
    service: Arc<RwLock<Option<Arc<PlayerService>>>>,
    sessions: Arc<Mutex<HashMap<String, ResolvedPlaybackSession>>>,
    id_sequence: Arc<AtomicU64>,
    poll_generation: Arc<AtomicU64>,
    #[cfg(desktop)]
    fullscreen: Arc<AtomicBool>,
    #[cfg(desktop)]
    bounds_sync_pending: Arc<AtomicBool>,
    #[cfg(desktop)]
    bounds_sync_side: Arc<Mutex<Option<PlayerBoundsSyncSide>>>,
    #[cfg(target_os = "macos")]
    window_maximized: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    window_restore_frame: Arc<Mutex<Option<MacOSWindowFrame>>>,
    #[cfg(target_os = "macos")]
    fullscreen_restore_frame: Arc<Mutex<Option<MacOSWindowFrame>>>,
    #[cfg(target_os = "macos")]
    drag_state: Arc<Mutex<Option<DesktopWindowDragState>>>,
}

impl AppPlayerState {
    /// 创建尚未打开媒体窗口的播放器状态。
    pub(crate) fn new(app: &AppHandle, storage: Arc<Mutex<Storage>>) -> Self {
        #[cfg(mobile)]
        let service = Some(Arc::new(PlayerService::new(app.ani_player().transport())));
        #[cfg(desktop)]
        let service = None;
        Self {
            app: app.clone(),
            storage,
            service: Arc::new(RwLock::new(service)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            id_sequence: Arc::new(AtomicU64::new(0)),
            poll_generation: Arc::new(AtomicU64::new(0)),
            #[cfg(desktop)]
            fullscreen: Arc::new(AtomicBool::new(false)),
            #[cfg(desktop)]
            bounds_sync_pending: Arc::new(AtomicBool::new(false)),
            #[cfg(desktop)]
            bounds_sync_side: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            window_maximized: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            window_restore_frame: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            fullscreen_restore_frame: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            drag_state: Arc::new(Mutex::new(None)),
        }
    }

    /// 合并连续窗口事件，避免全屏过渡期间重复调整两个原生窗口。
    #[cfg(desktop)]
    fn schedule_bounds_sync(&self, side: PlayerBoundsSyncSide) {
        self.schedule_bounds_sync_after(side, PLAYER_BOUNDS_SYNC_DELAY);
    }

    /// 延迟全屏后的窗口边界同步，避开 AppKit 原生切换动画中的阻塞查询。
    #[cfg(desktop)]
    fn schedule_bounds_sync_after(&self, side: PlayerBoundsSyncSide, delay: Duration) {
        if let Ok(mut pending_side) = self.bounds_sync_side.lock() {
            *pending_side = Some(side);
        }
        if self.bounds_sync_pending.swap(true, Ordering::AcqRel) {
            return;
        }

        let app = self.app.clone();
        let pending = self.bounds_sync_pending.clone();
        let pending_side = self.bounds_sync_side.clone();
        let generation = self.poll_generation.load(Ordering::Acquire);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            let side = pending_side
                .lock()
                .ok()
                .and_then(|mut value| value.take())
                .unwrap_or(PlayerBoundsSyncSide::Video);
            pending.store(false, Ordering::Release);

            let Some(state) = app.try_state::<AppPlayerState>() else {
                return;
            };
            if state.poll_generation.load(Ordering::Acquire) != generation {
                return;
            }
            if state.fullscreen.load(Ordering::Acquire) {
                return;
            }
            let Some(video) = app.get_window(PLAYER_VIDEO_WINDOW_LABEL) else {
                return;
            };
            let Some(controls) = app.get_webview_window(PLAYER_CONTROL_WINDOW_LABEL) else {
                return;
            };
            let started = Instant::now();
            let result = match side {
                PlayerBoundsSyncSide::Video => {
                    #[cfg(target_os = "macos")]
                    {
                        sync_macos_player_window_bounds(&video, &controls.as_ref().window())
                    }
                    #[cfg(not(target_os = "macos"))]
                    sync_controls_window_bounds(&video, &controls.as_ref().window())
                }
                PlayerBoundsSyncSide::Controls => {
                    #[cfg(target_os = "macos")]
                    {
                        sync_macos_player_window_bounds(&video, &controls.as_ref().window())
                    }
                    #[cfg(not(target_os = "macos"))]
                    sync_video_window_bounds(&controls.as_ref().window(), &video)
                }
            };
            match result {
                Ok(()) => log::debug!(
                    "播放器窗口边界同步已合并 side={side:?} elapsed_ms={:.2}",
                    started.elapsed().as_secs_f64() * 1_000.0
                ),
                Err(error) => log::warn!(
                    "播放器窗口边界同步失败 side={side:?} elapsed_ms={:.2} error={error}",
                    started.elapsed().as_secs_f64() * 1_000.0
                ),
            }
        });
    }

    /// 在移动端创建受控会话并启动平台原生 libVLC 页面。
    #[cfg(mobile)]
    pub(crate) async fn open_desktop_window(
        &self,
        input: DesktopPlayerWindowInput,
    ) -> Result<(), String> {
        let session = self.create_session(input)?;
        let (anime_title, description, artwork_uri) =
            self.resolve_mobile_player_presentation(&session.task_id)?;
        let command_id = format!(
            "load-{}",
            self.id_sequence.fetch_add(1, Ordering::Relaxed) + 1
        );
        let command = PlayerCommand {
            command_id,
            session_id: session.id.clone(),
            action: PlayerCommandAction::Load {
                source: PlayerMediaSource {
                    task_id: session.task_id.clone(),
                    file_index: session.file_index,
                    title: session.file_name.clone(),
                    anime_title: Some(anime_title),
                    description,
                    artwork_uri,
                    uri: session.stream_url.clone(),
                    mode: session.mode,
                    duration_seconds: session.duration_seconds,
                    subtitles: session
                        .subtitles
                        .iter()
                        .map(|subtitle| PlayerSubtitleSource {
                            id: subtitle.id.clone(),
                            label: subtitle.label.clone(),
                            language: subtitle.language.clone(),
                            subtitle_type: subtitle.subtitle_type.clone(),
                            uri: subtitle.url.clone(),
                            default: subtitle.default,
                        })
                        .collect(),
                },
                start_position_seconds: session.start_position_seconds,
            },
        };
        let result = self.dispatch(command).await;
        if !result.accepted {
            let message = result
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "移动播放器拒绝加载媒体".to_owned());
            let _ = self.close_session(&session.id);
            return Err(message);
        }
        self.start_snapshot_polling();
        log::info!(
            "Tauri 移动原生播放器已打开 session_id={} task_id={} file_index={:?}",
            session.id,
            session.task_id,
            session.file_index
        );
        Ok(())
    }

    /// 读取移动原生播放器需要的番剧标题、简介和封面地址。
    #[cfg(mobile)]
    fn resolve_mobile_player_presentation(
        &self,
        task_id: &str,
    ) -> Result<(String, Option<String>, Option<String>), String> {
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("读取播放器展示数据失败：{error}"))?;
        let repository = storage.repository();
        let task = repository
            .list_downloads()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "播放任务不存在或已被删除".to_owned())?;
        let anime = match task.anime_id.as_deref() {
            Some(anime_id) => repository
                .list_my_anime()
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|item| item.anime.id == anime_id)
                .map(|item| item.anime),
            None => None,
        };
        let anime_title = anime
            .as_ref()
            .map(|item| item.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .or_else(|| task.anime_title.filter(|title| !title.trim().is_empty()))
            .unwrap_or_else(|| task.name.clone());
        Ok((
            anime_title,
            anime.as_ref().and_then(|item| item.summary.clone()),
            anime.and_then(|item| item.cover_url),
        ))
    }

    /// 创建视频原生窗口与透明控制层，并装配桌面 libmpv transport。
    #[cfg(desktop)]
    pub(crate) async fn open_desktop_window(
        &self,
        input: DesktopPlayerWindowInput,
    ) -> Result<(), String> {
        validate_player_target(&input)?;
        self.close_desktop_window().await?;

        let video = WindowBuilder::new(&self.app, PLAYER_VIDEO_WINDOW_LABEL)
            .title("Ani Tracker Player Video")
            .inner_size(PLAYER_WINDOW_WIDTH, PLAYER_WINDOW_HEIGHT)
            .min_inner_size(640.0, 360.0)
            // 透明控制层是用户实际拖动和缩放的唯一权威窗口，避免双窗口互相调整尺寸。
            .resizable(false)
            .decorations(false)
            .focusable(false)
            .shadow(false)
            .background_color(Color(0, 0, 0, 255))
            .visible(false)
            .build()
            .map_err(|error| format!("创建 MPV 视频窗口失败：{error}"))?;
        video
            .center()
            .map_err(|error| format!("定位 MPV 视频窗口失败：{error}"))?;

        let route = player_route(&input);
        let controls_builder = WebviewWindowBuilder::new(
            &self.app,
            PLAYER_CONTROL_WINDOW_LABEL,
            WebviewUrl::App(route.into()),
        )
        .title("Ani Tracker Player")
        .inner_size(PLAYER_WINDOW_WIDTH, PLAYER_WINDOW_HEIGHT)
        .min_inner_size(640.0, 360.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .visible(false);
        #[cfg(target_os = "windows")]
        let controls_builder = controls_builder.owner_raw(
            video
                .hwnd()
                .map_err(|error| format!("读取播放器所有者 HWND 失败：{error}"))?,
        );
        #[cfg(target_os = "macos")]
        let controls_builder = controls_builder.parent_raw(
            video
                .ns_window()
                .map_err(|error| format!("读取播放器父窗口 NSWindow 失败：{error}"))?,
        );
        #[cfg(target_os = "linux")]
        let controls_builder = {
            let video_gtk_window = video
                .gtk_window()
                .map_err(|error| format!("读取播放器父窗口 GTK Window 失败：{error}"))?;
            controls_builder.transient_for_raw(&video_gtk_window)
        };
        let controls = match controls_builder.build() {
            Ok(window) => window,
            Err(error) => {
                let _ = video.close();
                return Err(format!("创建 MPV 控制层失败：{error}"));
            }
        };
        #[cfg(target_os = "macos")]
        log::info!("macOS Tauri 播放器窗口层级已建立 video_parent=true controls_child=true");
        let initial_position = video
            .outer_position()
            .map_err(|error| format!("读取视频窗口初始位置失败：{error}"))?;
        controls
            .set_position(initial_position)
            .map_err(|error| format!("定位 MPV 控制层失败：{error}"))?;
        let controls_window = controls.as_ref().window();
        #[cfg(target_os = "macos")]
        sync_macos_player_window_bounds(&video, &controls_window)?;
        #[cfg(not(target_os = "macos"))]
        sync_video_window_bounds(&controls_window, &video)?;

        let target = resolve_video_target(&video)?;
        video
            .show()
            .map_err(|error| format!("显示 MPV 视频窗口失败：{error}"))?;
        controls
            .show()
            .map_err(|error| format!("显示 MPV 控制层失败：{error}"))?;
        #[cfg(target_os = "macos")]
        ensure_macos_player_window_layering(&video, &controls)?;
        controls
            .set_focus()
            .map_err(|error| format!("聚焦 MPV 控制层失败：{error}"))?;

        let controller = Arc::new(TauriPlayerWindowController {
            app: self.app.clone(),
            fullscreen: self.fullscreen.clone(),
            #[cfg(target_os = "macos")]
            fullscreen_restore_frame: self.fullscreen_restore_frame.clone(),
        });
        let transport = self
            .app
            .ani_player()
            .create_desktop_transport(target, controller);
        *self.service.write().await = Some(Arc::new(PlayerService::new(transport)));
        controls
            .set_focus()
            .map_err(|error| format!("恢复 MPV 控制层焦点失败：{error}"))?;
        self.start_snapshot_polling();
        log::info!(
            "Tauri 桌面播放器窗口已打开 task_id={} file_index={:?}",
            input.task_id,
            input.file_index
        );
        Ok(())
    }

    /// 关闭桌面播放器窗口并幂等释放 libmpv。
    pub(crate) async fn close_desktop_window(&self) -> Result<(), String> {
        self.poll_generation.fetch_add(1, Ordering::SeqCst);
        #[cfg(desktop)]
        self.fullscreen.store(false, Ordering::Release);
        #[cfg(target_os = "macos")]
        {
            self.window_maximized.store(false, Ordering::Release);
            if let Ok(mut restore_frame) = self.window_restore_frame.lock() {
                *restore_frame = None;
            }
            if let Ok(mut restore_frame) = self.fullscreen_restore_frame.lock() {
                *restore_frame = None;
            }
            if let Ok(mut drag_state) = self.drag_state.lock() {
                *drag_state = None;
            }
        }
        #[cfg(desktop)]
        if let Some(service) = self.service.write().await.take() {
            service
                .shutdown()
                .await
                .map_err(|error| error.to_string())?;
        }
        #[cfg(mobile)]
        if let Some(service) = self.service.read().await.clone() {
            service
                .shutdown()
                .await
                .map_err(|error| error.to_string())?;
        }
        #[cfg(desktop)]
        if let Some(window) = self.app.get_webview_window(PLAYER_CONTROL_WINDOW_LABEL) {
            window
                .close()
                .map_err(|error| format!("关闭播放器控制层失败：{error}"))?;
        }
        #[cfg(desktop)]
        if let Some(window) = self.app.get_window(PLAYER_VIDEO_WINDOW_LABEL) {
            window
                .close()
                .map_err(|error| format!("关闭播放器视频窗口失败：{error}"))?;
        }
        Ok(())
    }

    /// 按当前平台处理透明控制层的窗口拖动。
    pub(crate) fn drag_desktop_window(
        &self,
        input: DesktopPlayerWindowDragInput,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.drag_macos_window(input)
        }
        #[cfg(not(target_os = "macos"))]
        {
            #[cfg(mobile)]
            {
                let _ = input;
                return Err("移动原生播放器不支持桌面窗口拖动".to_owned());
            }
            #[cfg(desktop)]
            {
                if !matches!(input, DesktopPlayerWindowDragInput::Start { .. }) {
                    return Ok(());
                }
                self.start_native_dragging()
            }
        }
    }

    /// 切换播放器窗口模式的最大化状态。
    pub(crate) fn toggle_desktop_window_maximize(&self) -> Result<bool, String> {
        #[cfg(mobile)]
        {
            return Err("移动原生播放器不支持桌面窗口最大化".to_owned());
        }
        #[cfg(desktop)]
        {
            if self.fullscreen.load(Ordering::Acquire) {
                return Ok(false);
            }
            #[cfg(target_os = "macos")]
            {
                self.toggle_macos_window_maximize()
            }
            #[cfg(not(target_os = "macos"))]
            {
                let window = self
                    .app
                    .get_window(PLAYER_CONTROL_WINDOW_LABEL)
                    .ok_or_else(|| "播放器控制层不存在".to_owned())?;
                let maximized = window
                    .is_maximized()
                    .map_err(|error| format!("读取播放器最大化状态失败：{error}"))?;
                if maximized {
                    window
                        .unmaximize()
                        .map_err(|error| format!("还原播放器窗口失败：{error}"))?;
                } else {
                    window
                        .maximize()
                        .map_err(|error| format!("最大化播放器窗口失败：{error}"))?;
                }
                log::info!(
                    "Tauri 播放器窗口最大化状态已切换 maximized={} driver=controls",
                    !maximized
                );
                Ok(!maximized)
            }
        }
    }

    /// 在 AppKit 主线程内无动画同步切换视频父窗与控制子窗边界。
    #[cfg(target_os = "macos")]
    fn toggle_macos_window_maximize(&self) -> Result<bool, String> {
        let video = self
            .app
            .get_window(PLAYER_VIDEO_WINDOW_LABEL)
            .ok_or_else(|| "播放器视频窗口不存在".to_owned())?;
        let controls = self
            .app
            .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL)
            .ok_or_else(|| "播放器控制层不存在".to_owned())?;
        let was_maximized = self.window_maximized.fetch_xor(true, Ordering::AcqRel);
        let maximized = !was_maximized;
        let maximized_state = self.window_maximized.clone();
        let restore_frame = self.window_restore_frame.clone();
        let video_for_update = video.clone();
        let controls_for_update = controls.as_ref().window().clone();
        if let Err(error) = video.run_on_main_thread(move || {
            match apply_macos_window_mode(
                &video_for_update,
                &controls_for_update,
                maximized,
                &restore_frame,
            ) {
                Ok(target) => log::info!(
                    "macOS 播放器双窗边界已同步 maximized={} x={} y={} width={} height={}",
                    maximized,
                    target.x,
                    target.y,
                    target.width,
                    target.height
                ),
                Err(error) => {
                    maximized_state.store(was_maximized, Ordering::Release);
                    log::error!("同步 macOS 播放器最大化边界失败 error={error}");
                }
            }
        }) {
            self.window_maximized
                .store(was_maximized, Ordering::Release);
            return Err(format!("提交播放器窗口最大化任务失败：{error}"));
        }
        log::info!(
            "Tauri 播放器窗口最大化状态已切换 maximized={} driver=appkit-synchronized",
            maximized
        );
        Ok(maximized)
    }

    /// 将非 macOS 播放器控制层拖动委托给 Tauri 原生窗口。
    #[cfg(all(desktop, not(target_os = "macos")))]
    fn start_native_dragging(&self) -> Result<(), String> {
        let window = self
            .app
            .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL)
            .ok_or_else(|| "播放器控制层不存在".to_owned())?;
        window
            .start_dragging()
            .map_err(|error| format!("拖动播放器窗口失败：{error}"))
    }

    /// 在 macOS 透明窗口上使用逻辑坐标同步移动视频窗和控制层。
    #[cfg(target_os = "macos")]
    fn drag_macos_window(&self, input: DesktopPlayerWindowDragInput) -> Result<(), String> {
        if matches!(input, DesktopPlayerWindowDragInput::End) {
            *self
                .drag_state
                .lock()
                .map_err(|error| format!("结束播放器拖动失败：{error}"))? = None;
            log::debug!("macOS Tauri 播放器窗口拖动结束");
            return Ok(());
        }
        let (screen_x, screen_y) = drag_screen_point(input)
            .filter(|(x, y)| valid_screen_point(*x, *y))
            .ok_or_else(|| "播放器拖动坐标无效".to_owned())?;
        let controls = self
            .app
            .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL)
            .ok_or_else(|| "播放器控制层不存在".to_owned())?;
        let video = self
            .app
            .get_window(PLAYER_VIDEO_WINDOW_LABEL)
            .ok_or_else(|| "播放器视频窗口不存在".to_owned())?;
        if self.fullscreen.load(Ordering::Acquire)
            || self.window_maximized.load(Ordering::Acquire)
            || video
                .is_fullscreen()
                .map_err(|error| format!("读取播放器全屏状态失败：{error}"))?
            || video
                .is_maximized()
                .map_err(|error| format!("读取播放器视频窗最大化状态失败：{error}"))?
            || controls
                .is_maximized()
                .map_err(|error| format!("读取播放器最大化状态失败：{error}"))?
        {
            if let Ok(mut drag_state) = self.drag_state.lock() {
                *drag_state = None;
            }
            return Ok(());
        }

        match input {
            DesktopPlayerWindowDragInput::Start { .. } => {
                let scale_factor = controls
                    .scale_factor()
                    .map_err(|error| format!("读取播放器缩放比例失败：{error}"))?;
                let position = controls
                    .outer_position()
                    .map_err(|error| format!("读取播放器窗口位置失败：{error}"))?
                    .to_logical::<f64>(scale_factor);
                *self
                    .drag_state
                    .lock()
                    .map_err(|error| format!("开始播放器拖动失败：{error}"))? =
                    Some(DesktopWindowDragState {
                        pointer_start_x: screen_x,
                        pointer_start_y: screen_y,
                        window_start_x: position.x,
                        window_start_y: position.y,
                    });
                log::debug!("macOS Tauri 播放器窗口拖动开始");
            }
            DesktopPlayerWindowDragInput::Move { .. } => {
                let drag_state = *self
                    .drag_state
                    .lock()
                    .map_err(|error| format!("读取播放器拖动状态失败：{error}"))?;
                let Some(drag_state) = drag_state else {
                    return Ok(());
                };
                let (x, y) = next_drag_position(drag_state, screen_x, screen_y);
                let position = LogicalPosition::new(x, y);
                video
                    .set_position(position)
                    .map_err(|error| format!("移动播放器视频父窗口失败：{error}"))?;
            }
            DesktopPlayerWindowDragInput::End => {}
        }
        Ok(())
    }
}

#[cfg(desktop)]
struct TauriPlayerWindowController {
    app: AppHandle,
    fullscreen: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    fullscreen_restore_frame: Arc<Mutex<Option<MacOSWindowFrame>>>,
}

#[cfg(desktop)]
impl DesktopWindowController for TauriPlayerWindowController {
    fn set_fullscreen(&self, fullscreen: bool) -> Result<bool, String> {
        let started = Instant::now();
        let lookup_started = Instant::now();
        let video = self
            .app
            .get_window(PLAYER_VIDEO_WINDOW_LABEL)
            .ok_or_else(|| "播放器视频窗口不存在".to_owned())?;
        let controls = self
            .app
            .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL)
            .ok_or_else(|| "播放器控制层不存在".to_owned())?;
        let lookup_elapsed_ms = lookup_started.elapsed().as_secs_f64() * 1_000.0;
        let maximized_started = Instant::now();
        let controls_maximized = controls
            .is_maximized()
            .map_err(|error| format!("读取播放器控制层最大化状态失败：{error}"))?;
        let maximized_elapsed_ms = maximized_started.elapsed().as_secs_f64() * 1_000.0;
        #[cfg(not(target_os = "macos"))]
        let controls_window = controls.as_ref().window();
        log::info!("播放器全屏切换开始 fullscreen={fullscreen}");
        let native_started = Instant::now();
        #[cfg(target_os = "macos")]
        {
            controls
                .set_resizable(!fullscreen)
                .map_err(|error| format!("切换控制层缩放能力失败：{error}"))?;
            let fullscreen_restore_frame = self.fullscreen_restore_frame.clone();
            let video_for_update = video.clone();
            let controls_for_update = controls.as_ref().window().clone();
            video
                .run_on_main_thread(move || {
                    match apply_macos_fullscreen_window_mode(
                        &video_for_update,
                        &controls_for_update,
                        fullscreen,
                        &fullscreen_restore_frame,
                    ) {
                        Ok(target) => {
                            log::debug!(
                                "macOS 播放器无动画全屏边界已同步 fullscreen={} x={} y={} width={} height={}",
                                fullscreen,
                                target.x,
                                target.y,
                                target.width,
                                target.height
                            );
                        }
                        Err(error) => {
                            log::error!("macOS 播放器无动画全屏边界失败 error={error}");
                        }
                    }
                })
                .map_err(|error| format!("提交 macOS 播放器全屏边界失败：{error}"))?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            controls
                .set_fullscreen(fullscreen)
                .map_err(|error| format!("切换控制层全屏失败：{error}"))?;
        }
        let native_elapsed_ms = native_started.elapsed().as_secs_f64() * 1_000.0;
        #[cfg(target_os = "macos")]
        let sync_elapsed_ms: f64 = 0.0;
        #[cfg(not(target_os = "macos"))]
        let sync_elapsed_ms = {
            let sync_started = Instant::now();
            sync_video_window_bounds(&controls_window, &video)?;
            sync_started.elapsed().as_secs_f64() * 1_000.0
        };
        self.fullscreen.store(fullscreen, Ordering::Release);
        log::info!(
            "Tauri 播放器窗口模式已同步 fullscreen={} controls_maximized={} driver={} lookup_ms={:.2} maximized_ms={:.2} native_ms={:.2} sync_ms={:.2} total_ms={:.2} sync_mode={}",
            fullscreen,
            controls_maximized,
            if cfg!(target_os = "macos") {
                "video-parent-frame"
            } else {
                "controls"
            },
            lookup_elapsed_ms,
            maximized_elapsed_ms,
            native_elapsed_ms,
            sync_elapsed_ms.max(0.0),
            started.elapsed().as_secs_f64() * 1_000.0,
            if cfg!(target_os = "macos") {
                "appkit-transaction"
            } else {
                "immediate"
            }
        );
        Ok(fullscreen)
    }

    fn close(&self) -> Result<(), String> {
        self.fullscreen.store(false, Ordering::Release);
        if let Some(window) = self.app.get_webview_window(PLAYER_CONTROL_WINDOW_LABEL) {
            window
                .close()
                .map_err(|error| format!("关闭播放器控制层失败：{error}"))?;
        }
        if let Some(window) = self.app.get_window(PLAYER_VIDEO_WINDOW_LABEL) {
            window
                .close()
                .map_err(|error| format!("关闭播放器视频窗口失败：{error}"))?;
        }
        Ok(())
    }
}

fn validate_player_target(input: &DesktopPlayerWindowInput) -> Result<(), String> {
    validate_identifier(&input.task_id, true)
}

fn validate_identifier(value: &str, allow_colon: bool) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 160
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.')
                || (allow_colon && byte == b':')
        });
    if valid {
        Ok(())
    } else {
        Err("播放器标识无效".to_owned())
    }
}

#[cfg(any(desktop, test))]
fn player_route(input: &DesktopPlayerWindowInput) -> String {
    let mut route = format!("index.html?aniView=desktop-player&taskId={}", input.task_id);
    if let Some(file_index) = input.file_index {
        route.push_str(&format!("&fileIndex={file_index}"));
    }
    route
}

fn select_playable_file(
    task: &DownloadTask,
    requested_index: Option<u32>,
) -> Result<&TorrentFile, String> {
    task.files
        .iter()
        .filter(|file| file.selected && (task.is_completed() || file.progress >= 1.0))
        .filter(|file| is_video_path(&file.name))
        .find(|file| requested_index.is_none_or(|index| file.index == i64::from(index)))
        .ok_or_else(|| "当前任务没有已完成的可播放视频".to_owned())
}

fn resolve_task_file_path(task: &DownloadTask, file: &TorrentFile) -> Result<PathBuf, String> {
    let file_path = Path::new(&file.name);
    let unresolved = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        Path::new(&task.save_path).join(file_path)
    };
    let resolved = crate::path_utils::canonicalize(&unresolved)
        .map_err(|error| format!("播放文件不可访问：{error}"))?;
    if !file_path.is_absolute() {
        let root = crate::path_utils::canonicalize(&task.save_path)
            .map_err(|error| format!("下载目录不可访问：{error}"))?;
        if !resolved.starts_with(root) {
            return Err("播放文件路径超出任务保存目录".to_owned());
        }
    }
    Ok(resolved)
}

fn discover_sidecar_subtitles(
    session_id: &str,
    media_path: &Path,
) -> (Vec<PlaybackSubtitle>, HashMap<String, PathBuf>) {
    let Some(directory) = media_path.parent() else {
        return (Vec::new(), HashMap::new());
    };
    let media_stem = media_path
        .file_stem()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut candidates = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let extension = path
                .extension()
                .map(|value| value.to_string_lossy().to_lowercase());
            let stem = path
                .file_stem()
                .map(|value| value.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(extension.as_deref(), Some("ass" | "vtt")) && stem.starts_with(&media_stem)
        })
        .take(32)
        .collect::<Vec<_>>();
    candidates.sort();
    let mut subtitles = Vec::new();
    let mut paths = HashMap::new();
    for (index, path) in candidates.into_iter().enumerate() {
        let id = format!("subtitle-{session_id}-{index}");
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "vtt".to_owned());
        subtitles.push(PlaybackSubtitle {
            id: id.clone(),
            label: path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("字幕 {}", index + 1)),
            language: None,
            subtitle_type: if extension == "ass" {
                PlayerSubtitleType::Ass
            } else {
                PlayerSubtitleType::Vtt
            },
            url: format!("ani-player://session/{session_id}/subtitle/{index}"),
            default: index == 0,
        });
        paths.insert(id, path);
    }
    (subtitles, paths)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left =
        crate::path_utils::canonicalize(left).unwrap_or_else(|_| crate::path_utils::simplify(left));
    let right = crate::path_utils::canonicalize(right)
        .unwrap_or_else(|_| crate::path_utils::simplify(right));
    if cfg!(target_os = "windows") {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn is_video_path(value: &str) -> bool {
    let value = value.to_lowercase();
    [".mkv", ".mp4", ".avi", ".mov", ".webm"]
        .iter()
        .any(|extension| value.ends_with(extension))
}

#[cfg(target_os = "macos")]
fn drag_screen_point(input: DesktopPlayerWindowDragInput) -> Option<(f64, f64)> {
    match input {
        DesktopPlayerWindowDragInput::Start { screen_x, screen_y }
        | DesktopPlayerWindowDragInput::Move { screen_x, screen_y } => Some((screen_x, screen_y)),
        DesktopPlayerWindowDragInput::End => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn valid_screen_point(screen_x: f64, screen_y: f64) -> bool {
    screen_x.is_finite()
        && screen_y.is_finite()
        && screen_x.abs() <= 1_000_000.0
        && screen_y.abs() <= 1_000_000.0
}

#[cfg(any(target_os = "macos", test))]
fn next_drag_position(state: DesktopWindowDragState, screen_x: f64, screen_y: f64) -> (f64, f64) {
    (
        state.window_start_x + screen_x - state.pointer_start_x,
        state.window_start_y + screen_y - state.pointer_start_y,
    )
}

/// 计算 macOS 最大化目标，并维护窗口模式的还原边界。
#[cfg(any(target_os = "macos", test))]
fn resolve_macos_window_mode_frame(
    maximized: bool,
    current_frame: MacOSWindowFrame,
    visible_frame: Option<MacOSWindowFrame>,
    restore_frame: &mut Option<MacOSWindowFrame>,
) -> Result<MacOSWindowFrame, String> {
    if maximized {
        let target = visible_frame.ok_or_else(|| "播放器窗口不在可用屏幕内".to_owned())?;
        *restore_frame = Some(current_frame);
        return Ok(target);
    }
    restore_frame
        .take()
        .ok_or_else(|| "播放器窗口缺少还原边界".to_owned())
}

/// 在同一次 AppKit 主线程回调内设置父窗和子窗，避免异步尺寸事件产生延迟。
#[cfg(target_os = "macos")]
fn apply_macos_window_mode<R: Runtime>(
    video: &Window<R>,
    controls: &Window<R>,
    maximized: bool,
    restore_frame: &Mutex<Option<MacOSWindowFrame>>,
) -> Result<MacOSWindowFrame, String> {
    let video_pointer = video
        .ns_window()
        .map_err(|error| format!("读取播放器视频父窗失败：{error}"))?
        .cast::<NSWindow>();
    let controls_pointer = controls
        .ns_window()
        .map_err(|error| format!("读取播放器控制子窗失败：{error}"))?
        .cast::<NSWindow>();
    if video_pointer.is_null() || controls_pointer.is_null() {
        return Err("播放器原生窗口指针无效".to_owned());
    }

    // SAFETY: 两个指针均由仍存活的 Tauri Window 持有，且本方法只在 AppKit 主线程回调执行。
    unsafe {
        let video_window = &*video_pointer;
        let controls_window = &*controls_pointer;
        let current_frame = MacOSWindowFrame::from(NSWindow::frame(video_window));
        let visible_frame = NSWindow::screen(video_window)
            .map(|screen| MacOSWindowFrame::from(screen.visibleFrame()));
        let target = {
            let mut restore_frame = restore_frame
                .lock()
                .map_err(|error| format!("读取播放器还原边界失败：{error}"))?;
            resolve_macos_window_mode_frame(
                maximized,
                current_frame,
                visible_frame,
                &mut restore_frame,
            )?
        };
        let target_rect = target.into_ns_rect();
        NSWindow::setFrame_display_animate(video_window, target_rect, true, false);
        NSWindow::setFrame_display_animate(controls_window, target_rect, true, false);
        Ok(target)
    }
}

/// 在 AppKit 单次事务内切换无动画全屏，并保存还原边界。
#[cfg(target_os = "macos")]
fn apply_macos_fullscreen_window_mode<R: Runtime>(
    video: &Window<R>,
    controls: &Window<R>,
    fullscreen: bool,
    restore_frame: &Mutex<Option<MacOSWindowFrame>>,
) -> Result<MacOSWindowFrame, String> {
    let video_pointer = video
        .ns_window()
        .map_err(|error| format!("读取播放器视频父窗失败：{error}"))?
        .cast::<NSWindow>();
    let controls_pointer = controls
        .ns_window()
        .map_err(|error| format!("读取播放器控制子窗失败：{error}"))?
        .cast::<NSWindow>();
    if video_pointer.is_null() || controls_pointer.is_null() {
        return Err("播放器原生窗口指针无效".to_owned());
    }

    // SAFETY: 两个指针由仍存活的 Tauri Window 持有，且调用发生在 AppKit 主线程。
    unsafe {
        let video_window = &*video_pointer;
        let controls_window = &*controls_pointer;
        let current_frame = MacOSWindowFrame::from(NSWindow::frame(video_window));
        let screen_frame =
            NSWindow::screen(video_window).map(|screen| MacOSWindowFrame::from(screen.frame()));
        let target = {
            let mut restore_frame = restore_frame
                .lock()
                .map_err(|error| format!("读取播放器全屏还原边界失败：{error}"))?;
            resolve_macos_window_mode_frame(
                fullscreen,
                current_frame,
                screen_frame,
                &mut restore_frame,
            )?
        };
        let target_rect = target.into_ns_rect();
        NSWindow::setFrame_display_animate(video_window, target_rect, true, false);
        NSWindow::setFrame_display_animate(controls_window, target_rect, true, false);
        Ok(target)
    }
}

/// 按透明控制层的物理边界同步视频窗口。
#[cfg(all(desktop, not(target_os = "macos")))]
fn sync_video_window_bounds<R: Runtime>(
    controls: &Window<R>,
    video: &Window<R>,
) -> Result<(), String> {
    let position: PhysicalPosition<i32> = controls
        .outer_position()
        .map_err(|error| format!("读取控制层位置失败：{error}"))?;
    let controls_outer_size: PhysicalSize<u32> = controls
        .outer_size()
        .map_err(|error| format!("读取控制层外框尺寸失败：{error}"))?;
    let video_outer_size: PhysicalSize<u32> = video
        .outer_size()
        .map_err(|error| format!("读取视频窗口外框尺寸失败：{error}"))?;
    let video_position: PhysicalPosition<i32> = video
        .outer_position()
        .map_err(|error| format!("读取视频窗口位置失败：{error}"))?;
    let video_inner_size: PhysicalSize<u32> = video
        .inner_size()
        .map_err(|error| format!("读取视频窗口内容尺寸失败：{error}"))?;
    let target_inner_size =
        inner_size_for_outer_bounds(controls_outer_size, video_outer_size, video_inner_size);
    if video_position != position {
        video
            .set_position(position)
            .map_err(|error| format!("同步视频窗口位置失败：{error}"))?;
    }
    if video_outer_size != controls_outer_size {
        video
            .set_size(target_inner_size)
            .map_err(|error| format!("同步视频窗口尺寸失败：{error}"))?;
    }
    Ok(())
}

/// 在 AppKit 主线程按控制层绝对 frame 同步双窗口，避免左上角缩放产生坐标漂移。
#[cfg(target_os = "macos")]
fn sync_macos_player_window_bounds<R: Runtime>(
    video: &Window<R>,
    controls: &Window<R>,
) -> Result<(), String> {
    let video_pointer = video
        .ns_window()
        .map_err(|error| format!("读取播放器视频父窗失败：{error}"))?
        .cast::<NSWindow>();
    let controls_pointer = controls
        .ns_window()
        .map_err(|error| format!("读取播放器控制层失败：{error}"))?
        .cast::<NSWindow>();
    if video_pointer.is_null() || controls_pointer.is_null() {
        return Err("播放器原生窗口指针无效".to_owned());
    }
    let video_address = video_pointer as usize;
    let controls_address = controls_pointer as usize;
    video
        .run_on_main_thread(move || {
            // SAFETY: 两个窗口指针由仍存活的 Tauri Window 持有，闭包在 AppKit 主线程执行。
            unsafe {
                let video_window = &*(video_address as *mut NSWindow);
                let controls_window = &*(controls_address as *mut NSWindow);
                let target = NSWindow::frame(controls_window);
                let video_frame = NSWindow::frame(video_window);
                if !macos_ns_rects_close(video_frame, target) {
                    NSWindow::setFrame_display_animate(video_window, target, false, false);
                }
                let controls_frame = NSWindow::frame(controls_window);
                if !macos_ns_rects_close(controls_frame, target) {
                    NSWindow::setFrame_display_animate(controls_window, target, false, false);
                }
                log::debug!(
                    "macOS 播放器缩放边界已按绝对 frame 同步 x={} y={} width={} height={}",
                    target.origin.x,
                    target.origin.y,
                    target.size.width,
                    target.size.height
                );
            }
        })
        .map_err(|error| format!("提交 macOS 播放器缩放边界失败：{error}"))?;
    Ok(())
}

/// 判断两个 AppKit 窗口边界是否已足够接近，避免重复 setFrame 触发 resize 事件。
#[cfg(target_os = "macos")]
fn macos_ns_rects_close(left: NSRect, right: NSRect) -> bool {
    const EPSILON: f64 = 0.5;
    (left.origin.x - right.origin.x).abs() <= EPSILON
        && (left.origin.y - right.origin.y).abs() <= EPSILON
        && (left.size.width - right.size.width).abs() <= EPSILON
        && (left.size.height - right.size.height).abs() <= EPSILON
}

/// 按视频宿主的物理边界同步透明控制层。
#[cfg(all(desktop, not(target_os = "macos")))]
fn sync_controls_window_bounds<R: Runtime>(
    video: &Window<R>,
    controls: &Window<R>,
) -> Result<(), String> {
    let position: PhysicalPosition<i32> = video
        .outer_position()
        .map_err(|error| format!("读取视频父窗口位置失败：{error}"))?;
    let video_outer_size: PhysicalSize<u32> = video
        .outer_size()
        .map_err(|error| format!("读取视频父窗口外框尺寸失败：{error}"))?;
    let controls_outer_size: PhysicalSize<u32> = controls
        .outer_size()
        .map_err(|error| format!("读取控制子窗外框尺寸失败：{error}"))?;
    let controls_position: PhysicalPosition<i32> = controls
        .outer_position()
        .map_err(|error| format!("读取控制子窗位置失败：{error}"))?;
    let controls_inner_size: PhysicalSize<u32> = controls
        .inner_size()
        .map_err(|error| format!("读取控制子窗内容尺寸失败：{error}"))?;
    let target_inner_size =
        inner_size_for_outer_bounds(video_outer_size, controls_outer_size, controls_inner_size);
    if controls_position != position {
        controls
            .set_position(position)
            .map_err(|error| format!("同步控制子窗位置失败：{error}"))?;
    }
    if controls_outer_size != video_outer_size {
        controls
            .set_size(target_inner_size)
            .map_err(|error| format!("同步控制子窗尺寸失败：{error}"))?;
    }
    Ok(())
}

#[cfg(desktop)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoWindowEventAction {
    None,
    RestoreControlFocus,
    SyncControlBounds,
    CloseControlWindow,
}

#[cfg(desktop)]
fn video_window_event_action(event: &WindowEvent) -> VideoWindowEventAction {
    match event {
        WindowEvent::Focused(true) => VideoWindowEventAction::RestoreControlFocus,
        WindowEvent::Moved(_)
        | WindowEvent::Resized(_)
        | WindowEvent::ScaleFactorChanged { .. } => VideoWindowEventAction::SyncControlBounds,
        WindowEvent::Destroyed => VideoWindowEventAction::CloseControlWindow,
        _ => VideoWindowEventAction::None,
    }
}

/// 在 AppKit 主线程显式恢复视频父窗和控制子窗层级，避免 OpenGL 窗口截获输入。
#[cfg(target_os = "macos")]
fn ensure_macos_player_window_layering<R: Runtime>(
    video: &Window<R>,
    controls: &WebviewWindow<R>,
) -> Result<(), String> {
    let video_window = video
        .ns_window()
        .map_err(|error| format!("读取播放器视频父窗失败：{error}"))?
        as usize;
    let controls_window = controls
        .ns_window()
        .map_err(|error| format!("读取播放器控制子窗失败：{error}"))?
        as usize;
    video
        .run_on_main_thread(move || {
            // SAFETY: 指针由调用期间仍存活的 Tauri 窗口持有，闭包在 AppKit 主线程执行。
            unsafe {
                let video_window = &*(video_window as *mut NSWindow);
                let controls_window = &*(controls_window as *mut NSWindow);
                // live resize 期间保留上一帧内容，避免 WebView 与 OpenGL 表面同步重绘拖慢拖动。
                video_window.setPreservesContentDuringLiveResize(true);
                controls_window.setPreservesContentDuringLiveResize(true);
                match controls_window.parentWindow() {
                    Some(parent) if std::ptr::eq(&*parent, video_window) => {}
                    Some(parent) => {
                        parent.removeChildWindow(controls_window);
                        video_window
                            .addChildWindow_ordered(controls_window, NSWindowOrderingMode::Above);
                    }
                    None => video_window
                        .addChildWindow_ordered(controls_window, NSWindowOrderingMode::Above),
                }
                controls_window.makeKeyAndOrderFront(None);
            }
        })
        .map_err(|error| format!("提交播放器窗口层级同步失败：{error}"))?;
    Ok(())
}

/// 扣除视频窗原生边框，使其物理外框与透明控制层完全重合。
#[cfg(desktop)]
#[allow(dead_code)]
fn inner_size_for_outer_bounds(
    target_outer_size: PhysicalSize<u32>,
    current_outer_size: PhysicalSize<u32>,
    current_inner_size: PhysicalSize<u32>,
) -> PhysicalSize<u32> {
    let frame_width = current_outer_size
        .width
        .saturating_sub(current_inner_size.width);
    let frame_height = current_outer_size
        .height
        .saturating_sub(current_inner_size.height);
    PhysicalSize::new(
        target_outer_size.width.saturating_sub(frame_width).max(1),
        target_outer_size.height.saturating_sub(frame_height).max(1),
    )
}

#[cfg(target_os = "windows")]
fn resolve_video_target(video: &Window) -> Result<DesktopVideoTarget, String> {
    let hwnd = video
        .hwnd()
        .map_err(|error| format!("读取播放器 HWND 失败：{error}"))?;
    Ok(DesktopVideoTarget::Windows(hwnd.0 as isize))
}

#[cfg(target_os = "macos")]
fn resolve_video_target(video: &Window) -> Result<DesktopVideoTarget, String> {
    video
        .ns_window()
        .map(|window| DesktopVideoTarget::MacOs(window as usize))
        .map_err(|error| format!("读取播放器 NSWindow 失败：{error}"))
}

#[cfg(target_os = "linux")]
fn resolve_video_target(video: &Window) -> Result<DesktopVideoTarget, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match video
        .window_handle()
        .map_err(|error| format!("读取播放器 X11 窗口失败：{error}"))?
        .as_raw()
    {
        RawWindowHandle::Xlib(handle) => Ok(DesktopVideoTarget::X11(handle.window as u32)),
        RawWindowHandle::Xcb(handle) => Ok(DesktopVideoTarget::X11(handle.window.get())),
        _ => Err("Linux 首期仅支持 X11/XWayland 播放窗口".to_owned()),
    }
}

fn unavailable_capabilities(reason: String) -> PlayerCapabilities {
    PlayerCapabilities {
        backend: PlayerBackend::Mpv,
        platform: PlayerHostPlatform::TauriDesktop,
        availability: PlayerAvailability::Unavailable,
        can_seek: false,
        can_set_volume: false,
        can_mute: false,
        playback_rates: vec![1.0],
        supports_audio_tracks: false,
        supports_subtitle_tracks: false,
        supports_subtitle_scale: false,
        supports_video_enhancement: false,
        supports_frame_interpolation: false,
        supports_model_enhancement: false,
        supports_aspect_ratio: false,
        supports_fullscreen: false,
        supports_picture_in_picture: false,
        supports_playlist_navigation: false,
        supports_direct_playback: false,
        supports_transcoding_fallback: false,
        supports_hdr: false,
        unavailable_reason: Some(reason),
    }
}

fn rejected_command(command_id: &str, message: String) -> PlayerCommandResult {
    PlayerCommandResult {
        command_id: command_id.to_owned(),
        accepted: false,
        error: Some(ani_contracts::PlayerError {
            code: ani_contracts::PlayerErrorCode::ResourceUnavailable,
            message,
            recoverable: true,
            recovery_actions: vec![
                ani_contracts::PlayerRecoveryAction::Retry,
                ani_contracts::PlayerRecoveryAction::Close,
            ],
        }),
    }
}

impl AppPlayerState {
    /// 为下载文件创建不泄漏真实路径的短期播放会话。
    pub(crate) fn create_session(
        &self,
        input: DesktopPlaybackSessionInput,
    ) -> Result<PlaybackSession, String> {
        validate_player_target(&input)?;
        self.prune_expired_sessions()?;
        let storage = self
            .storage
            .lock()
            .map_err(|error| format!("读取播放器数据失败：{error}"))?;
        let repository = storage.repository();
        let task = repository
            .list_downloads()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|task| task.id == input.task_id)
            .ok_or_else(|| "播放任务不存在或已被删除".to_owned())?;
        let file = select_playable_file(&task, input.file_index)?;
        let file_index =
            u32::try_from(file.index).map_err(|_| "播放文件索引超出支持范围".to_owned())?;
        let file_name = file.name.clone();
        let media_path = resolve_task_file_path(&task, file)?;
        let media_files = repository
            .list_media_files()
            .map_err(|error| error.to_string())?;
        let duration_seconds = media_files
            .iter()
            .find(|media| {
                media.download_task_id.as_deref() == Some(task.id.as_str())
                    && same_path(Path::new(&media.file_path), &media_path)
            })
            .and_then(|media| media.duration_seconds)
            .map(|value| value as f64);
        let checkpoint = repository
            .get_playback_checkpoint(&task.id, Some(i64::from(file_index)))
            .map_err(|error| error.to_string())?;
        drop(storage);

        let session_id = self.next_session_id();
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs((SESSION_TTL_HOURS * 60 * 60) as u64))
            .ok_or_else(|| "计算播放器会话有效期失败".to_owned())?;
        let (subtitles, subtitle_paths) = discover_sidecar_subtitles(&session_id, &media_path);
        let public = PlaybackSession {
            id: session_id.clone(),
            task_id: task.id.clone(),
            file_index: Some(file_index),
            file_name: media_path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or(file_name),
            mode: PlayerMediaMode::Direct,
            stream_url: format!("ani-player://session/{session_id}/media"),
            expires_at: (Utc::now() + ChronoDuration::hours(SESSION_TTL_HOURS))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            duration_seconds,
            start_position_seconds: checkpoint
                .filter(|value| !value.completed)
                .map(|value| value.position_seconds),
            subtitles,
        };
        self.sessions
            .lock()
            .map_err(|error| format!("保存播放器会话失败：{error}"))?
            .insert(
                session_id,
                ResolvedPlaybackSession {
                    public: public.clone(),
                    media_path,
                    subtitle_paths,
                    expires_at,
                },
            );
        log::info!(
            "Tauri 受控播放会话已创建 session_id={} task_id={} file_index={}",
            public.id,
            public.task_id,
            file_index
        );
        Ok(public)
    }

    /// 删除指定受控会话及真实路径映射。
    pub(crate) fn close_session(&self, session_id: &str) -> Result<(), String> {
        validate_identifier(session_id, false)?;
        self.sessions
            .lock()
            .map_err(|error| format!("关闭播放器会话失败：{error}"))?
            .remove(session_id);
        Ok(())
    }

    /// 返回当前平台播放器能力；窗口未打开时给出明确不可用状态。
    pub(crate) async fn capabilities(&self) -> PlayerCapabilities {
        let started_at = Instant::now();
        let mut waiting_logged = false;
        loop {
            if let Some(service) = self.service.read().await.clone() {
                return service.capabilities().await.unwrap_or_else(|error| {
                    unavailable_capabilities(format!("读取 MPV 能力失败：{error}"))
                });
            }
            #[cfg(desktop)]
            if self
                .app
                .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL)
                .is_none()
            {
                return unavailable_capabilities("播放器窗口尚未打开".to_owned());
            }
            if started_at.elapsed() >= PLAYER_SERVICE_READY_TIMEOUT {
                return unavailable_capabilities("MPV 初始化超时".to_owned());
            }
            if !waiting_logged {
                log::info!("播放器控制层正在等待 MPV 服务初始化");
                waiting_logged = true;
            }
            tokio::time::sleep(PLAYER_SERVICE_READY_POLL_INTERVAL).await;
        }
    }

    /// 返回当前播放器完整快照，用于补偿控制页订阅建立前丢失的事件。
    pub(crate) async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, String> {
        let service = self.service.read().await.clone();
        match service {
            Some(service) => service.snapshot().await.map_err(|error| error.to_string()),
            None => Ok(None),
        }
    }

    /// 解析受控 URI 后将命令交给统一播放器服务。
    pub(crate) async fn dispatch(&self, mut command: PlayerCommand) -> PlayerCommandResult {
        if let PlayerCommandAction::Load { source, .. } = &mut command.action {
            if let Err(error) = self.resolve_source(&command.session_id, source) {
                return rejected_command(&command.command_id, error);
            }
        }
        let service = self.service.read().await.clone();
        match service {
            Some(service) => service.dispatch(command).await,
            None => rejected_command(&command.command_id, "播放器窗口尚未打开".to_owned()),
        }
    }

    /// 同步控制层移动与缩放，并在窗口销毁后回收播放器。
    pub(crate) fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
        #[cfg(mobile)]
        {
            let _ = (window, event);
            return;
        }
        #[cfg(desktop)]
        {
            if window.label() == PLAYER_VIDEO_WINDOW_LABEL {
                let controls = window
                    .app_handle()
                    .get_webview_window(PLAYER_CONTROL_WINDOW_LABEL);
                match video_window_event_action(event) {
                    VideoWindowEventAction::RestoreControlFocus => {
                        if let Some(controls) = controls {
                            #[cfg(target_os = "macos")]
                            if window
                                .app_handle()
                                .try_state::<AppPlayerState>()
                                .is_none_or(|state| !state.fullscreen.load(Ordering::Acquire))
                            {
                                if let Err(error) =
                                    ensure_macos_player_window_layering(window, &controls)
                                {
                                    log::warn!("恢复 macOS 播放器控制层焦点失败 error={error}");
                                }
                            }
                            #[cfg(not(target_os = "macos"))]
                            if let Err(error) = controls.set_focus() {
                                log::warn!("恢复 MPV 控制层焦点失败 error={error}");
                            }
                        }
                    }
                    VideoWindowEventAction::SyncControlBounds => {
                        // 视频父窗不接受手动缩放；全屏、最大化和初始布局都在同一处同步，
                        // 因此这里不再反向调整控制层，避免两个窗口形成 resize 反馈链。
                    }
                    VideoWindowEventAction::CloseControlWindow => {
                        if let Some(controls) = controls {
                            if let Err(error) = controls.close() {
                                log::warn!("视频宿主销毁后关闭播放器控制层失败 error={error}");
                            }
                        }
                    }
                    VideoWindowEventAction::None => {}
                }
                return;
            }
            if window.label() != PLAYER_CONTROL_WINDOW_LABEL {
                return;
            }
            match event {
                WindowEvent::Moved(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. } => {
                    if window
                        .app_handle()
                        .get_window(PLAYER_VIDEO_WINDOW_LABEL)
                        .is_some()
                    {
                        if let Some(state) = window.app_handle().try_state::<AppPlayerState>() {
                            if !state.fullscreen.load(Ordering::Acquire) {
                                state.schedule_bounds_sync(PlayerBoundsSyncSide::Controls);
                            }
                        }
                    }
                }
                WindowEvent::Destroyed => {
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(state) = app.try_state::<AppPlayerState>() {
                            state.poll_generation.fetch_add(1, Ordering::SeqCst);
                            #[cfg(target_os = "macos")]
                            {
                                state.window_maximized.store(false, Ordering::Release);
                                if let Ok(mut restore_frame) = state.window_restore_frame.lock() {
                                    *restore_frame = None;
                                }
                            }
                            if let Some(service) = state.service.write().await.take() {
                                if let Err(error) = service.shutdown().await {
                                    log::warn!("播放器窗口销毁后释放 MPV 失败 error={error}");
                                }
                            }
                        }
                        if let Some(video) = app.get_window(PLAYER_VIDEO_WINDOW_LABEL) {
                            let _ = video.close();
                        }
                    });
                }
                _ => {}
            }
        }
    }

    /// 应用退出时关闭播放器和全部受控会话。
    pub(crate) async fn shutdown(&self) {
        if let Err(error) = self.close_desktop_window().await {
            log::warn!("Tauri 退出时关闭播放器失败 error={error}");
        }
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    fn resolve_source(
        &self,
        session_id: &str,
        source: &mut ani_contracts::PlayerMediaSource,
    ) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|error| format!("读取播放器会话失败：{error}"))?;
        if sessions
            .get(session_id)
            .is_some_and(|session| session.expires_at <= SystemTime::now())
        {
            sessions.remove(session_id);
            return Err("播放会话不存在或已过期".to_owned());
        }
        let session = sessions
            .get(session_id)
            .ok_or_else(|| "播放会话不存在或已过期".to_owned())?;
        if session.public.task_id != source.task_id
            || session.public.file_index != source.file_index
            || source.uri != session.public.stream_url
        {
            return Err("播放媒体与受控会话不匹配".to_owned());
        }
        source.uri = crate::path_utils::simplify(&session.media_path)
            .to_string_lossy()
            .into_owned();
        for subtitle in &mut source.subtitles {
            let path = session
                .subtitle_paths
                .get(&subtitle.id)
                .ok_or_else(|| "外挂字幕不属于当前播放会话".to_owned())?;
            subtitle.uri = crate::path_utils::simplify(path)
                .to_string_lossy()
                .into_owned();
        }
        Ok(())
    }

    /// 清理超过有效期的路径映射，避免关闭异常时长期保留本地资源引用。
    fn prune_expired_sessions(&self) -> Result<(), String> {
        let now = SystemTime::now();
        self.sessions
            .lock()
            .map_err(|error| format!("清理播放器会话失败：{error}"))?
            .retain(|_, session| session.expires_at > now);
        Ok(())
    }

    fn start_snapshot_polling(&self) {
        let generation = self.poll_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if state.poll_generation.load(Ordering::SeqCst) != generation {
                    break;
                }
                let Some(service) = state.service.read().await.clone() else {
                    break;
                };
                match service.snapshot().await {
                    Ok(Some(snapshot)) => {
                        if let Err(error) = state.app.emit(PLAYER_SNAPSHOT_EVENT, snapshot) {
                            log::warn!("发布 MPV 播放快照失败 error={error}");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => log::warn!("读取 MPV 播放快照失败 error={error}"),
                }
            }
        });
    }

    fn next_session_id(&self) -> String {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = self.id_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        format!("tauri-{epoch}-{sequence}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 播放器路由只携带经过验证的任务和文件索引。
    #[test]
    fn builds_player_route() {
        let route = player_route(&DesktopPlayerWindowInput {
            task_id: "download-1".to_owned(),
            file_index: Some(3),
        });

        assert_eq!(
            route,
            "index.html?aniView=desktop-player&taskId=download-1&fileIndex=3"
        );
    }

    /// 播放器只接受已选择、已完成的视频文件。
    #[test]
    fn selects_completed_video_file() {
        let task = DownloadTask {
            id: "download-1".to_owned(),
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
            name: "测试任务".to_owned(),
            status: ani_domain::DownloadStatus::Completed,
            progress: 1.0,
            download_speed: 0,
            upload_speed: 0,
            eta_seconds: Some(0),
            save_path: "C:/downloads".to_owned(),
            files: vec![TorrentFile {
                id: "file-1".to_owned(),
                index: 1,
                name: "episode.mkv".to_owned(),
                episode_id: None,
                episode_no: None,
                size: 4,
                progress: 1.0,
                priority: 1,
                selected: true,
            }],
            created_at: "2026-07-25T00:00:00.000Z".to_owned(),
            completed_at: Some("2026-07-25T00:10:00.000Z".to_owned()),
        };

        assert_eq!(
            select_playable_file(&task, Some(1))
                .expect("select media")
                .index,
            1
        );
        assert!(select_playable_file(&task, Some(2)).is_err());
    }

    /// macOS 拖动使用逻辑坐标差值，Retina 缩放下不会重复放大位移。
    #[test]
    fn calculates_logical_macos_drag_position() {
        let state = DesktopWindowDragState {
            pointer_start_x: 320.0,
            pointer_start_y: 180.0,
            window_start_x: 100.0,
            window_start_y: 80.0,
        };

        assert_eq!(next_drag_position(state, 410.0, 235.0), (190.0, 135.0));
        assert!(valid_screen_point(410.0, 235.0));
        assert!(!valid_screen_point(f64::NAN, 235.0));
    }

    /// macOS 双窗最大化需保存原边界，并在还原后清空临时状态。
    #[test]
    fn resolves_macos_maximize_and_restore_frames() {
        let original = MacOSWindowFrame {
            x: 120.0,
            y: 80.0,
            width: 1120.0,
            height: 630.0,
        };
        let visible = MacOSWindowFrame {
            x: 0.0,
            y: 25.0,
            width: 1728.0,
            height: 1080.0,
        };
        let mut restore_frame = None;

        assert_eq!(
            resolve_macos_window_mode_frame(true, original, Some(visible), &mut restore_frame)
                .expect("maximize frame"),
            visible
        );
        assert_eq!(restore_frame, Some(original));
        assert_eq!(
            resolve_macos_window_mode_frame(false, visible, None, &mut restore_frame)
                .expect("restore frame"),
            original
        );
        assert_eq!(restore_frame, None);
    }

    /// 视频窗内容尺寸需扣除自身边框，保证双窗口物理外框完全重合。
    #[cfg(desktop)]
    #[test]
    fn compensates_video_window_frame_size() {
        assert_eq!(
            inner_size_for_outer_bounds(
                PhysicalSize::new(1920, 1080),
                PhysicalSize::new(1136, 646),
                PhysicalSize::new(1120, 630),
            ),
            PhysicalSize::new(1904, 1064)
        );
        assert_eq!(
            inner_size_for_outer_bounds(
                PhysicalSize::new(8, 8),
                PhysicalSize::new(32, 32),
                PhysicalSize::new(8, 8),
            ),
            PhysicalSize::new(1, 1)
        );
    }

    #[cfg(desktop)]
    #[test]
    fn maps_video_window_events_to_cross_platform_pair_actions() {
        assert_eq!(
            video_window_event_action(&WindowEvent::Focused(true)),
            VideoWindowEventAction::RestoreControlFocus
        );
        assert_eq!(
            video_window_event_action(&WindowEvent::Moved(PhysicalPosition::new(10, 20))),
            VideoWindowEventAction::SyncControlBounds
        );
        assert_eq!(
            video_window_event_action(&WindowEvent::Resized(PhysicalSize::new(1280, 720))),
            VideoWindowEventAction::SyncControlBounds
        );
        assert_eq!(
            video_window_event_action(&WindowEvent::Destroyed),
            VideoWindowEventAction::CloseControlWindow
        );
        assert_eq!(
            video_window_event_action(&WindowEvent::Focused(false)),
            VideoWindowEventAction::None
        );
    }
}
