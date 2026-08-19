use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use ani_contracts::{
    PlayerAspectRatio, PlayerAvailability, PlayerBackend, PlayerCapabilities, PlayerCommand,
    PlayerCommandAction, PlayerCommandResult, PlayerError, PlayerErrorCode,
    PlayerFrameInterpolation, PlayerHdrCapabilities, PlayerHdrMode, PlayerHostPlatform,
    PlayerMediaSource, PlayerRecoveryAction, PlayerSnapshot, PlayerStatus, PlayerTrack,
    PlayerTrackKind, PlayerVideoEnhancement,
};
use ani_media::player::{unsupported, PlayerTransport, PlayerTransportError};
use async_trait::async_trait;
use libloading::Library;

use crate::desktop::{DesktopVideoTarget, DesktopWindowController};
use crate::enhancement::EnhancementRegistry;

#[cfg(target_os = "macos")]
#[path = "macos_render.rs"]
mod macos_render;

const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const DROP_SCORE_THRESHOLD: u32 = 8;
#[cfg(target_os = "macos")]
const MACOS_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);

type MpvHandle = c_void;
type MpvCreate = unsafe extern "C" fn() -> *mut MpvHandle;
type MpvInitialize = unsafe extern "C" fn(*mut MpvHandle) -> c_int;
type MpvTerminateDestroy = unsafe extern "C" fn(*mut MpvHandle);
type MpvClientApiVersion = unsafe extern "C" fn() -> u64;
type MpvSetOptionString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type MpvSetPropertyString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type MpvGetPropertyString = unsafe extern "C" fn(*mut MpvHandle, *const c_char) -> *mut c_char;
type MpvCommand = unsafe extern "C" fn(*mut MpvHandle, *const *const c_char) -> c_int;
type MpvWaitEvent = unsafe extern "C" fn(*mut MpvHandle, f64) -> *mut MpvEvent;
type MpvFree = unsafe extern "C" fn(*mut c_void);
type MpvErrorString = unsafe extern "C" fn(c_int) -> *const c_char;

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

const MPV_EVENT_NONE: c_int = 0;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_FILE_LOADED: c_int = 8;
const MPV_END_FILE_REASON_ERROR: c_int = 4;

enum MpvLoadEvent {
    Loaded,
    Failed(String),
}

/// 动态加载的稳定 libmpv client API；不向其他 crate 暴露 FFI 类型。
struct MpvApi {
    _library: Library,
    create: MpvCreate,
    initialize: MpvInitialize,
    terminate_destroy: MpvTerminateDestroy,
    client_api_version: MpvClientApiVersion,
    set_option_string: MpvSetOptionString,
    set_property_string: MpvSetPropertyString,
    get_property_string: MpvGetPropertyString,
    command: MpvCommand,
    wait_event: MpvWaitEvent,
    free: MpvFree,
    error_string: MpvErrorString,
    #[cfg(target_os = "macos")]
    render_context_create: macos_render::MpvRenderContextCreate,
    #[cfg(target_os = "macos")]
    render_context_set_update_callback: macos_render::MpvRenderContextSetUpdateCallback,
    #[cfg(target_os = "macos")]
    render_context_update: macos_render::MpvRenderContextUpdate,
    #[cfg(target_os = "macos")]
    render_context_render: macos_render::MpvRenderContextRender,
    #[cfg(target_os = "macos")]
    render_context_report_swap: macos_render::MpvRenderContextReportSwap,
    #[cfg(target_os = "macos")]
    render_context_free: macos_render::MpvRenderContextFree,
}

impl MpvApi {
    fn load(path: &Path) -> Result<Self, PlayerTransportError> {
        let library = load_mpv_library(path)?;
        unsafe {
            Ok(Self {
                create: load_mpv_symbol(&library, b"mpv_create\0")?,
                initialize: load_mpv_symbol(&library, b"mpv_initialize\0")?,
                terminate_destroy: load_mpv_symbol(&library, b"mpv_terminate_destroy\0")?,
                client_api_version: load_mpv_symbol(&library, b"mpv_client_api_version\0")?,
                set_option_string: load_mpv_symbol(&library, b"mpv_set_option_string\0")?,
                set_property_string: load_mpv_symbol(&library, b"mpv_set_property_string\0")?,
                get_property_string: load_mpv_symbol(&library, b"mpv_get_property_string\0")?,
                command: load_mpv_symbol(&library, b"mpv_command\0")?,
                wait_event: load_mpv_symbol(&library, b"mpv_wait_event\0")?,
                free: load_mpv_symbol(&library, b"mpv_free\0")?,
                error_string: load_mpv_symbol(&library, b"mpv_error_string\0")?,
                #[cfg(target_os = "macos")]
                render_context_create: load_mpv_symbol(&library, b"mpv_render_context_create\0")?,
                #[cfg(target_os = "macos")]
                render_context_set_update_callback: load_mpv_symbol(
                    &library,
                    b"mpv_render_context_set_update_callback\0",
                )?,
                #[cfg(target_os = "macos")]
                render_context_update: load_mpv_symbol(&library, b"mpv_render_context_update\0")?,
                #[cfg(target_os = "macos")]
                render_context_render: load_mpv_symbol(&library, b"mpv_render_context_render\0")?,
                #[cfg(target_os = "macos")]
                render_context_report_swap: load_mpv_symbol(
                    &library,
                    b"mpv_render_context_report_swap\0",
                )?,
                #[cfg(target_os = "macos")]
                render_context_free: load_mpv_symbol(&library, b"mpv_render_context_free\0")?,
                _library: library,
            })
        }
    }

