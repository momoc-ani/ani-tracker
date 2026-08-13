use std::ffi::{c_char, c_float, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use ani_contracts::{
    PlayerAspectRatio, PlayerAvailability, PlayerBackend, PlayerCapabilities, PlayerCommand,
    PlayerCommandAction, PlayerCommandResult, PlayerError, PlayerErrorCode, PlayerHostPlatform,
    PlayerMediaSource, PlayerRecoveryAction, PlayerSnapshot, PlayerStatus, PlayerTrack,
    PlayerTrackKind,
};
use ani_media::player::{unsupported, PlayerTransport, PlayerTransportError};
use async_trait::async_trait;
use libloading::Library;

#[cfg(test)]
use crate::desktop::platform_directory;
use crate::desktop::{DesktopVideoTarget, DesktopWindowController};

const PLAYBACK_RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

type VlcInstance = c_void;
type VlcMediaPlayer = c_void;
type VlcMedia = c_void;

#[repr(C)]
struct VlcTrackDescription {
    id: c_int,
    name: *mut c_char,
    next: *mut VlcTrackDescription,
}

type VlcNew = unsafe extern "C" fn(c_int, *const *const c_char) -> *mut VlcInstance;
type VlcRelease = unsafe extern "C" fn(*mut VlcInstance);
type VlcGetVersion = unsafe extern "C" fn() -> *const c_char;
type VlcMediaPlayerNew = unsafe extern "C" fn(*mut VlcInstance) -> *mut VlcMediaPlayer;
type VlcMediaPlayerRelease = unsafe extern "C" fn(*mut VlcMediaPlayer);
type VlcMediaNew = unsafe extern "C" fn(*mut VlcInstance, *const c_char) -> *mut VlcMedia;
type VlcMediaRelease = unsafe extern "C" fn(*mut VlcMedia);
type VlcMediaAddOption = unsafe extern "C" fn(*mut VlcMedia, *const c_char);
type VlcMediaSlavesAdd =
    unsafe extern "C" fn(*mut VlcMedia, c_uint, c_uint, *const c_char) -> c_int;
type VlcSetMedia = unsafe extern "C" fn(*mut VlcMediaPlayer, *mut VlcMedia);
type VlcPlay = unsafe extern "C" fn(*mut VlcMediaPlayer) -> c_int;
type VlcSetPause = unsafe extern "C" fn(*mut VlcMediaPlayer, c_int);
type VlcStop = unsafe extern "C" fn(*mut VlcMediaPlayer);
type VlcGetI64 = unsafe extern "C" fn(*mut VlcMediaPlayer) -> i64;
type VlcSetTime = unsafe extern "C" fn(*mut VlcMediaPlayer, i64);
type VlcGetInt = unsafe extern "C" fn(*mut VlcMediaPlayer) -> c_int;
type VlcSetInt = unsafe extern "C" fn(*mut VlcMediaPlayer, c_int) -> c_int;
type VlcSetMute = unsafe extern "C" fn(*mut VlcMediaPlayer, c_int);
type VlcGetRate = unsafe extern "C" fn(*mut VlcMediaPlayer) -> c_float;
type VlcSetRate = unsafe extern "C" fn(*mut VlcMediaPlayer, c_float) -> c_int;
type VlcGetState = unsafe extern "C" fn(*mut VlcMediaPlayer) -> c_uint;
type VlcGetTrackDescriptions =
    unsafe extern "C" fn(*mut VlcMediaPlayer) -> *mut VlcTrackDescription;
type VlcTrackDescriptionsRelease = unsafe extern "C" fn(*mut VlcTrackDescription);
type VlcSetAspectRatio = unsafe extern "C" fn(*mut VlcMediaPlayer, *const c_char);
type VlcSetScale = unsafe extern "C" fn(*mut VlcMediaPlayer, c_float);
type VlcSetSpuTextScale = unsafe extern "C" fn(*mut VlcMediaPlayer, c_uint) -> c_int;

#[cfg(target_os = "windows")]
type VlcSetVideoTarget = unsafe extern "C" fn(*mut VlcMediaPlayer, *mut c_void);
#[cfg(target_os = "macos")]
type VlcSetVideoTarget = unsafe extern "C" fn(*mut VlcMediaPlayer, *mut c_void);
#[cfg(target_os = "linux")]
type VlcSetVideoTarget = unsafe extern "C" fn(*mut VlcMediaPlayer, u32);

/// 动态加载的 libVLC 3.0 API；Library 字段保证函数指针始终有效。
struct VlcApi {
    _library: Library,
    new: VlcNew,
    release: VlcRelease,
    get_version: VlcGetVersion,
    media_player_new: VlcMediaPlayerNew,
    media_player_release: VlcMediaPlayerRelease,
    media_new_path: VlcMediaNew,
    media_new_location: VlcMediaNew,
    media_release: VlcMediaRelease,
    media_add_option: VlcMediaAddOption,
    media_slaves_add: VlcMediaSlavesAdd,
    media_player_set_media: VlcSetMedia,
    media_player_play: VlcPlay,
    media_player_set_pause: VlcSetPause,
    media_player_stop: VlcStop,
    media_player_get_time: VlcGetI64,
    media_player_set_time: VlcSetTime,
    media_player_get_length: VlcGetI64,
    audio_get_volume: VlcGetInt,
    audio_set_volume: VlcSetInt,
    audio_get_mute: VlcGetInt,
    audio_set_mute: VlcSetMute,
    media_player_get_rate: VlcGetRate,
    media_player_set_rate: VlcSetRate,
    media_player_get_state: VlcGetState,
    audio_get_track_description: VlcGetTrackDescriptions,
    audio_get_track: VlcGetInt,
    audio_set_track: VlcSetInt,
    video_get_spu_description: VlcGetTrackDescriptions,
    video_get_spu: VlcGetInt,
    video_set_spu: VlcSetInt,
    track_description_list_release: VlcTrackDescriptionsRelease,
    video_set_aspect_ratio: VlcSetAspectRatio,
    video_set_scale: VlcSetScale,
    video_set_spu_text_scale: Option<VlcSetSpuTextScale>,
    set_video_target: VlcSetVideoTarget,
}

impl VlcApi {
    /// 从确定的动态库路径加载全部必需符号。
    fn load(path: &Path) -> Result<Self, PlayerTransportError> {
        let library = load_library(path)?;
        unsafe {
            Ok(Self {
                new: load_symbol(&library, b"libvlc_new\0")?,
                release: load_symbol(&library, b"libvlc_release\0")?,
                get_version: load_symbol(&library, b"libvlc_get_version\0")?,
                media_player_new: load_symbol(&library, b"libvlc_media_player_new\0")?,
                media_player_release: load_symbol(&library, b"libvlc_media_player_release\0")?,
                media_new_path: load_symbol(&library, b"libvlc_media_new_path\0")?,
                media_new_location: load_symbol(&library, b"libvlc_media_new_location\0")?,
                media_release: load_symbol(&library, b"libvlc_media_release\0")?,
                media_add_option: load_symbol(&library, b"libvlc_media_add_option\0")?,
                media_slaves_add: load_symbol(&library, b"libvlc_media_slaves_add\0")?,
                media_player_set_media: load_symbol(&library, b"libvlc_media_player_set_media\0")?,
                media_player_play: load_symbol(&library, b"libvlc_media_player_play\0")?,
                media_player_set_pause: load_symbol(&library, b"libvlc_media_player_set_pause\0")?,
                media_player_stop: load_symbol(&library, b"libvlc_media_player_stop\0")?,
                media_player_get_time: load_symbol(&library, b"libvlc_media_player_get_time\0")?,
                media_player_set_time: load_symbol(&library, b"libvlc_media_player_set_time\0")?,
                media_player_get_length: load_symbol(
                    &library,
                    b"libvlc_media_player_get_length\0",
                )?,
                audio_get_volume: load_symbol(&library, b"libvlc_audio_get_volume\0")?,
                audio_set_volume: load_symbol(&library, b"libvlc_audio_set_volume\0")?,
                audio_get_mute: load_symbol(&library, b"libvlc_audio_get_mute\0")?,
                audio_set_mute: load_symbol(&library, b"libvlc_audio_set_mute\0")?,
                media_player_get_rate: load_symbol(&library, b"libvlc_media_player_get_rate\0")?,
                media_player_set_rate: load_symbol(&library, b"libvlc_media_player_set_rate\0")?,
                media_player_get_state: load_symbol(&library, b"libvlc_media_player_get_state\0")?,
                audio_get_track_description: load_symbol(
                    &library,
                    b"libvlc_audio_get_track_description\0",
                )?,
                audio_get_track: load_symbol(&library, b"libvlc_audio_get_track\0")?,
                audio_set_track: load_symbol(&library, b"libvlc_audio_set_track\0")?,
                video_get_spu_description: load_symbol(
                    &library,
                    b"libvlc_video_get_spu_description\0",
                )?,
                video_get_spu: load_symbol(&library, b"libvlc_video_get_spu\0")?,
                video_set_spu: load_symbol(&library, b"libvlc_video_set_spu\0")?,
                track_description_list_release: load_symbol(
                    &library,
                    b"libvlc_track_description_list_release\0",
                )?,
                video_set_aspect_ratio: load_symbol(&library, b"libvlc_video_set_aspect_ratio\0")?,
                video_set_scale: load_symbol(&library, b"libvlc_video_set_scale\0")?,
                video_set_spu_text_scale: load_optional_symbol(
                    &library,
                    b"libvlc_video_set_spu_text_scale\0",
                ),
                #[cfg(target_os = "windows")]
                set_video_target: load_symbol(&library, b"libvlc_media_player_set_hwnd\0")?,
                #[cfg(target_os = "macos")]
                set_video_target: load_symbol(&library, b"libvlc_media_player_set_nsobject\0")?,
                #[cfg(target_os = "linux")]
                set_video_target: load_symbol(&library, b"libvlc_media_player_set_xwindow\0")?,
                _library: library,
            })
        }
    }

    fn version(&self) -> String {
        let pointer = unsafe { (self.get_version)() };
        if pointer.is_null() {
            return "unknown".to_owned();
        }
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(target_os = "windows")]
fn load_library(path: &Path) -> Result<Library, PlayerTransportError> {
    const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
    const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
    let library = unsafe {
        libloading::os::windows::Library::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map_err(|error| {
        PlayerTransportError::Unavailable(format!(
            "加载 libVLC 失败 path={} error={error}",
            path.display()
        ))
    })?;
    Ok(library.into())
}

#[cfg(not(target_os = "windows"))]
fn load_library(path: &Path) -> Result<Library, PlayerTransportError> {
    unsafe { Library::new(path) }.map_err(|error| {
        PlayerTransportError::Unavailable(format!(
            "加载 libVLC 失败 path={} error={error}",
            path.display()
        ))
    })
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, PlayerTransportError> {
    let symbol = unsafe { library.get::<T>(name) }.map_err(|error| {
        let symbol_name = String::from_utf8_lossy(name)
            .trim_end_matches('\0')
            .to_owned();
        PlayerTransportError::Unavailable(format!(
            "加载 libVLC 符号失败 symbol={symbol_name} error={error}"
        ))
    })?;
    Ok(*symbol)
}

/// 读取仅部分 libVLC 版本导出的可选符号。
unsafe fn load_optional_symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

struct RuntimeState {
    active_session_id: Option<String>,
    active_source: Option<PlayerMediaSource>,
    pending_start_position_ms: Option<i64>,
    pending_pause_after_start: bool,
    subtitle_scale: u16,
    snapshot: Option<PlayerSnapshot>,
    sequence: u64,
    closed: bool,
}

struct VlcRuntime {
    api: Arc<VlcApi>,
    instance: usize,
    player: usize,
    target: DesktopVideoTarget,
    controller: Arc<dyn DesktopWindowController>,
    capabilities: PlayerCapabilities,
    state: Mutex<RuntimeState>,
}

impl VlcRuntime {
    /// 初始化 libVLC、绑定视频窗口并验证 3.0.x 运行时。
    fn new(
        target: DesktopVideoTarget,
        controller: Arc<dyn DesktopWindowController>,
        library_path: &Path,
        plugin_directory: Option<&Path>,
    ) -> Result<Self, PlayerTransportError> {
        log::info!(
            "正在初始化 Tauri 桌面 libVLC library={} plugins={}",
            library_path.display(),
            plugin_directory
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<system>".to_owned())
        );
        let api = Arc::new(VlcApi::load(library_path)?);
        let version = api.version();
        if !version.starts_with("3.0.") {
            return Err(PlayerTransportError::Unavailable(format!(
                "需要 libVLC 3.0.x，当前为 {version}"
            )));
        }
        let options = runtime_options(plugin_directory)?;
        let option_pointers = options.iter().map(|item| item.as_ptr()).collect::<Vec<_>>();
        let instance = unsafe {
            (api.new)(
                c_int::try_from(option_pointers.len()).unwrap_or(c_int::MAX),
                option_pointers.as_ptr(),
            )
        };
        if instance.is_null() {
            return Err(PlayerTransportError::Unavailable(
                "libVLC 实例创建失败".to_owned(),
            ));
        }
        let player = unsafe { (api.media_player_new)(instance) };
        if player.is_null() {
            unsafe { (api.release)(instance) };
            return Err(PlayerTransportError::Unavailable(
                "libVLC 播放器创建失败".to_owned(),
            ));
        }
        if let Err(error) = bind_video_target(&api, player, target) {
            unsafe {
                (api.media_player_release)(player);
                (api.release)(instance);
            }
            return Err(error);
        }
        log::info!("Tauri 桌面 libVLC 初始化完成 version={version}");
        Ok(Self {
            api,
            instance: instance as usize,
            player: player as usize,
            target,
            controller,
            capabilities: desktop_capabilities(),
            state: Mutex::new(RuntimeState {
                active_session_id: None,
                active_source: None,
                pending_start_position_ms: None,
                pending_pause_after_start: false,
                subtitle_scale: 100,
                snapshot: None,
                sequence: 0,
                closed: false,
            }),
        })
    }

    fn player(&self) -> *mut VlcMediaPlayer {
        self.player as *mut VlcMediaPlayer
    }

    fn instance(&self) -> *mut VlcInstance {
        self.instance as *mut VlcInstance
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RuntimeState>, PlayerTransportError> {
        self.state
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))
    }

    /// 执行一条命令并更新内存快照。
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
                return Ok(unsupported(&command_id, "桌面 libVLC 暂不支持画中画"));
            }
            PlayerCommandAction::SetVideoEnhancement { .. } => {
                return Ok(unsupported(
                    &command_id,
                    "桌面 libVLC 不支持 GPU shader 画质增强",
                ));
            }
            PlayerCommandAction::SetFrameInterpolation { .. } => {
                return Ok(unsupported(&command_id, "桌面 libVLC 不支持模型补帧"));
            }
            PlayerCommandAction::SetHdr { hdr } if *hdr != ani_contracts::PlayerHdrMode::Off => {
                return Ok(unsupported(
                    &command_id,
                    "桌面 libVLC 尚未完成 HDR 输出能力探测",
                ));
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
                "libVLC 播放器已关闭".to_owned(),
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
                true,
            )?,
            PlayerCommandAction::Play => {
                ensure_success(
                    unsafe { (self.api.media_player_play)(self.player()) },
                    "开始播放失败",
                )?;
            }
            PlayerCommandAction::Pause => unsafe {
                (self.api.media_player_set_pause)(self.player(), 1)
            },
            PlayerCommandAction::Seek { position_seconds } => unsafe {
                (self.api.media_player_set_time)(self.player(), seconds_to_ms(*position_seconds))
            },
            PlayerCommandAction::SetVolume { volume } => {
                ensure_success(
                    unsafe {
                        (self.api.audio_set_volume)(
                            self.player(),
                            (*volume * 100.0).round() as c_int,
                        )
                    },
                    "设置音量失败",
                )?;
                unsafe { (self.api.audio_set_mute)(self.player(), 0) };
            }
            PlayerCommandAction::SetMuted { muted } => unsafe {
                (self.api.audio_set_mute)(self.player(), c_int::from(*muted))
            },
            PlayerCommandAction::SetRate { rate } => {
                ensure_success(
                    unsafe { (self.api.media_player_set_rate)(self.player(), *rate as c_float) },
                    "设置倍速失败",
                )?;
            }
            PlayerCommandAction::SelectAudioTrack { track_id } => {
                let track = parse_track_id(track_id)?;
                ensure_success(
                    unsafe { (self.api.audio_set_track)(self.player(), track) },
                    "切换音轨失败",
                )?;
            }
            PlayerCommandAction::SelectSubtitleTrack { track_id } => {
                let track = track_id
                    .as_deref()
                    .map(parse_track_id)
                    .transpose()?
                    .unwrap_or(-1);
                ensure_success(
                    unsafe { (self.api.video_set_spu)(self.player(), track) },
                    "切换字幕轨失败",
                )?;
            }
            PlayerCommandAction::SetSubtitleScale { subtitle_scale } => {
                if state.subtitle_scale == *subtitle_scale {
                    log::debug!(
                        "Tauri 桌面 libVLC 字幕大小未变化，跳过更新 scale={} session_id={}",
                        subtitle_scale,
                        command.session_id
                    );
                    return Ok(());
                }
                if let Some(set_scale) = self.api.video_set_spu_text_scale {
                    ensure_success(
                        unsafe { set_scale(self.player(), c_uint::from(*subtitle_scale)) },
                        "设置字幕大小失败",
                    )?;
                    state.subtitle_scale = *subtitle_scale;
                    if let Some(snapshot) = state.snapshot.as_mut() {
                        snapshot.subtitle_scale = *subtitle_scale;
                    }
                } else {
                    let source = state.active_source.clone().ok_or_else(|| {
                        PlayerTransportError::InvalidResponse("没有可重载的媒体资源".to_owned())
                    })?;
                    let position =
                        ms_to_seconds(unsafe { (self.api.media_player_get_time)(self.player()) });
                    let resume_playback = state.snapshot.as_ref().is_some_and(|snapshot| {
                        matches!(
                            snapshot.status,
                            PlayerStatus::Loading | PlayerStatus::Buffering | PlayerStatus::Playing
                        )
                    });
                    let previous_scale = state.subtitle_scale;
                    state.subtitle_scale = *subtitle_scale;
                    if let Err(error) = self.load_source(
                        &mut state,
                        &command.session_id,
                        source,
                        Some(position),
                        resume_playback,
                    ) {
                        state.subtitle_scale = previous_scale;
                        return Err(error);
                    }
                }
                log::info!(
                    "Tauri 桌面 libVLC 字幕大小已更新 scale={} session_id={} immediate={}",
                    subtitle_scale,
                    command.session_id,
                    self.api.video_set_spu_text_scale.is_some()
                );
            }
            PlayerCommandAction::SetAspectRatio {
                aspect_ratio,
                value,
            } => {
                self.set_aspect_ratio(aspect_ratio.clone(), value.as_deref())?;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.aspect_ratio = aspect_ratio.clone();
                }
            }
            PlayerCommandAction::Retry => {
                let source = state.active_source.clone().ok_or_else(|| {
                    PlayerTransportError::InvalidResponse("没有可重试的媒体资源".to_owned())
                })?;
                self.load_source(&mut state, &command.session_id, source, None, true)?;
            }
            PlayerCommandAction::SetFullscreen { fullscreen } => {
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.fullscreen = *fullscreen;
                }
            }
            PlayerCommandAction::Close
            | PlayerCommandAction::SetVideoEnhancement { .. }
            | PlayerCommandAction::SetFrameInterpolation { .. }
            | PlayerCommandAction::SetHdr { .. }
            | PlayerCommandAction::SetPictureInPicture { .. }
            | PlayerCommandAction::PreviousItem
            | PlayerCommandAction::NextItem => {}
        }
        self.refresh_snapshot_locked(&mut state)?;
        Ok(())
    }

    /// 创建 VLC Media、安装字幕并自动播放。
    fn load_source(
        &self,
        state: &mut RuntimeState,
        session_id: &str,
        source: PlayerMediaSource,
        start_position_seconds: Option<f64>,
        play_when_ready: bool,
    ) -> Result<(), PlayerTransportError> {
        let media_uri = if source.uri.contains("://") {
            source.uri.clone()
        } else {
            local_path_string(&source.uri)
        };
        let uri = CString::new(media_uri.as_bytes())
            .map_err(|_| PlayerTransportError::InvalidResponse("媒体地址包含空字符".to_owned()))?;
        let media = unsafe {
            if source.uri.contains("://") {
                (self.api.media_new_location)(self.instance(), uri.as_ptr())
            } else {
                (self.api.media_new_path)(self.instance(), uri.as_ptr())
            }
        };
        if media.is_null() {
            return Err(PlayerTransportError::Native("媒体对象创建失败".to_owned()));
        }
        let result = (|| {
            for option in media_options(&source, state.subtitle_scale)? {
                unsafe { (self.api.media_add_option)(media, option.as_ptr()) };
            }
            for subtitle in &source.subtitles {
                let subtitle_uri = subtitle_uri(&subtitle.uri)?;
                let subtitle_uri = CString::new(subtitle_uri.as_bytes()).map_err(|_| {
                    PlayerTransportError::InvalidResponse("字幕地址包含空字符".to_owned())
                })?;
                let priority = if subtitle.default { 4 } else { 1 };
                let status = unsafe {
                    (self.api.media_slaves_add)(media, 0, priority, subtitle_uri.as_ptr())
                };
                if status != 0 {
                    log::warn!(
                        "Tauri 桌面 libVLC 外挂字幕安装失败 subtitle_id={}",
                        subtitle.id
                    );
                }
            }
            unsafe { (self.api.media_player_set_media)(self.player(), media) };
            ensure_success(
                unsafe { (self.api.media_player_play)(self.player()) },
                "媒体播放启动失败",
            )?;
            Ok(())
        })();
        unsafe { (self.api.media_release)(media) };
        result?;

        state.active_source = Some(source.clone());
        state.pending_start_position_ms = start_position_seconds.map(seconds_to_ms);
        state.pending_pause_after_start = !play_when_ready;
        state.sequence = next_media_sequence(
            state.active_session_id.as_deref(),
            state.snapshot.is_some(),
            session_id,
            state.sequence,
        );
        state.active_session_id = Some(session_id.to_owned());
        state.snapshot = Some(initial_snapshot(
            session_id,
            source,
            self.capabilities.clone(),
            state.sequence,
            state.subtitle_scale,
        ));
        log::info!(
            "Tauri 桌面 libVLC 已加载媒体 session_id={} task_id={}",
            session_id,
            state
                .active_source
                .as_ref()
                .map(|value| value.task_id.as_str())
                .unwrap_or_default()
        );
        Ok(())
    }

    fn set_aspect_ratio(
        &self,
        aspect_ratio: PlayerAspectRatio,
        custom: Option<&str>,
    ) -> Result<(), PlayerTransportError> {
        let ratio = match aspect_ratio {
            PlayerAspectRatio::Default | PlayerAspectRatio::Fit | PlayerAspectRatio::Fill => "",
            PlayerAspectRatio::Ratio16x9 => "16:9",
            PlayerAspectRatio::Ratio4x3 => "4:3",
            PlayerAspectRatio::Custom => custom.unwrap_or_default(),
        };
        let ratio = CString::new(ratio)
            .map_err(|_| PlayerTransportError::InvalidResponse("画面比例包含空字符".to_owned()))?;
        unsafe {
            (self.api.video_set_scale)(
                self.player(),
                if aspect_ratio == PlayerAspectRatio::Fill {
                    1.0
                } else {
                    0.0
                },
            );
            (self.api.video_set_aspect_ratio)(self.player(), ratio.as_ptr());
        }
        Ok(())
    }

    /// 从 libVLC 拉取状态、轨道和播放位置并生成递增快照。
    fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        let mut state = self.lock_state()?;
        if state.closed || state.snapshot.is_none() {
            return Ok(state.snapshot.clone());
        }
        self.refresh_snapshot_locked(&mut state)?;
        Ok(state.snapshot.clone())
    }

    fn refresh_snapshot_locked(
        &self,
        state: &mut RuntimeState,
    ) -> Result<(), PlayerTransportError> {
        let Some(mut snapshot) = state.snapshot.clone() else {
            return Ok(());
        };
        let vlc_state = unsafe { (self.api.media_player_get_state)(self.player()) };
        let mut paused_after_start = false;
        if matches!(vlc_state, 3 | 4) {
            if let Some(start_position) = state.pending_start_position_ms.take() {
                unsafe { (self.api.media_player_set_time)(self.player(), start_position) };
            }
            if state.pending_pause_after_start {
                unsafe { (self.api.media_player_set_pause)(self.player(), 1) };
                state.pending_pause_after_start = false;
                paused_after_start = true;
            }
        }
        let position_seconds =
            ms_to_seconds(unsafe { (self.api.media_player_get_time)(self.player()) });
        let duration_seconds =
            ms_to_seconds(unsafe { (self.api.media_player_get_length)(self.player()) });
        let reported_status = player_status(vlc_state);
        let next_status = if paused_after_start {
            PlayerStatus::Paused
        } else {
            resolve_advancing_player_status(
                snapshot.status.clone(),
                reported_status.clone(),
                snapshot.position_seconds,
                position_seconds,
            )
        };
        if next_status == PlayerStatus::Playing
            && reported_status != PlayerStatus::Playing
            && snapshot.status != PlayerStatus::Playing
        {
            log::info!(
                "libVLC 播放时间已推进，播放器状态恢复为 playing session_id={}",
                snapshot.session_id
            );
        }
        snapshot.status = next_status;
        snapshot.position_seconds = position_seconds;
        if duration_seconds > 0.0 {
            snapshot.duration_seconds = duration_seconds;
        }
        snapshot.buffered_seconds = position_seconds;
        snapshot.volume =
            f64::from(unsafe { (self.api.audio_get_volume)(self.player()) }.max(0)) / 100.0;
        snapshot.muted = unsafe { (self.api.audio_get_mute)(self.player()) } != 0;
        snapshot.playback_rate =
            f64::from(unsafe { (self.api.media_player_get_rate)(self.player()) });
        snapshot.audio_tracks = self.read_tracks(PlayerTrackKind::Audio);
        snapshot.subtitle_tracks = self.read_tracks(PlayerTrackKind::Subtitle);
        snapshot.error = (snapshot.status == PlayerStatus::Error).then(decoder_error);
        state.sequence = state.sequence.saturating_add(1);
        snapshot.sequence = state.sequence;
        state.snapshot = Some(snapshot);
        Ok(())
    }

    fn read_tracks(&self, kind: PlayerTrackKind) -> Vec<PlayerTrack> {
        let (head, selected) = unsafe {
            match kind {
                PlayerTrackKind::Audio => (
                    (self.api.audio_get_track_description)(self.player()),
                    (self.api.audio_get_track)(self.player()),
                ),
                PlayerTrackKind::Subtitle => (
                    (self.api.video_get_spu_description)(self.player()),
                    (self.api.video_get_spu)(self.player()),
                ),
            }
        };
        let mut tracks = Vec::new();
        let mut current = head;
        while !current.is_null() {
            let item = unsafe { &*current };
            let selectable = kind == PlayerTrackKind::Audio || item.id >= 0;
            if selectable {
                let label = if item.name.is_null() {
                    format!("轨道 {}", item.id)
                } else {
                    unsafe { CStr::from_ptr(item.name) }
                        .to_string_lossy()
                        .into_owned()
                };
                tracks.push(PlayerTrack {
                    id: item.id.to_string(),
                    kind: kind.clone(),
                    label,
                    language: None,
                    selected: item.id == selected,
                });
            }
            current = item.next;
        }
        if !head.is_null() {
            unsafe { (self.api.track_description_list_release)(head) };
        }
        tracks
    }

    /// 幂等停止媒体并按 libVLC 所有权顺序释放句柄。
    fn shutdown(&self) -> Result<(), PlayerTransportError> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        unsafe {
            (self.api.media_player_stop)(self.player());
            (self.api.media_player_release)(self.player());
            (self.api.release)(self.instance());
        }
        if let Some(snapshot) = state.snapshot.as_mut() {
            snapshot.status = PlayerStatus::Closed;
            snapshot.sequence = snapshot.sequence.saturating_add(1);
        }
        log::info!("Tauri 桌面 libVLC 已释放 target={:?}", self.target);
        Ok(())
    }
}

