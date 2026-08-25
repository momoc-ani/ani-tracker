use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ani_media::player::PlayerTransportError;

use super::{MpvApi, MpvHandle};

pub(super) type MpvRenderContext = c_void;
pub(super) type MpvRenderContextCreate =
    unsafe extern "C" fn(*mut *mut MpvRenderContext, *mut MpvHandle, *mut MpvRenderParam) -> c_int;
pub(super) type MpvRenderContextSetUpdateCallback = unsafe extern "C" fn(
    *mut MpvRenderContext,
    Option<unsafe extern "C" fn(*mut c_void)>,
    *mut c_void,
);
pub(super) type MpvRenderContextUpdate = unsafe extern "C" fn(*mut MpvRenderContext) -> u64;
pub(super) type MpvRenderContextRender =
    unsafe extern "C" fn(*mut MpvRenderContext, *mut MpvRenderParam) -> c_int;
pub(super) type MpvRenderContextReportSwap = unsafe extern "C" fn(*mut MpvRenderContext);
pub(super) type MpvRenderContextFree = unsafe extern "C" fn(*mut MpvRenderContext);

type CglContext = c_void;

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
const MPV_RENDER_UPDATE_FRAME: u64 = 1;
const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const RESIZE_RENDER_INTERVAL: Duration = Duration::from_millis(50);

#[repr(C)]
pub(super) struct MpvRenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvOpenGlInitParams {
    get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct MpvOpenGlFbo {
    fbo: c_int,
    width: c_int,
    height: c_int,
    internal_format: c_int,
}

#[derive(Default)]
struct RenderSignalState {
    pending: bool,
    stopped: bool,
}

#[derive(Default)]
struct RenderHealthState {
    rendered_frames: u64,
    render_update_calls: u64,
    frame_update_requests: u64,
    render_attempts: u64,
    last_backing_size: Option<(c_int, c_int)>,
    last_backing_status: Option<c_int>,
    last_error: Option<String>,
}

#[derive(Default)]
struct RenderHealth {
    state: Mutex<RenderHealthState>,
}

#[derive(Default)]
struct RenderSignal {
    state: Mutex<RenderSignalState>,
    ready: Condvar,
}

pub(super) struct MacMpvRenderer {
    api: Arc<MpvApi>,
    render_context: usize,
    surface: usize,
    signal: Arc<RenderSignal>,
    health: Arc<RenderHealth>,
    thread: Option<JoinHandle<()>>,
}

impl MacMpvRenderer {
    pub(super) fn new(
        api: Arc<MpvApi>,
        handle: *mut MpvHandle,
        parent_window: usize,
    ) -> Result<Self, PlayerTransportError> {
        let surface = unsafe { ani_mpv_macos_surface_create(parent_window as *mut c_void) };
        if surface.is_null() {
            return Err(PlayerTransportError::Unavailable(
                "创建 macOS libmpv OpenGL 表面失败".to_owned(),
            ));
        }
        let cgl_context = unsafe { ani_mpv_macos_surface_cgl_context(surface) };
        if cgl_context.is_null() {
            unsafe { ani_mpv_macos_surface_destroy(surface) };
            return Err(PlayerTransportError::Unavailable(
                "macOS libmpv OpenGL 表面没有 CGLContext".to_owned(),
            ));
        }

        let mut render_context = ptr::null_mut();
        let mut api_name = b"opengl\0".to_vec();
        let mut init = MpvOpenGlInitParams {
            get_proc_address: Some(resolve_open_gl_symbol),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut params = [
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_API_TYPE,
                data: api_name.as_mut_ptr().cast(),
            },
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&mut init as *mut MpvOpenGlInitParams).cast(),
            },
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        let create_status = unsafe {
            CGLLockContext(cgl_context);
            CGLSetCurrentContext(cgl_context);
            let status =
                (api.render_context_create)(&mut render_context, handle, params.as_mut_ptr());
            CGLSetCurrentContext(ptr::null_mut());
            CGLUnlockContext(cgl_context);
            status
        };
        if create_status < 0 || render_context.is_null() {
            unsafe { ani_mpv_macos_surface_destroy(surface) };
            return Err(PlayerTransportError::Unavailable(format!(
                "创建 macOS libmpv render context 失败：{}",
                api.error_message(create_status)
            )));
        }

