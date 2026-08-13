use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ani_contracts::{
    PlayerCapabilities, PlayerCommand, PlayerCommandAction, PlayerCommandResult, PlayerSnapshot,
};
use ani_media::player::{PlayerTransport, PlayerTransportError};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tauri::{plugin::PluginApi, AppHandle, Manager, Runtime};

use crate::desktop_runtime::DesktopPlayerTransport;
use crate::mpv_runtime::{MpvPlayerTransport, MpvRuntimeConfig};

/// libVLC 视频输出绑定的平台原生窗口句柄。
#[derive(Debug, Clone, Copy)]
pub enum DesktopVideoTarget {
    Windows(isize),
    MacOs(usize),
    X11(u32),
}

/// 桌面控制层窗口由应用宿主实现的最小操作端口。
pub trait DesktopWindowController: Send + Sync {
    /// 同步视频窗口和控制层的全屏状态。
    fn set_fullscreen(&self, fullscreen: bool) -> Result<bool, String>;
    /// 关闭视频窗口和控制层窗口。
    fn close(&self) -> Result<(), String>;
}

/// 保存应用句柄并按播放窗口创建独立 libVLC transport。
pub struct AniPlayer<R: Runtime>(AppHandle<R>);

impl<R: Runtime> AniPlayer<R> {
    /// 为一个原生视频窗口创建播放器 transport。
    pub fn create_desktop_transport(
        &self,
        target: DesktopVideoTarget,
        controller: Arc<dyn DesktopWindowController>,
    ) -> Arc<dyn PlayerTransport> {
        if cfg!(target_os = "macos") {
            log::info!("macOS 继续使用已验证的 libVLC transport，等待 libmpv render API 接入");
            return Arc::new(DesktopPlayerTransport::new(
                target,
                controller,
                desktop_runtime_roots(&self.0),
            ));
        }
        let mpv = MpvPlayerTransport::new(
            target,
            controller.clone(),
            MpvRuntimeConfig {
                library_roots: desktop_mpv_runtime_roots(&self.0),
                shader_roots: desktop_shader_roots(&self.0),
            },
        );
        if mpv.is_available() {
            log::info!("Tauri 桌面播放器已选择 libmpv transport");
            Arc::new(DesktopFallbackTransport {
                mpv: Arc::new(mpv),
                vlc: Mutex::new(None),
                target,
                controller,
                vlc_roots: desktop_runtime_roots(&self.0),
                fallback_active: AtomicBool::new(false),
                last_load: Mutex::new(None),
                last_mpv_sequence: AtomicU64::new(0),
                fallback_sequence_offset: AtomicU64::new(0),
            })
        } else {
            log::warn!("Tauri 桌面播放器回退到 libVLC transport");
            Arc::new(DesktopPlayerTransport::new(
                target,
                controller,
                desktop_runtime_roots(&self.0),
            ))
        }
    }
}

/// 首次媒体加载失败时把桌面会话从 libmpv 原子切换到兼容 libVLC。
struct DesktopFallbackTransport {
    mpv: Arc<MpvPlayerTransport>,
    vlc: Mutex<Option<Arc<DesktopPlayerTransport>>>,
    target: DesktopVideoTarget,
    controller: Arc<dyn DesktopWindowController>,
    vlc_roots: Vec<PathBuf>,
    fallback_active: AtomicBool,
    last_load: Mutex<Option<PlayerCommand>>,
    last_mpv_sequence: AtomicU64,
    fallback_sequence_offset: AtomicU64,
}

impl DesktopFallbackTransport {
    fn fallback(&self) -> Result<Arc<DesktopPlayerTransport>, PlayerTransportError> {
        let mut fallback = self
            .vlc
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))?;
        Ok(fallback
            .get_or_insert_with(|| {
                Arc::new(DesktopPlayerTransport::new(
                    self.target,
                    self.controller.clone(),
                    self.vlc_roots.clone(),
                ))
            })
            .clone())
    }

    fn active_fallback(&self) -> Result<Arc<DesktopPlayerTransport>, PlayerTransportError> {
        self.fallback()
    }

    async fn fallback_snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        let snapshot = self.active_fallback()?.snapshot().await?;
        Ok(snapshot.map(|mut snapshot| {
            snapshot.sequence = fallback_snapshot_sequence(
                self.fallback_sequence_offset.load(Ordering::Acquire),
                snapshot.sequence,
            );
            snapshot
        }))
    }
}

fn fallback_snapshot_sequence(mpv_sequence: u64, vlc_sequence: u64) -> u64 {
    mpv_sequence.saturating_add(vlc_sequence)
}

