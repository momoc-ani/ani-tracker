use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

#[cfg(desktop)]
mod desktop;
#[cfg(desktop)]
mod desktop_runtime;
#[cfg(mobile)]
mod mobile;
#[cfg(desktop)]
mod mpv_runtime;

mod error;

pub use error::{Error, Result};

#[cfg(desktop)]
pub use desktop::{AniPlayer, AniPlayerExt, DesktopVideoTarget, DesktopWindowController};
#[cfg(mobile)]
pub use mobile::{AniPlayer, AniPlayerExt};

/// 初始化内部播放器插件；Renderer 只能调用应用层业务命令。
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("ani-player")
        .setup(|app, api| {
            #[cfg(mobile)]
            let ani_player = mobile::init(app, api)?;
            #[cfg(desktop)]
            let ani_player = desktop::init(app, api)?;
            app.manage(ani_player);
            Ok(())
        })
        .build()
}
