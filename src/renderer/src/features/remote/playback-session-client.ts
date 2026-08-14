import {
  closeRemotePlaybackSession,
  createRemotePlaybackSession,
  getRemotePlaybackSession,
  reportRemoteDirectEnhancementDiagnostics
} from "@/lib/api";
import type { PlaybackSessionClient } from "@/features/player/playback-session-client";

export type { PlaybackSessionClient } from "@/features/player/playback-session-client";

/** 使用远程 HTTP 鉴权接口创建播放器会话。 */
export const remotePlaybackSessionClient: PlaybackSessionClient = {
  create: createRemotePlaybackSession,
  refresh: getRemotePlaybackSession,
  reportDirectEnhancement: reportRemoteDirectEnhancementDiagnostics,
  close: closeRemotePlaybackSession
};