    fn error_message(&self, status: c_int) -> String {
        let value = unsafe { (self.error_string)(status) };
        if value.is_null() {
            return format!("libmpv error {status}");
        }
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(target_os = "windows")]
fn load_mpv_library(path: &Path) -> Result<Library, PlayerTransportError> {
    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
    const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
    unsafe {
        libloading::os::windows::Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map(Into::into)
    .map_err(|error| {
        PlayerTransportError::Unavailable(format!(
            "加载 libmpv 失败 path={} error={error}",
            path.display()
        ))
    })
}

#[cfg(not(target_os = "windows"))]
fn load_mpv_library(path: &Path) -> Result<Library, PlayerTransportError> {
    unsafe { Library::new(path) }.map_err(|error| {
        PlayerTransportError::Unavailable(format!(
            "加载 libmpv 失败 path={} error={error}",
            path.display()
        ))
    })
}

unsafe fn load_mpv_symbol<T: Copy>(
    library: &Library,
    name: &[u8],
) -> Result<T, PlayerTransportError> {
    let symbol = unsafe { library.get::<T>(name) }.map_err(|error| {
        PlayerTransportError::Unavailable(format!(
            "加载 libmpv 符号失败 symbol={} error={error}",
            String::from_utf8_lossy(name).trim_end_matches('\0')
        ))
    })?;
    Ok(*symbol)
}

#[derive(Clone)]
pub struct MpvRuntimeConfig {
    pub library_roots: Vec<PathBuf>,
    pub shader_roots: Vec<PathBuf>,
}

struct MpvRuntimeState {
    active_session_id: Option<String>,
    active_source: Option<PlayerMediaSource>,
    snapshot: Option<PlayerSnapshot>,
    subtitle_scale: u16,
    enhancement: PlayerVideoEnhancement,
    frame_interpolation: PlayerFrameInterpolation,
    enhancement_degraded: bool,
    last_dropped_frames: u64,
    drop_score: u32,
    sequence: u64,
    load_pending: bool,
    subtitles_configured: bool,
    #[cfg(target_os = "macos")]
    file_loaded: bool,
    #[cfg(target_os = "macos")]
    first_frame_started_at: Option<Instant>,
    #[cfg(target_os = "macos")]
    first_frame_baseline: u64,
    closed: bool,
}

struct MpvRuntime {
    api: Arc<MpvApi>,
    handle: usize,
    target: DesktopVideoTarget,
    controller: Arc<dyn DesktopWindowController>,
    capabilities: PlayerCapabilities,
    enhancements: EnhancementRegistry,
    state: Mutex<MpvRuntimeState>,
    #[cfg(target_os = "macos")]
    macos_renderer: Mutex<Option<macos_render::MacMpvRenderer>>,
}

impl MpvRuntime {
    fn new(
        target: DesktopVideoTarget,
        controller: Arc<dyn DesktopWindowController>,
        library: &Path,
        shader_roots: &[PathBuf],
    ) -> Result<Self, PlayerTransportError> {
        log::info!("正在初始化 Tauri 桌面 libmpv library={}", library.display());
        let api = Arc::new(MpvApi::load(library)?);
        let version = unsafe { (api.client_api_version)() };
        let major = version >> 16;
        if major < 2 {
            return Err(PlayerTransportError::Unavailable(format!(
                "需要 libmpv client API 2.x，当前为 {major}.{}",
                version & 0xffff
            )));
        }
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err(PlayerTransportError::Unavailable(
                "libmpv 实例创建失败".to_owned(),
            ));
        }
        let initialize = (|| {
            #[cfg(not(target_os = "macos"))]
            set_option(&api, handle, "wid", &video_target_id(target))?;
            for (name, value) in platform_options() {
                set_option(&api, handle, name, value)?;
            }
            for (name, value) in common_options() {
                set_option(&api, handle, name, value)?;
            }
            ensure_mpv_success(&api, unsafe { (api.initialize)(handle) }, "初始化 libmpv")
        })();
        if let Err(error) = initialize {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(error);
        }
        #[cfg(target_os = "macos")]
        let macos_renderer = match target {
            DesktopVideoTarget::MacOs(view) => {
                match macos_render::MacMpvRenderer::new(api.clone(), handle, view) {
                    Ok(renderer) => renderer,
                    Err(error) => {
                        unsafe { (api.terminate_destroy)(handle) };
                        return Err(error);
                    }
                }
            }
            _ => {
                unsafe { (api.terminate_destroy)(handle) };
                return Err(PlayerTransportError::Unavailable(
                    "macOS libmpv render API 缺少 NSView 宿主".to_owned(),
                ));
            }
        };
        let strategy = std::env::var("ANI_MPV_ENHANCEMENT_STRATEGY").ok();
        let enhancements = EnhancementRegistry::resolve(shader_roots, strategy.as_deref());
        log::info!(
            "Tauri 桌面 libmpv 初始化完成 client_api={}.{} enhancements={}",
            major,
            version & 0xffff,
            enhancements.describe()
        );
        Ok(Self {
            api,
            handle: handle as usize,
            target,
            controller,
            capabilities: mpv_capabilities(enhancements.available()),
            enhancements,
            state: Mutex::new(MpvRuntimeState {
                active_session_id: None,
                active_source: None,
                snapshot: None,
                subtitle_scale: 100,
                enhancement: PlayerVideoEnhancement::Off,
                frame_interpolation: PlayerFrameInterpolation::Off,
                enhancement_degraded: false,
                last_dropped_frames: 0,
                drop_score: 0,
                sequence: 0,
                load_pending: false,
                subtitles_configured: true,
                #[cfg(target_os = "macos")]
                file_loaded: false,
                #[cfg(target_os = "macos")]
                first_frame_started_at: None,
                #[cfg(target_os = "macos")]
                first_frame_baseline: 0,
                closed: false,
            }),
            #[cfg(target_os = "macos")]
            macos_renderer: Mutex::new(Some(macos_renderer)),
        })
    }

    fn handle(&self) -> *mut MpvHandle {
        self.handle as *mut MpvHandle
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, MpvRuntimeState>, PlayerTransportError> {
        self.state
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))
    }

