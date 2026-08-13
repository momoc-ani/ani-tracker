import type { RemotePlaybackEnhancement, RemotePlaybackRequestMode } from "@shared/contracts";

const REMOTE_PLAYBACK_MODE_KEY = "ani.remotePlayer.defaultMode";
const REMOTE_PLAYBACK_ENHANCEMENT_KEY = "ani.remotePlayer.enhancement";

export const DEFAULT_REMOTE_PLAYBACK_ENHANCEMENT: RemotePlaybackEnhancement = {
  videoEnhancement: "off",
  frameInterpolation: "off"
};

/** 读取当前远程设备的默认播放模式。 */
export function readRemotePlaybackMode(): RemotePlaybackRequestMode {
  try {
    return window.localStorage.getItem(REMOTE_PLAYBACK_MODE_KEY) === "transcode"
      ? "transcode"
      : "direct";
  } catch (error) {
    console.warn("[remote] 默认播放模式读取失败", { error });
    return "direct";
  }
}

/** 保存当前远程设备的默认播放模式。 */
export function storeRemotePlaybackMode(mode: RemotePlaybackRequestMode): void {
  try {
    window.localStorage.setItem(REMOTE_PLAYBACK_MODE_KEY, mode);
    console.info("[remote] 默认播放模式已保存", { mode });
  } catch (error) {
    console.warn("[remote] 默认播放模式保存失败", { mode, error });
  }
}

/** 读取远程转码增强偏好，并丢弃旧版本或伪造客户端写入的模式。 */
export function readRemotePlaybackEnhancement(): RemotePlaybackEnhancement {
  try {
    const value = JSON.parse(window.localStorage.getItem(REMOTE_PLAYBACK_ENHANCEMENT_KEY) ?? "null") as Partial<RemotePlaybackEnhancement> | null;
    return {
      videoEnhancement: value?.videoEnhancement === "balanced" || value?.videoEnhancement === "clear"
        ? value.videoEnhancement
        : "off",
      frameInterpolation: value?.frameInterpolation === "motion-compensated"
        ? value.frameInterpolation
        : "off"
    };
  } catch (error) {
    console.warn("[remote] 转码增强偏好读取失败", { error });
    return { ...DEFAULT_REMOTE_PLAYBACK_ENHANCEMENT };
  }
}

/** 保存只会交给实时转码链的画质增强与运动补偿偏好。 */
export function storeRemotePlaybackEnhancement(enhancement: RemotePlaybackEnhancement): void {
  try {
    window.localStorage.setItem(REMOTE_PLAYBACK_ENHANCEMENT_KEY, JSON.stringify(enhancement));
  } catch (error) {
    console.warn("[remote] 转码增强偏好保存失败", { enhancement, error });
  }
}
