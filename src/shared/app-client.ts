import type {
  Anime,
  AppSettings,
  DashboardData,
  DownloadTask,
  Episode,
  EpisodePreference,
  FansubGroup,
  MediaFile,
  MyAnime,
  NotificationRecord,
  PlayerProfile,
  ReleaseSourceConfig
} from "./domain";
import type {
  AddDownloadUrlInput,
  AddReleaseDownloadInput,
  AnimeDetailResult,
  AnimeDiscoveryQuery,
  AnimeDiscoveryResult,
  AnimeDiscoverySearchResult,
  AnimeDiscoverySeasonQuery,
  AnimeDiscoverySeasonResult,
  AnimeDiscoverySyncTaskStatus,
  AnimeSeasonSyncState,
  AnimeReleaseQuery,
  AnimeSourceBindingState,
  AnimeWatchProgress,
  AppWindowState,
  AutomationRunResult,
  AutomationSchedulerStatus,
  BangumiBrowseQuery,
  BangumiBrowseResult,
  ConfirmAnimeSourceBindingInput,
  DesktopPlayerWindowDragInput,
  DesktopPlayerWindowInput,
  DesktopPlaybackSessionInput,
  DownloadServiceStatus,
  DesktopMediaToolsStatus,
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
  RemoveAnimeSourceCandidateMismatchInput,
  ReportAnimeSourceCandidateMismatchInput,
  ReportPlaybackProgressInput,
  ReleaseQuery,
  ReleaseSearchResult,
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
} from "./contracts";
import type {
  PlayerCapabilities,
  PlayerCommand,
  PlayerCommandResult,
  PlayerSnapshot
} from "./player-contract";

/** 桌面、Android 与远程客户端共同提供给页面的业务 API。 */
export interface AppClient {
  platform: string;

  /** 读取桌面窗口状态。 */
  getWindowState(): Promise<AppWindowState>;
  /** 最小化桌面窗口。 */
  minimizeWindow(): Promise<void>;
  /** 切换桌面窗口最大化状态。 */
  toggleMaximizeWindow(): Promise<AppWindowState>;
  /** 关闭桌面窗口。 */
  closeWindow(): Promise<void>;
  /** 订阅桌面窗口状态变化。 */
  onWindowStateChanged(listener: (state: AppWindowState) => void): () => void;

  /** 读取移动宿主的生命周期与系统约束。 */
  getMobilePlatformStatus?(): Promise<MobilePlatformStatus>;
  /** 读取并清除原生通知导航。 */
  consumeMobileNavigation?(): Promise<MobileNavigationIntent | undefined>;
  /** 读取并清除原生后台调度要求的前台补跑标记。 */
  consumeMobileBackgroundRefresh?(): Promise<boolean>;
  /** 由用户操作请求移动通知权限。 */
  requestMobileNotificationPermission?(): Promise<MobileNotificationPermission>;

  /** 解析当前平台可读取的缓存图片地址。 */
  resolveCachedImageUrl(sourceUrl: string): Promise<ImageCacheResolveResult>;
  /** 删除本地宿主管理的指定图片缓存。 */
  invalidateCachedImageUrl?(sourceUrl: string): Promise<void>;
  /** 将规范化主题背景写入应用私有目录。 */
  saveThemeBackground(input: SaveThemeBackgroundInput): Promise<ThemeBackgroundAsset>;
  /** 解析主题 JSON 引用的背景图片。 */
  resolveThemeBackground(themeId: string, fileName: string): Promise<ThemeBackgroundAsset | undefined>;
  /** 清理已不再被设置引用的主题背景文件。 */
  pruneThemeBackgrounds(references: ThemeBackgroundReference[]): Promise<void>;
  /** 通过系统文件选择器导出主题 JSON 或 ZIP。 */
  exportThemePackage(input: ExportThemePackageInput): Promise<string | undefined>;
  /** 读取首页聚合数据。 */
  getDashboard(): Promise<DashboardData>;

  /** 列出应用内通知。 */
  listNotifications(): Promise<NotificationRecord[]>;
  /** 读取未读通知数量。 */
  getUnreadNotificationCount(): Promise<number>;
  /** 标记一条通知已读。 */
  markNotificationRead(notificationId: string): Promise<NotificationRecord[]>;
  /** 标记全部通知已读。 */
  markAllNotificationsRead(): Promise<NotificationRecord[]>;
  /** 清空应用内通知。 */
  clearNotifications(): Promise<NotificationRecord[]>;