    fn dispatch(
        &self,
        command: PlayerCommand,
    ) -> Result<PlayerCommandResult, PlayerTransportError> {
        let command_id = command.command_id.clone();
        match &command.action {
            PlayerCommandAction::SetFullscreen { fullscreen } => {
                let fullscreen = self
                    .controller
                    .set_fullscreen(*fullscreen)
                    .map_err(PlayerTransportError::Native)?;
                let mut state = self.lock_state()?;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.fullscreen = fullscreen;
                    snapshot.sequence = snapshot.sequence.saturating_add(1);
                    state.sequence = snapshot.sequence;
                }
            }
            PlayerCommandAction::Close => {
                self.shutdown()?;
                self.controller
                    .close()
                    .map_err(PlayerTransportError::Native)?;
            }
            PlayerCommandAction::SetPictureInPicture { .. } => {
                return Ok(unsupported(&command_id, "桌面 libmpv 暂不支持画中画"));
            }
            PlayerCommandAction::PreviousItem | PlayerCommandAction::NextItem => {
                return Ok(unsupported(&command_id, "播放列表切换由页面会话管理"));
            }
            PlayerCommandAction::SetFrameInterpolation {
                frame_interpolation:
                    PlayerFrameInterpolation::MotionCompensated | PlayerFrameInterpolation::RifeRealtime,
            } => {
                return Ok(unsupported(
                    &command_id,
                    "当前桌面 libmpv 仅支持刷新率平滑；运动估计和 RIFE 需要独立帧处理后端",
                ));
            }
            PlayerCommandAction::SetHdr { hdr } if *hdr != PlayerHdrMode::Off => {
                let state = self.lock_state()?;
                let available = state.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .enhancement_diagnostics
                        .hdr_capabilities
                        .available()
                });
                if !available {
                    return Ok(unsupported(
                        &command_id,
                        "当前媒体、MPV GPU 渲染器或显示输出未形成完整 HDR 链路",
                    ));
                }
            }
            _ => self.dispatch_media_command(&command)?,
        }
        Ok(accepted(command_id))
    }

    fn dispatch_media_command(&self, command: &PlayerCommand) -> Result<(), PlayerTransportError> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(PlayerTransportError::Unavailable(
                "libmpv 播放器已关闭".to_owned(),
            ));
        }
        match &command.action {
            PlayerCommandAction::Load {
                source,
                start_position_seconds,
            } => self.load_source(
                &mut state,
                &command.session_id,
                source.clone(),
                *start_position_seconds,
            )?,
            PlayerCommandAction::Play => self.set_property("pause", "no")?,
            PlayerCommandAction::Pause => self.set_property("pause", "yes")?,
            PlayerCommandAction::Seek { position_seconds } => {
                self.command(&["seek", &position_seconds.to_string(), "absolute+exact"])?;
            }
            PlayerCommandAction::SetVolume { volume } => {
                self.set_property("volume", &format!("{}", volume * 100.0))?;
                self.set_property("mute", "no")?;
            }
            PlayerCommandAction::SetMuted { muted } => {
                self.set_property("mute", yes_no(*muted))?;
            }
            PlayerCommandAction::SetRate { rate } => {
                self.set_property("speed", &rate.to_string())?;
            }
            PlayerCommandAction::SelectAudioTrack { track_id } => {
                self.set_property("aid", track_id)?;
            }
            PlayerCommandAction::SelectSubtitleTrack { track_id } => {
                self.set_property("sid", track_id.as_deref().unwrap_or("no"))?;
            }
            PlayerCommandAction::SetSubtitleScale { subtitle_scale } => {
                self.set_property(
                    "sub-scale",
                    &format!("{:.2}", f64::from(*subtitle_scale) / 100.0),
                )?;
                state.subtitle_scale = *subtitle_scale;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.subtitle_scale = *subtitle_scale;
                }
            }
            PlayerCommandAction::SetVideoEnhancement { video_enhancement } => {
                self.apply_enhancement(*video_enhancement)?;
                state.enhancement = *video_enhancement;
                state.enhancement_degraded = false;
                state.drop_score = 0;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.video_enhancement = *video_enhancement;
                    snapshot.video_enhancement_degraded = false;
                    snapshot.enhancement_diagnostics.degradation_reason = None;
                    snapshot.enhancement_diagnostics.pipeline =
                        self.enhancement_pipeline(*video_enhancement);
                }
                log::info!(
                    "Tauri 桌面 libmpv 画质增强已更新 preset={:?} session_id={}",
                    video_enhancement,
                    command.session_id
                );
            }
            PlayerCommandAction::SetFrameInterpolation {
                frame_interpolation,
            } => {
                self.apply_frame_interpolation(*frame_interpolation)?;
                state.frame_interpolation = *frame_interpolation;
                state.enhancement_degraded = false;
                state.drop_score = 0;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.frame_interpolation = *frame_interpolation;
                    snapshot.enhancement_diagnostics.degradation_reason = None;
                }
            }
            PlayerCommandAction::SetHdr { hdr } => {
                self.apply_hdr(*hdr)?;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.hdr = *hdr;
                }
            }
            PlayerCommandAction::SetAspectRatio {
                aspect_ratio,
                value,
            } => {
                self.set_aspect_ratio(aspect_ratio, value.as_deref())?;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.aspect_ratio = aspect_ratio.clone();
                }
            }
            PlayerCommandAction::Retry => {
                let source = state.active_source.clone().ok_or_else(|| {
                    PlayerTransportError::InvalidResponse("没有可重试的媒体资源".to_owned())
                })?;
                let position = self.property_f64("time-pos").unwrap_or(0.0);
                self.load_source(&mut state, &command.session_id, source, Some(position))?;
            }
            PlayerCommandAction::SetFullscreen { .. }
            | PlayerCommandAction::Close
            | PlayerCommandAction::SetPictureInPicture { .. }
            | PlayerCommandAction::PreviousItem
            | PlayerCommandAction::NextItem => {}
        }
        self.refresh_snapshot_locked(&mut state)?;
        Ok(())
    }

    fn load_source(
        &self,
        state: &mut MpvRuntimeState,
        session_id: &str,
        source: PlayerMediaSource,
        start_position_seconds: Option<f64>,
    ) -> Result<(), PlayerTransportError> {
        #[cfg(target_os = "macos")]
        self.begin_macos_first_frame_check(state)?;
        let source_value = local_path_string(&source.uri);
        let mut args = vec!["loadfile".to_owned(), source_value, "replace".to_owned()];
        if let Some(position) = start_position_seconds {
            args.push("-1".to_owned());
            args.push(format!("start={}", position.max(0.0)));
        }
        self.command_owned(&args)?;
        self.set_property(
            "sub-scale",
            &format!("{:.2}", f64::from(state.subtitle_scale) / 100.0),
        )?;
        self.apply_enhancement(state.enhancement)?;
        self.apply_frame_interpolation(state.frame_interpolation)?;
        state.sequence = next_media_sequence(
            state.active_session_id.as_deref(),
            state.snapshot.is_some(),
            session_id,
            state.sequence,
        );
        state.active_session_id = Some(session_id.to_owned());
        state.active_source = Some(source.clone());
        state.last_dropped_frames = 0;
        state.drop_score = 0;
        state.enhancement_degraded = false;
        state.load_pending = true;
        state.subtitles_configured = false;
        state.snapshot = Some(initial_snapshot(
            session_id,
            source,
            self.capabilities.clone(),
            SnapshotInit {
                sequence: state.sequence,
                subtitle_scale: state.subtitle_scale,
                video_enhancement: state.enhancement,
                frame_interpolation: state.frame_interpolation,
                pipeline: self.enhancement_pipeline(state.enhancement),
            },
        ));
        log::info!("Tauri 桌面 libmpv 已加载媒体 session_id={session_id}");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn begin_macos_first_frame_check(
        &self,
        state: &mut MpvRuntimeState,
    ) -> Result<(), PlayerTransportError> {
        let renderer = self
            .macos_renderer
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))?;
        let renderer = renderer.as_ref().ok_or_else(|| {
            PlayerTransportError::Unavailable("macOS libmpv render API 已释放".to_owned())
        })?;
        state.first_frame_baseline = renderer.begin_media_load()?;
        state.first_frame_started_at = Some(Instant::now());
        state.file_loaded = false;
        Ok(())
    }

    fn set_aspect_ratio(
        &self,
        aspect_ratio: &PlayerAspectRatio,
        custom: Option<&str>,
    ) -> Result<(), PlayerTransportError> {
        let ratio = match aspect_ratio {
            PlayerAspectRatio::Default | PlayerAspectRatio::Fit | PlayerAspectRatio::Fill => "-1",
            PlayerAspectRatio::Ratio16x9 => "16:9",
            PlayerAspectRatio::Ratio4x3 => "4:3",
            PlayerAspectRatio::Custom => custom.unwrap_or("-1"),
        };
        self.set_property("video-aspect-override", ratio)?;
        self.set_property(
            "panscan",
            if *aspect_ratio == PlayerAspectRatio::Fill {
                "1.0"
            } else {
                "0.0"
            },
        )
    }

    fn apply_enhancement(
        &self,
        enhancement: PlayerVideoEnhancement,
    ) -> Result<(), PlayerTransportError> {
        self.command(&["change-list", "glsl-shaders", "clr", ""])?;
        for shader in self.enhancements.shaders_for(enhancement)? {
            self.command(&[
                "change-list",
                "glsl-shaders",
                "append",
                &shader.to_string_lossy(),
            ])?;
        }
        Ok(())
    }

    /// 返回当前策略与平台渲染器组合后的诊断名称。
    fn enhancement_pipeline(&self, enhancement: PlayerVideoEnhancement) -> String {
        enhancement_pipeline(
            &self.capabilities,
            enhancement,
            self.enhancements.pipeline_name(),
        )
    }

    /// 使用 libmpv 的显示刷新率重采样平滑播放，不冒充模型运动补帧。
    fn apply_frame_interpolation(
        &self,
        interpolation: PlayerFrameInterpolation,
    ) -> Result<(), PlayerTransportError> {
        match interpolation {
            PlayerFrameInterpolation::Off => {
                self.set_property("interpolation", "no")?;
                self.set_property("video-sync", "audio")
            }
            PlayerFrameInterpolation::DisplayResample => {
                self.set_property("video-sync", "display-resample")?;
                self.set_property("interpolation", "yes")?;
                self.set_property("tscale", "oversample")
            }
            PlayerFrameInterpolation::MotionCompensated
            | PlayerFrameInterpolation::RifeRealtime => Err(PlayerTransportError::Unavailable(
                "当前 libmpv 运行时不提供该插帧后端".to_owned(),
            )),
        }
    }

    /// 仅在已探测的 HDR 链路上启用 gpu-next swapchain 色彩提示。
    fn apply_hdr(&self, hdr: PlayerHdrMode) -> Result<(), PlayerTransportError> {
        match hdr {
            PlayerHdrMode::Off => {
                self.set_property("target-colorspace-hint-mode", "target")?;
                self.set_property("target-colorspace-hint", "auto")
            }
            PlayerHdrMode::Auto => {
                self.set_property("target-colorspace-hint-mode", "source")?;
                self.set_property("target-colorspace-hint", "auto")
            }
        }
    }

    fn refresh_snapshot_locked(
        &self,
        state: &mut MpvRuntimeState,
    ) -> Result<(), PlayerTransportError> {
        let Some(mut snapshot) = state.snapshot.clone() else {
            return Ok(());
        };
        let load_event = self.poll_events();
        if state.load_pending {
            match load_event {
                Some(MpvLoadEvent::Loaded) => {
                    if !state.subtitles_configured {
                        let source = state.active_source.clone().ok_or_else(|| {
                            PlayerTransportError::InvalidResponse(
                                "libmpv 文件加载完成但媒体源已丢失".to_owned(),
                            )
                        })?;
                        if let Err(error) = self.configure_subtitles_after_load(&source) {
                            state.load_pending = false;
                            snapshot.status = PlayerStatus::Error;
                            snapshot.error = Some(decoder_error(error.to_string()));
                            state.snapshot = Some(snapshot);
                            return Err(error);
                        }
                        state.subtitles_configured = true;
                    }
                    #[cfg(target_os = "macos")]
                    {
                        state.file_loaded = true;
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        state.load_pending = false;
                    }
                }
                Some(MpvLoadEvent::Failed(error)) => {
                    state.load_pending = false;
                    snapshot.status = PlayerStatus::Error;
                    snapshot.error = Some(decoder_error(error.clone()));
                    state.snapshot = Some(snapshot);
                    return Err(PlayerTransportError::LoadFailed(error));
                }
                None => {}
            }
        }
        #[cfg(target_os = "macos")]
        if state.load_pending {
            let renderer = self
                .macos_renderer
                .lock()
                .map_err(|error| PlayerTransportError::Native(error.to_string()))?;
            let renderer = renderer.as_ref().ok_or_else(|| {
                PlayerTransportError::Unavailable("macOS libmpv render API 已释放".to_owned())
            })?;
            if let Some(error) = renderer.last_error()? {
                state.load_pending = false;
                snapshot.status = PlayerStatus::Error;
                snapshot.error = Some(decoder_error(error.clone()));
                state.snapshot = Some(snapshot);
                return Err(PlayerTransportError::LoadFailed(error));
            }
            if state.file_loaded && renderer.rendered_frames()? > state.first_frame_baseline {
                state.load_pending = false;
                state.first_frame_started_at = None;
                log::info!("macOS libmpv 首帧已交换到原生 OpenGL 表面");
            } else if state
                .first_frame_started_at
                .is_some_and(|started| started.elapsed() >= MACOS_FIRST_FRAME_TIMEOUT)
            {
                let error = format!(
                    "macOS libmpv 在 8 秒内未输出首帧：{}",
                    renderer.diagnostics()?
                );
                state.load_pending = false;
                snapshot.status = PlayerStatus::Error;
                snapshot.error = Some(decoder_error(error.clone()));
                state.snapshot = Some(snapshot);
                return Err(PlayerTransportError::LoadFailed(error));
            }
        }
        let position = self
            .property_f64("time-pos")
            .unwrap_or(snapshot.position_seconds);
        let duration = self
            .property_f64("duration")
            .unwrap_or(snapshot.duration_seconds);
        let paused = self.property_bool("pause").unwrap_or(false);
        let buffering = self.property_bool("paused-for-cache").unwrap_or(false);
        let idle = self.property_bool("idle-active").unwrap_or(false);
        let ended = self.property_bool("eof-reached").unwrap_or(false);
        snapshot.status = if state.load_pending {
            PlayerStatus::Loading
        } else if ended {
            PlayerStatus::Ended
        } else if buffering {
            PlayerStatus::Buffering
        } else if paused {
            PlayerStatus::Paused
        } else if idle && position <= 0.0 {
            PlayerStatus::Loading
        } else {
            PlayerStatus::Playing
        };
        snapshot.position_seconds = position.max(0.0);
        snapshot.duration_seconds = duration.max(0.0);
        snapshot.buffered_seconds = buffered_seconds(
            snapshot.position_seconds,
            self.property_f64("demuxer-cache-duration").unwrap_or(0.0),
            snapshot.duration_seconds,
        );
        snapshot.volume = (self.property_f64("volume").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0);
        snapshot.muted = self.property_bool("mute").unwrap_or(false);
        snapshot.playback_rate = self.property_f64("speed").unwrap_or(1.0);
        snapshot.audio_tracks = self.read_tracks(PlayerTrackKind::Audio);
        snapshot.subtitle_tracks = self.read_tracks(PlayerTrackKind::Subtitle);
        snapshot.enhancement_diagnostics.renderer = self
            .property_string("current-vo")
            .or_else(|| Some(platform_renderer_name().to_owned()));
        snapshot.enhancement_diagnostics.decoder = self
            .property_string("hwdec-current")
            .filter(|decoder| decoder != "no")
            .or_else(|| self.property_string("video-codec"))
            .or_else(|| Some(platform_decoder_name().to_owned()));
        let hdr_capabilities = self.read_hdr_capabilities();
        snapshot.enhancement_diagnostics.hdr_capabilities = hdr_capabilities;
        snapshot.capabilities.supports_hdr = hdr_capabilities.available();
        if snapshot.hdr != PlayerHdrMode::Off && !hdr_capabilities.available() {
            self.apply_hdr(PlayerHdrMode::Off)?;
            snapshot.hdr = PlayerHdrMode::Off;
            snapshot.enhancement_diagnostics.degradation_reason =
                Some("HDR 输出链路已变化，自动恢复 SDR".to_owned());
        }
        snapshot.enhancement_diagnostics.dropped_frames = self
            .property_u64("frame-drop-count")
            .unwrap_or(snapshot.enhancement_diagnostics.dropped_frames);
        self.maybe_degrade_enhancement(state, &mut snapshot)?;
        state.sequence = state.sequence.saturating_add(1);
        snapshot.sequence = state.sequence;
        state.snapshot = Some(snapshot);
        Ok(())
    }

    /// 在 FILE_LOADED 后添加外挂字幕，并为内封字幕显式选择中文优先的默认轨。
    fn configure_subtitles_after_load(
        &self,
        source: &PlayerMediaSource,
    ) -> Result<(), PlayerTransportError> {
        if !source.subtitles.is_empty() {
            let selected_index = source
                .subtitles
                .iter()
                .position(|subtitle| subtitle.default)
                .unwrap_or(0);
            for (index, subtitle) in source.subtitles.iter().enumerate() {
                let flag = if index == selected_index {
                    "select"
                } else {
                    "auto"
                };
                self.command(&[
                    "sub-add",
                    &local_path_string(&subtitle.uri),
                    flag,
                    &subtitle.label,
                    subtitle.language.as_deref().unwrap_or("und"),
                ])?;
            }
            log::info!(
                "libmpv 外挂字幕已加载 count={} selected={}",
                source.subtitles.len(),
                source.subtitles[selected_index].label
            );
            return Ok(());
        }

        let tracks = self.read_subtitle_candidates();
        if let Some(track) = preferred_subtitle_track(&tracks) {
            self.set_property("sid", &track.id)?;
            log::info!(
                "libmpv 内封字幕已默认选择 id={} language={} title={}",
                track.id,
                track.language.as_deref().unwrap_or("und"),
                track.title.as_deref().unwrap_or("<untitled>")
            );
        } else {
            log::info!("libmpv 当前媒体没有可用字幕轨");
        }
        Ok(())
    }

    fn read_subtitle_candidates(&self) -> Vec<SubtitleTrackCandidate> {
        let count = self.property_u64("track-list/count").unwrap_or(0).min(128);
        (0..count)
            .filter_map(|index| {
                let prefix = format!("track-list/{index}");
                if self.property_string(&format!("{prefix}/type")).as_deref() != Some("sub") {
                    return None;
                }
                Some(SubtitleTrackCandidate {
                    id: self.property_string(&format!("{prefix}/id"))?,
                    title: self.property_string(&format!("{prefix}/title")),
                    language: self.property_string(&format!("{prefix}/lang")),
                    default: self
                        .property_bool(&format!("{prefix}/default"))
                        .unwrap_or(false),
                    forced: self
                        .property_bool(&format!("{prefix}/forced"))
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    fn read_hdr_capabilities(&self) -> PlayerHdrCapabilities {
        let source_hdr = self
            .property_string("video-params/gamma")
            .is_some_and(|gamma| is_hdr_transfer(&gamma));
        let renderer_hdr = self
            .property_string("current-vo")
            .is_some_and(|renderer| renderer == "gpu-next");
        let display_hdr = platform_hdr_output_supported()
            && self
                .property_string("video-target-params/gamma")
                .is_some_and(|gamma| is_hdr_transfer(&gamma));
        PlayerHdrCapabilities {
            source_hdr,
            renderer_hdr,
            display_hdr,
        }
    }

    /// 非阻塞消费 libmpv 事件，识别异步首帧/解码失败。
    fn poll_events(&self) -> Option<MpvLoadEvent> {
        let mut load_event = None;
        loop {
            let event = unsafe { (self.api.wait_event)(self.handle(), 0.0) };
            if event.is_null() {
                return load_event;
            }
            let event = unsafe { &*event };
            if event.event_id == MPV_EVENT_NONE {
                return load_event;
            }
            if event.event_id == MPV_EVENT_FILE_LOADED {
                load_event = Some(MpvLoadEvent::Loaded);
                continue;
            }
            if event.event_id != MPV_EVENT_END_FILE || event.data.is_null() {
                continue;
            }
            let end_file = unsafe { &*(event.data as *const MpvEventEndFile) };
            if end_file.reason == MPV_END_FILE_REASON_ERROR {
                load_event = Some(MpvLoadEvent::Failed(format!(
                    "libmpv 异步加载媒体失败：{}",
                    end_file.error
                )));
            }
        }
    }

    fn maybe_degrade_enhancement(
        &self,
        state: &mut MpvRuntimeState,
        snapshot: &mut PlayerSnapshot,
    ) -> Result<(), PlayerTransportError> {
        if state.enhancement == PlayerVideoEnhancement::Off
            && state.frame_interpolation == PlayerFrameInterpolation::Off
        {
            return Ok(());
        }
        let dropped = self
            .property_u64("frame-drop-count")
            .unwrap_or(state.last_dropped_frames);
        let delta = dropped.saturating_sub(state.last_dropped_frames);
        state.last_dropped_frames = dropped;
        state.drop_score = if delta >= 2 {
            state.drop_score.saturating_add(delta.min(4) as u32)
        } else {
            state.drop_score.saturating_sub(1)
        };
        if state.drop_score < DROP_SCORE_THRESHOLD {
            return Ok(());
        }
        if state.frame_interpolation != PlayerFrameInterpolation::Off {
            self.apply_frame_interpolation(PlayerFrameInterpolation::Off)?;
            log::warn!(
                "libmpv 检测到持续掉帧，自动关闭插帧 from={:?} dropped={dropped}",
                state.frame_interpolation
            );
            state.frame_interpolation = PlayerFrameInterpolation::Off;
            state.enhancement_degraded = true;
            state.drop_score = 0;
            snapshot.frame_interpolation = PlayerFrameInterpolation::Off;
            snapshot.video_enhancement_degraded = true;
            snapshot.enhancement_diagnostics.degradation_reason =
                Some("持续掉帧，已关闭插帧".to_owned());
            return Ok(());
        }
        let next = match state.enhancement {
            PlayerVideoEnhancement::Clear => PlayerVideoEnhancement::Balanced,
            PlayerVideoEnhancement::Balanced => PlayerVideoEnhancement::Off,
            PlayerVideoEnhancement::Off => return Ok(()),
        };
        self.apply_enhancement(next)?;
        log::warn!(
            "libmpv 检测到持续掉帧，自动降低画质增强 from={:?} to={:?} dropped={dropped}",
            state.enhancement,
            next
        );
        state.enhancement = next;
        state.enhancement_degraded = true;
        state.drop_score = 0;
        snapshot.video_enhancement = next;
        snapshot.video_enhancement_degraded = true;
        snapshot.enhancement_diagnostics.pipeline = self.enhancement_pipeline(next);
        snapshot.enhancement_diagnostics.degradation_reason = Some("持续掉帧".to_owned());
        Ok(())
    }

    fn read_tracks(&self, kind: PlayerTrackKind) -> Vec<PlayerTrack> {
        let count = self.property_u64("track-list/count").unwrap_or(0).min(128);
        (0..count)
            .filter_map(|index| {
                let prefix = format!("track-list/{index}");
                let track_type = self.property_string(&format!("{prefix}/type"))?;
                let expected = match kind {
                    PlayerTrackKind::Audio => "audio",
                    PlayerTrackKind::Subtitle => "sub",
                };
                if track_type != expected
                    || self.property_bool(&format!("{prefix}/external")) == Some(true)
                        && kind == PlayerTrackKind::Audio
                {
                    return None;
                }
                Some(PlayerTrack {
                    id: self.property_string(&format!("{prefix}/id"))?,
                    kind: kind.clone(),
                    label: self
                        .property_string(&format!("{prefix}/title"))
                        .or_else(|| self.property_string(&format!("{prefix}/lang")))
                        .unwrap_or_else(|| format!("轨道 {}", index + 1)),
                    language: self.property_string(&format!("{prefix}/lang")),
                    selected: self
                        .property_bool(&format!("{prefix}/selected"))
                        .unwrap_or(false),
                })
            })
            .collect()
    }

    fn command(&self, values: &[&str]) -> Result<(), PlayerTransportError> {
        self.command_owned(
            &values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    fn command_owned(&self, values: &[String]) -> Result<(), PlayerTransportError> {
        let values = values
            .iter()
            .map(|value| CString::new(value.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| PlayerTransportError::InvalidResponse("mpv 命令包含空字符".to_owned()))?;
        let mut pointers = values
            .iter()
            .map(|value| value.as_ptr())
            .collect::<Vec<_>>();
        pointers.push(std::ptr::null());
        ensure_mpv_success(
            &self.api,
            unsafe { (self.api.command)(self.handle(), pointers.as_ptr()) },
            "执行 libmpv 命令",
        )
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), PlayerTransportError> {
        let name = c_string(name, "mpv 属性名")?;
        let value = c_string(value, "mpv 属性值")?;
        ensure_mpv_success(
            &self.api,
            unsafe { (self.api.set_property_string)(self.handle(), name.as_ptr(), value.as_ptr()) },
            "设置 libmpv 属性",
        )
    }

    fn property_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let value = unsafe { (self.api.get_property_string)(self.handle(), name.as_ptr()) };
        if value.is_null() {
            return None;
        }
        let result = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.api.free)(value.cast()) };
        Some(result)
    }

    fn property_bool(&self, name: &str) -> Option<bool> {
        self.property_string(name)
            .and_then(|value| match value.as_str() {
                "yes" | "true" => Some(true),
                "no" | "false" => Some(false),
                _ => None,
            })
    }

    fn property_f64(&self, name: &str) -> Option<f64> {
        self.property_string(name)?.parse().ok()
    }

    fn property_u64(&self, name: &str) -> Option<u64> {
        self.property_string(name)?.parse().ok()
    }

    fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        let mut state = self.lock_state()?;
        if state.closed || state.snapshot.is_none() {
            return Ok(state.snapshot.clone());
        }
        self.refresh_snapshot_locked(&mut state)?;
        Ok(state.snapshot.clone())
    }

    fn shutdown(&self) -> Result<(), PlayerTransportError> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        #[cfg(target_os = "macos")]
        let renderer_result = self
            .macos_renderer
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))?
            .take()
            .map(macos_render::MacMpvRenderer::shutdown)
            .transpose();
        unsafe { (self.api.terminate_destroy)(self.handle()) };
        #[cfg(target_os = "macos")]
        renderer_result?;
        if let Some(snapshot) = state.snapshot.as_mut() {
            snapshot.status = PlayerStatus::Closed;
            snapshot.sequence = snapshot.sequence.saturating_add(1);
        }
        log::info!("Tauri 桌面 libmpv 已释放 target={:?}", self.target);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubtitleTrackCandidate {
    id: String,
    title: Option<String>,
    language: Option<String>,
    default: bool,
    forced: bool,
}

fn preferred_subtitle_track(tracks: &[SubtitleTrackCandidate]) -> Option<&SubtitleTrackCandidate> {
    let mut best: Option<(&SubtitleTrackCandidate, i32)> = None;
    for track in tracks {
        let score = subtitle_track_score(track);
        if best.is_none_or(|(_, current_score)| score > current_score) {
            best = Some((track, score));
        }
    }
    best.map(|(track, _)| track)
}

fn subtitle_track_score(track: &SubtitleTrackCandidate) -> i32 {
    let language = track
        .language
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    let title = track
        .title
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let simplified = matches!(language.as_str(), "zh-cn" | "zh-hans" | "chs" | "sc" | "cn")
        || ["简体", "简中", "简日", "chs", "simplified"]
            .iter()
            .any(|marker| title.contains(marker));
    let chinese = simplified
        || language == "zh"
        || language.starts_with("zh-")
        || matches!(language.as_str(), "chi" | "zho" | "cht" | "tc")
        || ["中文", "中字", "繁体", "繁中", "cht"]
            .iter()
            .any(|marker| title.contains(marker));
    let mut score = 0;
    if simplified {
        score += 400;
    } else if chinese {
        score += 300;
    }
    if track.default {
        score += 100;
    }
    if track.forced {
        score -= 200;
    }
    score
}

impl Drop for MpvRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::warn!("Tauri 桌面 libmpv Drop 释放失败 error={error}");
        }
    }
}