impl Drop for VlcRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::warn!("Tauri 桌面 libVLC Drop 释放失败 error={error}");
        }
    }
}

/// 允许运行时缺失时仍返回明确能力和结构化错误。
pub struct DesktopPlayerTransport {
    runtime: Option<Arc<VlcRuntime>>,
    unavailable_reason: Option<String>,
}

impl DesktopPlayerTransport {
    /// 解析动态库和插件目录，失败时创建可查询的不可用 transport。
    pub fn new(
        target: DesktopVideoTarget,
        controller: Arc<dyn DesktopWindowController>,
        roots: Vec<PathBuf>,
    ) -> Self {
        let resolved = resolve_runtime(&roots).and_then(|(library, plugins)| {
            VlcRuntime::new(target, controller, &library, plugins.as_deref()).map(Arc::new)
        });
        match resolved {
            Ok(runtime) => Self {
                runtime: Some(runtime),
                unavailable_reason: None,
            },
            Err(error) => {
                let message = error.to_string();
                log::error!("Tauri 桌面 libVLC 不可用 error={message}");
                Self {
                    runtime: None,
                    unavailable_reason: Some(message),
                }
            }
        }
    }
}

#[async_trait]
impl PlayerTransport for DesktopPlayerTransport {
    async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError> {
        Ok(self
            .runtime
            .as_ref()
            .map(|runtime| runtime.capabilities.clone())
            .unwrap_or_else(|| unavailable_capabilities(self.unavailable_reason.as_deref())))
    }