#[async_trait]
impl PlayerTransport for DesktopFallbackTransport {
    async fn capabilities(&self) -> Result<PlayerCapabilities, PlayerTransportError> {
        if self.fallback_active.load(Ordering::Acquire) {
            self.active_fallback()?.capabilities().await
        } else {
            self.mpv.capabilities().await
        }
    }

    async fn dispatch(
        &self,
        command: PlayerCommand,
    ) -> Result<PlayerCommandResult, PlayerTransportError> {
        if self.fallback_active.load(Ordering::Acquire) {
            return self.active_fallback()?.dispatch(command).await;
        }
        if matches!(command.action, PlayerCommandAction::Load { .. }) {
            self.last_load
                .lock()
                .map_err(|error| PlayerTransportError::Native(error.to_string()))?
                .replace(command.clone());
        }
        let fallback_command = command.clone();
        match self.mpv.dispatch(command).await {
            Ok(result) => Ok(result),
            Err(error) if matches!(fallback_command.action, PlayerCommandAction::Load { .. }) => {
                log::warn!(
                    "libmpv 首次媒体加载失败，切换到 libVLC session_id={} error={error}",
                    fallback_command.session_id
                );
                self.mpv.shutdown().await?;
                let fallback = self.fallback()?;
                let capabilities = fallback.capabilities().await?;
                if capabilities.availability != ani_contracts::PlayerAvailability::Available {
                    return Err(PlayerTransportError::Unavailable(
                        capabilities
                            .unavailable_reason
                            .unwrap_or_else(|| "libmpv 与 libVLC 均不可用".to_owned()),
                    ));
                }
                self.fallback_active.store(true, Ordering::Release);
                fallback.dispatch(fallback_command).await
            }
            Err(error) => Err(error),
        }
    }

    async fn snapshot(&self) -> Result<Option<PlayerSnapshot>, PlayerTransportError> {
        if self.fallback_active.load(Ordering::Acquire) {
            self.fallback_snapshot().await
        } else {
            match self.mpv.snapshot().await {
                Ok(snapshot) => {
                    if let Some(snapshot) = &snapshot {
                        self.last_mpv_sequence
                            .store(snapshot.sequence, Ordering::Release);
                        if snapshot.status != ani_contracts::PlayerStatus::Loading {
                            self.last_load
                                .lock()
                                .map_err(|error| PlayerTransportError::Native(error.to_string()))?
                                .take();
                        }
                    }
                    Ok(snapshot)
                }
                Err(error @ PlayerTransportError::LoadFailed(_)) => {
                    let load = self
                        .last_load
                        .lock()
                        .map_err(|lock_error| PlayerTransportError::Native(lock_error.to_string()))?
                        .clone();
                    let Some(load) = load else {
                        return Err(error);
                    };
                    log::warn!(
                        "libmpv 异步加载失败，切换到 libVLC session_id={} error={error}",
                        load.session_id
                    );
                    self.mpv.shutdown().await?;
                    let fallback = self.fallback()?;
                    let capabilities = fallback.capabilities().await?;
                    if capabilities.availability != ani_contracts::PlayerAvailability::Available {
                        return Err(error);
                    }
                    fallback.dispatch(load).await?;
                    self.fallback_sequence_offset.store(
                        self.last_mpv_sequence.load(Ordering::Acquire),
                        Ordering::Release,
                    );
                    self.fallback_active.store(true, Ordering::Release);
                    self.fallback_snapshot().await
                }
                Err(error) => Err(error),
            }
        }
    }

    async fn shutdown(&self) -> Result<(), PlayerTransportError> {
        self.mpv.shutdown().await?;
        let fallback = self
            .vlc
            .lock()
            .map_err(|error| PlayerTransportError::Native(error.to_string()))?
            .clone();
        if let Some(fallback) = fallback {
            fallback.shutdown().await?;
        }
        Ok(())
    }
}

fn desktop_mpv_runtime_roots<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from));
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let platform = platform_directory();
    let mut roots = Vec::new();
    if let Some(configured) = std::env::var_os("ANI_LIBMPV_DIR") {
        push_unique_root(&mut roots, PathBuf::from(configured));
    }
    if let Ok(resource_directory) = app.path().resource_dir() {
        push_unique_root(
            &mut roots,
            resource_directory.join("libmpv").join(&platform),
        );
    }
    if let Some(executable_directory) = executable_directory {
        push_unique_root(
            &mut roots,
            executable_directory.join("libmpv").join(&platform),
        );
    }
    push_unique_root(&mut roots, current.join("out/libmpv").join(&platform));
    push_unique_root(&mut roots, current.join("resources/libmpv").join(&platform));
    roots
}

