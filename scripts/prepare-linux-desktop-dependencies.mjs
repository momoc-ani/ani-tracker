#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import process from "node:process";

const REQUIRED_PACKAGES = Object.freeze([
  "build-essential",
  "cmake",
  "curl",
  "ffmpeg",
  "file",
  "fonts-noto-cjk",
  "gnome-keyring",
  "libayatana-appindicator3-dev",
  "libboost-system-dev",
  "libmpv1",
  "librsvg2-dev",
  "libssl-dev",
  "libwebkit2gtk-4.1-dev",
  "libx11-dev",
  "libxdo-dev",
  "ninja-build",
  "patchelf",
  "pax-utils",
  "pkg-config",
  "wget"
]);

const options = parseArgs(process.argv.slice(2));
if (process.platform !== "linux") {
  throw new Error(`[linux-deps] Linux dependency preparation requires linux, received: ${process.platform}`);
}

ensureCommand("apt-get");
ensureCommand("dpkg-query");
ensureCommand("rustup");

const missingPackages = REQUIRED_PACKAGES.filter((packageName) => !isPackageInstalled(packageName));
if (missingPackages.length === 0) {
  console.log(`[linux-deps] Linux 桌面编译与打包依赖已满足，共 ${REQUIRED_PACKAGES.length} 项`);
  process.exit(0);
}

console.log(`[linux-deps] 检测到 ${missingPackages.length} 个缺失依赖：${missingPackages.join(", ")}`);
if (options.checkOnly) {
  console.error(`[linux-deps] 请执行：${formatInstallCommand(missingPackages)}`);
  process.exit(1);
}

const useSudo = typeof process.getuid === "function" && process.getuid() !== 0;
if (useSudo) ensureCommand("sudo");
const command = useSudo ? "sudo" : "apt-get";
const commandPrefix = useSudo ? ["apt-get"] : [];

console.log("[linux-deps] 正在刷新 APT 软件包索引");
runCommand(command, [...commandPrefix, "update"]);
console.log("[linux-deps] 正在安装 Linux 桌面编译与打包依赖");
runCommand(command, [...commandPrefix, "install", "-y", ...missingPackages]);

const unresolvedPackages = REQUIRED_PACKAGES.filter((packageName) => !isPackageInstalled(packageName));
if (unresolvedPackages.length > 0) {
  throw new Error(`[linux-deps] 安装后仍缺少依赖：${unresolvedPackages.join(", ")}`);
}
console.log(`[linux-deps] Linux 桌面编译与打包依赖准备完成，共 ${REQUIRED_PACKAGES.length} 项`);

/** 解析依赖检查参数。 */
function parseArgs(args) {
  const parsed = { checkOnly: false };
  for (const arg of args) {
    if (arg === "--") continue;
    if (arg === "--check") {
      parsed.checkOnly = true;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  return parsed;
}

/** 判断 Debian 软件包是否处于完整安装状态。 */
function isPackageInstalled(packageName) {
  const result = spawnSync("dpkg-query", ["-W", "-f=${Status}", packageName], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"]
  });
  return result.status === 0 && result.stdout?.trim() === "install ok installed";
}

/** 校验当前系统是否提供指定命令。 */
function ensureCommand(command) {
  const result = spawnSync(command, ["--version"], { stdio: "ignore" });
  if (result.error?.code === "ENOENT") {
    throw new Error(`[linux-deps] 缺少命令 ${command}；当前仅支持使用 APT 的 Debian/Ubuntu 系统`);
  }
  if (result.error) throw result.error;
}

/** 执行系统依赖命令并透传日志。 */
function runCommand(command, args) {
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit"
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[linux-deps] command failed (${result.status ?? "unknown"}): ${command}`);
  }
}

/** 生成可供人工执行的依赖安装命令。 */
function formatInstallCommand(packages) {
  const prefix = typeof process.getuid === "function" && process.getuid() === 0 ? "" : "sudo ";
  return `${prefix}apt-get update && ${prefix}apt-get install -y ${packages.join(" ")}`;
}