/// libmpv 缺失时保留可查询能力，并向桌面页面返回结构化不可用原因。
pub struct MpvPlayerTransport {
    runtime: Option<Arc<MpvRuntime>>,
    unavailable_reason: Option<String>,
}

impl MpvPlayerTransport {
    pub fn new(
        target: DesktopVideoTarget,
        controller: Arc<dyn DesktopWindowController>,
        config: MpvRuntimeConfig,
    ) -> Self {
        let resolved = resolve_mpv_library(&config.library_roots).and_then(|library| {
            MpvRuntime::new(target, controller, &library, &config.shader_roots).map(Arc::new)
        });
        match resolved {
            Ok(runtime) => Self {
                runtime: Some(runtime),
                unavailable_reason: None,
            },
            Err(error) => {
                let message = error.to_string();
                log::error!("Tauri 桌面 libmpv 不可用 error={message}");
                Self {
                    runtime: None,
                    unavailable_reason: Some(message),
                }
            }
        }
    }

    pub fn is_available(&self) -> bool {
        self.runtime.is_some()
    }
}

#[async_trait]
impl PlayerTransport for MpvPlayerTransport {
    async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError> {
        Ok(self
            .runtime
            .as_ref()
            .map(|runtime| runtime.capabilities.clone())
            .unwrap_or_else(|| unavailable_mpv_capabilities(self.unavailable_reason.as_deref())))
    }

