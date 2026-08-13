use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use ani_contracts::{
    PlayerAspectRatio, PlayerAvailability, PlayerBackend, PlayerCapabilities, PlayerCommand,
    PlayerCommandAction, PlayerCommandResult, PlayerError, PlayerErrorCode, PlayerHostPlatform,
    PlayerMediaSource, PlayerRecoveryAction, PlayerSnapshot, PlayerStatus, PlayerTrack,
    PlayerTrackKind, PlayerVideoEnhancement,
};
use ani_media::player::{unsupported, PlayerTransport, PlayerTransportError};
use async_trait::async_trait;
use libloading::Library;

use crate::desktop::{DesktopVideoTarget, DesktopWindowController};

const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const DROP_SCORE_THRESHOLD: u32 = 8;

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
    enhancement_degraded: bool,
    last_dropped_frames: u64,
    drop_score: u32,
    sequence: u64,
    load_pending: bool,
    closed: bool,
}

struct MpvRuntime {
    api: Arc<MpvApi>,
    handle: usize,
    target: DesktopVideoTarget,
    controller: Arc<dyn DesktopWindowController>,
    capabilities: PlayerCapabilities,
    shaders: ShaderResources,
    state: Mutex<MpvRuntimeState>,
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
        let shaders = ShaderResources::resolve(shader_roots);
        log::info!(
            "Tauri 桌面 libmpv 初始化完成 client_api={}.{} shaders={}",
            major,
            version & 0xffff,
            shaders.describe()
        );
        Ok(Self {
            api,
            handle: handle as usize,
            target,
            controller,
            capabilities: mpv_capabilities(shaders.available()),
            shaders,
            state: Mutex::new(MpvRuntimeState {
                active_session_id: None,
                active_source: None,
                snapshot: None,
                subtitle_scale: 100,
                enhancement: PlayerVideoEnhancement::Off,
                enhancement_degraded: false,
                last_dropped_frames: 0,
                drop_score: 0,
                sequence: 0,
                load_pending: false,
                closed: false,
            }),
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
                }
                log::info!(
                    "Tauri 桌面 libmpv 画质增强已更新 preset={:?} session_id={}",
                    video_enhancement,
                    command.session_id
                );
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
        let source_value = local_path_string(&source.uri);
        let mut args = vec!["loadfile".to_owned(), source_value, "replace".to_owned()];
        if let Some(position) = start_position_seconds {
            args.push("-1".to_owned());
            args.push(format!("start={}", position.max(0.0)));
        }
        self.command_owned(&args)?;
        for subtitle in &source.subtitles {
            let flag = if subtitle.default { "select" } else { "auto" };
            self.command(&[
                "sub-add",
                &local_path_string(&subtitle.uri),
                flag,
                &subtitle.label,
                subtitle.language.as_deref().unwrap_or("und"),
            ])?;
        }
        self.set_property(
            "sub-scale",
            &format!("{:.2}", f64::from(state.subtitle_scale) / 100.0),
        )?;
        self.apply_enhancement(state.enhancement)?;
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
        state.snapshot = Some(initial_snapshot(
            session_id,
            source,
            self.capabilities.clone(),
            state.sequence,
            state.subtitle_scale,
            state.enhancement,
        ));
        log::info!("Tauri 桌面 libmpv 已加载媒体 session_id={session_id}");
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
        for shader in self.shaders.for_preset(enhancement)? {
            self.command(&[
                "change-list",
                "glsl-shaders",
                "append",
                &shader.to_string_lossy(),
            ])?;
        }
        Ok(())
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
                Some(MpvLoadEvent::Loaded) => state.load_pending = false,
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
        self.maybe_degrade_enhancement(state, &mut snapshot)?;
        state.sequence = state.sequence.saturating_add(1);
        snapshot.sequence = state.sequence;
        state.snapshot = Some(snapshot);
        Ok(())
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
        if state.enhancement == PlayerVideoEnhancement::Off {
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
        unsafe { (self.api.terminate_destroy)(self.handle()) };
        if let Some(snapshot) = state.snapshot.as_mut() {
            snapshot.status = PlayerStatus::Closed;
            snapshot.sequence = snapshot.sequence.saturating_add(1);
        }
        log::info!("Tauri 桌面 libmpv 已释放 target={:?}", self.target);
        Ok(())
    }
}

impl Drop for MpvRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::warn!("Tauri 桌面 libmpv Drop 释放失败 error={error}");
        }
    }
}

