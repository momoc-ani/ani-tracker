import type {
  ImageCacheResolveResult,
  RemoteDirectEnhancementReport,
  RemotePlaybackEnhancement,
  RemotePlaybackRequestMode,
  RemotePlaybackSession
} from "@shared/contracts";
import type { AppClient } from "@shared/app-client";
import { createRemoteClient } from "@/lib/clients/remote-client";

interface RemoteRpcResponse {
  result?: unknown;
  error?: string;
  code?: string;
}

const REMOTE_TOKEN_STORAGE_KEY = "ani.remoteAccessToken";
export const REMOTE_AUTH_CHANGED_EVENT = "ani:remote-auth-changed";
const imageResolveRequests = new Map<string, Promise<string>>();

export interface RemotePairingState {
  needsPairing: boolean;
  remoteUrl?: string;
}

/** 远程 PWA 不具备本地宿主能力。 */
export function isLocalClient(): boolean {
  return false;
}

/** 远程 PWA 不运行在 Tauri WebView 中。 */
export function isTauriClient(): boolean {
  return false;
}

/** 远程 PWA 不冒充 Android 本地应用。 */
export function isAndroidClient(): boolean {
  return false;
}

/** 返回 PWA 使用的同源远程地址。 */
export function getRemoteBaseUrl(): string {
  return window.location.origin.replace(/\/+$/, "");
}

/** 返回远程客户端是否已保存设备令牌。 */
export function getRemotePairingState(): RemotePairingState {
  const remoteUrl = getRemoteBaseUrl();
  return {
    needsPairing: !window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY),
    remoteUrl
  };
}

/** 使用桌面端一次性配对码换取设备令牌。 */
export async function pairRemoteDevice(code: string, deviceName: string): Promise<void> {
  const baseUrl = getRemoteBaseUrl();
  const response = await fetch(`${baseUrl}/api/pair`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, deviceName })
  });
  const payload = (await response.json().catch(() => ({}))) as { token?: string; error?: string };
  if (!response.ok || !payload.token) {
    throw new Error(payload.error ?? `配对失败：${response.status}`);
  }
  window.localStorage.setItem(REMOTE_TOKEN_STORAGE_KEY, payload.token);
  window.dispatchEvent(new Event(REMOTE_AUTH_CHANGED_EVENT));
}

/** 清除本机设备令牌，用于重新配对。 */
export function clearRemoteDeviceToken(): void {
  window.localStorage.removeItem(REMOTE_TOKEN_STORAGE_KEY);
  imageResolveRequests.clear();
  window.dispatchEvent(new Event(REMOTE_AUTH_CHANGED_EVENT));
}

/** 将公网图片地址解析为远程同源缓存地址。 */
export function resolveCachedImageUrl(sourceUrl: string): Promise<string> {
  const normalizedSourceUrl = sourceUrl.trim();
  if (!normalizedSourceUrl) {
    return Promise.reject(new Error("图片地址不能为空"));
  }
  const existing = imageResolveRequests.get(normalizedSourceUrl);
  if (existing) return existing;

  const request = resolveCachedImageUrlOnce(normalizedSourceUrl).catch((error) => {
    if (imageResolveRequests.get(normalizedSourceUrl) === request) {
      imageResolveRequests.delete(normalizedSourceUrl);
    }
    throw error;
  });
  imageResolveRequests.set(normalizedSourceUrl, request);
  return request;
}

/** 远程页面清理已解析地址，下一次加载重新请求短期签名 URL。 */
export async function invalidateCachedImageUrl(sourceUrl: string): Promise<void> {
  imageResolveRequests.delete(sourceUrl.trim());
}

