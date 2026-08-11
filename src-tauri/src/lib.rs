use std::sync::Arc;

use ani_repository::prelude::*;
use log::LevelFilter;
use tauri::Manager;
#[cfg(target_os = "android")]
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

mod automation;
mod commands;
mod discovery_sync;
mod downloads;
#[cfg(desktop)]
mod external_player;
mod image_cache;
mod media;
mod path_utils;
mod player;
mod qbittorrent_managed;
#[cfg(desktop)]
mod remote;
#[cfg(mobile)]
mod secure_store;
mod source_sync;
mod sources;
mod storage;
mod system_integration;
mod theme_assets;

/// 装配并启动 Tauri 应用宿主。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(not(target_os = "android"))]
    let builder = builder.plugin(
        tauri_plugin_log::Builder::new()
            .level(LevelFilter::Info)
            .build(),
    );
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_ani_mobile::init())
        .plugin(tauri_plugin_ani_player::init())
        .plugin(tauri_plugin_ani_torrent::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        None,
    ));
    let builder = builder.register_asynchronous_uri_scheme_protocol(
        "ani-image",
        image_cache::handle_protocol_request,
    );
    let builder = builder.register_asynchronous_uri_scheme_protocol(
        "ani-theme",
        theme_assets::handle_protocol_request,
    );
    let builder = builder
        .setup(|app| {
            let storage_state = storage::initialize(app.handle())?;
            let theme_asset_state = theme_assets::AppThemeAssetState::initialize(
                storage_state.theme_asset_directory().to_path_buf(),
            )
            .map_err(std::io::Error::other)?;
            #[cfg(target_os = "android")]
            app.handle().plugin(
                tauri_plugin_log::Builder::new()
                    .level(LevelFilter::Info)
                    .rotation_strategy(RotationStrategy::KeepSome(5))
                    .max_file_size(1024 * 1024)
                    .targets([
                        Target::new(TargetKind::Stdout),
                        Target::new(TargetKind::Folder {
                            path: storage_state.log_directory().to_path_buf(),
                            file_name: Some("ani-tracker".to_owned()),
                        }),
                    ])
                    .build(),
            )?;
            #[cfg(target_os = "android")]
            log::info!(
                "Android 文件日志已启用 directory={}",
                storage_state.log_directory().display()
            );
            let startup_settings = storage_state
                .storage()
                .lock()
                .map_err(|error| std::io::Error::other(format!("读取启动设置锁失败：{error}")))?
                .repository()
                .get_settings(storage_state.platform_defaults())
                .map_err(|error| std::io::Error::other(format!("读取启动设置失败：{error}")))?;
            #[cfg(mobile)]
            let image_cache_state = image_cache::AppImageCacheState::initialize(&startup_settings)
                .map_err(std::io::Error::other)?;
            let system_integration_state =
                system_integration::AppSystemIntegrationState::initialize(
                    app.handle(),
                    &startup_settings,
                );
            let source_state = sources::AppSourceState::new();
            let source_sync_state = source_sync::AppSourceSyncState::new(
                Arc::clone(storage_state.storage()),
                storage_state.platform_defaults().clone(),
                source_state.clone(),
            );
            source_sync_state.start();
            let discovery_sync_state = discovery_sync::AppDiscoverySyncState::new(
                app.handle().clone(),
                Arc::clone(storage_state.storage()),
                storage_state.platform_defaults().clone(),
                source_state.clone(),
            );
            discovery_sync_state.start();
            let download_state = downloads::AppDownloadState::new(
                app.handle(),
                Arc::clone(storage_state.storage()),
                storage_state.platform_defaults().clone(),
            )?;
            download_state.start();
            let media_state = media::AppMediaState::new(
                app.handle(),
                Arc::clone(storage_state.storage()),
                storage_state.platform_defaults().clone(),
                source_state.clone(),
            );
            let player_state =
                player::AppPlayerState::new(app.handle(), Arc::clone(storage_state.storage()));
            #[cfg(desktop)]
            let remote_state =
                tauri::async_runtime::block_on(remote::AppRemoteGatewayState::initialize(
                    app.handle(),
                    Arc::clone(storage_state.storage()),
                    storage_state.platform_defaults().clone(),
                    download_state.clone(),
                    media_state.clone(),
                ))
                .unwrap_or_else(|error| {
                    log::error!("Tauri 远程网关初始化失败，应用继续启动 error={error}");
                    remote::AppRemoteGatewayState::unavailable(error)
                });
            let automation_state = automation::AppAutomationState::new(
                app.handle().clone(),
                Arc::clone(storage_state.storage()),
                storage_state.platform_defaults().clone(),
                source_state.clone(),
                Arc::new(automation::TauriAutomaticDownloadExecutor::new(
                    download_state.clone(),
                )),
            );
            automation_state.start();
            let reminder_storage = Arc::clone(storage_state.storage());
            let reminder_defaults = storage_state.platform_defaults().clone();
            let reminder_app = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let result = reminder_storage
                    .lock()
                    .map_err(|error| error.to_string())
                    .and_then(|storage| {
                        let settings = storage
                            .repository()
                            .get_settings(&reminder_defaults)
                            .map_err(|error| error.to_string())?;
                        let record = ani_automation::DailyReminderService::run_once(
                            &storage.repository(),
                            chrono::Utc::now(),
                        )
                        .map_err(|error| error.to_string())?;
                        Ok((record, settings))
                    });
                match result {
                    Ok((Some(record), settings)) => {
                        log::info!("Tauri 每日追番提醒已写入 id={}", record.id);
                        system_integration::notify_reminder(&reminder_app, &settings, &record);
                    }
                    Ok((None, _)) => {}
                    Err(error) => {
                        log::error!("Tauri 每日追番提醒执行失败 error={error}");
                    }
                }
            });
            app.manage(storage_state);
            app.manage(source_state);
            app.manage(source_sync_state);
            app.manage(discovery_sync_state);
            app.manage(download_state);
            app.manage(media_state);
            app.manage(player_state);
            app.manage(automation_state);
            app.manage(system_integration_state);
            app.manage(theme_asset_state);
            #[cfg(mobile)]
            app.manage(image_cache_state);
            #[cfg(desktop)]
            app.manage(remote_state);
            log::info!(
                "Tauri 宿主初始化完成 platform={} arch={}",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::data::get_dashboard,
            commands::data::get_settings,
            commands::data::update_settings,
            commands::data::reset_settings_to_defaults,
            commands::data::list_notifications,
            commands::data::get_unread_notification_count,
            commands::data::mark_notification_read,
            commands::data::mark_all_notifications_read,
            commands::data::clear_notifications,
            commands::data::list_my_anime,
            commands::data::upsert_my_anime,
            commands::data::remove_my_anime,
            commands::data::list_my_anime_watch_progress,
            commands::data::set_anime_watch_progress,
            commands::data::report_playback_progress,
            commands::data::save_playback_checkpoint,
            commands::data::list_episodes,
            commands::data::upsert_episode,
            commands::data::list_episode_preferences,
            commands::data::upsert_episode_preference,
            commands::data::remove_episode_preference,
            commands::data::list_anime_catalog,
            commands::data::search_anime_catalog,
            commands::data::get_anime_season_sync_state,
            commands::data::get_anime_season_sync_task_status,
            commands::data::start_anime_season_sync,
            commands::data::collect_anime_month,
            commands::data::collect_anime_season,
            commands::data::get_anime_detail,
            commands::data::refresh_anime_detail,
            commands::data::list_fansubs,
            commands::data::list_sources,
            commands::data::set_source_enabled,
            commands::data::upsert_source,
            commands::backup::export_database_backup,
            commands::backup::restore_database_backup,
            commands::logs::export_logs,
            commands::themes::save_theme_background,
            commands::themes::resolve_theme_background,
            commands::themes::prune_theme_backgrounds,
            commands::themes::export_theme_package,
            commands::sources::search_releases,
            commands::sources::search_anime_releases,
            commands::sources::search_rss_subscription_releases,
            commands::sources::preview_episode_releases,
            commands::sources::get_anime_source_binding_state,
            commands::sources::confirm_anime_source_binding,
            commands::sources::report_anime_source_candidate_mismatch,
            commands::sources::remove_anime_source_candidate_mismatch,
            commands::sources::set_anime_source_excluded,
            commands::sources::remove_anime_source_binding,
            commands::source_sync::get_source_sync_status,
            commands::source_sync::sync_sources_now,
            commands::automation::get_automation_scheduler_status,
            commands::automation::run_automation_once,
            commands::automation::start_automation_scan,
            commands::automation::restart_automation_scheduler,
            commands::downloads::list_downloads,
            commands::downloads::test_qbittorrent,
            commands::downloads::get_download_service_status,
            commands::downloads::get_qbittorrent_managed_status,
            commands::downloads::start_qbittorrent_managed,
            commands::downloads::stop_qbittorrent_managed,
            commands::downloads::get_embedded_torrent_status,
            commands::downloads::start_embedded_torrent,
            commands::downloads::stop_embedded_torrent,
            commands::downloads::restart_embedded_torrent,
            commands::downloads::refresh_downloads,
            commands::downloads::pause_download,
            commands::downloads::resume_download,
            commands::downloads::remove_download,
            commands::downloads::set_download_file_priority,
            commands::downloads::add_download_url,
            commands::downloads::import_torrent_file,
            commands::downloads::add_release_download,
            commands::media::list_media_files,
            commands::media::scan_download_media,
            commands::media::get_desktop_media_tools_status,
            commands::media::start_local_media_import,
            commands::media::get_local_media_import_status,
            commands::media::confirm_local_media_import,
            commands::media::cancel_local_media_import,
            commands::media::start_media_availability_check,
            commands::media::list_local_media_sources,
            commands::mobile::get_mobile_platform_status,
            commands::mobile::consume_mobile_navigation,
            commands::mobile::consume_mobile_background_refresh,
            commands::mobile::request_mobile_notification_permission,
            commands::player::open_desktop_player_window,
            commands::player::close_desktop_player_window,
            commands::player::drag_desktop_player_window,
            commands::player::toggle_desktop_player_window_maximize,
            commands::player::create_desktop_playback_session,
            commands::player::close_desktop_playback_session,
            commands::player::get_desktop_player_capabilities,
            commands::player::get_desktop_player_snapshot,
            commands::player::dispatch_desktop_player_command,
            commands::remote::get_remote_gateway_status,
            commands::remote::create_remote_pairing_code,
            commands::remote::revoke_remote_device,
            commands::remote::resolve_cached_image_url,
            commands::remote::invalidate_cached_image_url,
            commands::window::get_window_state,
            commands::window::minimize_window,
            commands::window::toggle_maximize_window,
            commands::window::close_window,
            commands::external::open_external,
            commands::external::detect_players,
            commands::external::select_player_executable,
            commands::external::play_media,
            commands::external::reveal_media
        ])
        .on_window_event(|window, event| {
            commands::handle_window_event(window, event);
            player::AppPlayerState::handle_window_event(window, event);
            system_integration::handle_window_event(window, event);
        });

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("[tauri] 应用宿主构建失败: {error}");
            return;
        }
    };
    app.run(|app_handle, event| {
        #[cfg(target_os = "macos")]
        if matches!(event, tauri::RunEvent::Reopen { .. }) {
            system_integration::show_main_window(app_handle);
        }
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(state) =
                app_handle.try_state::<system_integration::AppSystemIntegrationState>()
            {
                state.prepare_to_quit();
            }
            #[cfg(desktop)]
            {
                log::info!("Tauri 桌面宿主退出，开始关闭播放器和下载引擎");
                let player_state = app_handle.state::<player::AppPlayerState>();
                let download_state = app_handle.state::<downloads::AppDownloadState>();
                tauri::async_runtime::block_on(async {
                    if let Some(remote_state) =
                        app_handle.try_state::<remote::AppRemoteGatewayState>()
                    {
                        remote_state.shutdown().await;
                    }
                    player_state.shutdown().await;
                    download_state.shutdown().await;
                });
            }
            #[cfg(mobile)]
            log::info!("Tauri 移动宿主退出，原生播放器和下载资源交由平台生命周期释放");
        }
        if matches!(event, tauri::RunEvent::Resumed) {
            if let Some(state) = app_handle.try_state::<discovery_sync::AppDiscoverySyncState>() {
                state.wake();
            }
        }
    });
}