  /** 列出我的追番。 */
  listMyAnime(): Promise<MyAnime[]>;
  /** 新增或更新追番。 */
  upsertMyAnime(item: MyAnime): Promise<MyAnime[]>;
  /** 删除追番。 */
  removeMyAnime(itemId: string): Promise<MyAnime[]>;
  /** 列出追番观看进度。 */
  listMyAnimeWatchProgress(): Promise<AnimeWatchProgress[]>;
  /** 更新追番观看进度。 */
  setAnimeWatchProgress(input: SetAnimeWatchProgressInput): Promise<AnimeWatchProgress>;
  /** 按播放百分比回写已看状态。 */
  reportPlaybackProgress(input: ReportPlaybackProgressInput): Promise<boolean>;
  /** 保存播放器续播检查点。 */
  savePlaybackCheckpoint(input: SavePlaybackCheckpointInput): Promise<PlaybackCheckpoint>;

  /** 列出本地番剧目录。 */
  listAnimeCatalog(year?: number, month?: number): Promise<Anime[]>;
  /** 搜索本地与在线番剧目录。 */
  searchAnimeCatalog(keyword: string): Promise<AnimeDiscoverySearchResult>;
  /** 直接在线浏览 Bangumi，不读取或写入季度目录缓存。 */
  browseBangumiAnime(query: BangumiBrowseQuery): Promise<BangumiBrowseResult>;
  /** 保存 Bangumi 追番并在后台补全 AniList、Mikan 元数据。 */
  followBangumiAnime(item: MyAnime): Promise<MyAnime[]>;
  /** 采集指定月份番剧。 */
  collectAnimeMonth(query: AnimeDiscoveryQuery): Promise<AnimeDiscoveryResult>;
  /** 采集指定季度番剧。 */
  collectAnimeSeason(query: AnimeDiscoverySeasonQuery): Promise<AnimeDiscoverySeasonResult>;
  /** 将指定季度采集加入宿主后台任务。 */
  startAnimeSeasonSync(query: AnimeDiscoverySeasonQuery): Promise<AnimeDiscoverySyncTaskStatus>;
  /** 读取季度采集后台任务状态。 */
  getAnimeSeasonSyncTaskStatus(): Promise<AnimeDiscoverySyncTaskStatus>;
  /** 读取指定季度的后台同步状态。 */
  getAnimeSeasonSyncState(year: number, season: AnimeDiscoverySeasonQuery["season"]): Promise<AnimeSeasonSyncState | undefined>;
  /** 读取番剧详情。 */
  getAnimeDetail(animeId: string): Promise<AnimeDetailResult>;
  /** 强制刷新番剧详情。 */
  refreshAnimeDetail(animeId: string): Promise<AnimeDetailResult>;

  /** 列出番剧单集。 */
  listEpisodes(animeId: string): Promise<Episode[]>;
  /** 新增或更新单集。 */
  upsertEpisode(episode: Episode): Promise<Episode[]>;
  /** 列出单集偏好。 */
  listEpisodePreferences(animeId: string): Promise<EpisodePreference[]>;
  /** 新增或更新单集偏好。 */
  upsertEpisodePreference(preference: EpisodePreference): Promise<EpisodePreference[]>;
  /** 删除单集偏好。 */
  removeEpisodePreference(episodeId: string): Promise<EpisodePreference[]>;
  /** 预览单集候选资源。 */
  previewEpisodeReleases(animeId: string, episodeId: string): Promise<EpisodeReleasePreview>;

  /** 立即执行一次自动扫描。 */
  runAutomationOnce(): Promise<AutomationRunResult>;
  /** 将一次手动扫描加入宿主后台任务。 */
  startAutomationScan(): Promise<AutomationSchedulerStatus>;
  /** 读取自动扫描调度状态。 */
  getAutomationSchedulerStatus(): Promise<AutomationSchedulerStatus>;
  /** 按当前设置重启自动扫描调度。 */
  restartAutomationScheduler(): Promise<AutomationSchedulerStatus>;

