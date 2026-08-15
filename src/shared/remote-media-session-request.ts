import type {
  RemotePlaybackEnhancement,
  RemotePlaybackRequestMode
} from "./contracts";

export interface RemoteMediaSessionRequestInput {
  taskId: string;
  mode: RemotePlaybackRequestMode;
  fileIndex?: number;
  enhancement: RemotePlaybackEnhancement;
  startPositionSeconds?: number;
  subtitleMode?: "soft" | "burned" | "off";
  subtitleId?: string;
}

export type RemoteMediaSessionRequest = RemoteMediaSessionRequestInput;

/** 仅发送调用方明确指定的可选字段，保持远程前后端滚动升级兼容。 */
export function buildRemoteMediaSessionRequest(
  input: RemoteMediaSessionRequestInput
): RemoteMediaSessionRequest {
  return {
    taskId: input.taskId,
    mode: input.mode,
    enhancement: input.enhancement,
    ...(input.fileIndex === undefined ? {} : { fileIndex: input.fileIndex }),
    ...(input.startPositionSeconds === undefined
      ? {}
      : { startPositionSeconds: input.startPositionSeconds }),
    ...(input.subtitleMode === undefined ? {} : { subtitleMode: input.subtitleMode }),
    ...(input.subtitleId === undefined ? {} : { subtitleId: input.subtitleId })
  };
}
