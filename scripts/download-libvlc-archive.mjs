#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { access, mkdir, rename, rm } from "node:fs/promises";
import { get as httpGet } from "node:http";
import { get as httpsGet } from "node:https";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { pipeline } from "node:stream/promises";
import { pathToFileURL } from "node:url";
import { HttpsProxyAgent } from "https-proxy-agent";
import {
  DESKTOP_LIBVLC_SOURCE,
  DESKTOP_LIBVLC_VERSION,
  findDesktopLibVlcAsset
} from "./libvlc-resource-manifest.mjs";

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await main(process.argv.slice(2));
}

/** 下载并校验命令行指定平台的官方 VLC 归档。 */
async function main(args) {
  const options = parseArgs(args);
  const asset = options.sourceCode
    ? DESKTOP_LIBVLC_SOURCE
    : findDesktopLibVlcAsset(options.platform, options.arch);
  if (!asset?.archiveName || !asset.url || !asset.archiveSha256) {
    const target = options.sourceCode ? "source code" : `${options.platform}-${options.arch}`;
    throw new Error(`[libvlc] no downloadable archive for ${target}`);
  }

  const destination = options.output
    ?? resolve(".cache", "libvlc", DESKTOP_LIBVLC_VERSION, asset.archiveName);
  const proxyUrl = options.proxyUrl || resolveProxyUrl(process.env);
  const downloadAgent = proxyUrl ? new HttpsProxyAgent(proxyUrl) : undefined;
  if (proxyUrl) {
    console.log(`[libvlc] using proxy: ${formatProxyUrl(proxyUrl)}`);
  }

  if (!(await hasExpectedHash(destination, asset.archiveSha256))) {
    if (options.offline) {
      throw new Error(`[libvlc] offline archive missing or invalid: ${destination}`);
    }
    await mkdir(dirname(destination), { recursive: true });
    await downloadVerified(asset.url, destination, asset.archiveSha256, options, downloadAgent);
  } else {
    console.log(`[libvlc] cache hit: ${destination}`);
  }

  console.log(`[libvlc] archive ready: ${destination}`);
}

/** 下载固定摘要的官方归档，并在失败时清理临时文件。 */
async function downloadVerified(url, outputPath, expectedSha256, currentOptions, downloadAgent) {
  let lastError;
  for (let attempt = 1; attempt <= currentOptions.retries; attempt += 1) {
    const temporaryPath = `${outputPath}.part-${process.pid}`;
    try {
      await downloadFile(url, temporaryPath, currentOptions.timeoutMs, 5, downloadAgent);
      const actualSha256 = await sha256(temporaryPath);
      if (actualSha256 !== expectedSha256) {
        throw new Error(`SHA-256 mismatch: expected ${expectedSha256}, received ${actualSha256}`);
      }
      await rm(outputPath, { force: true });
      await rename(temporaryPath, outputPath);
      console.log(`[libvlc] downloaded: ${url}`);
      return;
    } catch (error) {
      lastError = error;
      await rm(temporaryPath, { force: true });
      console.warn(`[libvlc] download attempt ${attempt}/${currentOptions.retries} failed: ${errorMessage(error)}`);
    }
  }
  throw new Error(`[libvlc] failed to download ${url}: ${errorMessage(lastError)}`);
}

/** 使用系统代理和空闲超时读取 HTTP(S) 资源。固定摘要校验保护重定向后的产物。 */
export async function downloadFile(url, destination, timeoutMs, redirectsRemaining = 5, agent) {
  await new Promise((resolveDownload, rejectDownload) => {
    const requestUrl = new URL(url);
    const requestGet = requestClient(requestUrl);
    const request = requestGet(requestUrl, {
      agent,
      headers: { "User-Agent": "ani-tracker-build" }
    }, async (response) => {
      try {
        const location = response.headers.location;
        if (response.statusCode && response.statusCode >= 300 && response.statusCode < 400 && location) {
          response.resume();
          if (redirectsRemaining <= 0) throw new Error("Too many redirects");
          await downloadFile(
            new URL(location, requestUrl).href,
            destination,
            timeoutMs,
            redirectsRemaining - 1,
            agent
          );
          resolveDownload();
          return;
        }
        if (response.statusCode !== 200) {
          response.resume();
          throw new Error(`HTTP ${response.statusCode ?? "unknown"}`);
        }
        await pipeline(response, createWriteStream(destination));
        resolveDownload();
      } catch (error) {
        rejectDownload(error);
      }
    });
    request.setTimeout(timeoutMs, () => request.destroy(new Error(`Request timed out after ${timeoutMs}ms`)));
    request.once("error", rejectDownload);
  });
}

/** 根据 URL 协议选择 Node 下载客户端，并拒绝非 HTTP(S) 重定向。 */
function requestClient(url) {
  if (url.protocol === "https:") return httpsGet;
  if (url.protocol === "http:") return httpGet;
  throw new Error(`Unsupported download protocol: ${url.protocol}`);
}

/** 判断归档是否已存在且摘要匹配。 */
async function hasExpectedHash(path, expectedSha256) {
  try {
    await access(path);
    return await sha256(path) === expectedSha256;
  } catch {
    return false;
  }
}

/** 以流式方式计算文件 SHA-256。 */
async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

/** 解析下载目标、网络超时和离线模式。 */
function parseArgs(args) {
  const parsed = {
    platform: process.env.npm_config_platform || process.platform,
    arch: process.env.npm_config_arch || process.arch,
    output: undefined,
    timeoutMs: 120_000,
    retries: 3,
    offline: false,
    sourceCode: false,
    proxyUrl: undefined
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (arg === "--offline") {
      parsed.offline = true;
      continue;
    }
    if (arg === "--source-code") {
      parsed.sourceCode = true;
      continue;
    }
    if (["--platform", "--arch", "--output", "--proxy", "--timeout-ms", "--retries"].includes(arg)) {
      const value = readValue(args, index, arg);
      index += 1;
      if (arg === "--platform") parsed.platform = value;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--output") parsed.output = resolve(value);
      if (arg === "--proxy") parsed.proxyUrl = value;
      if (arg === "--timeout-ms") parsed.timeoutMs = positiveInteger(value, arg);
      if (arg === "--retries") parsed.retries = positiveInteger(value, arg);
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  return parsed;
}

/** 读取命令行参数后的必填值。 */
function readValue(args, index, arg) {
  const value = args[index + 1];
  if (!value) throw new Error(`${arg} requires a value`);
  return value;
}

/** 将参数转换为正整数。 */
function positiveInteger(value, arg) {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) throw new Error(`${arg} requires a positive integer`);
  return parsed;
}

/** 按 HTTPS、HTTP 优先级读取标准代理环境变量。 */
function resolveProxyUrl(environment) {
  return environment.HTTPS_PROXY
    || environment.https_proxy
    || environment.HTTP_PROXY
    || environment.http_proxy;
}

/** 隐藏代理凭据后输出代理节点。 */
function formatProxyUrl(value) {
  const parsed = new URL(value);
  return `${parsed.protocol}//${parsed.hostname}${parsed.port ? `:${parsed.port}` : ""}`;
}

/** 返回适合构建日志的错误说明。 */
function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