    async fn dispatch(
        &self,
        command: PlayerCommand,
    ) -> Result<PlayerCommandResult, PlayerTransportError> {
        self.runtime
            .as_ref()
            .ok_or_else(|| {
                PlayerTransportError::Unavailable(
                    self.unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "libmpv 运行时不可用".to_owned()),
                )
            })?
            .dispatch(command)
    }

    async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        match &self.runtime {
            Some(runtime) => runtime.snapshot(),
            None => Ok(None),
        }
    }

    async fn shutdown(&self) -> Result<(), PlayerTransportError> {
        match &self.runtime {
            Some(runtime) => runtime.shutdown(),
            None => Ok(()),
        }
    }
}

fn resolve_mpv_library(roots: &[PathBuf]) -> Result<PathBuf, PlayerTransportError> {
    for root in roots {
        for library in mpv_library_candidates(root) {
            if library.is_file() {
                log::info!("已定位 Tauri 桌面 libmpv library={}", library.display());
                return Ok(library);
            }
        }
    }
    for library in system_mpv_candidates() {
        if library.is_file() || library.components().count() == 1 {
            return Ok(library);
        }
    }
    Err(PlayerTransportError::Unavailable(
        "未找到 libmpv 运行时".to_owned(),
    ))
}

fn mpv_library_candidates(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    #[cfg(target_os = "windows")]
    return ["mpv-2.dll", "libmpv-2.dll", "libmpv.dll", "mpv.dll"]
        .into_iter()
        .map(|name| root.join(name))
        .collect();
    #[cfg(target_os = "macos")]
    return [
        "libmpv.2.dylib",
        "libmpv.dylib",
        "Frameworks/libmpv.2.dylib",
    ]
    .into_iter()
    .map(|name| root.join(name))
    .collect();
    #[cfg(target_os = "linux")]
    return ["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
        .into_iter()
        .map(|name| root.join(name))
        .collect();
}

fn system_mpv_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    return Vec::new();
    #[cfg(target_os = "macos")]
    return vec![
        PathBuf::from("/Applications/IINA.app/Contents/Frameworks/libmpv.2.dylib"),
        PathBuf::from("/opt/homebrew/lib/libmpv.dylib"),
        PathBuf::from("/usr/local/lib/libmpv.dylib"),
    ];
    #[cfg(target_os = "linux")]
    {
        let multiarch = match std::env::consts::ARCH {
            "x86_64" => Some("x86_64-linux-gnu"),
            "aarch64" => Some("aarch64-linux-gnu"),
            _ => None,
        };
        let mut roots = Vec::new();
        if let Some(multiarch) = multiarch {
            roots.push(PathBuf::from("/usr/lib").join(multiarch));
            roots.push(PathBuf::from("/lib").join(multiarch));
        }
        roots.extend([PathBuf::from("/usr/lib64"), PathBuf::from("/usr/lib")]);
        roots
            .into_iter()
            .flat_map(|root| [root.join("libmpv.so.2"), root.join("libmpv.so.1")])
            .collect()
    }
}

