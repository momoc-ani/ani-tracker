/** 播放器后端类型，网页端与原生端通过同一行为契约接入。 */
export type PlayerBackend = "artplayer" | "libvlc" | "mpv";

/** GPU 视频增强预设；字幕和 OSD 在增强后合成。 */
export type PlayerVideoEnhancement = "off" | "balanced" | "clear";

/** 基于模型的实时补帧模式；当前仅在真实模型运行时可用时展示。 */
export type PlayerFrameInterpolation = "off" | "rife-realtime";

/** 增强链路的诊断快照，便于区分 Shader、模型和降级状态。 */
export interface PlayerEnhancementDiagnostics {
  pipeline: string;
  gpuVendor?: string;
  renderer?: string;
  decoder?: string;
  modelBackend?: string;
  frameTimeMs?: number;
  droppedFrames: number;
  degradationReason?: string;
}

/** 播放器宿主平台。 */
export type PlayerHostPlatform = "remote-web" | "tauri-desktop" | "android" | "ios";

/** 播放器运行时可用状态。 */
export type PlayerAvailability = "unknown" | "available" | "unavailable";

/** 播放器生命周期与播放状态。 */
export type PlayerStatus =
  | "idle"
  | "loading"
  | "ready"
  | "buffering"
  | "playing"
  | "paused"
  | "ended"
  | "error"
  | "closed";

/** 播放器可识别的结构化错误。 */
export type PlayerErrorCode =
  | "resource-unavailable"
  | "network"
  | "decoder"
  | "permission"
  | "transcode"
  | "runtime-missing"
  | "unsupported"
  | "unknown";

/** 错误提示可提供的恢复动作。 */
export type PlayerRecoveryAction = "retry" | "transcode" | "close";

/** 画面比例选项，custom 用于后端返回的额外比例。 */
export type PlayerAspectRatio = "default" | "16:9" | "4:3" | "fill" | "fit" | "custom";

/** 内置播放器支持的离散字幕缩放比例。 */
export type PlayerSubtitleScale = 100 | 125 | 150 | 175 | 200;

/** UI 与各平台后端共用的字幕缩放选项。 */
export const PLAYER_SUBTITLE_SCALES: readonly PlayerSubtitleScale[] = [100, 125, 150, 175, 200];

/** 播放进度达到该百分比后视为已完成。 */
export const PLAYER_COMPLETION_THRESHOLD_PERCENT = 90;

/** 根据播放器状态和有效进度判断当前媒体是否完成。 */
export function isPlaybackCompleted(
  status: PlayerStatus,
  positionSeconds: number,
  durationSeconds: number
): boolean {
  if (status === "ended") return true;
  if (
    !Number.isFinite(positionSeconds)
    || !Number.isFinite(durationSeconds)
    || positionSeconds < 0
    || durationSeconds <= 0
  ) return false;
  return positionSeconds / durationSeconds * 100 >= PLAYER_COMPLETION_THRESHOLD_PERCENT;
}

/** 播放器错误的跨平台展示模型。 */
export interface PlayerError {
  code: PlayerErrorCode;
  message: string;
  recoverable: boolean;
  recoveryActions: PlayerRecoveryAction[];
}

/** 后端能力声明，UI 只展示当前实现支持的操作。 */
export interface PlayerCapabilities {
  backend: PlayerBackend;
  platform: PlayerHostPlatform;
  availability: PlayerAvailability;
  canSeek: boolean;
  canSetVolume: boolean;
  canMute: boolean;
  playbackRates: number[];
  supportsAudioTracks: boolean;
  supportsSubtitleTracks: boolean;
  supportsSubtitleScale: boolean;
  supportsVideoEnhancement: boolean;
  supportsFrameInterpolation: boolean;
  supportsModelEnhancement: boolean;
  supportsAspectRatio: boolean;
  supportsFullscreen: boolean;
  supportsPictureInPicture: boolean;
  supportsPlaylistNavigation: boolean;
  supportsDirectPlayback: boolean;
  supportsTranscodingFallback: boolean;
  supportsHdr: boolean;
  unavailableReason?: string;
}

/** VLC 与 ArtPlayer 均可消费的字幕描述。 */
export interface PlayerSubtitleSource {
  id: string;
  label: string;
  language?: string;
  type: "ass" | "vtt";
  uri: string;
  default: boolean;
}

/** 播放后端加载的受控媒体资源，不包含未经授权的本地真实路径。 */
export interface PlayerMediaSource {
  taskId: string;
  fileIndex?: number;
  title: string;
  animeTitle?: string;
  description?: string;
  artworkUri?: string;
  uri: string;
  mode: "direct" | "hls";
  durationSeconds?: number;
  subtitles: PlayerSubtitleSource[];
}