        let signal = Arc::new(RenderSignal::default());
        let health = Arc::new(RenderHealth::default());
        let thread_signal = signal.clone();
        let thread_health = health.clone();
        let thread_api = api.clone();
        let render_context_address = render_context as usize;
        let cgl_context_address = cgl_context as usize;
        let surface_address = surface as usize;
        let render_thread = thread::Builder::new()
            .name("ani-mpv-macos-render".to_owned())
            .spawn(move || {
                render_loop(
                    thread_api,
                    render_context_address,
                    cgl_context_address,
                    surface_address,
                    thread_signal,
                    thread_health,
                );
            })
            .map_err(|error| {
                unsafe {
                    CGLLockContext(cgl_context);
                    CGLSetCurrentContext(cgl_context);
                    (api.render_context_free)(render_context);
                    CGLSetCurrentContext(ptr::null_mut());
                    CGLUnlockContext(cgl_context);
                    ani_mpv_macos_surface_destroy(surface);
                }
                PlayerTransportError::Native(format!("创建 macOS MPV 渲染线程失败：{error}"))
            })?;

        unsafe {
            (api.render_context_set_update_callback)(
                render_context,
                Some(render_update_callback),
                Arc::as_ptr(&signal).cast_mut().cast(),
            );
        }
        signal_frame(&signal);
        log::info!("macOS libmpv render API 已绑定 NSOpenGLView/CGL");
        Ok(Self {
            api,
            render_context: render_context_address,
            surface: surface as usize,
            signal,
            health,
            thread: Some(render_thread),
        })
    }

    /// 开始一次媒体首帧检测并返回当前已交换帧基线。
    pub(super) fn begin_media_load(&self) -> Result<u64, PlayerTransportError> {
        let mut health = self
            .health
            .state
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))?;
        health.last_error = None;
        health.render_update_calls = 0;
        health.frame_update_requests = 0;
        health.render_attempts = 0;
        health.last_backing_size = None;
        health.last_backing_status = None;
        Ok(health.rendered_frames)
    }

    /// 返回渲染线程已成功交换到窗口的累计帧数。
    pub(super) fn rendered_frames(&self) -> Result<u64, PlayerTransportError> {
        self.health
            .state
            .lock()
            .map(|health| health.rendered_frames)
            .map_err(|error| PlayerTransportError::Native(error.to_string()))
    }

    /// 读取渲染线程最近一次稳定错误。
    pub(super) fn last_error(&self) -> Result<Option<String>, PlayerTransportError> {
        self.health
            .state
            .lock()
            .map(|health| health.last_error.clone())
            .map_err(|error| PlayerTransportError::Native(error.to_string()))
    }

    /// 返回首帧超时时的 Render API 诊断计数。
    pub(super) fn diagnostics(&self) -> Result<String, PlayerTransportError> {
        self.health
            .state
            .lock()
            .map(|health| {
                let backing = health
                    .last_backing_size
                    .map_or_else(|| "unknown".to_owned(), |(width, height)| {
                        format!("{width}x{height}")
                    });
                format!(
                    "updates={} frame_requests={} attempts={} swapped={} backing={} backing_status={}",
                    health.render_update_calls,
                    health.frame_update_requests,
                    health.render_attempts,
                    health.rendered_frames,
                    backing,
                    health
                        .last_backing_status
                        .map_or_else(|| "unknown".to_owned(), |status| status.to_string())
                )
            })
            .map_err(|error| PlayerTransportError::Native(error.to_string()))
    }

    pub(super) fn shutdown(mut self) -> Result<(), PlayerTransportError> {
        let render_context = self.render_context as *mut MpvRenderContext;
        unsafe {
            (self.api.render_context_set_update_callback)(render_context, None, ptr::null_mut());
        }
        {
            let mut state = self
                .signal
                .state
                .lock()
                .map_err(|error| PlayerTransportError::Native(error.to_string()))?;
            state.stopped = true;
            self.signal.ready.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| {
                PlayerTransportError::Native("macOS MPV 渲染线程异常退出".to_owned())
            })?;
        }

        let surface = self.surface as *mut c_void;
        let cgl_context = unsafe { ani_mpv_macos_surface_cgl_context(surface) };
        unsafe {
            if !cgl_context.is_null() {
                CGLLockContext(cgl_context);
                CGLSetCurrentContext(cgl_context);
            }
            (self.api.render_context_free)(render_context);
            if !cgl_context.is_null() {
                CGLSetCurrentContext(ptr::null_mut());
                CGLUnlockContext(cgl_context);
            }
            ani_mpv_macos_surface_destroy(surface);
        }
        log::info!("macOS libmpv render API 已释放");
        Ok(())
    }
}

