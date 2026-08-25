import type { PlayerVideoEnhancement } from "@shared/player-contract";

const VIDEO_ENHANCEMENT_STORAGE_KEY = "ani.player.videoEnhancement";

export function normalizeVideoEnhancement(value: unknown): PlayerVideoEnhancement {
  return value === "balanced" || value === "clear" ? value : "off";
}

export function readStoredVideoEnhancement(): PlayerVideoEnhancement {
  try {
    return normalizeVideoEnhancement(window.localStorage.getItem(VIDEO_ENHANCEMENT_STORAGE_KEY));
  } catch (error) {
    console.warn("[player] 画质增强预设读取失败", { error });
    return "off";
  }
}

export function storeVideoEnhancement(value: PlayerVideoEnhancement): void {
  try {
    window.localStorage.setItem(VIDEO_ENHANCEMENT_STORAGE_KEY, value);
  } catch (error) {
    console.warn("[player] 画质增强预设保存失败", { value, error });
  }
}