fn platform_options() -> Vec<(&'static str, &'static str)> {
    #[cfg(target_os = "windows")]
    return vec![
        ("vo", "gpu-next"),
        ("gpu-api", "d3d11"),
        ("hwdec", "d3d11va"),
    ];
    #[cfg(target_os = "macos")]
    return vec![
        ("vo", "libmpv"),
        ("gpu-hwdec-interop", "auto"),
        ("hwdec", "videotoolbox"),
    ];
    #[cfg(target_os = "linux")]
    return vec![
        ("vo", "gpu-next"),
        ("gpu-api", "vulkan"),
        ("hwdec", "vaapi"),
    ];
}

fn common_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("terminal", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("osc", "no"),
        ("osd-level", "0"),
        ("keep-open", "yes"),
        ("sub-auto", "no"),
        ("audio-pitch-correction", "yes"),
        ("cache", "yes"),
        ("demuxer-max-bytes", "256MiB"),
    ]
}

fn set_option(
    api: &MpvApi,
    handle: *mut MpvHandle,
    name: &str,
    value: &str,
) -> Result<(), PlayerTransportError> {
    let option_name = c_string(name, "mpv 选项名")?;
    let option_value = c_string(value, "mpv 选项值")?;
    let status =
        unsafe { (api.set_option_string)(handle, option_name.as_ptr(), option_value.as_ptr()) };
    if status >= 0 {
        Ok(())
    } else {
        Err(PlayerTransportError::Native(format!(
            "配置 libmpv 失败 option={name} value={value} status={status}：{}",
            api.error_message(status)
        )))
    }
}

