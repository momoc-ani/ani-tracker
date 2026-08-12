import type { ReleaseSourceConfig } from "./domain";

/** 用户新建下载源且未填写间隔时使用的默认采集间隔。 */
export const DEFAULT_SOURCE_REQUEST_INTERVAL_MS = 600;
/** 普通下载源允许配置的最小采集间隔。 */
export const MIN_SOURCE_REQUEST_INTERVAL_MS = 250;
/** 所有下载源允许配置的最大采集间隔。 */
export const MAX_SOURCE_REQUEST_INTERVAL_MS = 60_000;
/** AniBT 固定执行的最小采集间隔。 */
export const ANIBT_MIN_REQUEST_INTERVAL_MS = 500;

type SourceRequestTarget = Pick<ReleaseSourceConfig, "id" | "name" | "baseUrl" | "rssUrl">;
type SourceProxyTarget = SourceRequestTarget & Pick<ReleaseSourceConfig, "useProxy">;

/** 判断下载源或实际请求地址是否指向 AniBT。 */
export function isAniBtRequestTarget(source: SourceRequestTarget, requestUrl?: string | URL): boolean {
  const identity = `${source.id} ${source.name}`.toLowerCase();
  if (identity.includes("anibt")) {
    return true;
  }

  return [source.baseUrl, source.rssUrl, requestUrl?.toString()].some((value) => isAniBtUrl(value));
}

/** 判断下载源请求是否允许使用元数据代理；AniBT 始终固定直连。 */
export function shouldUseSourceProxy(source: SourceProxyTarget, requestUrl?: string | URL): boolean {
  return !isAniBtRequestTarget(source, requestUrl) && source.useProxy !== false;
}

/** 返回下载源允许配置的最小请求间隔。 */
export function getSourceMinimumRequestIntervalMs(
  source: SourceRequestTarget,
  requestUrl?: string | URL
): number {
  return isAniBtRequestTarget(source, requestUrl)
    ? ANIBT_MIN_REQUEST_INTERVAL_MS
    : MIN_SOURCE_REQUEST_INTERVAL_MS;
}

/** 判断 URL 是否使用 AniBT 主域或其子域。 */
function isAniBtUrl(value?: string): boolean {
  if (!value) {
    return false;
  }

  try {
    const hostname = new URL(value).hostname.toLowerCase();
    return hostname === "anibt.net" || hostname.endsWith(".anibt.net");
  } catch {
    return value.toLowerCase().includes("anibt.net");
  }
}
