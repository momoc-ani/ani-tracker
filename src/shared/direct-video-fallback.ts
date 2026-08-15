export const DIRECT_VIDEO_FRAME_TIMEOUT_MS = 8_000;
export const DIRECT_VIDEO_MIN_PROGRESS_SECONDS = 1;

export interface DirectVideoFallbackInput {
  mode: "direct" | "hls";
  directEnhancementActive: boolean;
  playing: boolean;
  elapsedMs: number;
  mediaTimeProgressSeconds: number;
  videoWidth: number;
  videoHeight: number;
}

/** 浏览器可能只解出音频且不抛媒体错误；超过首帧门限后应升级到 HLS。 */
export function shouldFallbackDirectVideo(input: DirectVideoFallbackInput): boolean {
  return input.mode === "direct"
    && !input.directEnhancementActive
    && input.playing
    && input.elapsedMs >= DIRECT_VIDEO_FRAME_TIMEOUT_MS
    && input.mediaTimeProgressSeconds >= DIRECT_VIDEO_MIN_PROGRESS_SECONDS
    && (!(input.videoWidth > 0) || !(input.videoHeight > 0));
}
