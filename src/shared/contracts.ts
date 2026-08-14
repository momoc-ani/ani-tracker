import type {
  Anime,
  AnimeSourceBinding,
  AppSettings,
  DownloadTask,
  Episode,
  FansubGroup,
  MediaFile,
  MyAnime,
  Release,
  ReleaseSourceConfig,
  Season,
  TorrentFile
} from "./domain";
import type { DiscoveryBrowseFilters, DiscoveryBrowseSortKey } from "./discovery-filter";

/** 返回当前客户端可加载的签名图片缓存地址。 */
export interface ImageCacheResolveResult {
  url: string;
}

/** Renderer 写入应用私有主题目录的规范化背景图片。 */
export interface SaveThemeBackgroundInput {
  themeId: string;
  fileName: string;
  contentType: "image/jpeg" | "image/png" | "image/webp";
  dataBase64: string;
}

/** 宿主返回的主题背景资产及受控读取地址。 */
export interface ThemeBackgroundAsset {
  themeId: string;
  fileName: string;
  contentType: string;
  size: number;
  url: string;
}

/** 设置保存后仍被主题 JSON 引用的背景文件。 */
export interface ThemeBackgroundReference {
  themeId: string;
  fileName: string;
}

/** 交由系统文件选择器导出的主题 JSON 或 ZIP。 */
export interface ExportThemePackageInput {
  fileName: string;
  contentType: "application/json" | "application/zip";
  dataBase64: string;
}

/** 无边框窗口控制区需要的最小窗口状态。 */
export interface AppWindowState {
  maximized: boolean;
}

export type MobileLifecycleState = "foreground" | "background" | "inactive";
export type MobileNetworkState = "online" | "limited" | "offline" | "unknown";
export type MobileStorageState = "ok" | "low" | "critical";
export type MobileOrientation = "portrait" | "landscape" | "unknown";
export type MobileNotificationPermission = "granted" | "denied" | "prompt" | "prompt-with-rationale" | "not-required";

/** 移动宿主提供的生命周期与系统资源确定状态。 */
export interface MobilePlatformStatus {
  lifecycle: MobileLifecycleState;
  network: MobileNetworkState;
  metered: boolean;
  storage: MobileStorageState;
  availableBytes: number;
  orientation: MobileOrientation;
  notificationPermission: MobileNotificationPermission;
}

/** 原生通知或系统入口要求打开的应用内页面。 */
export interface MobileNavigationIntent {
  pageId: "home" | "downloads" | "notifications";
}

export interface AnimeSearchQuery {
  keyword: string;
  includeAliases?: boolean;
}

export interface AnimeDiscoveryQuery {
  year: number;
  month: number;
  forceRefresh?: boolean;
}

export interface AnimeDiscoverySeasonQuery {
  year: number;
  season: Season;
  forceRefresh?: boolean;
}

/** Bangumi 在线浏览的分页输入。 */
export interface BangumiBrowseQuery {
  keyword: string;
  sort: DiscoveryBrowseSortKey;
  filters: Omit<DiscoveryBrowseFilters, "airingStatuses">;
  page: number;
  pageSize: number;
}

/** Bangumi 在线浏览的分页响应。 */
export interface BangumiBrowseResult {
  query: BangumiBrowseQuery;
  items: Anime[];
  total: number;
  hasMore: boolean;
  source: "bangumi";
}

export interface AnimeDiscoveryResult {
  query: AnimeDiscoveryQuery;
  items: Anime[];
  addedCount: number;
  existingCount: number;
  source: string;
  errors: string[];
}

export interface AnimeDiscoverySeasonResult {
  query: AnimeDiscoverySeasonQuery;
  items: Anime[];
  addedCount: number;
  existingCount: number;
  source: string;
  errors: string[];
}

/** 一次季度后台同步的紧凑结果。 */
export interface AnimeDiscoverySyncTaskResult {
  query: AnimeDiscoverySeasonQuery;
  itemCount: number;
  addedCount: number;
  existingCount: number;
  errorCount: number;
}