    async fn dispatch(
        &self,
        command: PlayerCommand,
    ) -> Result<PlayerCommandResult, PlayerTransportError> {
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            PlayerTransportError::Unavailable(
                self.unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "libVLC 运行时不可用".to_owned()),
            )
        })?;
        runtime.dispatch(command)
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

fn resolve_runtime(roots: &[PathBuf]) -> Result<(PathBuf, Option<PathBuf>), PlayerTransportError> {
    let mut attempted_libraries = Vec::new();
    for root in roots {
        for library in library_candidates(root) {
            attempted_libraries.push(library.clone());
            if library.is_file() {
                let plugins = plugin_candidates(root)
                    .into_iter()
                    .find(|path| path.is_dir());
                log::info!(
                    "已定位 Tauri 桌面 libVLC library={} plugins={}",
                    library.display(),
                    plugins
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "<missing>".to_owned())
                );
                return Ok((library, plugins));
            }
        }
    }
    for library in system_library_candidates() {
        attempted_libraries.push(library.clone());
        if library.is_file() || library.components().count() == 1 {
            return Ok((library, None));
        }
    }
    log::warn!(
        "Tauri 桌面 libVLC 搜索耗尽 candidates={}",
        attempted_libraries
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    Err(PlayerTransportError::Unavailable(
        "未找到 libVLC 3.0.x 运行时".to_owned(),
    ))
}

