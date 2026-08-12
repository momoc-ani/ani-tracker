import type { AppearanceSettings } from "./theme";

export type AnimeStatus = "watching" | "planned" | "completed" | "paused" | "dropped";

export type EpisodeStatus =
  | "upcoming"
  | "aired"
  | "matched"
  | "downloading"
  | "downloaded"
  | "watched";

export type DownloadStatus =
  | "queued"
  | "fetching_metadata"
  | "downloading"
  | "stalled"
  | "waiting_network"
  | "paused"
  | "checking"
  | "moving"
  | "completed"
  | "seeding"
  | "error"
  | "missing_files";

export type Season = "winter" | "spring" | "summer" | "fall";

export type AnimeFormat = "tv" | "movie" | "ova" | "ona" | "special" | "music" | "unknown";

export type AnimeAiringStatus = "upcoming" | "airing" | "finished" | "hiatus" | "cancelled" | "unknown";

export interface AnimeBroadcastSchedule {
  weekday?: number;
  time?: string;
  timezone?: string;
}

export interface AnimeStaffCredit {
  name: string;
  role: string;
  source?: string;
}

export interface AnimeRanking {
  rank: number;
  source: string;
  category?: string;
}

export interface AnimeDetailMetadata {
  bannerUrl?: string;
  format?: AnimeFormat;
  episodeCount?: number;
  airingStatus?: AnimeAiringStatus;
  endDate?: string;
  nextAiringAt?: string;
  nextAiringEpisodeNo?: number;
  broadcast?: AnimeBroadcastSchedule;
  genres?: string[];
  studios?: string[];
  staff?: AnimeStaffCredit[];
  sourceMaterial?: string;
  durationMinutes?: number;
  contentRating?: string;
  demographic?: string;
  countryOfOrigin?: string;
  ranking?: AnimeRanking;
  metadataSources?: string[];
  refreshedAt?: string;
}

export type SubtitleLanguage = "chs" | "cht" | "jpn" | "eng";

export type SubtitlePreference = SubtitleLanguage | "multi";

export type VideoBitDepth = 8 | 10 | 12;

export type NormalizedVideoCodec = "H.264/AVC" | "H.265/HEVC" | "AV1" | "VP9" | "Unknown";

export type SourceKind = "rss" | "torznab" | "site_adapter" | "manual";

export type TorrentEngineKind = "embedded" | "qbittorrent";

export type ReleaseContentKind = "episode" | "range" | "batch" | "unknown";

export interface Anime {
  id: string;
  title: string;
  originalTitle?: string;
  aliases: AnimeAlias[];
  premiereDate?: string;
  premiereYear: number;
  premiereMonth: number;
  season?: Season;
  summary?: string;
  coverUrl?: string;
  rating?: AnimeRating;
  externalIds: Record<string, string>;
  detail?: AnimeDetailMetadata;
}

export interface AnimeRating {
  score: number;
  count?: number;
  source: string;
}

export interface AnimeAlias {
  id: string;
  animeId: string;
  alias: string;
  language: "zh" | "ja" | "en" | "romaji" | "custom";
  priority: number;
}

export interface MyAnime {
  id: string;
  anime: Anime;
  status: AnimeStatus;
  defaultFansubGroupId?: string;
  autoDownload: boolean;
  downloadDir?: string;
  rssSubscriptions?: AnimeRssSubscription[];
  preferredResolution?: "720p" | "1080p" | "2160p";
  preferredCodec?: NormalizedVideoCodec;
  preferredBitDepth?: VideoBitDepth;
  preferredSubtitleLanguages?: SubtitleLanguage[];
  /** 兼容旧数据，读取时会迁移为 preferredSubtitleLanguages。 */
  preferredSubtitle?: SubtitlePreference;
  addedAt: string;
  updatedAt: string;
}

export interface AnimeRssSubscription {
  id: string;
  myAnimeId: string;
  name: string;
  url: string;
  enabled: boolean;
  preferredSubtitleLanguages?: SubtitleLanguage[];
  /** 兼容旧数据，读取时会迁移为 preferredSubtitleLanguages。 */
  preferredSubtitle?: SubtitlePreference;
  refreshIntervalMinutes?: number;
  lastFetchedAt?: string;
  createdAt: string;
  updatedAt: string;
}

export type AnimeSourceBindingMatchMethod = "manual" | "external_id" | "scored";

export interface AnimeSourceBinding {
  id: string;
  animeId: string;
  sourceId: string;
  sourceAnimeId: string;
  sourceAnimeTitle?: string;
  sourceUrl?: string;
  matchMethod: AnimeSourceBindingMatchMethod;
  confidence: number;
  confirmed: boolean;
  createdAt: string;
  updatedAt: string;
}

export type AnimeSourceExclusionScope = "candidate" | "source";

/** 记录当前番剧对单个候选或整个下载源的人工排除决定。 */
export interface AnimeSourceExclusion {
  id: string;
  animeId: string;
  sourceId: string;
  scope: AnimeSourceExclusionScope;
  sourceAnimeId?: string;
  sourceAnimeTitle?: string;
  createdAt: string;
  updatedAt: string;
}