/** 新番季度同步调度器的当前任务状态。 */
export interface AnimeDiscoverySyncTaskStatus {
  inFlight: boolean;
  phase?: "catalog" | "details";
  activeQuery?: AnimeDiscoverySeasonQuery;
  startedAt?: string;
  finishedAt?: string;
  catalogFinishedAt?: string;
  detailCompletedCount: number;
  detailTotalCount: number;
  detailErrorCount: number;
  lastResult?: AnimeDiscoverySyncTaskResult;
  lastError?: string;
}

/** 单个自然季度的新番目录后台同步状态。 */
export interface AnimeSeasonSyncState {
  year: number;
  season: Season;
  lastAttemptAt?: string;
  lastSuccessfulSyncAt?: string;
  completedAt?: string;
  lastAnilistError?: string;
}

/** 新番关键词搜索返回的本地与在线聚合结果。 */
export interface AnimeDiscoverySearchResult {
  keyword: string;
  items: Anime[];
  source: string;
  errors: string[];
}

export interface ReleaseQuery {
  keyword: string;
  animeId?: string;
  episodeNo?: number;
  fansubGroupId?: string;
  preferredResolution?: string;
  limit?: number;
  cacheTtlMs?: number;
  forceRefresh?: boolean;
}

export interface AnimeReleaseQuery {
  animeId: string;
  episodeNo?: number;
  fansubGroupId?: string;
  preferredResolution?: string;
  limit?: number;
  cacheTtlMs?: number;
  forceRefresh?: boolean;
}

/** 生成单番 RSS 订阅时需要的番剧和来源绑定上下文。 */
export interface AnimeRssSubscriptionContext {
  anime: Anime;
  binding?: AnimeSourceBinding;
  limit?: number;
  allowExternalIdFallback?: boolean;
}

/** 下载源生成的单番 RSS 描述，由对应适配器负责读取和解析。 */
export interface AnimeRssFeedDescriptor {
  sourceId: string;
  sourceName: string;
  sourceAnimeId?: string;
  url: string;
  limit: number;
  exactAnimeMatch: boolean;
}

export interface ReleaseSourceSearchResult {
  sourceId: string;
  sourceName: string;
  releases: Release[];
}

export interface ReleaseSearchResult {
  query: ReleaseQuery;
  releases: Release[];
  sourceResults: ReleaseSourceSearchResult[];
  searchedSourceIds: string[];
  errors: Array<{
    sourceId: string;
    message: string;
  }>;
}

export interface AnimeSourceCandidate {
  sourceId: string;
  sourceName: string;
  sourceAnimeId: string;
  title: string;
  originalTitle?: string;
  aliases: string[];
  premiereYear?: number;
  premiereMonth?: number;
  episodeCount?: number;
  sourceUrl?: string;
  score: number;
  reasons: string[];
}

export interface AnimeSourceBindingState {
  animeId: string;
  bindings: AnimeSourceBinding[];
  candidates: AnimeSourceCandidate[];
  excludedSources: Array<{
    sourceId: string;
    sourceName: string;
  }>;
  errors: Array<{
    sourceId: string;
    message: string;
  }>;
}

export interface ConfirmAnimeSourceBindingInput {
  animeId: string;
  sourceId: string;
  sourceAnimeId: string;
  sourceAnimeTitle?: string;
  sourceUrl?: string;
  confidence?: number;
}

export interface ReportAnimeSourceCandidateMismatchInput {
  animeId: string;
  sourceId: string;
  sourceAnimeId: string;
  sourceAnimeTitle: string;
  score: number;
  reasons: string[];
}

export interface SetAnimeSourceExclusionInput {
  animeId: string;
  sourceId: string;
  excluded: boolean;
}

export interface RemoveAnimeSourceCandidateMismatchInput {
  animeId: string;
  sourceId: string;
  sourceAnimeId: string;
}

export interface RssSubscriptionReleaseQuery {
  animeId: string;
  subscriptionId: string;
  preferredResolution?: string;
  limit?: number;
}

export interface RssSubscriptionReleaseResult {
  query: RssSubscriptionReleaseQuery;
  releases: Release[];
  errors: Array<{
    sourceId: string;
    message: string;
  }>;
}

