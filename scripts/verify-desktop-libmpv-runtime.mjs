#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readdir, readFile, stat } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}

/** 校验当前桌面目标最终使用的 libmpv 运行时。 */
async function main(args) {
  const options = parseArgs(args);
  if (options.platform === "linux") {
    verifyLinuxSystemRuntime();
    return;
  }

  const directory = runtimeDirectory(options.root, options.platform, options.arch);
  const files = await listFiles(directory);
  const core = findMpvCore(files, options.platform);
  if (!core) throw new Error(`[libmpv] runtime core missing: ${directory}`);
  const source = JSON.parse(await readFile(join(directory, "SOURCE.json"), "utf8"));
  const target = `${options.platform}-${options.arch}`;
  if (source.target !== target) {
    throw new Error(`[libmpv] SOURCE target mismatch: expected ${target}, received ${source.target}`);
  }
  if (options.requirePinned && (!source.version || !source.archive || !source.sha256)) {
    throw new Error(`[libmpv] pinned SOURCE metadata required: ${directory}`);
  }

  if (options.platform === "darwin") verifyMacRuntime(directory, files, core, options.arch);
  console.log(`[libmpv] 桌面运行时校验通过：${directory}`);
}

export function runtimeDirectory(root, platform, arch) {
  return resolve(root, `${platform}-${arch}`);
}

export function findMpvCore(files, platform) {
  const expression = platform === "win32"
    ? /^(?:lib)?mpv(?:-\d+)?\.dll$/i
    : /^libmpv(?:\.\d+)?\.dylib$/i;
  return files.find((file) => expression.test(runtimeName(file)));
}

function runtimeName(file) {
  return file.split(/[\\/]/).at(-1);
}

export function parseArgs(args) {
  const parsed = {
    platform: process.platform,
    arch: process.arch === "x64" ? "x64" : "arm64",
    root: "out/libmpv",
    requirePinned: false
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (arg === "--require-pinned") {
      parsed.requirePinned = true;
      continue;
    }
    if (["--platform", "--arch", "--root"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--platform") parsed.platform = value;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--root") parsed.root = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  if (!["win32", "darwin", "linux"].includes(parsed.platform)) {
    throw new Error(`[libmpv] unsupported platform: ${parsed.platform}`);
  }
  return parsed;
}

function verifyMacRuntime(directory, files, core, arch) {
  const expectedArchitecture = arch === "x64" ? "x86_64" : "arm64";
  const names = new Set(files.map((file) => basename(file)));
  for (const file of files.filter((file) => file.endsWith(".dylib"))) {
    const architectures = capture("lipo", ["-archs", file]).match(/(?:^|\s)(x86_64|arm64)(?=\s|$)/g)
      ?.map((value) => value.trim()) ?? [];
    if (architectures.length !== 1 || architectures[0] !== expectedArchitecture) {
      throw new Error(`[libmpv] unexpected architecture: ${basename(file)} -> ${architectures.join(" ")}`);
    }
    for (const line of capture("otool", ["-L", file]).split(/\r?\n/)) {
      const dependency = line.trim().split(/\s+/)[0];
      if (!dependency?.endsWith(".dylib")) continue;
      if (!dependency.startsWith("@rpath/") && !dependency.startsWith("@loader_path/")) continue;
      const dependencyName = basename(dependency);
      if (!names.has(dependencyName)) {
        throw new Error(`[libmpv] unresolved dependency: ${basename(file)} -> ${dependencyName}`);
      }
    }
  }
  capture("ruby", ["-rfiddle", "-e", "Fiddle.dlopen(ARGV.fetch(0))", core]);
  console.log(`[libmpv] macOS 动态加载与依赖闭合通过：${directory}`);
}

function verifyLinuxSystemRuntime() {
  const result = capture("ldconfig", ["-p"]);
  if (!/libmpv\.so\.(?:1|2)\b/.test(result)) {
    throw new Error("[libmpv] Linux system libmpv is unavailable");
  }
  console.log("[libmpv] Linux 系统运行时校验通过");
}

async function listFiles(directory) {
  if (!(await isDirectory(directory))) return [];
  return (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile())
    .map((entry) => join(directory, entry.name));
}

function capture(command, args) {
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    env: process.env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"]
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[libmpv] command failed (${result.status ?? "unknown"}): ${command}\n${result.stderr ?? ""}`);
  }
  return result.stdout ?? "";
}

async function isDirectory(path) {
  try {
    return (await stat(path)).isDirectory();
  } catch {
    return false;
  }
}