export interface Episode {
  id: string;
  animeId: string;
  episodeNo: number;
  title?: string;
  airTime?: string;
  status: EpisodeStatus;
}

export interface EpisodePreference {
  id: string;
  animeId: string;
  episodeId: string;
  fansubGroupId?: string;
  releaseId?: string;
  isManualOverride: boolean;
}

export interface FansubGroup {
  id: string;
  name: string;
  aliases: string[];
  sourceIds: string[];
}

export interface ReleaseSourceConfig {
  id: string;
  name: string;
  kind: SourceKind;
  enabled: boolean;
  useProxy?: boolean;
  requestIntervalMs?: number;
  baseUrl?: string;
  apiKey?: string;
  rssUrl?: string;
  tags?: string[];
}

export interface ReleaseSourceSyncState {
  sourceId: string;
  requestHost?: string;
  lastRequestAt?: string;
  requestFailureCount: number;
  backoffUntil?: string;
  lastSyncAttemptAt?: string;
  lastSuccessfulSyncAt?: string;
  lastSyncError?: string;
  etag?: string;
  lastModified?: string;
}

/** 记录通用网络请求熔断状态，与具体业务同步状态解耦。 */
export interface RequestCircuitState {
  key: string;
  group: string;
  requestHost?: string;
  lastRequestAt?: string;
  failureCount: number;
  backoffUntil?: string;
  /** 网络级失败所属的运行时上下文；provider 级失败为空。 */
  networkContext?: string;
}

export interface ReleaseSourceMeta {
  sourceUrl?: string;
  rssUrl?: string;
  mikanBangumiId?: string;
  mikanSubgroupId?: string;
  mikanSubgroupName?: string;
}

export interface ReleaseEpisodeRange {
  start: number;
  end: number;
}

export interface Release {
  id: string;
  title: string;
  animeId?: string;
  episodeNo?: number;
  episodeRange?: ReleaseEpisodeRange;
  seriesSeasonNo?: number;
  contentKind?: ReleaseContentKind;
  fansubGroupId?: string;
  fansubName?: string;
  sourceId: string;
  sourceName: string;
  magnetUrl?: string;
  torrentUrl?: string;
  infoHash?: string;
  size?: number;
  resolution?: "720p" | "1080p" | "2160p";
  declaredVideoCodec?: string;
  normalizedVideoCodec?: NormalizedVideoCodec;
  bitDepth?: VideoBitDepth;
  subtitleLanguages?: SubtitleLanguage[];
  subtitle?: SubtitlePreference;
  publishedAt: string;
  seeders?: number;
  sourceMeta?: ReleaseSourceMeta;
}

export interface TorrentFile {
  id: string;
  index: number;
  name: string;
  episodeId?: string;
  episodeNo?: number;
  size: number;
  progress: number;
  priority: number;
  selected: boolean;
}

export interface DownloadTask {
  id: string;
  releaseId?: string;
  animeId?: string;
  episodeId?: string;
  animeTitle?: string;
  episodeNo?: number;
  fansubGroupId?: string;
  fansubName?: string;
  resolution?: Release["resolution"];
  declaredVideoCodec?: string;
  normalizedVideoCodec?: NormalizedVideoCodec;
  bitDepth?: VideoBitDepth;
  subtitleLanguages?: SubtitleLanguage[];
  subtitle?: SubtitlePreference;
  correlationTag?: string;
  engine: TorrentEngineKind;
  torrentHash?: string;
  name: string;
  status: DownloadStatus;
  progress: number;
  downloadSpeed: number;
  uploadSpeed: number;
  etaSeconds?: number;
  savePath: string;
  files: TorrentFile[];
  createdAt: string;
  completedAt?: string;
}

export interface MediaFile {
  id: string;
  animeId: string;
  episodeId?: string;
  downloadTaskId?: string;
  contentKind: MediaContentKind;
  specialNo?: string;
  filePath: string;
  fileName: string;
  size: number;
  container?: "mkv" | "mp4" | "avi" | "unknown";
  declaredVideoCodec?: string;
  detectedVideoCodec?: string;
  normalizedVideoCodec: NormalizedVideoCodec;
  resolution?: string;
  bitDepth?: number;
  audioCodecs: string[];
  subtitleTracks: string[];
  durationSeconds?: number;
  downloadedAt?: string;
  probedAt?: string;
  origin?: "download" | "imported";
  sourceRoot?: string;
  fingerprint?: string;
  fileModifiedAt?: string;
  availability?: "available" | "changed" | "missing" | "unavailable";
  lastVerifiedAt?: string;
  availabilityError?: string;
}

export type MediaContentKind =
  | "episode"
  | "special"
  | "ova"
  | "oad"
  | "opening"
  | "ending"
  | "pv"
  | "cm"
  | "extra"
  | "unknown";