export interface AddDownloadUrlInput {
  url: string;
  name?: string;
  savePath?: string;
  paused?: boolean;
}

export interface AddReleaseDownloadInput {
  release: Release;
  animeId?: string;
  episodeId?: string;
  episodeNo?: number;
  fansubGroupId?: string;
  savePath?: string;
  paused?: boolean;
  confirmUnknownSeason?: boolean;
}

export interface AddTorrentOptions {
  savePath: string;
  selectedFileIndexes?: number[];
  category?: string;
  correlationTag?: string;
  paused?: boolean;
}

export interface TorrentConnectionTestResult {
  ok: boolean;
  message: string;
  taskCount?: number;
}

export type DownloadServiceMode = "embedded" | "managed" | "external";
export type DownloadServiceState = "online" | "idle" | "error";

/** 描述当前默认下载引擎的实际运行状态，供应用壳统一展示。 */
export interface DownloadServiceStatus {
  mode: DownloadServiceMode;
  state: DownloadServiceState;
  message: string;
  taskCount?: number;
}

export interface QbittorrentManagedStatus {
  enabled: boolean;
  autoStart: boolean;
  running: boolean;
  webUiUrl: string;
  platform: string;
  arch: string;
  binaryPath?: string;
  profileDir?: string;
  pid?: number;
  lastStartedAt?: string;
  lastStoppedAt?: string;
  lastError?: string;
}

/** 内置 libtorrent sidecar 的进程与核心状态。 */
export interface EmbeddedTorrentCoreStatus {
  enabled: boolean;
  running: boolean;
  platform: string;
  arch: string;
  binaryPath?: string;
  dataDir?: string;
  pid?: number;
  foregroundService?: boolean;
  version?: string;
  taskCount?: number;
  listenPort?: number;
  networkPolicyBlocked?: boolean;
  lastStartedAt?: string;
  lastStoppedAt?: string;
  lastError?: string;
}

export interface MediaExtractInput {
  release?: Release;
  filePath?: string;
  fileName?: string;
}

export interface MediaProbeContext {
  animeId?: string;
  episodeId?: string;
  downloadTaskId?: string;
  release?: Release;
  size?: number;
  downloadedAt?: string;
}

export interface PartialMediaInfo {
  container?: MediaFile["container"];
  declaredVideoCodec?: string;
  detectedVideoCodec?: string;
  normalizedVideoCodec?: MediaFile["normalizedVideoCodec"];
  resolution?: string;
  bitDepth?: number;
  audioCodecs?: string[];
  subtitleTracks?: string[];
  durationSeconds?: number;
  confidence: number;
  source: string;
}

export interface MediaScanResult {
  taskId: string;
  mediaFiles: MediaFile[];
  skippedFiles: Array<{
    name: string;
    reason: string;
  }>;
  errors: Array<{
    filePath: string;
    message: string;
  }>;
}

export type LocalMediaImportPhase =
  | "idle"
  | "scanning"
  | "matching"
  | "importing"
  | "awaiting_review"
  | "verifying"
  | "completed"
  | "cancelled"
  | "failed";

export interface LocalMediaImportCandidate {
  id: string;
  titleHint: string;
  relativeDirectory: string;
  fileCount: number;
  episodeNumbers: number[];
  confidence: number;
  fileTitleConsensus: number;
  suggestedAnimeId?: string;
  alternatives: Anime[];
  currentAssociations: LocalMediaImportAssociation[];
}

export interface LocalMediaImportAssociation {
  animeId: string;
  animeTitle: string;
  fileCount: number;
}

export interface LocalMediaImportSelection {
  candidateId: string;
  animeId?: string;
  createLocal?: boolean;
}

export interface LocalMediaImportJobStatus {
  jobId?: string;
  phase: LocalMediaImportPhase;
  sourceRoot?: string;
  discoveredFiles: number;
  processedFiles: number;
  totalFiles: number;
  importedAnimeCount: number;
  importedMediaCount: number;
  availableFiles: number;
  changedFiles: number;
  missingFiles: number;
  unavailableFiles: number;
  candidates: LocalMediaImportCandidate[];
  message?: string;
  error?: string;
  startedAt?: string;
  completedAt?: string;
}

