#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import process from "node:process";

const platformScript = {
  win32: "prepare:tauri:win-libvlc",
  darwin: "prepare:tauri:mac-libvlc",
  linux: "prepare:tauri:linux-libvlc"
}[process.platform];

if (!platformScript) {
  throw new Error(`[tauri-runtime] 不支持的桌面平台：${process.platform}`);
}

if (process.platform === "linux") {
  // Linux 先准备系统与原生运行依赖，避免 Renderer 构建完成后才暴露环境问题。
  runPnpm(platformScript);
  runPnpm("prepare:desktop-torrent-core-dev");
  prepareLinuxOptionalRuntimeDirectories();
  runPnpm("build:tauri:remote-renderer");
} else {
  runPnpm("build:tauri:remote-renderer");
  runPnpm(platformScript);
  if (process.platform === "win32") runPnpm("prepare:tauri:win-libmpv");
}
console.log(`[tauri-runtime] 桌面运行资源已准备：${process.platform}-${process.arch}`);

/** 为 Linux 开发构建创建可缺省托管 qBittorrent 的资源边界。 */
function prepareLinuxOptionalRuntimeDirectories() {
  for (const relativePath of ["out/qbittorrent/linux-x64"]) {
    const directory = resolve(relativePath);
    mkdirSync(directory, { recursive: true });
    console.log(`[tauri-runtime] Linux 可选运行资源目录已准备：${directory}`);
  }
}

/** 执行项目脚本并透传日志，失败时返回稳定错误。 */
function runPnpm(script) {
  const isWindows = process.platform === "win32";
  const command = isWindows ? process.env.ComSpec ?? "cmd.exe" : "pnpm";
  const args = isWindows
    ? ["/d", "/s", "/c", `"pnpm.cmd run ${script}"`]
    : ["run", script];
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit",
    windowsVerbatimArguments: isWindows
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[tauri-runtime] 脚本执行失败：${script}`);
  }
}
