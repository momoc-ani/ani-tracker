import type {
  RemotePlaybackEnhancement,
  RemotePlaybackRequestMode,
  RemotePlaybackSession
} from "@shared/contracts";
import { appApi } from "@/lib/api";

export interface PlaybackSessionClient {
  create(
    taskId: string,
    mode: RemotePlaybackRequestMode,
    fileIndex: number | undefined,
    enhancement: RemotePlaybackEnhancement
  ): Promise<RemotePlaybackSession>;
  close(sessionId: string): Promise<void>;
}

/** 使用本地 AppClient 创建受控播放器会话。 */
export const desktopPlaybackSessionClient: PlaybackSessionClient = {
  create: (taskId, _mode, fileIndex, _enhancement) => appApi.createDesktopPlaybackSession({
    taskId,
    ...(fileIndex === undefined ? {} : { fileIndex })
  }),
  close: (sessionId) => appApi.closeDesktopPlaybackSession(sessionId)
};
