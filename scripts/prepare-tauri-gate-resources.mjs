#!/usr/bin/env node
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await main(process.argv.slice(2));
}

/** 创建无 bundle 编译门禁需要的平台资源目录。 */
async function main(args) {
  const options = parseArgs(args);
  const directories = compileOnlyResourceDirectories(options.platform, options.arch);
  for (const relativePath of directories) {
    const directory = resolve(relativePath);
    await mkdir(directory, { recursive: true });
    console.log(`[tauri-gate] 编译期资源目录已准备：${directory}`);
  }
}

/** 返回无 bundle Gate 编译仍需存在的平台资源目录。 */
export function compileOnlyResourceDirectories(platform, arch) {
  const target = `${platform}-${arch}`;
  if (platform === "win32") {
    return [
      `out/ffmpeg/${target}`,
      `out/libmpv/${target}`,
      `out/model-sidecar/${target}`,
      `out/realesrgan-model-sidecar/${target}`,
      `out/qbittorrent/${target}`,
      `out/torrent-core/${target}`
    ];
  }
  if (platform === "darwin") {
    return [
      "out/ffmpeg",
      "out/model-sidecar",
      "out/realesrgan-model-sidecar",
      "out/qbittorrent",
      "out/torrent-core"
    ];
  }
  if (platform === "linux") {
    return [
      `out/model-sidecar/${target}`,
      `out/realesrgan-model-sidecar/${target}`,
      `out/qbittorrent/${target}`,
      `out/torrent-core/${target}`
    ];
  }
  throw new Error(`[tauri-gate] unsupported target: ${target}`);
}

/** 解析 Gate 平台与架构参数。 */
function parseArgs(args) {
  const parsed = { platform: undefined, arch: undefined };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (["--platform", "--arch"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--platform") parsed.platform = value;
      if (arg === "--arch") parsed.arch = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  if (!parsed.platform || !parsed.arch) throw new Error("--platform and --arch are required");
  return parsed;
}