unsafe extern "C" fn render_update_callback(context: *mut c_void) {
    if context.is_null() {
        return;
    }
    let signal = unsafe { &*(context.cast::<RenderSignal>()) };
    signal_frame(signal);
}

fn signal_frame(signal: &RenderSignal) {
    if let Ok(mut state) = signal.state.lock() {
        state.pending = true;
        signal.ready.notify_one();
    }
}

fn render_loop(
    api: Arc<MpvApi>,
    render_context: usize,
    cgl_context: usize,
    surface: usize,
    signal: Arc<RenderSignal>,
    health: Arc<RenderHealth>,
) {
    let render_context = render_context as *mut MpvRenderContext;
    let cgl_context = cgl_context as *mut CglContext;
    let surface = surface as *mut c_void;
    let mut drawable_update_warned = false;
    let mut last_drawable_size: Option<(c_int, c_int)> = None;
    let mut last_render_size: Option<(c_int, c_int)> = None;
    let mut next_resize_render_at: Option<Instant> = None;
    loop {
        let mut state = match signal.state.lock() {
            Ok(state) => state,
            Err(error) => {
                log::error!("macOS MPV 渲染状态锁失败 error={error}");
                break;
            }
        };
        while !state.pending && !state.stopped {
            state = match signal.ready.wait(state) {
                Ok(state) => state,
                Err(error) => {
                    log::error!("macOS MPV 渲染等待失败 error={error}");
                    return;
                }
            };
        }
        if state.stopped {
            break;
        }
        state.pending = false;
        drop(state);

        // 缩放期间限制重渲染频率，保留上一帧，让窗口跟手而不反复触发 Anime4K。
        let mut resize_size = [0_i32; 2];
        let resize_status = unsafe {
            ani_mpv_macos_surface_backing_size(surface, &mut resize_size[0], &mut resize_size[1])
        };
        if resize_status == 0 && resize_size[0] > 0 && resize_size[1] > 0 {
            let size = (resize_size[0], resize_size[1]);
            let size_changed = Some(size) != last_render_size;
            let first_render_size = last_render_size.is_none();
            if size_changed && !first_render_size {
                let now = Instant::now();
                if next_resize_render_at.is_some_and(|deadline| now < deadline) {
                    continue;
                }
                next_resize_render_at = Some(now + RESIZE_RENDER_INTERVAL);
            }
            if size_changed {
                last_render_size = Some(size);
            }
        }

        unsafe {
            if CGLLockContext(cgl_context) != 0 {
                record_render_error(&health, "锁定 CGLContext 失败".to_owned());
                break;
            }
            if CGLSetCurrentContext(cgl_context) != 0 {
                CGLUnlockContext(cgl_context);
                record_render_error(&health, "激活 CGLContext 失败".to_owned());
                break;
            }
            let mut backing_width = 0;
            let mut backing_height = 0;
            let backing_status = ani_mpv_macos_surface_backing_size(
                surface,
                &mut backing_width,
                &mut backing_height,
            );
            let backing_size = (backing_width, backing_height);
            let drawable_needs_update = backing_status != 0
                || Some(backing_size) != last_drawable_size
                || drawable_update_warned;
            if drawable_needs_update {
                let drawable_update_status = CGLUpdateContext(cgl_context);
                if drawable_update_status != 0 && !drawable_update_warned {
                    log::warn!(
                        "macOS MPV 更新 CGLContext drawable 失败，将继续消费渲染回调 status={drawable_update_status}"
                    );
                    drawable_update_warned = true;
                } else if drawable_update_status == 0 {
                    if drawable_update_warned {
                        log::info!("macOS MPV CGLContext drawable 已恢复");
                    }
                    drawable_update_warned = false;
                    if backing_status == 0 && backing_width > 0 && backing_height > 0 {
                        last_drawable_size = Some(backing_size);
                    }
                }
            }
            let update = (api.render_context_update)(render_context);
            record_render_update(&health, update);
            if update & MPV_RENDER_UPDATE_FRAME != 0 {
                match render_frame(&api, render_context, cgl_context, surface, &health) {
                    Ok(true) => record_rendered_frame(&health),
                    Ok(false) => record_render_attempt(&health),
                    Err(error) => record_render_error(&health, error),
                }
            }
            CGLSetCurrentContext(ptr::null_mut());
            CGLUnlockContext(cgl_context);
        }
    }
}