/// libmpv 缺失时保留可查询能力，由桌面装配层决定是否回退 libVLC。
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
                log::warn!("Tauri 桌面 libmpv 不可用，将尝试 libVLC error={message}");
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

#[derive(Default)]
struct ShaderResources {
    clamp: Option<PathBuf>,
    upscale: Option<PathBuf>,
}

impl ShaderResources {
    fn resolve(roots: &[PathBuf]) -> Self {
        Self {
            clamp: find_resource(roots, "Anime4K_Clamp_Highlights.glsl"),
            upscale: find_resource(roots, "Anime4K_Upscale_Original_x2.glsl"),
        }
    }

    fn available(&self) -> bool {
        self.upscale.is_some()
    }

    fn describe(&self) -> String {
        format!(
            "clamp={} upscale={}",
            self.clamp
                .as_deref()
                .map_or("missing".to_owned(), |path| path.display().to_string()),
            self.upscale
                .as_deref()
                .map_or("missing".to_owned(), |path| path.display().to_string())
        )
    }

    fn for_preset(
        &self,
        preset: PlayerVideoEnhancement,
    ) -> Result<Vec<&Path>, PlayerTransportError> {
        if preset == PlayerVideoEnhancement::Off {
            return Ok(Vec::new());
        }
        let upscale = self.upscale.as_deref().ok_or_else(|| {
            PlayerTransportError::Unavailable("Anime4K shader 资源缺失".to_owned())
        })?;
        let mut shaders = Vec::new();
        shaders.push(upscale);
        if preset == PlayerVideoEnhancement::Clear {
            if let Some(clamp) = self.clamp.as_deref() {
                shaders.push(clamp);
            }
        }
        Ok(shaders)
    }
}

fn find_resource(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(name))
        .find(|path| path.is_file())
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
    return vec![("vo", "gpu-next"), ("hwdec", "videotoolbox")];
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
    let name = c_string(name, "mpv 选项名")?;
    let value = c_string(value, "mpv 选项值")?;
    ensure_mpv_success(
        api,
        unsafe { (api.set_option_string)(handle, name.as_ptr(), value.as_ptr()) },
        "配置 libmpv",
    )
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

fn initial_snapshot(
    session_id: &str,
    source: PlayerMediaSource,
    capabilities: PlayerCapabilities,
    sequence: u64,
    subtitle_scale: u16,
    video_enhancement: PlayerVideoEnhancement,
) -> PlayerSnapshot {
    PlayerSnapshot {
        session_id: session_id.to_owned(),
        sequence,
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
        subtitle_scale,
        video_enhancement,
        video_enhancement_degraded: false,
        aspect_ratio: PlayerAspectRatio::Default,
        fullscreen: false,
        picture_in_picture: false,
        error: None,
    }
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
    fn computes_buffered_position_without_exceeding_duration() {
        assert_eq!(buffered_seconds(12.0, 8.0, 30.0), 20.0);
        assert_eq!(buffered_seconds(28.0, 8.0, 30.0), 30.0);
    }

    #[test]
    fn resolves_bundled_anime4k_presets() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/shaders/anime4k");
        let resources = ShaderResources::resolve(&[root]);
        assert!(resources.available());
        assert_eq!(
            resources
                .for_preset(PlayerVideoEnhancement::Balanced)
                .expect("balanced preset")
                .len(),
            1
        );
        assert_eq!(
            resources
                .for_preset(PlayerVideoEnhancement::Clear)
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
}