  /** 列出下载任务。 */
  listDownloads(): Promise<DownloadTask[]>;
  /** 刷新下载任务。 */
  refreshDownloads(): Promise<DownloadTask[]>;
  /** 暂停下载任务。 */
  pauseDownload(taskId: string): Promise<DownloadTask[]>;
  /** 恢复下载任务。 */
  resumeDownload(taskId: string): Promise<DownloadTask[]>;
  /** 删除下载任务及可选文件。 */
  removeDownload(taskId: string, deleteFiles: boolean): Promise<DownloadTask[]>;
  /** 设置下载任务文件优先级。 */
  setDownloadFilePriority(taskId: string, fileIndexes: number[], priority: number): Promise<DownloadTask[]>;
  /** 通过磁链或 torrent 地址添加任务。 */
  addDownloadUrl(input: AddDownloadUrlInput): Promise<DownloadTask[]>;
  /** 通过系统文件选择器导入本地 torrent。 */
  importTorrentFile?(): Promise<DownloadTask[] | null>;
  /** 从资源搜索结果添加任务。 */
  addReleaseDownload(input: AddReleaseDownloadInput): Promise<DownloadTask[]>;

  /** 列出字幕组。 */
  listFansubs(animeId?: string): Promise<FansubGroup[]>;
  /** 列出下载源。 */
  listSources(): Promise<ReleaseSourceConfig[]>;
  /** 启用或停用下载源。 */
  setSourceEnabled(sourceId: string, enabled: boolean): Promise<ReleaseSourceConfig[]>;
  /** 新增或更新下载源。 */
  upsertSource(source: ReleaseSourceConfig): Promise<ReleaseSourceConfig[]>;
  /** 读取来源同步状态。 */
  getSourceSyncStatus(): Promise<SourceSyncSchedulerStatus>;
  /** 立即同步全部启用来源。 */
  syncSourcesNow(): Promise<SourceSyncRunResult>;

  /** 读取番剧来源绑定状态。 */
  getAnimeSourceBindingState(animeId: string, discoverCandidates?: boolean): Promise<AnimeSourceBindingState>;
  /** 确认番剧来源绑定。 */
  confirmAnimeSourceBinding(input: ConfirmAnimeSourceBindingInput): Promise<AnimeSourceBindingState>;
  /** 记录错误的番剧来源候选。 */
  reportAnimeSourceCandidateMismatch(input: ReportAnimeSourceCandidateMismatchInput): Promise<void>;
  /** 删除错误候选记录。 */
  removeAnimeSourceCandidateMismatch(input: RemoveAnimeSourceCandidateMismatchInput): Promise<AnimeSourceBindingState>;
  /** 设置番剧来源排除状态。 */
  setAnimeSourceExcluded(input: SetAnimeSourceExclusionInput): Promise<AnimeSourceBindingState>;
  /** 删除番剧来源绑定。 */
  removeAnimeSourceBinding(animeId: string, sourceId: string): Promise<AnimeSourceBindingState>;

  /** 按任意关键词搜索资源。 */
  searchReleases(query: ReleaseQuery): Promise<ReleaseSearchResult>;
  /** 按番剧上下文搜索资源。 */
  searchAnimeReleases(query: AnimeReleaseQuery): Promise<ReleaseSearchResult>;
  /** 搜索指定追番 RSS 资源。 */
  searchRssSubscriptionReleases(query: RssSubscriptionReleaseQuery): Promise<RssSubscriptionReleaseResult>;

  /** 读取应用设置。 */
  getSettings(): Promise<AppSettings>;
  /** 更新应用设置。 */
  updateSettings(patch: Partial<AppSettings>): Promise<AppSettings>;
  /** 恢复当前平台默认设置。 */
  resetSettingsToDefaults(): Promise<AppSettings>;
  /** 通过系统文件面板导出 SQLite 数据备份。 */
  exportDatabaseBackup?(): Promise<string | null>;
  /** 通过系统文件面板恢复 SQLite 数据备份。 */
  restoreDatabaseBackup?(): Promise<string | null>;
  /** 通过系统文件面板导出当前及轮转日志。 */
  exportLogs?(): Promise<string | null>;

  /** 探测桌面外部播放器。 */
  detectPlayers(profiles?: PlayerProfile[]): Promise<PlayerDetectionResult>;
  /** 选择桌面播放器可执行文件。 */
  selectPlayerExecutable(input: SelectPlayerExecutableInput): Promise<string | undefined>;
  /** 测试外部 qBittorrent 连接。 */
  testQbittorrent(): Promise<TorrentConnectionTestResult>;
  /** 读取统一下载服务状态。 */
  getDownloadServiceStatus(): Promise<DownloadServiceStatus>;
  /** 订阅下载服务状态变化。 */
  onDownloadServiceStatusChanged(listener: () => void): () => void;