fn library_candidates(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    #[cfg(target_os = "windows")]
    return vec![root.join("libvlc.dll")];
    #[cfg(target_os = "macos")]
    return vec![
        root.join("lib/libvlc.dylib"),
        root.join("libvlc.dylib"),
        root.join("VLC.app/Contents/MacOS/lib/libvlc.dylib"),
    ];
    #[cfg(target_os = "linux")]
    return vec![root.join("libvlc.so.5"), root.join("libvlc.so")];
}

fn plugin_candidates(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("vlc/plugins"),
        root.join("plugins"),
        root.join("lib/vlc/plugins"),
        root.join("VLC.app/Contents/MacOS/plugins"),
    ]
}

fn system_library_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut values = Vec::new();
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(variable) {
                values.push(PathBuf::from(root).join("VideoLAN/VLC/libvlc.dll"));
            }
        }
        values
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib",
        )]
    }
    #[cfg(target_os = "linux")]
    {
        vec![PathBuf::from("libvlc.so.5"), PathBuf::from("libvlc.so")]
    }
}

fn runtime_options(plugin_directory: Option<&Path>) -> Result<Vec<CString>, PlayerTransportError> {
    if let Some(directory) = plugin_directory {
        std::env::set_var("VLC_PLUGIN_PATH", directory);
        log::info!(
            "已配置 Tauri 桌面 libVLC 插件目录 path={}",
            directory.display()
        );
    }
    let values = vec![
        "--no-video-title-show".to_owned(),
        "--audio-time-stretch".to_owned(),
        "--network-caching=1000".to_owned(),
        "--file-caching=500".to_owned(),
    ];
    #[cfg(target_os = "windows")]
    let values = {
        let mut values = values;
        values.push("--avcodec-hw=d3d11va".to_owned());
        values
    };
    #[cfg(target_os = "macos")]
    let values = {
        let mut values = values;
        values.push("--avcodec-hw=videotoolbox".to_owned());
        values
    };
    values
        .into_iter()
        .map(|value| {
            CString::new(value)
                .map_err(|_| PlayerTransportError::InvalidResponse("VLC 参数包含空字符".to_owned()))
        })
        .collect()
}