export interface PlayerProfile {
  id: string;
  name: string;
  executablePath: string;
  argumentTemplate: string;
  supportsMadVr?: boolean;
  platform: "windows" | "macos" | "linux" | "any";
}

export interface DownloadSettings {
  defaultDownloadDir: string;
  createAnimeFolder: boolean;
  animeFolderPattern: string;
  temporaryDownloadDir?: string;
  defaultTorrentEngine: TorrentEngineKind;
  allowMeteredDownloads?: boolean;
  embedded: EmbeddedTorrentSettings;
  qbittorrent: QbittorrentSettings;
}

export interface EmbeddedTorrentSettings {
  enabled: boolean;
  listenPort?: number;
  dhtEnabled?: boolean;
  upnpEnabled?: boolean;
  maxActiveDownloads?: number;
  maxDownloadSpeed?: number;
  maxUploadSpeed?: number;
  seedingLimits?: QbittorrentSeedingLimits;
}

export interface QbittorrentSettings {
  baseUrl: string;
  username: string;
  password?: string;
  autoConnect: boolean;
  downloadLimitKiBps: number;
  uploadLimitKiBps: number;
  seedingLimits: QbittorrentSeedingLimits;
  managed: QbittorrentManagedSettings;
}

export interface QbittorrentSeedingLimits {
  enabled: boolean;
  ratioEnabled: boolean;
  ratioLimit: number;
  timeEnabled: boolean;
  timeLimitMinutes: number;
}

export interface QbittorrentManagedSettings {
  enabled: boolean;
  binaryPath?: string;
  profileDir?: string;
  startupTimeoutMs: number;
}

export interface StorageSettings {
  userDataDir: string;
  databasePath: string;
  cacheDir: string;
  logDir: string;
  backupDir?: string;
}

export interface AutomationSettings {
  scheduledCheckEnabled: boolean;
  checkIntervalMinutes: number;
  notifyOnNewEpisode: boolean;
  autoDownloadEnabledGlobally: boolean;
  fallbackWhenDefaultFansubMissing: "wait" | "candidate" | "notify_only";
  candidateFansubNames: string[];
}

export interface SourceSyncSettings {
  enabled: boolean;
  dailyTime: string;
}

export interface MediaSettings {
  ffprobePath: string;
  ffprobeTimeoutSeconds: number;
  videoExtensions: string[];
}

export interface DesktopSettings {
  minimizeToTray: boolean;
  launchAtLogin: boolean;
}

export type MetadataProxyMode = "off" | "system" | "manual";

export interface MetadataProxySettings {
  mode: MetadataProxyMode;
  url?: string;
  timeoutMs: number;
}

export interface RemoteAccessSettings {
  lanEnabled: boolean;
  port: number;
}

export interface NetworkSettings {
  metadataProxy: MetadataProxySettings;
  remoteAccess: RemoteAccessSettings;
}

export interface AppSettings {
  appearance: AppearanceSettings;
  download: DownloadSettings;
  storage: StorageSettings;
  players: PlayerProfile[];
  defaultPlayerProfileId?: string;
  automation: AutomationSettings;
  sourceSync?: SourceSyncSettings;
  media: MediaSettings;
  desktop: DesktopSettings;
  network: NetworkSettings;
}

export interface DashboardData {
  dailyReminder: DailyReminderSummary;
  todayEpisodes: EpisodeSummary[];
  pendingActions: PendingAction[];
  activeDownloads: DownloadTask[];
  recentCompleted: MediaFile[];
  weeklySchedule: WeeklyScheduleDay[];
  sourceHealth: SourceHealth[];
}

export interface DailyReminderSummary {
  date: string;
  total: number;
  upcoming: number;
  aired: number;
  downloading: number;
  downloaded: number;
  items: DailyReminderItem[];
}

export interface DailyReminderItem {
  id: string;
  animeId: string;
  animeTitle: string;
  episodeId: string;
  episodeNo: number;
  airTime?: string;
  status: EpisodeStatus;
  fansubName?: string;
  downloadTaskId?: string;
}

export interface EpisodeSummary {
  id: string;
  animeTitle: string;
  episodeNo: number;
  airTime?: string;
  status: EpisodeStatus;
  fansubName?: string;
  downloadTaskId?: string;
}

export interface PendingAction {
  id: string;
  title: string;
  description: string;
  severity: "info" | "warning" | "error";
  animeId?: string;
  episodeId?: string;
  episodeNo?: number;
}

export interface WeeklyScheduleDay {
  day: string;
  items: EpisodeSummary[];
}

export interface SourceHealth {
  sourceId: string;
  name: string;
  status: "ok" | "warning" | "offline";
  lastCheckedAt?: string;
}

export type NotificationKind = "automation" | "download" | "reminder" | "system";

export interface NotificationRecord {
  id: string;
  kind: NotificationKind;
  title: string;
  body: string;
  severity: "info" | "success" | "warning" | "error";
  animeId?: string;
  episodeId?: string;
  downloadTaskId?: string;
  createdAt: string;
  readAt?: string;
}
