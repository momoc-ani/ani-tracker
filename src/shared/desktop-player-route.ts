import type { DesktopPlayerWindowInput } from "./contracts";

export const DESKTOP_PLAYER_VIEW = "desktop-player";

/** 校验并规范化桌面播放器窗口参数。 */
export function normalizeDesktopPlayerWindowInput(input: DesktopPlayerWindowInput): DesktopPlayerWindowInput {
  if (!input || !/^[a-zA-Z0-9._:-]{1,160}$/.test(input.taskId)) {
    throw new Error("下载任务标识无效");
  }
  if (input.fileIndex !== undefined && (
    !Number.isSafeInteger(input.fileIndex) || input.fileIndex < 0
  )) {
    throw new Error("媒体文件标识无效");
  }
  return {
    taskId: input.taskId,
    ...(input.fileIndex === undefined ? {} : { fileIndex: input.fileIndex })
  };
}

/** 构建独立播放器窗口使用的查询参数。 */
export function createDesktopPlayerSearchParams(input: DesktopPlayerWindowInput): URLSearchParams {
  const normalized = normalizeDesktopPlayerWindowInput(input);
  const params = new URLSearchParams({
    aniView: DESKTOP_PLAYER_VIEW,
    taskId: normalized.taskId
  });
  if (normalized.fileIndex !== undefined) {
    params.set("fileIndex", String(normalized.fileIndex));
  }
  return params;
}

/** 从当前地址解析独立播放器目标，无效地址不进入播放器页面。 */
export function resolveDesktopPlayerWindowInput(search: string): DesktopPlayerWindowInput | null {
  const params = new URLSearchParams(search);
  if (params.get("aniView") !== DESKTOP_PLAYER_VIEW) {
    return null;
  }
  const fileValue = params.get("fileIndex");
  if (fileValue !== null && !/^\d+$/.test(fileValue)) {
    return null;
  }
  const input: DesktopPlayerWindowInput = {
    taskId: params.get("taskId") ?? "",
    ...(fileValue === null || fileValue === "" ? {} : { fileIndex: Number(fileValue) })
  };
  try {
    return normalizeDesktopPlayerWindowInput(input);
  } catch {
    return null;
  }
}