  /** 读取托管 qBittorrent 状态。 */
  getQbittorrentManagedStatus(): Promise<QbittorrentManagedStatus>;
  /** 启动托管 qBittorrent。 */
  startQbittorrentManaged(): Promise<QbittorrentManagedStatus>;
  /** 停止托管 qBittorrent。 */
  stopQbittorrentManaged(): Promise<QbittorrentManagedStatus>;
  /** 读取内置 torrent-core 状态。 */
  getEmbeddedTorrentStatus(): Promise<EmbeddedTorrentCoreStatus>;
  /** 启动内置 torrent-core。 */
  startEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus>;
  /** 停止内置 torrent-core。 */
  stopEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus>;
  /** 重启内置 torrent-core。 */
  restartEmbeddedTorrent(): Promise<EmbeddedTorrentCoreStatus>;

  /** 列出已登记媒体文件。 */
  listMediaFiles(): Promise<MediaFile[]>;
  /** 扫描下载任务媒体文件。 */
  scanDownloadMedia(taskId: string): Promise<MediaScanResult>;
  /** 读取桌面 FFprobe 与 FFmpeg 状态。 */
  getDesktopMediaToolsStatus(): Promise<DesktopMediaToolsStatus>;
  /** 选择本机目录并启动后台媒体扫描。 */
  startLocalMediaImport(): Promise<LocalMediaImportJobStatus | undefined>;
  /** 读取本地媒体后台任务状态。 */
  getLocalMediaImportStatus(): Promise<LocalMediaImportJobStatus>;
  /** 按用户选择继续导入低置信度候选。 */
  confirmLocalMediaImport(
    jobId: string,
    selections: LocalMediaImportSelection[]
  ): Promise<LocalMediaImportJobStatus>;
  /** 请求取消当前本地媒体后台任务。 */
  cancelLocalMediaImport(): Promise<LocalMediaImportJobStatus>;
  /** 启动全部已登记媒体的后台可用性校验。 */
  startMediaAvailabilityCheck(): Promise<LocalMediaImportJobStatus>;
  /** 汇总全部原地导入目录。 */
  listLocalMediaSources(): Promise<LocalMediaSourceSummary[]>;
  /** 订阅本地媒体后台任务状态。 */
  onLocalMediaImportStatusChanged(listener: (status: LocalMediaImportJobStatus) => void): () => void;
  /** 使用当前平台播放器播放媒体。 */
  playMedia(filePath: string, profileId?: string): Promise<void>;
  /** 打开桌面内置播放器窗口。 */
  openDesktopPlayerWindow(input: DesktopPlayerWindowInput): Promise<void>;
  /** 关闭桌面内置播放器窗口。 */
  closeDesktopPlayerWindow(): void;
  /** 拖动桌面内置播放器窗口。 */
  dragDesktopPlayerWindow(input: DesktopPlayerWindowDragInput): void;
  /** 切换桌面内置播放器窗口最大化状态。 */
  toggleDesktopPlayerWindowMaximize(): Promise<boolean>;
  /** 创建桌面受控播放会话。 */
  createDesktopPlaybackSession(input: DesktopPlaybackSessionInput): Promise<RemotePlaybackSession>;
  /** 关闭桌面受控播放会话。 */
  closeDesktopPlaybackSession(sessionId: string): Promise<void>;
  /** 读取桌面内置播放器能力。 */
  getDesktopPlayerCapabilities(): Promise<PlayerCapabilities>;
  /** 向桌面内置播放器发送命令。 */
  dispatchDesktopPlayerCommand(command: PlayerCommand): Promise<PlayerCommandResult>;
  /** 订阅桌面内置播放器快照。 */
  onDesktopPlayerSnapshot(listener: (snapshot: PlayerSnapshot) => void): () => void;
  /** 在当前平台文件管理器中定位媒体。 */
  revealMedia(filePath: string): Promise<void>;
  /** 使用当前平台打开外部 URL。 */
  openExternal(url: string): Promise<void>;

  /** 读取桌面远程网关状态。 */
  getRemoteGatewayStatus(): Promise<RemoteGatewayStatus>;
  /** 创建桌面远程配对码。 */
  createRemotePairingCode(): Promise<RemotePairingChallenge>;
  /** 吊销桌面远程设备。 */
  revokeRemoteDevice(deviceId: string): Promise<RemoteGatewayStatus>;
}