fn media_options(
    source: &PlayerMediaSource,
    subtitle_scale: u16,
) -> Result<Vec<CString>, PlayerTransportError> {
    let mut values = vec![
        ":no-video-title-show".to_owned(),
        format!(
            ":freetype-rel-fontsize={}",
            subtitle_relative_font_size(subtitle_scale)
        ),
    ];
    if source.mode == ani_contracts::PlayerMediaMode::Hls {
        values.push(":network-caching=1000".to_owned());
    }
    values
        .into_iter()
        .map(|value| {
            CString::new(value)
                .map_err(|_| PlayerTransportError::InvalidResponse("媒体参数包含空字符".to_owned()))
        })
        .collect()
}

fn subtitle_uri(value: &str) -> Result<String, PlayerTransportError> {
    if value.contains("://") {
        return Ok(value.to_owned());
    }
    url::Url::from_file_path(local_path_string(value))
        .map(|url| url.to_string())
        .map_err(|_| PlayerTransportError::InvalidResponse("外挂字幕路径无效".to_owned()))
}

/// 将本地媒体路径转换为 libVLC 可识别的系统常规形式。
fn local_path_string(value: &str) -> String {
    dunce::simplified(Path::new(value))
        .to_string_lossy()
        .into_owned()
}

fn bind_video_target(
    api: &VlcApi,
    player: *mut VlcMediaPlayer,
    target: DesktopVideoTarget,
) -> Result<(), PlayerTransportError> {
    #[cfg(target_os = "windows")]
    if let DesktopVideoTarget::Windows(hwnd) = target {
        unsafe { (api.set_video_target)(player, hwnd as *mut c_void) };
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if let DesktopVideoTarget::MacOs(view) = target {
        unsafe { (api.set_video_target)(player, view as *mut c_void) };
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let DesktopVideoTarget::X11(window) = target {
        unsafe { (api.set_video_target)(player, window) };
        return Ok(());
    }
    Err(PlayerTransportError::Unavailable(
        "播放窗口句柄与当前平台不匹配".to_owned(),
    ))
}

fn desktop_capabilities() -> PlayerCapabilities {
    PlayerCapabilities {
        backend: PlayerBackend::Libvlc,
        platform: PlayerHostPlatform::TauriDesktop,
        availability: PlayerAvailability::Available,
        can_seek: true,
        can_set_volume: true,
        can_mute: true,
        playback_rates: PLAYBACK_RATES.to_vec(),
        supports_audio_tracks: true,
        supports_subtitle_tracks: true,
        supports_subtitle_scale: true,
        supports_video_enhancement: false,
        supports_frame_interpolation: false,
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

fn unavailable_capabilities(reason: Option<&str>) -> PlayerCapabilities {
    PlayerCapabilities {
        availability: PlayerAvailability::Unavailable,
        can_seek: false,
        can_set_volume: false,
        can_mute: false,
        playback_rates: vec![1.0],
        supports_audio_tracks: false,
        supports_subtitle_tracks: false,
        supports_subtitle_scale: false,
        supports_aspect_ratio: false,
        supports_fullscreen: false,
        supports_picture_in_picture: false,
        supports_playlist_navigation: false,
        supports_direct_playback: false,
        supports_transcoding_fallback: false,
        supports_hdr: false,
        unavailable_reason: Some(reason.unwrap_or("libVLC 运行时不可用").to_owned()),
        ..desktop_capabilities()
    }
}

fn initial_snapshot(
    session_id: &str,
    source: PlayerMediaSource,
    capabilities: PlayerCapabilities,
    sequence: u64,
    subtitle_scale: u16,
) -> PlayerSnapshot {
    PlayerSnapshot {
        session_id: session_id.to_owned(),
        sequence,
        backend: PlayerBackend::Libvlc,
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
        video_enhancement: ani_contracts::PlayerVideoEnhancement::Off,
        video_enhancement_degraded: false,
        frame_interpolation: ani_contracts::PlayerFrameInterpolation::Off,
        hdr: ani_contracts::PlayerHdrMode::Off,
        enhancement_diagnostics: Default::default(),
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

fn decoder_error() -> PlayerError {
    PlayerError {
        code: PlayerErrorCode::Decoder,
        message: "libVLC 无法解码或读取当前媒体".to_owned(),
        recoverable: true,
        recovery_actions: vec![PlayerRecoveryAction::Retry, PlayerRecoveryAction::Close],
    }
}

fn player_status(value: c_uint) -> PlayerStatus {
    match value {
        1 => PlayerStatus::Loading,
        2 => PlayerStatus::Buffering,
        3 => PlayerStatus::Playing,
        4 => PlayerStatus::Paused,
        5 => PlayerStatus::Idle,
        6 => PlayerStatus::Ended,
        7 => PlayerStatus::Error,
        _ => PlayerStatus::Idle,
    }
}

/// 播放时间前进时纠正 libVLC 偶发滞留的 loading/buffering 状态。
fn resolve_advancing_player_status(
    previous_status: PlayerStatus,
    reported_status: PlayerStatus,
    previous_position_seconds: f64,
    position_seconds: f64,
) -> PlayerStatus {
    if position_seconds > previous_position_seconds
        && matches!(
            reported_status,
            PlayerStatus::Loading | PlayerStatus::Buffering
        )
        && matches!(
            previous_status,
            PlayerStatus::Loading | PlayerStatus::Buffering | PlayerStatus::Playing
        )
    {
        PlayerStatus::Playing
    } else {
        reported_status
    }
}

fn parse_track_id(value: &str) -> Result<c_int, PlayerTransportError> {
    value
        .parse()
        .map_err(|_| PlayerTransportError::InvalidResponse("轨道标识无效".to_owned()))
}

fn ensure_success(status: c_int, message: &str) -> Result<(), PlayerTransportError> {
    if status == 0 {
        Ok(())
    } else {
        Err(PlayerTransportError::Native(message.to_owned()))
    }
}

fn seconds_to_ms(value: f64) -> i64 {
    (value.max(0.0) * 1_000.0).round().min(i64::MAX as f64) as i64
}

fn ms_to_seconds(value: i64) -> f64 {
    value.max(0) as f64 / 1_000.0
}

/// 将百分比换算为 VLC 使用的视频高度相对字号。
fn subtitle_relative_font_size(scale: u16) -> u16 {
    (1_600 + scale / 2) / scale
}

/// 新会话从首个快照开始，同会话重载沿用递增序号。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn removes_verbatim_prefix_before_calling_libvlc() {
        let local_path = r"\\?\C:\Anime\Episode 01.ass";
        assert_eq!(local_path_string(local_path), r"C:\Anime\Episode 01.ass");
        let uri = subtitle_uri(local_path).expect("subtitle URI");
        assert!(uri.starts_with("file:///C:/Anime/Episode%2001.ass"));
    }

    /// 桌面端使用平台硬解策略，且不传递已被 libVLC 移除的插件参数。
    #[test]
    fn builds_desktop_runtime_options() {
        let options = runtime_options(Some(Path::new("C:/vlc/plugins")))
            .expect("runtime options")
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(!options.iter().any(|value| value.contains("plugin-path")));
        #[cfg(target_os = "windows")]
        assert!(options.iter().any(|value| value == "--avcodec-hw=d3d11va"));
        #[cfg(target_os = "macos")]
        assert!(options
            .iter()
            .any(|value| value == "--avcodec-hw=videotoolbox"));
        #[cfg(target_os = "linux")]
        assert!(!options
            .iter()
            .any(|value| value.starts_with("--avcodec-hw=")));
    }

    #[test]
    fn keeps_unprobed_hdr_and_model_features_disabled() {
        let capabilities = desktop_capabilities();
        assert!(!capabilities.supports_hdr);
        assert!(!capabilities.supports_model_enhancement);
        assert!(!capabilities.supports_frame_interpolation);
    }

    /// VLC 状态码必须稳定映射到跨平台快照状态。
    #[test]
    fn maps_libvlc_states() {
        assert_eq!(player_status(3), PlayerStatus::Playing);
        assert_eq!(player_status(5), PlayerStatus::Idle);
        assert_eq!(player_status(6), PlayerStatus::Ended);
        assert_eq!(player_status(7), PlayerStatus::Error);
    }

    /// 播放时间推进后不能继续向控制页报告缓冲状态。
    #[test]
    fn restores_playing_status_when_position_advances() {
        assert_eq!(
            resolve_advancing_player_status(
                PlayerStatus::Loading,
                PlayerStatus::Buffering,
                0.0,
                0.25,
            ),
            PlayerStatus::Playing
        );
        assert_eq!(
            resolve_advancing_player_status(
                PlayerStatus::Buffering,
                PlayerStatus::Buffering,
                1.0,
                1.0,
            ),
            PlayerStatus::Buffering
        );
    }

    /// 同会话重载必须保持快照序号递增，新会话则重新从 1 开始。
    #[test]
    fn advances_snapshot_sequence_when_reloading_same_session() {
        assert_eq!(
            next_media_sequence(Some("session-a"), true, "session-a", 8),
            9
        );
        assert_eq!(
            next_media_sequence(Some("session-a"), true, "session-b", 8),
            1
        );
        assert_eq!(
            next_media_sequence(Some("session-a"), false, "session-a", 8),
            1
        );
    }

    /// 五档百分比必须稳定映射为 VLC 的相对字号参数。
    #[test]
    fn maps_subtitle_scales_to_vlc_relative_font_sizes() {
        assert_eq!(subtitle_relative_font_size(100), 16);
        assert_eq!(subtitle_relative_font_size(125), 13);
        assert_eq!(subtitle_relative_font_size(150), 11);
        assert_eq!(subtitle_relative_font_size(175), 9);
        assert_eq!(subtitle_relative_font_size(200), 8);
    }

    /// 本地已准备 VLC 运行库时验证依赖搜索和核心符号可加载。
    #[test]
    fn loads_prepared_libvlc_runtime_when_available() {
        let required = std::env::var("ANI_REQUIRE_PREPARED_LIBVLC").as_deref() == Ok("1");
        let native_target = platform_directory();
        let target = std::env::var("ANI_LIBVLC_TARGET").unwrap_or_else(|_| native_target.clone());
        assert_eq!(
            target, native_target,
            "libVLC smoke target must match the native test binary"
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../out/libvlc")
            .join(&target);
        println!("[libvlc-smoke] target={target} root={}", root.display());
        let Some(library) = library_candidates(&root)
            .into_iter()
            .find(|path| path.is_file())
        else {
            assert!(
                !required,
                "required prepared libVLC core library is missing: {}",
                root.display()
            );
            return;
        };
        let plugins = plugin_candidates(&root)
            .into_iter()
            .find(|path| path.is_dir());
        if required {
            assert!(
                plugins.is_some(),
                "required prepared libVLC plugin directory is missing: {}",
                root.display()
            );
        }
        println!(
            "[libvlc-smoke] library={} plugins={}",
            library.display(),
            plugins
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<missing>".to_owned())
        );
        let api = VlcApi::load(&library)
            .unwrap_or_else(|error| panic!("load prepared libVLC failed: {error}"));
        assert!(api.version().starts_with("3.0."));
        let options = runtime_options(plugins.as_deref()).expect("build runtime options");
        let option_pointers = options.iter().map(|item| item.as_ptr()).collect::<Vec<_>>();
        let instance = unsafe {
            (api.new)(
                c_int::try_from(option_pointers.len()).expect("option count"),
                option_pointers.as_ptr(),
            )
        };
        assert!(!instance.is_null(), "libVLC instance must initialize");
        let player = unsafe { (api.media_player_new)(instance) };
        assert!(!player.is_null(), "libVLC media player must initialize");
        unsafe {
            (api.media_player_release)(player);
            (api.release)(instance);
        }
    }
}
