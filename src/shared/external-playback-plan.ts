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

const DISABLED_ENHANCEMENT: RemotePlaybackEnhancement = {
  videoEnhancement: "off",
  frameInterpolation: "off"
};

/** 外部协议只传递一个媒体 URL；选中字幕时切到 HLS，并在增强后烧录该轨道。 */
export function planExternalPlayback(
  requestedMode: RemotePlaybackRequestMode,
  enhancement: RemotePlaybackEnhancement,
  selectedSubtitleId?: string
): ExternalPlaybackPlan {
  const subtitleId = selectedSubtitleId?.trim() || undefined;
  if (!subtitleId) {
    return {
      mode: requestedMode,
      enhancement,
      subtitleMode: "off"
    };
  }
  return {
    mode: "transcode",
    enhancement: requestedMode === "transcode" ? enhancement : DISABLED_ENHANCEMENT,
    subtitleMode: "burned",
    subtitleId
  };
}
