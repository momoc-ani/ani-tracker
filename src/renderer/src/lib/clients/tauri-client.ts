import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppClient } from "@shared/app-client";
import type {
  AddDownloadUrlInput,
  AddReleaseDownloadInput,
  AnimeReleaseQuery,
  AnimeDetailResult,
  AnimeDiscoveryQuery,
  AnimeDiscoveryResult,
  AnimeDiscoverySearchResult,
  AnimeDiscoverySeasonQuery,
  AnimeDiscoverySeasonResult,
  AnimeDiscoverySyncTaskStatus,
  AnimeSeasonSyncState,
  AnimeSourceBindingState,
  AnimeWatchProgress,
  AutomationRunResult,
  AutomationSchedulerStatus,
  AppWindowState,
  BangumiBrowseQuery,
  BangumiBrowseResult,
  ConfirmAnimeSourceBindingInput,
  DesktopPlaybackSessionInput,
  DesktopPlayerWindowDragInput,
  DesktopPlayerWindowInput,
  DesktopMediaToolsStatus,
  DownloadServiceStatus,
  EmbeddedTorrentCoreStatus,
  ExportThemePackageInput,
  EpisodeReleasePreview,
  ImageCacheResolveResult,
  LocalMediaImportJobStatus,
  LocalMediaImportSelection,
  LocalMediaSourceSummary,
  MediaScanResult,
  MobileNavigationIntent,
  MobileNotificationPermission,
  MobilePlatformStatus,
  PlaybackCheckpoint,
  PlayerDetectionResult,
  QbittorrentManagedStatus,
  RemoteGatewayStatus,
  RemotePairingChallenge,
  RemotePlaybackSession,
  ReleaseQuery,
  ReleaseSearchResult,
  RemoveAnimeSourceCandidateMismatchInput,
  ReportPlaybackProgressInput,
  ReportAnimeSourceCandidateMismatchInput,
  RssSubscriptionReleaseQuery,
  RssSubscriptionReleaseResult,
  SavePlaybackCheckpointInput,
  SaveThemeBackgroundInput,
  SelectPlayerExecutableInput,
  SetAnimeSourceExclusionInput,
  SetAnimeWatchProgressInput,
  SourceSyncRunResult,
  SourceSyncSchedulerStatus,
  ThemeBackgroundAsset,
  ThemeBackgroundReference,
  TorrentConnectionTestResult
} from "@shared/contracts";
import type {
  Anime,
  AppSettings,
  DashboardData,
  DownloadTask,
  Episode,
  EpisodePreference,
  FansubGroup,
  MyAnime,
  MediaFile,
  NotificationRecord,
  ReleaseSourceConfig
} from "@shared/domain";
import type {
  PlayerCapabilities,
  PlayerCommand,
  PlayerCommandResult,
  PlayerSnapshot
} from "@shared/player-contract";
import { emitManualDownloadAdded } from "@/lib/mobile-download-notification";

const WINDOW_STATE_CHANGED_EVENT = "window-state-changed";
const DOWNLOAD_SERVICE_STATUS_CHANGED_EVENT = "download-service-status-changed";
const PLAYER_SNAPSHOT_EVENT = "player-snapshot";
const LOCAL_MEDIA_IMPORT_STATUS_CHANGED_EVENT = "local-media-import-status-changed";

interface TauriCommandError {
  code?: string;
  message?: string;
}

type TauriClientPlatform = "tauri-desktop" | "android" | "ios";

/** 将 Tauri 拒绝值转换为可展示错误。 */
function normalizeTauriError(method: string, error: unknown): Error {
  if (error && typeof error === "object") {
    const commandError = error as TauriCommandError;
    if (commandError.message) {
      return new Error(commandError.message);
    }
  }
  return new Error(`Tauri 命令 ${method} 执行失败：${String(error)}`);
}

/** 封装 P1 已开放的 Tauri 平台命令与事件。 */
class TauriClientCore implements AppClient {
  private desktopPlayerDragQueue: Promise<void> = Promise.resolve();

  /** 保存当前 Tauri 宿主对应的平台标识。 */
  constructor(readonly platform: TauriClientPlatform) {}

  /** 读取 Tauri 主窗口状态。 */
  async getWindowState(): Promise<AppWindowState> {
    return invoke<AppWindowState>("get_window_state").catch((error) => {
      throw normalizeTauriError("get_window_state", error);
    });
  }

  /** 最小化 Tauri 主窗口。 */
  async minimizeWindow(): Promise<void> {
    return invoke<void>("minimize_window").catch((error) => {
      throw normalizeTauriError("minimize_window", error);
    });
  }

  /** 切换 Tauri 主窗口最大化状态。 */
  async toggleMaximizeWindow(): Promise<AppWindowState> {
    return invoke<AppWindowState>("toggle_maximize_window").catch((error) => {
      throw normalizeTauriError("toggle_maximize_window", error);
    });
  }

  /** 关闭 Tauri 主窗口。 */
  async closeWindow(): Promise<void> {
    return invoke<void>("close_window").catch((error) => {
      throw normalizeTauriError("close_window", error);
    });
  }