export interface LocalMediaSourceSummary {
  rootPath: string;
  mediaCount: number;
  availableCount: number;
  problemCount: number;
  lastScannedAt?: string;
}

export interface EpisodeReleaseCandidate {
  release: Release;
  score: number;
  matchScore: number;
  preferenceScore: number;
  availabilityScore: number;
  reasons: string[];
  warnings: string[];
}

/** 单部追番的连续观看进度。 */
export interface AnimeWatchProgress {
  animeId: string;
  watchedEpisodeCount: number;
  totalEpisodeCount: number;
}

/** 原子更新单部追番观看进度的输入。 */
export interface SetAnimeWatchProgressInput {
  animeId: string;
  watchedEpisodeCount: number;
}

/** 播放器按下载任务上报观看百分比的输入。 */
export interface ReportPlaybackProgressInput {
  taskId: string;
  fileIndex?: number;
  percent: number;
}

/** 单个下载任务文件的持久化播放位置。 */
export interface PlaybackCheckpoint {
  taskId: string;
  fileIndex?: number;
  positionSeconds: number;
  durationSeconds: number;
  completed: boolean;
  watchedReported: boolean;
  updatedAt: string;
}

/** 播放器保存当前位置时使用的最小输入。 */
export interface SavePlaybackCheckpointInput {
  taskId: string;
  fileIndex?: number;
  positionSeconds: number;
  durationSeconds: number;
  completed?: boolean;
}

export interface AnimeDetailResult {
  anime: Anime;
  myAnime?: MyAnime;
  episodes: Episode[];
  fansubGroups: FansubGroup[];
  stale: boolean;
  partialErrors: Array<{
    source: string;
    message: string;
  }>;
}

export interface EpisodeReleasePreview {
  animeId: string;
  episodeId: string;
  searchedTerms: string[];
  candidates: EpisodeReleaseCandidate[];
  errors: ReleaseSearchResult["errors"];
}

export interface AutomationRunResult {
  startedAt: string;
  finishedAt: string;
  checkedEpisodes: number;
  downloaded: Array<{
    animeId: string;
    animeTitle: string;
    episodeId: string;
    episodeNo: number;
    releaseId: string;
    releaseTitle: string;
    downloadTaskId: string;
  }>;
  skipped: Array<{
    animeId: string;
    animeTitle: string;
    episodeId?: string;
    episodeNo?: number;
    reason: string;
  }>;
  errors: Array<{
    animeId?: string;
    animeTitle?: string;
    episodeId?: string;
    episodeNo?: number;
    message: string;
  }>;
}

export interface AutomationSchedulerStatus {
  enabled: boolean;
  running: boolean;
  inFlight: boolean;
  intervalMinutes: number;
  nextRunAt?: string;
  manualCooldownUntil?: string;
  lastRunAt?: string;
  lastResult?: AutomationRunResult;
  lastError?: string;
}

export interface RemoteDeviceInfo {
  id: string;
  name: string;
  scopes: string[];
  createdAt: string;
  lastAccessedAt: string | null;
}

export interface RemoteGatewayStatus {
  running: boolean;
  host: string;
  port: number;
  protocol: "http" | "https";
  lanEnabled: boolean;
  baseUrl: string;
  addresses: string[];
  devices: RemoteDeviceInfo[];
  certificate?: {
    fingerprint: string;
    expiresAt: string;
    authorityCertificatePath: string;
  };
  lastError?: string;
}

export interface SourceSyncRunResult {
  startedAt: string;
  finishedAt: string;
  syncedSourceIds: string[];
  skippedSourceIds: string[];
  addedReleaseCount: number;
  errors: Array<{ sourceId: string; message: string }>;
}

export interface SourceSyncSchedulerStatus {
  enabled: boolean;
  running: boolean;
  inFlight: boolean;
  dailyTime: string;
  nextRunAt?: string;
  lastRunAt?: string;
  lastResult?: SourceSyncRunResult;
  lastError?: string;
}