fn desktop_shader_roots<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(configured) = std::env::var_os("ANI_ANIME4K_SHADER_DIR") {
        push_unique_root(&mut roots, PathBuf::from(configured));
    }
    if let Ok(resource_directory) = app.path().resource_dir() {
        push_unique_root(&mut roots, resource_directory.join("shaders/anime4k"));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            push_unique_root(&mut roots, directory.join("shaders/anime4k"));
        }
    }
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    push_unique_root(&mut roots, current.join("resources/shaders/anime4k"));
    push_unique_root(&mut roots, current.join("../resources/shaders/anime4k"));
    roots
}

/// 从任意 Tauri Manager 读取播放器插件句柄。
pub trait AniPlayerExt<R: Runtime> {
    /// 返回当前宿主持有的平台播放器插件。
    fn ani_player(&self) -> &AniPlayer<R>;
}

impl<R: Runtime, T: Manager<R>> AniPlayerExt<R> for T {
    fn ani_player(&self) -> &AniPlayer<R> {
        self.state::<AniPlayer<R>>().inner()
    }
}

/// 注册桌面插件句柄，不向 Renderer 暴露原生 FFI。
pub fn init<R: Runtime, C: DeserializeOwned>(
    app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<AniPlayer<R>> {
    Ok(AniPlayer(app.clone()))
}

fn desktop_runtime_roots<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from));
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    runtime_roots_for(
        &platform_directory(),
        std::env::var_os("ANI_LIBVLC_DIR").map(PathBuf::from),
        executable_directory,
        app.path().resource_dir().ok(),
        current,
    )
}

/// 生成 libVLC 搜索根，兼容自定义 Cargo target-dir 的开发产物目录。
fn runtime_roots_for(
    platform: &str,
    configured: Option<PathBuf>,
    executable_directory: Option<PathBuf>,
    resource_directory: Option<PathBuf>,
    current_directory: PathBuf,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(configured) = configured {
        push_unique_root(&mut roots, configured);
    }
    if let Some(resource_directory) = resource_directory {
        push_unique_root(&mut roots, resource_directory.join("libvlc").join(platform));
    }
    if let Some(executable_directory) = executable_directory {
        push_unique_root(
            &mut roots,
            executable_directory.join("libvlc").join(platform),
        );
    }
    push_unique_root(
        &mut roots,
        current_directory.join("out/libvlc").join(platform),
    );
    push_unique_root(
        &mut roots,
        current_directory.join("resources/libvlc").join(platform),
    );
    roots
}

/// 追加尚未出现的运行时根目录，避免重复探测与重复日志。
fn push_unique_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
}

/// 返回当前桌面目标对应的资源目录名。
pub(crate) fn platform_directory() -> String {
    platform_directory_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// 将 Rust 平台和架构名称转换为桌面资源目录名。
fn platform_directory_for(os: &str, arch: &str) -> String {
    let platform = match os {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        value => value,
    };
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        value => value,
    };
    format!("{platform}-{arch}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{fallback_snapshot_sequence, platform_directory_for, runtime_roots_for};

    #[test]
    fn preserves_monotonic_sequence_after_fallback() {
        assert_eq!(fallback_snapshot_sequence(8, 1), 9);
        assert_eq!(fallback_snapshot_sequence(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn maps_supported_desktop_resource_directories() {
        assert_eq!(platform_directory_for("windows", "x86_64"), "win32-x64");
        assert_eq!(platform_directory_for("macos", "x86_64"), "darwin-x64");
        assert_eq!(platform_directory_for("macos", "aarch64"), "darwin-arm64");
        assert_eq!(platform_directory_for("linux", "x86_64"), "linux-x64");
    }

    #[test]
    fn resolves_runtime_next_to_custom_cargo_target_executable() {
        let roots = runtime_roots_for(
            "darwin-x64",
            Some(PathBuf::from("/override/vlc")),
            Some(PathBuf::from("/repo/out/cargo-target/debug")),
            Some(PathBuf::from("/bundle/Contents/Resources")),
            PathBuf::from("/repo/src-tauri"),
        );

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/override/vlc"),
                PathBuf::from("/bundle/Contents/Resources/libvlc/darwin-x64"),
                PathBuf::from("/repo/out/cargo-target/debug/libvlc/darwin-x64"),
                PathBuf::from("/repo/src-tauri/out/libvlc/darwin-x64"),
                PathBuf::from("/repo/src-tauri/resources/libvlc/darwin-x64"),
            ]
        );
    }
}