  /** 订阅 Tauri 主窗口最大化状态变化。 */
  onWindowStateChanged(listener: (state: AppWindowState) => void): () => void {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;

    void listen<AppWindowState>(WINDOW_STATE_CHANGED_EVENT, (event) => listener(event.payload))
      .then((disposeListener) => {
        if (disposed) {
          disposeListener();
          return;
        }
        unlisten = disposeListener;
      })
      .catch((error) => {
        console.error("[tauri-client] 窗口状态订阅失败", error);
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  /** 读取移动宿主的网络、存储、方向、权限和生命周期状态。 */
  async getMobilePlatformStatus(): Promise<MobilePlatformStatus> {
    return invoke<MobilePlatformStatus>("get_mobile_platform_status").catch((error) => {
      throw normalizeTauriError("get_mobile_platform_status", error);
    });
  }

  /** 原子读取并清除原生通知导航。 */
  async consumeMobileNavigation(): Promise<MobileNavigationIntent | undefined> {
    return invoke<MobileNavigationIntent | null>("consume_mobile_navigation")
      .then((intent) => intent ?? undefined)
      .catch((error) => {
        throw normalizeTauriError("consume_mobile_navigation", error);
      });
  }

  /** 原子读取并清除移动后台调度要求的前台补跑标记。 */
  async consumeMobileBackgroundRefresh(): Promise<boolean> {
    return invoke<boolean>("consume_mobile_background_refresh").catch((error) => {
      throw normalizeTauriError("consume_mobile_background_refresh", error);
    });
  }

  /** 由用户操作请求移动通知权限。 */
  async requestMobileNotificationPermission(): Promise<MobileNotificationPermission> {
    return invoke<MobileNotificationPermission>("request_mobile_notification_permission").catch((error) => {
      throw normalizeTauriError("request_mobile_notification_permission", error);
    });
  }

  /** 使用系统默认程序打开外部 HTTP 或 HTTPS 链接。 */
  async openExternal(url: string): Promise<void> {
    return invoke<void>("open_external", { url }).catch((error) => {
      throw normalizeTauriError("open_external", error);
    });
  }

  /** 将桌面图片地址解析为 Rust 网关签名缓存地址。 */
  async resolveCachedImageUrl(sourceUrl: string): Promise<ImageCacheResolveResult> {
    return invoke<ImageCacheResolveResult>("resolve_cached_image_url", { sourceUrl }).catch((error) => {
      throw normalizeTauriError("resolve_cached_image_url", error);
    });
  }

  /** 删除解码失败对应的宿主图片缓存。 */
  async invalidateCachedImageUrl(sourceUrl: string): Promise<void> {
    return invoke<void>("invalidate_cached_image_url", { sourceUrl }).catch((error) => {
      throw normalizeTauriError("invalidate_cached_image_url", error);
    });
  }

  /** 将规范化主题背景写入应用私有目录。 */
  async saveThemeBackground(input: SaveThemeBackgroundInput): Promise<ThemeBackgroundAsset> {
    return invoke<ThemeBackgroundAsset>("save_theme_background", { input }).catch((error) => {
      throw normalizeTauriError("save_theme_background", error);
    });
  }

  /** 返回主题 JSON 引用的受控本地图片地址。 */
  async resolveThemeBackground(themeId: string, fileName: string): Promise<ThemeBackgroundAsset | undefined> {
    return invoke<ThemeBackgroundAsset | null>("resolve_theme_background", { themeId, fileName })
      .then((asset) => asset ?? undefined)
      .catch((error) => {
        throw normalizeTauriError("resolve_theme_background", error);
      });
  }

  /** 清理设置中已不再引用的主题背景图片。 */
  async pruneThemeBackgrounds(references: ThemeBackgroundReference[]): Promise<void> {
    return invoke<void>("prune_theme_backgrounds", { references }).catch((error) => {
      throw normalizeTauriError("prune_theme_backgrounds", error);
    });
  }

  /** 通过桌面或移动系统文件面板导出主题包。 */
  async exportThemePackage(input: ExportThemePackageInput): Promise<string | undefined> {
    return invoke<string | null>("export_theme_package", { input })
      .then((fileName) => fileName ?? undefined)
      .catch((error) => {
        throw normalizeTauriError("export_theme_package", error);
      });
  }

  /** 读取桌面远程网关、证书和设备状态。 */
  async getRemoteGatewayStatus(): Promise<RemoteGatewayStatus> {
    return invoke<RemoteGatewayStatus>("get_remote_gateway_status").catch((error) => {
      throw normalizeTauriError("get_remote_gateway_status", error);
    });
  }

  /** 创建桌面远程网关的一次性配对码。 */
  async createRemotePairingCode(): Promise<RemotePairingChallenge> {
    return invoke<RemotePairingChallenge>("create_remote_pairing_code").catch((error) => {
      throw normalizeTauriError("create_remote_pairing_code", error);
    });
  }

  /** 吊销一个桌面远程设备及其令牌。 */
  async revokeRemoteDevice(deviceId: string): Promise<RemoteGatewayStatus> {
    return invoke<RemoteGatewayStatus>("revoke_remote_device", { deviceId }).catch((error) => {
      throw normalizeTauriError("revoke_remote_device", error);
    });
  }

  /** 从 Rust SQLite Repository 读取首页聚合数据。 */
  async getDashboard(): Promise<DashboardData> {
    return invoke<DashboardData>("get_dashboard").catch((error) => {
      throw normalizeTauriError("get_dashboard", error);
    });
  }

  /** 从 Rust SQLite Repository 读取当前平台设置。 */
  async getSettings(): Promise<AppSettings> {
    return invoke<AppSettings>("get_settings").catch((error) => {
      throw normalizeTauriError("get_settings", error);
    });
  }

  /** 递归合并应用设置，并由 Rust 保护宿主路径。 */
  async updateSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
    return invoke<AppSettings>("update_settings", { patch }).catch((error) => {
      throw normalizeTauriError("update_settings", error);
    });
  }

  /** 恢复当前 Tauri 平台默认设置。 */
  async resetSettingsToDefaults(): Promise<AppSettings> {
    return invoke<AppSettings>("reset_settings_to_defaults").catch((error) => {
      throw normalizeTauriError("reset_settings_to_defaults", error);
    });
  }

  /** 导出包含 WAL 内容的一致性 SQLite 备份。 */
  async exportDatabaseBackup(): Promise<string | null> {
    return invoke<string | null>("export_database_backup").catch((error) => {
      throw normalizeTauriError("export_database_backup", error);
    });
  }

  /** 恢复用户选择的 SQLite 备份并重新装配运行设置。 */
  async restoreDatabaseBackup(): Promise<string | null> {
    return invoke<string | null>("restore_database_backup").catch((error) => {
      throw normalizeTauriError("restore_database_backup", error);
    });
  }

  /** 导出当前及轮转日志，不执行分析或上传。 */
  async exportLogs(): Promise<string | null> {
    return invoke<string | null>("export_logs").catch((error) => {
      throw normalizeTauriError("export_logs", error);
    });
  }

  /** 从 Rust SQLite Repository 读取提醒中心通知。 */
  async listNotifications(): Promise<NotificationRecord[]> {
    return invoke<NotificationRecord[]>("list_notifications").catch((error) => {
      throw normalizeTauriError("list_notifications", error);
    });
  }

  /** 从 Rust SQLite Repository 读取未读通知数量。 */
  async getUnreadNotificationCount(): Promise<number> {
    return invoke<number>("get_unread_notification_count").catch((error) => {
      throw normalizeTauriError("get_unread_notification_count", error);
    });
  }

  /** 将一条提醒中心通知标记为已读。 */
  async markNotificationRead(notificationId: string): Promise<NotificationRecord[]> {
    return invoke<NotificationRecord[]>("mark_notification_read", { notificationId }).catch((error) => {
      throw normalizeTauriError("mark_notification_read", error);
    });
  }

  /** 将提醒中心全部通知标记为已读。 */
  async markAllNotificationsRead(): Promise<NotificationRecord[]> {
    return invoke<NotificationRecord[]>("mark_all_notifications_read").catch((error) => {
      throw normalizeTauriError("mark_all_notifications_read", error);
    });
  }

  /** 清空提醒中心全部通知。 */
  async clearNotifications(): Promise<NotificationRecord[]> {
    return invoke<NotificationRecord[]>("clear_notifications").catch((error) => {
      throw normalizeTauriError("clear_notifications", error);
    });
  }

  /** 从 Rust SQLite Repository 读取我的追番。 */
  async listMyAnime(): Promise<MyAnime[]> {
    return invoke<MyAnime[]>("list_my_anime").catch((error) => {
      throw normalizeTauriError("list_my_anime", error);
    });
  }

  /** 通过 Rust 事务新增或更新追番规则。 */
  async upsertMyAnime(item: MyAnime): Promise<MyAnime[]> {
    return invoke<MyAnime[]>("upsert_my_anime", { item }).catch((error) => {
      throw normalizeTauriError("upsert_my_anime", error);
    });
  }

  /** 删除追番及其单集业务数据。 */
  async removeMyAnime(itemId: string): Promise<MyAnime[]> {
    return invoke<MyAnime[]>("remove_my_anime", { itemId }).catch((error) => {
      throw normalizeTauriError("remove_my_anime", error);
    });
  }

  /** 读取全部追番观看进度。 */
  async listMyAnimeWatchProgress(): Promise<AnimeWatchProgress[]> {
    return invoke<AnimeWatchProgress[]>("list_my_anime_watch_progress").catch((error) => {
      throw normalizeTauriError("list_my_anime_watch_progress", error);
    });
  }

  /** 原子调整一部追番的已看集数。 */
  async setAnimeWatchProgress(input: SetAnimeWatchProgressInput): Promise<AnimeWatchProgress> {
    return invoke<AnimeWatchProgress>("set_anime_watch_progress", { input }).catch((error) => {
      throw normalizeTauriError("set_anime_watch_progress", error);
    });
  }

  /** 将达到阈值的播放进度回写为单集已看状态。 */
  async reportPlaybackProgress(input: ReportPlaybackProgressInput): Promise<boolean> {
    return invoke<boolean>("report_playback_progress", { input }).catch((error) => {
      throw normalizeTauriError("report_playback_progress", error);
    });
  }

  /** 保存当前下载文件的续播检查点。 */
  async savePlaybackCheckpoint(input: SavePlaybackCheckpointInput): Promise<PlaybackCheckpoint> {
    return invoke<PlaybackCheckpoint>("save_playback_checkpoint", { input }).catch((error) => {
      throw normalizeTauriError("save_playback_checkpoint", error);
    });
  }

  /** 读取指定番剧单集。 */
  async listEpisodes(animeId: string): Promise<Episode[]> {
    return invoke<Episode[]>("list_episodes", { animeId }).catch((error) => {
      throw normalizeTauriError("list_episodes", error);
    });
  }

  /** 新增或更新单集。 */
  async upsertEpisode(episode: Episode): Promise<Episode[]> {
    return invoke<Episode[]>("upsert_episode", { episode }).catch((error) => {
      throw normalizeTauriError("upsert_episode", error);
    });
  }

  /** 读取指定番剧的单集级规则。 */
  async listEpisodePreferences(animeId: string): Promise<EpisodePreference[]> {
    return invoke<EpisodePreference[]>("list_episode_preferences", { animeId }).catch((error) => {
      throw normalizeTauriError("list_episode_preferences", error);
    });
  }

  /** 新增或更新单集级规则。 */
  async upsertEpisodePreference(preference: EpisodePreference): Promise<EpisodePreference[]> {
    return invoke<EpisodePreference[]>("upsert_episode_preference", { preference }).catch((error) => {
      throw normalizeTauriError("upsert_episode_preference", error);
    });
  }

  /** 删除单集级规则。 */
  async removeEpisodePreference(episodeId: string): Promise<EpisodePreference[]> {
    return invoke<EpisodePreference[]>("remove_episode_preference", { episodeId }).catch((error) => {
      throw normalizeTauriError("remove_episode_preference", error);
    });
  }

  /** 使用 Rust 来源搜索与评分核心预览单集候选资源。 */
  async previewEpisodeReleases(animeId: string, episodeId: string): Promise<EpisodeReleasePreview> {
    return invoke<EpisodeReleasePreview>("preview_episode_releases", { animeId, episodeId }).catch((error) => {
      throw normalizeTauriError("preview_episode_releases", error);
    });
  }

  /** 按可选年月读取 Rust SQLite 番剧目录。 */
  async listAnimeCatalog(year?: number, month?: number): Promise<Anime[]> {
    return invoke<Anime[]>("list_anime_catalog", { year, month }).catch((error) => {
      throw normalizeTauriError("list_anime_catalog", error);
    });
  }

  /** 按标题、原名和别名搜索本地番剧目录。 */
  async searchAnimeCatalog(keyword: string): Promise<AnimeDiscoverySearchResult> {
    return invoke<AnimeDiscoverySearchResult>("search_anime_catalog", { keyword }).catch((error) => {
      throw normalizeTauriError("search_anime_catalog", error);
    });
  }

  /** 直接请求 Bangumi 在线浏览接口。 */
  async browseBangumiAnime(query: BangumiBrowseQuery): Promise<BangumiBrowseResult> {
    return invoke<BangumiBrowseResult>("browse_bangumi_anime", { query }).catch((error) => {
      throw normalizeTauriError("browse_bangumi_anime", error);
    });
  }

  /** 保存 Bangumi 追番，并由宿主启动后台元数据补全。 */
  async followBangumiAnime(item: MyAnime): Promise<MyAnime[]> {
    return invoke<MyAnime[]>("follow_bangumi_anime", { item }).catch((error) => {
      throw normalizeTauriError("follow_bangumi_anime", error);
    });
  }

  /** 使用 Rust 多来源元数据服务采集指定月份。 */
  async collectAnimeMonth(query: AnimeDiscoveryQuery): Promise<AnimeDiscoveryResult> {
    return invoke<AnimeDiscoveryResult>("collect_anime_month", { query }).catch((error) => {
      throw normalizeTauriError("collect_anime_month", error);
    });
  }

  /** 使用 Rust 多来源元数据服务采集指定季度。 */
  async collectAnimeSeason(query: AnimeDiscoverySeasonQuery): Promise<AnimeDiscoverySeasonResult> {
    return invoke<AnimeDiscoverySeasonResult>("collect_anime_season", { query }).catch((error) => {
      throw normalizeTauriError("collect_anime_season", error);
    });
  }

  /** 将季度采集交给 Rust 宿主后台执行。 */
  async startAnimeSeasonSync(query: AnimeDiscoverySeasonQuery): Promise<AnimeDiscoverySyncTaskStatus> {
    return invoke<AnimeDiscoverySyncTaskStatus>("start_anime_season_sync", { query }).catch((error) => {
      throw normalizeTauriError("start_anime_season_sync", error);
    });
  }

  /** 读取 Rust 季度采集后台任务状态。 */
  async getAnimeSeasonSyncTaskStatus(): Promise<AnimeDiscoverySyncTaskStatus> {
    return invoke<AnimeDiscoverySyncTaskStatus>("get_anime_season_sync_task_status").catch((error) => {
      throw normalizeTauriError("get_anime_season_sync_task_status", error);
    });
  }

  /** 读取指定季度的持久化后台同步状态。 */
  async getAnimeSeasonSyncState(
    year: number,
    season: AnimeDiscoverySeasonQuery["season"]
  ): Promise<AnimeSeasonSyncState | undefined> {
    return invoke<AnimeSeasonSyncState | null>("get_anime_season_sync_state", { year, season })
      .then((state) => state ?? undefined)
      .catch((error) => {
        throw normalizeTauriError("get_anime_season_sync_state", error);
      });
  }

  /** 读取番剧详情页所需的本地聚合数据。 */
  async getAnimeDetail(animeId: string): Promise<AnimeDetailResult> {
    return invoke<AnimeDetailResult>("get_anime_detail", { animeId }).catch((error) => {
      throw normalizeTauriError("get_anime_detail", error);
    });
  }

  /** 按本地 external id 使用 Rust 多来源服务刷新详情。 */
  async refreshAnimeDetail(animeId: string): Promise<AnimeDetailResult> {
    return invoke<AnimeDetailResult>("refresh_anime_detail", { animeId }).catch((error) => {
      throw normalizeTauriError("refresh_anime_detail", error);
    });
  }

  /** 读取全部或指定番剧的字幕组。 */
  async listFansubs(animeId?: string): Promise<FansubGroup[]> {
    return invoke<FansubGroup[]>("list_fansubs", { animeId }).catch((error) => {
      throw normalizeTauriError("list_fansubs", error);
    });
  }

  /** 从 Rust Repository 读取本地下载任务快照。 */
  async listDownloads(): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("list_downloads").catch((error) => {
      throw normalizeTauriError("list_downloads", error);
    });
  }

  /** 刷新默认引擎和历史任务所属引擎。 */
  async refreshDownloads(): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("refresh_downloads").catch((error) => {
      throw normalizeTauriError("refresh_downloads", error);
    });
  }