export interface RemotePairingChallenge {
  code: string;
  expiresAt: string;
}

export type RemotePlaybackMode = "direct" | "hls";

export type RemotePlaybackRequestMode = "direct" | "transcode";

export interface RemotePlaybackEnhancement {
  videoEnhancement: import("./player-contract").PlayerVideoEnhancement;
  frameInterpolation: import("./player-contract").PlayerFrameInterpolation;
}

export interface DesktopPlayerWindowInput {
  taskId: string;
  fileIndex?: number;
}

/** 描述 macOS 独立播放器自定义窗口拖动的指针阶段与屏幕坐标。 */
export type DesktopPlayerWindowDragInput =
  | { phase: "start" | "move"; screenX: number; screenY: number }
  | { phase: "end" };

export interface DesktopPlaybackSessionInput {
  taskId: string;
  fileIndex?: number;
}

export interface MediaToolStatus {
  available: boolean;
  command?: string;
  version?: string;
  error?: string;
}

export interface DesktopMediaToolsStatus {
  ffprobe: MediaToolStatus;
  ffmpeg: MediaToolStatus;
}

export type RemotePlaybackSubtitleType = "ass" | "vtt";

export interface RemotePlaybackSubtitle {
  id: string;
  label: string;
  language?: string;
  type: RemotePlaybackSubtitleType;
  url: string;
  default: boolean;
}

/** 基于当前源帧率、模型 P95、显存和输出上限得到的 AI 插帧计划。 */
export interface RemoteInterpolationCapacity {
  sourceFrameRate: number;
  targetFrameRate: number;
  selectedMultiplier: number;
  maxFeasibleMultiplier: number;
  outputFrameRateCap: number;
  intervalBudgetMs: number;
  estimatedIntervalCostMs: number;
  interpolationP95Ms: number;
  enhancementP95Ms?: number;
  latencySampleCount: number;
}

export type RemotePlaybackPath = "direct" | "direct-enhanced" | "hls";

export type RemoteDirectEnhancementStatus = "idle" | "probing" | "starting" | "active" | "degraded";

/** 远程浏览器上报的 WebCodecs/WebGPU 直传增强运行快照。 */
export interface RemoteDirectEnhancementReport {
  sequence: number;
  status: RemoteDirectEnhancementStatus;
  capabilitySupported: boolean;
  webCodecs: boolean;
  audioWebCodecs: boolean;
  audioContext: boolean;
  shader: boolean;
  webGpu: boolean;
  offscreenCanvas: boolean;
  mediaCapabilities: boolean;
  supportedCodecs: string[];
  smoothCodecs: string[];
  powerEfficientCodecs: string[];
  requestedPreset?: "balanced" | "clear";
  effectivePreset?: "balanced" | "clear";
  audioClock?: "audio-context";
  hasAudioTrack?: boolean;
  renderedFrames: number;
  droppedFrames: number;
  droppedFrameRatio: number;
  frameBudgetMs?: number;
  gpuQueueP95Ms?: number;
  currentAvDriftMs?: number;
  maximumAvDriftMs?: number;
  rangeRequestCount: number;
  receivedRangeBytes: number;
  rangeRetryCount: number;
  recoveredRangeCount: number;
  networkFailureCount: number;
  gpuEstimatedWorkingSetBytes?: number;
  gpuResourceBudgetBytes?: number;
  degradationReason?: string;
}

export interface RemoteDirectEnhancementDiagnostics extends RemoteDirectEnhancementReport {
  reportedAt: string;
}

/** 远程播放实际采用的传输、编码、模型和终端增强路径。 */
export interface RemotePlaybackDiagnostics {
  playbackPath?: RemotePlaybackPath;
  encoder?: string;
  encoderDegraded: boolean;
  subtitleMode?: "soft" | "burned";
  enhancedFrameInput: boolean;
  modelBackend?: string;
  videoEnhancement: import("./player-contract").PlayerVideoEnhancement;
  frameInterpolation: import("./player-contract").PlayerFrameInterpolation;
  interpolationCapacity?: RemoteInterpolationCapacity;
  degradationReason?: string;
  directEnhancement?: RemoteDirectEnhancementDiagnostics;
}