fn c_string(value: &str, label: &str) -> Result<CString, PlayerTransportError> {
    CString::new(value.as_bytes())
        .map_err(|_| PlayerTransportError::InvalidResponse(format!("{label}包含空字符")))
}

fn ensure_mpv_success(
    api: &MpvApi,
    status: c_int,
    operation: &str,
) -> Result<(), PlayerTransportError> {
    if status >= 0 {
        Ok(())
    } else {
        Err(PlayerTransportError::Native(format!(
            "{operation}失败：{}",
            api.error_message(status)
        )))
    }
}

#[cfg(not(target_os = "macos"))]
fn video_target_id(target: DesktopVideoTarget) -> String {
    match target {
        DesktopVideoTarget::Windows(value) => value.to_string(),
        DesktopVideoTarget::MacOs(value) => value.to_string(),
        DesktopVideoTarget::X11(value) => value.to_string(),
    }
}

fn local_path_string(value: &str) -> String {
    if value.contains("://") {
        value.to_owned()
    } else {
        dunce::simplified(Path::new(value))
            .to_string_lossy()
            .into_owned()
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn is_hdr_transfer(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "pq" | "hlg")
}

/// libmpv 当前仅明确支持 D3D11/WinVK/Wayland 的 swapchain colorspace hint。
fn platform_hdr_output_supported() -> bool {
    cfg!(target_os = "windows")
}

fn buffered_seconds(position: f64, cache_duration: f64, duration: f64) -> f64 {
    (position.max(0.0) + cache_duration.max(0.0)).min(duration.max(position))
}

fn next_media_sequence(
    active_session_id: Option<&str>,
    has_snapshot: bool,
    session_id: &str,
    current_sequence: u64,
) -> u64 {
    if has_snapshot && active_session_id == Some(session_id) {
        current_sequence.saturating_add(1)
    } else {
        1
    }
}

fn mpv_capabilities(shaders_available: bool) -> PlayerCapabilities {
    PlayerCapabilities {
        backend: PlayerBackend::Mpv,
        platform: PlayerHostPlatform::TauriDesktop,
        availability: PlayerAvailability::Available,
        can_seek: true,
        can_set_volume: true,
        can_mute: true,
        playback_rates: PLAYBACK_RATES.to_vec(),
        supports_audio_tracks: true,
        supports_subtitle_tracks: true,
        supports_subtitle_scale: true,
        supports_video_enhancement: shaders_available,
        supports_frame_interpolation: true,
        supports_model_enhancement: false,
        supports_aspect_ratio: true,
        supports_fullscreen: true,
        supports_picture_in_picture: false,
        supports_playlist_navigation: false,
        supports_direct_playback: true,
        supports_transcoding_fallback: false,
        supports_hdr: false,
        unavailable_reason: None,
    }
}

fn unavailable_mpv_capabilities(reason: Option<&str>) -> PlayerCapabilities {
    PlayerCapabilities {
        availability: PlayerAvailability::Unavailable,
        can_seek: false,
        can_set_volume: false,
        can_mute: false,
        playback_rates: vec![1.0],
        supports_audio_tracks: false,
        supports_subtitle_tracks: false,
        supports_subtitle_scale: false,
        supports_video_enhancement: false,
        supports_aspect_ratio: false,
        supports_fullscreen: false,
        supports_picture_in_picture: false,
        supports_playlist_navigation: false,
        supports_direct_playback: false,
        supports_transcoding_fallback: false,
        supports_hdr: false,
        unavailable_reason: Some(reason.unwrap_or("libmpv 运行时不可用").to_owned()),
        ..mpv_capabilities(false)
    }
}

struct SnapshotInit {
    sequence: u64,
    subtitle_scale: u16,
    video_enhancement: PlayerVideoEnhancement,
    frame_interpolation: PlayerFrameInterpolation,
    pipeline: String,
}

fn initial_snapshot(
    session_id: &str,
    source: PlayerMediaSource,
    capabilities: PlayerCapabilities,
    init: SnapshotInit,
) -> PlayerSnapshot {
    PlayerSnapshot {
        session_id: session_id.to_owned(),
        sequence: init.sequence,
        backend: PlayerBackend::Mpv,
        platform: PlayerHostPlatform::TauriDesktop,
        status: PlayerStatus::Loading,
        capabilities,
        duration_seconds: source.duration_seconds.unwrap_or(0.0),
        source: Some(source),
        playlist: ani_contracts::PlayerPlaylist {
            items: Vec::new(),
            active_item_id: None,
        },
        position_seconds: 0.0,
        buffered_seconds: 0.0,
        volume: 1.0,
        muted: false,
        playback_rate: 1.0,
        audio_tracks: Vec::new(),
        subtitle_tracks: Vec::new(),
        subtitle_scale: init.subtitle_scale,
        video_enhancement: init.video_enhancement,
        video_enhancement_degraded: false,
        frame_interpolation: init.frame_interpolation,
        hdr: ani_contracts::PlayerHdrMode::Off,
        enhancement_diagnostics: ani_contracts::PlayerEnhancementDiagnostics {
            pipeline: init.pipeline,
            renderer: Some(platform_renderer_name().to_owned()),
            decoder: Some(platform_decoder_name().to_owned()),
            dropped_frames: 0,
            ..Default::default()
        },
        aspect_ratio: PlayerAspectRatio::Default,
        fullscreen: false,
        picture_in_picture: false,
        error: None,
    }
}

/// 返回当前实际生效的桌面增强链路名称。
fn enhancement_pipeline(
    capabilities: &PlayerCapabilities,
    enhancement: PlayerVideoEnhancement,
    strategy_name: &str,
) -> String {
    if capabilities.supports_video_enhancement && enhancement != PlayerVideoEnhancement::Off {
        format!("{}-{strategy_name}", platform_renderer_pipeline())
    } else {
        platform_renderer_pipeline().to_owned()
    }
}

fn platform_renderer_name() -> &'static str {
    #[cfg(target_os = "macos")]
    return "libmpv-opengl-cgl";
    #[cfg(not(target_os = "macos"))]
    return "gpu-next";
}