/** 音频或字幕轨道。 */
export interface PlayerTrack {
  id: string;
  kind: "audio" | "subtitle";
  label: string;
  language?: string;
  selected: boolean;
}

/** 跨平台播放列表中的单集。 */
export interface PlayerPlaylistItem {
  id: string;
  taskId: string;
  fileIndex?: number;
  title: string;
  episodeLabel?: string;
  durationSeconds?: number;
}

/** 当前播放列表与活动项。 */
export interface PlayerPlaylist {
  items: PlayerPlaylistItem[];
  activeItemId?: string;
}

interface PlayerCommandBase {
  commandId: string;
  sessionId: string;
}

/** 所有后端必须识别的播放器命令。 */
export type PlayerCommand =
  | (PlayerCommandBase & { type: "load"; source: PlayerMediaSource; startPositionSeconds?: number })
  | (PlayerCommandBase & { type: "play" })
  | (PlayerCommandBase & { type: "pause" })
  | (PlayerCommandBase & { type: "seek"; positionSeconds: number })
  | (PlayerCommandBase & { type: "set-volume"; volume: number })
  | (PlayerCommandBase & { type: "set-muted"; muted: boolean })
  | (PlayerCommandBase & { type: "set-rate"; rate: number })
  | (PlayerCommandBase & { type: "select-audio-track"; trackId: string })
  | (PlayerCommandBase & { type: "select-subtitle-track"; trackId?: string })
  | (PlayerCommandBase & { type: "set-subtitle-scale"; subtitleScale: PlayerSubtitleScale })
  | (PlayerCommandBase & { type: "set-video-enhancement"; videoEnhancement: PlayerVideoEnhancement })
  | (PlayerCommandBase & { type: "set-frame-interpolation"; frameInterpolation: PlayerFrameInterpolation })
  | (PlayerCommandBase & { type: "set-aspect-ratio"; aspectRatio: PlayerAspectRatio; value?: string })
  | (PlayerCommandBase & { type: "set-fullscreen"; fullscreen: boolean })
  | (PlayerCommandBase & { type: "set-picture-in-picture"; enabled: boolean })
  | (PlayerCommandBase & { type: "previous-item" })
  | (PlayerCommandBase & { type: "next-item" })
  | (PlayerCommandBase & { type: "retry" })
  | (PlayerCommandBase & { type: "close" });

/** 命令执行结果；不支持的能力以结构化结果返回。 */
export type PlayerCommandResult =
  | { commandId: string; accepted: true }
  | { commandId: string; accepted: false; error: PlayerError };

/** 后端发给 UI 的完整状态快照。 */
export interface PlayerSnapshot {
  sessionId: string;
  sequence: number;
  backend: PlayerBackend;
  platform: PlayerHostPlatform;
  status: PlayerStatus;
  capabilities: PlayerCapabilities;
  source?: PlayerMediaSource;
  playlist: PlayerPlaylist;
  positionSeconds: number;
  durationSeconds: number;
  bufferedSeconds: number;
  volume: number;
  muted: boolean;
  playbackRate: number;
  audioTracks: PlayerTrack[];
  subtitleTracks: PlayerTrack[];
  subtitleScale: PlayerSubtitleScale;
  videoEnhancement: PlayerVideoEnhancement;
  videoEnhancementDegraded: boolean;
  frameInterpolation: PlayerFrameInterpolation;
  enhancementDiagnostics: PlayerEnhancementDiagnostics;
  aspectRatio: PlayerAspectRatio;
  fullscreen: boolean;
  pictureInPicture: boolean;
  error?: PlayerError;
}

/** 创建快照时必须提供的宿主信息。 */
export interface InitialPlayerSnapshotInput {
  sessionId: string;
  capabilities: PlayerCapabilities;
  playlist?: PlayerPlaylist;
}

/** 创建后端尚不可用时的保守能力声明。 */
export function createUnavailablePlayerCapabilities(
  backend: PlayerBackend,
  platform: PlayerHostPlatform,
  unavailableReason: string
): PlayerCapabilities {
  return {
    backend,
    platform,
    availability: "unavailable",
    canSeek: false,
    canSetVolume: false,
    canMute: false,
    playbackRates: [1],
    supportsAudioTracks: false,
    supportsSubtitleTracks: false,
    supportsSubtitleScale: false,
    supportsVideoEnhancement: false,
    supportsFrameInterpolation: false,
    supportsModelEnhancement: false,
    supportsAspectRatio: false,
    supportsFullscreen: false,
    supportsPictureInPicture: false,
    supportsPlaylistNavigation: false,
    supportsDirectPlayback: false,
    supportsTranscodingFallback: false,
    supportsHdr: false,
    unavailableReason
  };
}