unsafe fn render_frame(
    api: &MpvApi,
    render_context: *mut MpvRenderContext,
    cgl_context: *mut CglContext,
    surface: *mut c_void,
    health: &RenderHealth,
) -> Result<bool, String> {
    let mut size = [0_i32; 2];
    let size_status =
        unsafe { ani_mpv_macos_surface_backing_size(surface, &mut size[0], &mut size[1]) };
    record_backing_size(health, size_status, size);
    if size_status != 0 || size[0] <= 0 || size[1] <= 0 {
        return Ok(false);
    }
    unsafe {
        glViewport(0, 0, size[0], size[1]);
        glClearColor(0.0, 0.0, 0.0, 1.0);
        glClear(GL_COLOR_BUFFER_BIT);
    }
    let mut fbo = MpvOpenGlFbo {
        fbo: 0,
        width: size[0],
        height: size[1],
        internal_format: 0,
    };
    let mut flip_y = 1_i32;
    let mut params = [
        MpvRenderParam {
            kind: MPV_RENDER_PARAM_OPENGL_FBO,
            data: (&mut fbo as *mut MpvOpenGlFbo).cast(),
        },
        MpvRenderParam {
            kind: MPV_RENDER_PARAM_FLIP_Y,
            data: (&mut flip_y as *mut i32).cast(),
        },
        MpvRenderParam {
            kind: MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let status = unsafe { (api.render_context_render)(render_context, params.as_mut_ptr()) };
    if status < 0 {
        return Err(format!(
            "macOS MPV 帧渲染失败：{}",
            api.error_message(status)
        ));
    }
    let flush_status = unsafe { CGLFlushDrawable(cgl_context) };
    if flush_status != 0 {
        return Err(format!(
            "macOS MPV 交换 OpenGL drawable 失败：{flush_status}"
        ));
    }
    unsafe {
        (api.render_context_report_swap)(render_context);
    }
    Ok(true)
}

fn record_rendered_frame(health: &RenderHealth) {
    if let Ok(mut state) = health.state.lock() {
        state.render_attempts = state.render_attempts.saturating_add(1);
        state.rendered_frames = state.rendered_frames.saturating_add(1);
        state.last_error = None;
    }
}

fn record_render_update(health: &RenderHealth, update: u64) {
    if let Ok(mut state) = health.state.lock() {
        state.render_update_calls = state.render_update_calls.saturating_add(1);
        if update & MPV_RENDER_UPDATE_FRAME != 0 {
            state.frame_update_requests = state.frame_update_requests.saturating_add(1);
        }
    }
}

fn record_render_attempt(health: &RenderHealth) {
    if let Ok(mut state) = health.state.lock() {
        state.render_attempts = state.render_attempts.saturating_add(1);
    }
}

fn record_backing_size(health: &RenderHealth, status: c_int, size: [c_int; 2]) {
    if let Ok(mut state) = health.state.lock() {
        state.last_backing_status = Some(status);
        state.last_backing_size = Some((size[0], size[1]));
    }
}

fn record_render_error(health: &RenderHealth, error: String) {
    log::error!("macOS libmpv render API 错误 error={error}");
    if let Ok(mut state) = health.state.lock() {
        state.last_error = Some(error);
    }
}

unsafe extern "C" fn resolve_open_gl_symbol(
    _context: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    unsafe { dlsym((-2_isize) as *mut c_void, name) }
}

unsafe extern "C" {
    fn ani_mpv_macos_surface_create(parent_view: *mut c_void) -> *mut c_void;
    fn ani_mpv_macos_surface_cgl_context(surface: *mut c_void) -> *mut CglContext;
    fn ani_mpv_macos_surface_backing_size(
        surface: *mut c_void,
        width: *mut c_int,
        height: *mut c_int,
    ) -> c_int;
    fn ani_mpv_macos_surface_destroy(surface: *mut c_void);

    fn CGLLockContext(context: *mut CglContext) -> c_int;
    fn CGLUnlockContext(context: *mut CglContext) -> c_int;
    fn CGLSetCurrentContext(context: *mut CglContext) -> c_int;
    fn CGLUpdateContext(context: *mut CglContext) -> c_int;
    fn CGLFlushDrawable(context: *mut CglContext) -> c_int;

    fn glViewport(x: c_int, y: c_int, width: c_int, height: c_int);
    fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
    fn glClear(mask: u32);
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