  /** 暂停任务创建时所属的下载引擎。 */
  async pauseDownload(taskId: string): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("pause_download", { taskId }).catch((error) => {
      throw normalizeTauriError("pause_download", error);
    });
  }

  /** 恢复任务创建时所属的下载引擎。 */
  async resumeDownload(taskId: string): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("resume_download", { taskId }).catch((error) => {
      throw normalizeTauriError("resume_download", error);
    });
  }

  /** 从下载引擎和本地数据库删除任务。 */
  async removeDownload(taskId: string, deleteFiles: boolean): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("remove_download", { taskId, deleteFiles }).catch((error) => {
      throw normalizeTauriError("remove_download", error);
    });
  }

  /** 更新任务内一组文件的下载优先级。 */
  async setDownloadFilePriority(taskId: string, fileIndexes: number[], priority: number): Promise<DownloadTask[]> {
    return invoke<DownloadTask[]>("set_download_file_priority", { taskId, fileIndexes, priority }).catch((error) => {
      throw normalizeTauriError("set_download_file_priority", error);
    });
  }

  /** 通过磁链或远程 torrent 文件添加任务。 */
  async addDownloadUrl(input: AddDownloadUrlInput): Promise<DownloadTask[]> {
    const tasks = await invoke<DownloadTask[]>("add_download_url", { input }).catch((error) => {
      throw normalizeTauriError("add_download_url", error);
    });
    emitManualDownloadAdded();
    return tasks;
  }

  /** 通过原生文件选择器导入本地 torrent。 */
  async importTorrentFile(): Promise<DownloadTask[] | null> {
    const tasks = await invoke<DownloadTask[] | null>("import_torrent_file").catch((error) => {
      throw normalizeTauriError("import_torrent_file", error);
    });
    if (tasks) emitManualDownloadAdded();
    return tasks;
  }

  /** 将资源搜索结果加入当前默认下载引擎。 */
  async addReleaseDownload(input: AddReleaseDownloadInput): Promise<DownloadTask[]> {
    const tasks = await invoke<DownloadTask[]>("add_release_download", { input }).catch((error) => {
      throw normalizeTauriError("add_release_download", error);
    });
    emitManualDownloadAdded();
    return tasks;
  }

  /** 测试当前 qBittorrent WebUI 登录与任务读取。 */
  async testQbittorrent(): Promise<TorrentConnectionTestResult> {
    return invoke<TorrentConnectionTestResult>("test_qbittorrent").catch((error) => {
      throw normalizeTauriError("test_qbittorrent", error);
    });
  }

  /** 读取当前默认下载服务健康状态。 */
  async getDownloadServiceStatus(): Promise<DownloadServiceStatus> {
    return invoke<DownloadServiceStatus>("get_download_service_status").catch((error) => {
      throw normalizeTauriError("get_download_service_status", error);
    });
  }

  /** 订阅默认下载服务状态变化。 */
  onDownloadServiceStatusChanged(listener: () => void): () => void {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen(DOWNLOAD_SERVICE_STATUS_CHANGED_EVENT, () => listener()).then((dispose) => {
      if (disposed) {
        dispose();
      } else {
        unlisten = dispose;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  /** 读取托管 qBittorrent-nox 进程状态。 */
  async getQbittorrentManagedStatus(): Promise<QbittorrentManagedStatus> {
    return invoke<QbittorrentManagedStatus>("get_qbittorrent_managed_status").catch((error) => {
      throw normalizeTauriError("get_qbittorrent_managed_status", error);
    });
  }

  /** 启动托管 qBittorrent-nox。 */
  async startQbittorrentManaged(): Promise<QbittorrentManagedStatus> {
    return invoke<QbittorrentManagedStatus>("start_qbittorrent_managed").catch((error) => {
      throw normalizeTauriError("start_qbittorrent_managed", error);
    });
  }

  /** 停止托管 qBittorrent-nox。 */
  async stopQbittorrentManaged(): Promise<QbittorrentManagedStatus> {
    return invoke<QbittorrentManagedStatus>("stop_qbittorrent_managed").catch((error) => {
      throw normalizeTauriError("stop_qbittorrent_managed", error);
    });
  }

  /** 读取内置 torrent-core 状态。 */
  async getEmbeddedTorrentStatus(): Promise<EmbeddedTorrentCoreStatus> {
    return invoke<EmbeddedTorrentCoreStatus>("get_embedded_torrent_status").catch((error) => {
      throw normalizeTauriError("get_embedded_torrent_status", error);
    });
  }

  /** 启动内置 torrent-core。 */
  async startEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus> {
    return invoke<EmbeddedTorrentCoreStatus>("start_embedded_torrent").catch((error) => {
      throw normalizeTauriError("start_embedded_torrent", error);
    });
  }

  /** 停止内置 torrent-core。 */
  async stopEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus> {
    return invoke<EmbeddedTorrentCoreStatus>("stop_embedded_torrent").catch((error) => {
      throw normalizeTauriError("stop_embedded_torrent", error);
    });
  }

  /** 重启内置 torrent-core。 */
  async restartEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus> {
    return invoke<EmbeddedTorrentCoreStatus>("restart_embedded_torrent").catch((error) => {
      throw normalizeTauriError("restart_embedded_torrent", error);
    });
  }

  /** 读取全部已登记媒体文件。 */
  async listMediaFiles(): Promise<MediaFile[]> {
    return invoke<MediaFile[]>("list_media_files").catch((error) => {
      throw normalizeTauriError("list_media_files", error);
    });
  }

  /** 扫描一个下载任务中的已完成媒体。 */
  async scanDownloadMedia(taskId: string): Promise<MediaScanResult> {
    return invoke<MediaScanResult>("scan_download_media", { taskId }).catch((error) => {
      throw normalizeTauriError("scan_download_media", error);
    });
  }

  /** 读取桌面 FFprobe 与 FFmpeg 状态。 */
  async getDesktopMediaToolsStatus(): Promise<DesktopMediaToolsStatus> {
    return invoke<DesktopMediaToolsStatus>("get_desktop_media_tools_status").catch((error) => {
      throw normalizeTauriError("get_desktop_media_tools_status", error);
    });
  }

  /** 选择本机目录并启动后台媒体扫描。 */
  async startLocalMediaImport(): Promise<LocalMediaImportJobStatus | undefined> {
    const status = await invoke<LocalMediaImportJobStatus | null>("start_local_media_import").catch((error) => {
      throw normalizeTauriError("start_local_media_import", error);
    });
    return status ?? undefined;
  }

  /** 读取当前本地媒体后台任务状态。 */
  async getLocalMediaImportStatus(): Promise<LocalMediaImportJobStatus> {
    return invoke<LocalMediaImportJobStatus>("get_local_media_import_status").catch((error) => {
      throw normalizeTauriError("get_local_media_import_status", error);
    });
  }

  /** 按用户确认结果继续后台导入。 */
  async confirmLocalMediaImport(
    jobId: string,
    selections: LocalMediaImportSelection[]
  ): Promise<LocalMediaImportJobStatus> {
    return invoke<LocalMediaImportJobStatus>("confirm_local_media_import", { jobId, selections }).catch((error) => {
      throw normalizeTauriError("confirm_local_media_import", error);
    });
  }

  /** 请求取消当前本地媒体后台任务。 */
  async cancelLocalMediaImport(): Promise<LocalMediaImportJobStatus> {
    return invoke<LocalMediaImportJobStatus>("cancel_local_media_import").catch((error) => {
      throw normalizeTauriError("cancel_local_media_import", error);
    });
  }

  /** 启动全部已登记媒体的后台可用性校验。 */
  async startMediaAvailabilityCheck(): Promise<LocalMediaImportJobStatus> {
    return invoke<LocalMediaImportJobStatus>("start_media_availability_check").catch((error) => {
      throw normalizeTauriError("start_media_availability_check", error);
    });
  }

  /** 汇总原地导入目录及可用性状态。 */
  async listLocalMediaSources(): Promise<LocalMediaSourceSummary[]> {
    return invoke<LocalMediaSourceSummary[]>("list_local_media_sources").catch((error) => {
      throw normalizeTauriError("list_local_media_sources", error);
    });
  }

  /** 订阅本地媒体扫描、导入和校验状态。 */
  onLocalMediaImportStatusChanged(listener: (status: LocalMediaImportJobStatus) => void): () => void {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<LocalMediaImportJobStatus>(LOCAL_MEDIA_IMPORT_STATUS_CHANGED_EVENT, (event) => listener(event.payload))
      .then((dispose) => {
        if (disposed) {
          dispose();
        } else {
          unlisten = dispose;
        }
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  /** 探测 Tauri 桌面可用外部播放器。 */
  async detectPlayers(profiles?: AppSettings["players"]): Promise<PlayerDetectionResult> {
    return invoke<PlayerDetectionResult>("detect_players", { profiles }).catch((error) => {
      throw normalizeTauriError("detect_players", error);
    });
  }

  /** 使用原生文件选择器选择播放器程序。 */
  async selectPlayerExecutable(input: SelectPlayerExecutableInput): Promise<string | undefined> {
    const selected = await invoke<string | null>("select_player_executable", { input }).catch((error) => {
      throw normalizeTauriError("select_player_executable", error);
    });
    return selected ?? undefined;
  }

  /** 使用 Tauri Rust 外部播放器服务播放媒体。 */
  async playMedia(filePath: string, profileId?: string): Promise<void> {
    return invoke<void>("play_media", { filePath, profileId }).catch((error) => {
      throw normalizeTauriError("play_media", error);
    });
  }

  /** 在桌面文件管理器中定位受控媒体。 */
  async revealMedia(filePath: string): Promise<void> {
    return invoke<void>("reveal_media", { filePath }).catch((error) => {
      throw normalizeTauriError("reveal_media", error);
    });
  }

  /** 打开 Tauri 桌面 libVLC 双窗口。 */
  async openDesktopPlayerWindow(input: DesktopPlayerWindowInput): Promise<void> {
    return invoke<void>("open_desktop_player_window", { input }).catch((error) => {
      throw normalizeTauriError("open_desktop_player_window", error);
    });
  }

  /** 关闭 Tauri 桌面 libVLC 双窗口。 */
  closeDesktopPlayerWindow(): void {
    void invoke<void>("close_desktop_player_window").catch((error) => {
      console.error("[tauri-client] 关闭播放器窗口失败", normalizeTauriError("close_desktop_player_window", error));
    });
  }

  /** 将播放器拖动开始阶段交给 Tauri 原生窗口。 */
  dragDesktopPlayerWindow(input: DesktopPlayerWindowDragInput): void {
    this.desktopPlayerDragQueue = this.desktopPlayerDragQueue
      .then(() => invoke<void>("drag_desktop_player_window", { input }))
      .catch((error) => {
        console.error("[tauri-client] 拖动播放器窗口失败", normalizeTauriError("drag_desktop_player_window", error));
      });
  }

  /** 切换桌面内置播放器窗口最大化状态。 */
  async toggleDesktopPlayerWindowMaximize(): Promise<boolean> {
    return invoke<boolean>("toggle_desktop_player_window_maximize").catch((error) => {
      throw normalizeTauriError("toggle_desktop_player_window_maximize", error);
    });
  }

  /** 创建不暴露真实路径的桌面播放会话。 */
  async createDesktopPlaybackSession(input: DesktopPlaybackSessionInput): Promise<RemotePlaybackSession> {
    return invoke<RemotePlaybackSession>("create_desktop_playback_session", { input }).catch((error) => {
      throw normalizeTauriError("create_desktop_playback_session", error);
    });
  }

  /** 关闭桌面播放会话并清理路径映射。 */
  async closeDesktopPlaybackSession(sessionId: string): Promise<void> {
    return invoke<void>("close_desktop_playback_session", { sessionId }).catch((error) => {
      throw normalizeTauriError("close_desktop_playback_session", error);
    });
  }

  /** 读取 Tauri libmpv 后端能力。 */
  async getDesktopPlayerCapabilities(): Promise<PlayerCapabilities> {
    return invoke<PlayerCapabilities>("get_desktop_player_capabilities").catch((error) => {
      throw normalizeTauriError("get_desktop_player_capabilities", error);
    });
  }

  /** 向 Tauri libmpv 后端发送统一命令。 */
  async dispatchDesktopPlayerCommand(command: PlayerCommand): Promise<PlayerCommandResult> {
    return invoke<PlayerCommandResult>("dispatch_desktop_player_command", { command }).catch((error) => {
      throw normalizeTauriError("dispatch_desktop_player_command", error);
    });
  }

  /** 订阅 Tauri libmpv 完整快照。 */
  onDesktopPlayerSnapshot(listener: (snapshot: PlayerSnapshot) => void): () => void {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<PlayerSnapshot>(PLAYER_SNAPSHOT_EVENT, (event) => listener(event.payload)).then(async (dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
      if (disposed) return;
      try {
        const snapshot = await invoke<PlayerSnapshot | null>("get_desktop_player_snapshot");
        if (!disposed && snapshot) listener(snapshot);
      } catch (error) {
        console.error("[tauri-client] 播放器快照补拉失败", error);
      }
    }).catch((error) => {
      console.error("[tauri-client] 播放器快照订阅失败", error);
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }

  /** 从公共 Repository 端口读取下载源配置。 */
  async listSources(): Promise<ReleaseSourceConfig[]> {
    return invoke<ReleaseSourceConfig[]>("list_sources").catch((error) => {
      throw normalizeTauriError("list_sources", error);
    });
  }

  /** 启用或停用一个下载源。 */
  async setSourceEnabled(sourceId: string, enabled: boolean): Promise<ReleaseSourceConfig[]> {
    return invoke<ReleaseSourceConfig[]>("set_source_enabled", { sourceId, enabled }).catch((error) => {
      throw normalizeTauriError("set_source_enabled", error);
    });
  }

  /** 新增或更新一个下载源。 */
  async upsertSource(source: ReleaseSourceConfig): Promise<ReleaseSourceConfig[]> {
    return invoke<ReleaseSourceConfig[]>("upsert_source", { source }).catch((error) => {
      throw normalizeTauriError("upsert_source", error);
    });
  }

  /** 读取 Rust 每日来源同步调度器状态。 */
  async getSourceSyncStatus(): Promise<SourceSyncSchedulerStatus> {
    return invoke<SourceSyncSchedulerStatus>("get_source_sync_status").catch((error) => {
      throw normalizeTauriError("get_source_sync_status", error);
    });
  }

  /** 立即强制执行一次 Rust 来源增量同步。 */
  async syncSourcesNow(): Promise<SourceSyncRunResult> {
    return invoke<SourceSyncRunResult>("sync_sources_now").catch((error) => {
      throw normalizeTauriError("sync_sources_now", error);
    });
  }

  /** 立即执行一次 Rust 自动扫描。 */
  async runAutomationOnce(): Promise<AutomationRunResult> {
    return invoke<AutomationRunResult>("run_automation_once").catch((error) => {
      throw normalizeTauriError("run_automation_once", error);
    });
  }

  /** 将手动扫描交给 Rust 宿主后台执行。 */
  async startAutomationScan(): Promise<AutomationSchedulerStatus> {
    return invoke<AutomationSchedulerStatus>("start_automation_scan").catch((error) => {
      throw normalizeTauriError("start_automation_scan", error);
    });
  }

  /** 读取 Rust 自动扫描调度状态。 */
  async getAutomationSchedulerStatus(): Promise<AutomationSchedulerStatus> {
    return invoke<AutomationSchedulerStatus>("get_automation_scheduler_status").catch((error) => {
      throw normalizeTauriError("get_automation_scheduler_status", error);
    });
  }

  /** 按最新设置刷新 Rust 自动扫描调度。 */
  async restartAutomationScheduler(): Promise<AutomationSchedulerStatus> {
    return invoke<AutomationSchedulerStatus>("restart_automation_scheduler").catch((error) => {
      throw normalizeTauriError("restart_automation_scheduler", error);
    });
  }

  /** 读取来源绑定，并按需发现 AniBT/Mikan 候选。 */
  async getAnimeSourceBindingState(
    animeId: string,
    discoverCandidates = true
  ): Promise<AnimeSourceBindingState> {
    return invoke<AnimeSourceBindingState>("get_anime_source_binding_state", {
      animeId,
      discoverCandidates
    }).catch((error) => {
      throw normalizeTauriError("get_anime_source_binding_state", error);
    });
  }

  /** 确认并保存一个番剧来源绑定。 */
  async confirmAnimeSourceBinding(input: ConfirmAnimeSourceBindingInput): Promise<AnimeSourceBindingState> {
    return invoke<AnimeSourceBindingState>("confirm_anime_source_binding", { input }).catch((error) => {
      throw normalizeTauriError("confirm_anime_source_binding", error);
    });
  }

  /** 记录用户确认的不匹配来源候选。 */
  async reportAnimeSourceCandidateMismatch(input: ReportAnimeSourceCandidateMismatchInput): Promise<void> {
    return invoke<void>("report_anime_source_candidate_mismatch", { input }).catch((error) => {
      throw normalizeTauriError("report_anime_source_candidate_mismatch", error);
    });
  }

  /** 撤销一个来源候选的不匹配记录。 */
  async removeAnimeSourceCandidateMismatch(
    input: RemoveAnimeSourceCandidateMismatchInput
  ): Promise<AnimeSourceBindingState> {
    return invoke<AnimeSourceBindingState>("remove_anime_source_candidate_mismatch", { input }).catch((error) => {
      throw normalizeTauriError("remove_anime_source_candidate_mismatch", error);
    });
  }

  /** 设置或取消当前番剧对整个来源的候选排除。 */
  async setAnimeSourceExcluded(input: SetAnimeSourceExclusionInput): Promise<AnimeSourceBindingState> {
    return invoke<AnimeSourceBindingState>("set_anime_source_excluded", { input }).catch((error) => {
      throw normalizeTauriError("set_anime_source_excluded", error);
    });
  }

  /** 取消一个已确认的番剧来源绑定。 */
  async removeAnimeSourceBinding(animeId: string, sourceId: string): Promise<AnimeSourceBindingState> {
    return invoke<AnimeSourceBindingState>("remove_anime_source_binding", { animeId, sourceId }).catch((error) => {
      throw normalizeTauriError("remove_anime_source_binding", error);
    });
  }

  /** 使用 Rust 来源适配器按任意关键词搜索资源。 */
  async searchReleases(query: ReleaseQuery): Promise<ReleaseSearchResult> {
    return invoke<ReleaseSearchResult>("search_releases", { query }).catch((error) => {
      throw normalizeTauriError("search_releases", error);
    });
  }

  /** 使用 Rust 标题匹配与追番规则搜索资源。 */
  async searchAnimeReleases(query: AnimeReleaseQuery): Promise<ReleaseSearchResult> {
    return invoke<ReleaseSearchResult>("search_anime_releases", { query }).catch((error) => {
      throw normalizeTauriError("search_anime_releases", error);
    });
  }

  /** 使用 Rust RSS 解析器搜索一条追番订阅。 */
  async searchRssSubscriptionReleases(
    query: RssSubscriptionReleaseQuery
  ): Promise<RssSubscriptionReleaseResult> {
    return invoke<RssSubscriptionReleaseResult>("search_rss_subscription_releases", { query }).catch((error) => {
      throw normalizeTauriError("search_rss_subscription_releases", error);
    });
  }
}

/** 创建由编译器完整校验业务方法的 Tauri AppClient。 */
export function createTauriClient(platform: TauriClientPlatform): AppClient {
  return new TauriClientCore(platform);
}
