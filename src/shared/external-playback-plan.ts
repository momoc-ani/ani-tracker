import type {
  RemotePlaybackEnhancement,
  RemotePlaybackRequestMode
} from "./contracts";

export interface ExternalPlaybackPlan {
  mode: RemotePlaybackRequestMode;
  enhancement: RemotePlaybackEnhancement;
  subtitleMode: "burned" | "off";
  subtitleId?: string;
}

/**
 * 外部协议只传递一个媒体 URL。原文件模式必须保持完整文件直传；只有用户明确选择
 * 实时转码时，才允许把当前字幕烧录进 HLS。
 */
export function planExternalPlayback(
  requestedMode: RemotePlaybackRequestMode,
  enhancement: RemotePlaybackEnhancement,
  selectedSubtitleId?: string
): ExternalPlaybackPlan {
  const subtitleId = selectedSubtitleId?.trim() || undefined;
  if (requestedMode === "direct" || !subtitleId) {
    return {
      mode: requestedMode,
      enhancement,
      subtitleMode: "off"
    };
  }
  return {
    mode: "transcode",
    enhancement,
    subtitleMode: "burned",
    subtitleId
  };
}

/** 原文件交给外部播放器自行读取完整时间轴，只有 HLS 继承网页当前进度。 */
export function resolveExternalPlaybackStartPosition(
  mode: RemotePlaybackRequestMode,
  currentTimeSeconds: number
): number | undefined {
  return mode === "transcode"
    && Number.isFinite(currentTimeSeconds)
    && currentTimeSeconds >= 0
    ? currentTimeSeconds
    : undefined;
}