fn platform_renderer_pipeline() -> &'static str {
    #[cfg(target_os = "macos")]
    return "libmpv-render-api-opengl";
    #[cfg(not(target_os = "macos"))]
    return "libmpv-gpu-next";
}

fn platform_decoder_name() -> &'static str {
    #[cfg(target_os = "windows")]
    return "d3d11va";
    #[cfg(target_os = "macos")]
    return "videotoolbox";
    #[cfg(target_os = "linux")]
    return "vaapi";
    #[allow(unreachable_code)]
    "hardware-auto"
}

fn accepted(command_id: String) -> PlayerCommandResult {
    PlayerCommandResult {
        command_id,
        accepted: true,
        error: None,
    }
}

#[allow(dead_code)]
fn decoder_error(message: impl Into<String>) -> PlayerError {
    PlayerError {
        code: PlayerErrorCode::Decoder,
        message: message.into(),
        recoverable: true,
        recovery_actions: vec![PlayerRecoveryAction::Retry, PlayerRecoveryAction::Close],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_vendor_neutral_windows_gpu_path() {
        #[cfg(target_os = "windows")]
        assert_eq!(
            platform_options(),
            vec![
                ("vo", "gpu-next"),
                ("gpu-api", "d3d11"),
                ("hwdec", "d3d11va")
            ]
        );
    }

    #[test]
    fn configures_macos_render_api_without_global_gpu_api_option() {
        #[cfg(target_os = "macos")]
        assert_eq!(
            platform_options(),
            vec![
                ("vo", "libmpv"),
                ("gpu-hwdec-interop", "auto"),
                ("hwdec", "videotoolbox")
            ]
        );
    }

    #[test]
    fn computes_buffered_position_without_exceeding_duration() {
        assert_eq!(buffered_seconds(12.0, 8.0, 30.0), 20.0);
        assert_eq!(buffered_seconds(28.0, 8.0, 30.0), 30.0);
    }

    #[test]
    fn resolves_bundled_anime4k_presets() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let resources = EnhancementRegistry::resolve(&[root], None);
        assert!(resources.available());
        assert_eq!(
            resources
                .shaders_for(PlayerVideoEnhancement::Balanced)
                .expect("balanced preset")
                .len(),
            1
        );
        assert_eq!(
            resources
                .shaders_for(PlayerVideoEnhancement::Clear)
                .expect("clear preset")
                .len(),
            2
        );
    }

    #[test]
    fn advances_same_session_snapshot_sequence() {
        assert_eq!(next_media_sequence(Some("a"), true, "a", 4), 5);
        assert_eq!(next_media_sequence(Some("a"), true, "b", 4), 1);
    }

    #[test]
    fn keeps_hdr_disabled_until_full_output_chain_is_probed() {
        let capabilities = mpv_capabilities(true);
        assert!(!capabilities.supports_hdr);
        assert!(!capabilities.supports_model_enhancement);
        assert!(capabilities.supports_frame_interpolation);
    }

    #[test]
    fn recognizes_only_pq_and_hlg_as_hdr_transfers() {
        assert!(is_hdr_transfer("pq"));
        assert!(is_hdr_transfer("HLG"));
        assert!(!is_hdr_transfer("bt.1886"));
        assert!(!is_hdr_transfer("srgb"));
    }

    #[test]
    fn prefers_simplified_chinese_subtitles_over_default_english() {
        let tracks = vec![
            SubtitleTrackCandidate {
                id: "1".to_owned(),
                title: Some("English".to_owned()),
                language: Some("eng".to_owned()),
                default: true,
                forced: false,
            },
            SubtitleTrackCandidate {
                id: "2".to_owned(),
                title: Some("简体中文".to_owned()),
                language: Some("chi".to_owned()),
                default: false,
                forced: false,
            },
        ];

        assert_eq!(
            preferred_subtitle_track(&tracks).map(|track| track.id.as_str()),
            Some("2")
        );
    }

    #[test]
    fn avoids_forced_sign_tracks_when_a_full_subtitle_track_exists() {
        let tracks = vec![
            SubtitleTrackCandidate {
                id: "1".to_owned(),
                title: Some("简中 Signs".to_owned()),
                language: Some("zh-Hans".to_owned()),
                default: true,
                forced: true,
            },
            SubtitleTrackCandidate {
                id: "2".to_owned(),
                title: Some("简中 Full".to_owned()),
                language: Some("zh-Hans".to_owned()),
                default: false,
                forced: false,
            },
        ];

        assert_eq!(
            preferred_subtitle_track(&tracks).map(|track| track.id.as_str()),
            Some("2")
        );
    }
}