/** 请求一次远程签名图片缓存地址。 */
async function resolveCachedImageUrlOnce(sourceUrl: string): Promise<string> {
  const baseUrl = getRemoteBaseUrl();
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  if (!accessToken) throw new Error("当前设备尚未完成远程配对");
  const response = await fetch(`${baseUrl}/api/images/resolve`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${accessToken}`
    },
    body: JSON.stringify({ url: sourceUrl })
  });
  const payload = (await response.json().catch(() => ({}))) as ImageCacheResolveResult & { error?: string };
  if (!response.ok || !payload.url) {
    if (response.status === 401) clearRemoteDeviceToken();
    throw new Error(payload.error ?? `图片地址解析失败：${response.status}`);
  }
  return new URL(payload.url, `${baseUrl}/`).toString();
}

/** 为当前远程设备创建绑定下载任务的播放会话。 */
export async function createRemotePlaybackSession(
  taskId: string,
  mode: RemotePlaybackRequestMode,
  fileIndex: number | undefined,
  enhancement: RemotePlaybackEnhancement,
  startPositionSeconds?: number
): Promise<RemotePlaybackSession> {
  return createRemoteMediaSession(
    "/api/media/sessions",
    taskId,
    mode,
    fileIndex,
    enhancement,
    startPositionSeconds
  );
}

/** 为 PotPlayer 或 IINA 创建无需 Cookie 的短期拉流会话。 */
export async function createRemoteExternalPlaybackSession(
  taskId: string,
  mode: RemotePlaybackRequestMode,
  fileIndex: number | undefined,
  enhancement: RemotePlaybackEnhancement,
  startPositionSeconds?: number
): Promise<RemotePlaybackSession> {
  return createRemoteMediaSession(
    "/api/media/external-sessions",
    taskId,
    mode,
    fileIndex,
    enhancement,
    startPositionSeconds
  );
}

/** 调用指定媒体入口创建远程播放会话。 */
async function createRemoteMediaSession(
  endpoint: "/api/media/sessions" | "/api/media/external-sessions",
  taskId: string,
  mode: RemotePlaybackRequestMode,
  fileIndex: number | undefined,
  enhancement: RemotePlaybackEnhancement,
  startPositionSeconds?: number
): Promise<RemotePlaybackSession> {
  const baseUrl = getRemoteBaseUrl();
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  if (!accessToken) throw new Error("当前设备尚未完成远程配对");
  const response = await fetch(`${baseUrl}${endpoint}`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${accessToken}`
    },
    body: JSON.stringify({
      taskId,
      mode,
      enhancement,
      ...(fileIndex === undefined ? {} : { fileIndex }),
      ...(startPositionSeconds === undefined ? {} : { startPositionSeconds })
    })
  });
  const payload = (await response.json().catch(() => ({}))) as RemotePlaybackSession & { error?: string };
  if (!response.ok || !payload.id) {
    if (response.status === 401) clearRemoteDeviceToken();
    throw new Error(payload.error ?? `播放会话创建失败：${response.status}`);
  }
  return payload;
}

/** 关闭远程播放会话并通知桌面端回收转码资源。 */
export async function closeRemotePlaybackSession(sessionId: string): Promise<void> {
  const baseUrl = getRemoteBaseUrl();
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  if (!accessToken) return;
  const response = await fetch(`${baseUrl}/api/media/sessions/${encodeURIComponent(sessionId)}`, {
    method: "DELETE",
    credentials: "same-origin",
    headers: { Authorization: `Bearer ${accessToken}` }
  });
  if (!response.ok && response.status !== 404) {
    console.warn("[remote] 播放会话关闭失败", { sessionId, status: response.status });
  }
}

/** 读取远程播放会话的最新实际编码和模型降级状态。 */
export async function getRemotePlaybackSession(sessionId: string): Promise<RemotePlaybackSession> {
  const baseUrl = getRemoteBaseUrl();
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  if (!accessToken) throw new Error("当前设备尚未完成远程配对");
  const response = await fetch(`${baseUrl}/api/media/sessions/${encodeURIComponent(sessionId)}`, {
    credentials: "same-origin",
    headers: { Authorization: `Bearer ${accessToken}` }
  });
  const payload = (await response.json().catch(() => ({}))) as RemotePlaybackSession & { error?: string };
  if (!response.ok || !payload.id) {
    if (response.status === 401) clearRemoteDeviceToken();
    throw new Error(payload.error ?? `播放会话状态读取失败：${response.status}`);
  }
  return payload;
}

/** 将终端 WebCodecs/WebGPU 实际运行状态写回设备绑定的播放会话。 */
export async function reportRemoteDirectEnhancementDiagnostics(
  sessionId: string,
  diagnostics: RemoteDirectEnhancementReport
): Promise<RemotePlaybackSession> {
  const baseUrl = getRemoteBaseUrl();
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  if (!accessToken) throw new Error("当前设备尚未完成远程配对");
  const response = await fetch(
    `${baseUrl}/api/media/sessions/${encodeURIComponent(sessionId)}/diagnostics`,
    {
      method: "PUT",
      credentials: "same-origin",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${accessToken}`
      },
      body: JSON.stringify(diagnostics)
    }
  );
  const payload = (await response.json().catch(() => ({}))) as RemotePlaybackSession & { error?: string };
  if (!response.ok || !payload.id) {
    if (response.status === 401) clearRemoteDeviceToken();
    throw new Error(payload.error ?? `终端增强诊断上报失败：${response.status}`);
  }
  return payload;
}

/** 调用桌面端暴露的远程 RPC，并统一处理协议错误。 */
async function invokeRemote(baseUrl: string, method: string, args: unknown[]): Promise<unknown> {
  const accessToken = window.localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY);
  const response = await fetch(`${baseUrl}/api/rpc`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {})
    },
    body: JSON.stringify({ method, args })
  });
  const payload = (await response.json().catch(() => ({}))) as RemoteRpcResponse;
  if (!response.ok || payload.error) {
    if (response.status === 401) clearRemoteDeviceToken();
    throw new Error(payload.error ?? `远程请求失败：${response.status}`);
  }
  return payload.result;
}

/** 创建仅允许远程 HTTP RPC 的客户端。 */
function createAppClient(): AppClient {
  const remoteUrl = getRemoteBaseUrl();
  console.info("[renderer] 使用远程 HTTP 客户端", { remoteUrl });
  return createRemoteClient((method, args) => invokeRemote(remoteUrl, method, args));
}

export const appApi: AppClient = createAppClient();