export interface RemotePlaybackSession {
  id: string;
  taskId: string;
  fileIndex?: number;
  fileName: string;
  mode: RemotePlaybackMode;
  streamUrl: string;
  expiresAt: string;
  durationSeconds?: number;
  startPositionSeconds?: number;
  streamStartPositionSeconds?: number;
  subtitles: RemotePlaybackSubtitle[];
  diagnostics?: RemotePlaybackDiagnostics;
}

export interface MetadataProvider {
  searchAnime(query: AnimeSearchQuery): Promise<Anime[]>;
  getSeasonAnime(year: number, season: Season): Promise<Anime[]>;
  getAnimeDetail(id: string): Promise<Anime>;
}

export interface ReleaseSource {
  config: ReleaseSourceConfig;
  searchReleases(query: ReleaseQuery): Promise<Release[]>;
  listLatestByFansub(groupId: string): Promise<Release[]>;
  listLatestByAnime(animeId: string): Promise<Release[]>;
}

/** 可提供单番 RSS 的下载源能力；不支持 RSS 的来源无需实现。 */
export interface AnimeRssSubscriptionSource extends ReleaseSource {
  readonly animeRssBindingError?: string;
  buildAnimeRssSubscription(context: AnimeRssSubscriptionContext): AnimeRssFeedDescriptor | undefined;
  fetchAnimeRssSubscription(subscription: AnimeRssFeedDescriptor): Promise<Release[]>;
}

export interface TorrentEngine {
  addMagnet(magnetUrl: string, options: AddTorrentOptions): Promise<DownloadTask>;
  addTorrentFile(filePath: string, options: AddTorrentOptions): Promise<DownloadTask>;
  listTasks(): Promise<DownloadTask[]>;
  getTask(taskId: string): Promise<DownloadTask>;
  getFiles(taskId: string): Promise<TorrentFile[]>;
  setFilePriority(taskId: string, fileIndexes: number[], priority: number): Promise<void>;
  pause(taskId: string): Promise<void>;
  resume(taskId: string): Promise<void>;
  remove(taskId: string, deleteFiles: boolean): Promise<void>;
}

export interface MediaInfoExtractor {
  name: string;
  extract(input: MediaExtractInput): Promise<PartialMediaInfo | null>;
}

export interface MediaProbeService {
  probe(filePath: string, context?: MediaProbeContext): Promise<MediaFile>;
  extractFromChain(input: MediaExtractInput): Promise<PartialMediaInfo>;
}

export interface PlayerService {
  play(filePath: string, profileId?: string): Promise<void>;
  reveal(filePath: string): Promise<void>;
}

export type PlayerRuntimePlatform = "windows" | "macos" | "linux" | "other";

export interface PlayerDetectionCandidate {
  profileId: string;
  name: string;
  configuredPath: string;
  available: boolean;
  resolvedPath?: string;
}

export interface PlayerDetectionResult {
  platform: PlayerRuntimePlatform;
  candidates: PlayerDetectionCandidate[];
  detectedProfileId?: string;
  detectedExecutablePath?: string;
}

export interface SelectPlayerExecutableInput {
  profileId: string;
  currentPath?: string;
}

export interface PlatformService {
  getDefaultDownloadDir(): Promise<string>;
  getAppDataDir(): Promise<string>;
  openFolder(path: string): Promise<void>;
  revealFile(path: string): Promise<void>;
}

export interface NotificationService {
  notify(title: string, body: string): Promise<void>;
}

export interface SettingsService {
  getSettings(): Promise<AppSettings>;
  updateSettings(settings: Partial<AppSettings>): Promise<AppSettings>;
  resetSettingsToDefaults(): Promise<AppSettings>;
}

export interface FansubService {
  listGroups(): Promise<FansubGroup[]>;
  upsertGroup(group: FansubGroup): Promise<FansubGroup>;
}