/** 播放器后端的最小适配接口。 */
export interface UnifiedPlayerAdapter {
  /** 返回不会在会话中途隐式扩大的后端能力。 */
  getCapabilities(): PlayerCapabilities;
  /** 返回适配器当前的完整状态。 */
  getSnapshot(): PlayerSnapshot;
  /** 执行带会话标识的播放器命令。 */
  dispatch(command: PlayerCommand): Promise<PlayerCommandResult>;
  /** 订阅完整状态快照并返回取消订阅函数。 */
  subscribe(listener: (snapshot: PlayerSnapshot) => void): () => void;
  /** 幂等释放媒体、监听器和原生句柄。 */
  dispose(): Promise<void>;
}

/** 创建确定性的空闲播放器快照。 */
export function createInitialPlayerSnapshot(input: InitialPlayerSnapshotInput): PlayerSnapshot {
  return {
    sessionId: input.sessionId,
    sequence: 0,
    backend: input.capabilities.backend,
    platform: input.capabilities.platform,
    status: "idle",
    capabilities: input.capabilities,
    playlist: input.playlist ?? { items: [] },
    positionSeconds: 0,
    durationSeconds: 0,
    bufferedSeconds: 0,
    volume: 1,
    muted: false,
    playbackRate: 1,
    audioTracks: [],
    subtitleTracks: [],
    subtitleScale: 100,
    videoEnhancement: "off",
    videoEnhancementDegraded: false,
    frameInterpolation: "off",
    enhancementDiagnostics: {
      pipeline: "none",
      droppedFrames: 0
    },
    aspectRatio: "default",
    fullscreen: false,
    pictureInPicture: false
  };
}

/** 仅接受活动会话中序号递增的快照，过滤切集后的迟到事件。 */
export function acceptPlayerSnapshot(
  activeSessionId: string,
  current: PlayerSnapshot | undefined,
  incoming: PlayerSnapshot
): PlayerSnapshot | undefined {
  if (incoming.sessionId !== activeSessionId) {
    return current;
  }
  if (current?.sessionId === incoming.sessionId && incoming.sequence <= current.sequence) {
    return current;
  }
  const legacyPayload = incoming as unknown as {
    capabilities?: Partial<PlayerCapabilities>;
    enhancementDiagnostics?: Partial<PlayerEnhancementDiagnostics>;
    frameInterpolation?: PlayerFrameInterpolation;
    videoEnhancement?: PlayerVideoEnhancement;
    videoEnhancementDegraded?: boolean;
  };
  const diagnostics = legacyPayload.enhancementDiagnostics;
  const needsNormalization = legacyPayload.capabilities?.supportsFrameInterpolation === undefined
    || legacyPayload.capabilities?.supportsModelEnhancement === undefined
    || legacyPayload.videoEnhancement === undefined
    || legacyPayload.videoEnhancementDegraded === undefined
    || legacyPayload.frameInterpolation === undefined
    || diagnostics?.pipeline === undefined
    || diagnostics?.droppedFrames === undefined;
  if (!needsNormalization) return incoming;
  // 允许旧版原生后端缺少终版增强字段，避免控制层读取 undefined。
  return {
    ...incoming,
    capabilities: {
      ...incoming.capabilities,
      supportsFrameInterpolation: legacyPayload.capabilities?.supportsFrameInterpolation ?? false,
      supportsModelEnhancement: legacyPayload.capabilities?.supportsModelEnhancement ?? false
    },
    videoEnhancement: incoming.videoEnhancement ?? "off",
    videoEnhancementDegraded: incoming.videoEnhancementDegraded ?? false,
    frameInterpolation: incoming.frameInterpolation ?? "off",
    enhancementDiagnostics: {
      ...(diagnostics ?? {}),
      pipeline: diagnostics?.pipeline ?? "none",
      droppedFrames: diagnostics?.droppedFrames ?? 0
    }
  };
}

/** 构造后端不支持命令时的统一拒绝结果。 */
export function rejectUnsupportedPlayerCommand(commandId: string, message: string): PlayerCommandResult {
  return {
    commandId,
    accepted: false,
    error: {
      code: "unsupported",
      message,
      recoverable: false,
      recoveryActions: []
    }
  };
}
