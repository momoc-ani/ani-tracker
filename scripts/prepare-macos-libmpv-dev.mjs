#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { cp, mkdir, readdir, rm, stat } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import process from "node:process";

if (process.platform !== "darwin") {
  throw new Error(`[libmpv] macOS preparation requires darwin, received: ${process.platform}`);
}

const options = parseArgs(process.argv.slice(2));
const targetKey = `darwin-${options.arch}`;
const sourceDirectory = resolve(options.source);
const targetDirectory = resolve(options.targetRoot, targetKey);

if (!(await isDirectory(sourceDirectory))) {
  console.warn(`[libmpv] 可选 IINA Frameworks 不存在，保留 libVLC 回退：${sourceDirectory}`);
  process.exit(0);
}

await rm(targetDirectory, { recursive: true, force: true });
await mkdir(targetDirectory, { recursive: true });
for (const name of await readdir(sourceDirectory)) {
  if (!name.endsWith(".dylib")) continue;
  await cp(join(sourceDirectory, name), join(targetDirectory, name));
}

const files = (await readdir(targetDirectory))
  .filter((name) => name.endsWith(".dylib"))
  .map((name) => join(targetDirectory, name));
for (const file of files) addLoaderRpath(file);

const library = join(targetDirectory, "libmpv.2.dylib");
if (!(await isFile(library))) {
  throw new Error(`[libmpv] IINA libmpv core missing: ${library}`);
}
const smoke = spawnSync(
  "ruby",
  ["-rfiddle", "-e", "Fiddle.dlopen(ARGV.fetch(0)); puts '[libmpv] dynamic load passed'", library],
  { cwd: process.cwd(), env: process.env, encoding: "utf8" }
);
if (smoke.stdout) process.stdout.write(smoke.stdout);
if (smoke.stderr) process.stderr.write(smoke.stderr);
if (smoke.error) throw smoke.error;
if (smoke.status !== 0) throw new Error(`[libmpv] dynamic load failed: ${library}`);

console.log(`[libmpv] macOS 可选运行时已准备：${targetDirectory} (${files.length} dylibs)`);

function addLoaderRpath(file) {
  const current = spawnSync("otool", ["-l", file], { encoding: "utf8" });
  if (current.error) throw current.error;
  if (current.status !== 0) throw new Error(`[libmpv] otool failed: ${basename(file)}`);
  if (/path @loader_path \(offset/.test(current.stdout ?? "")) return;
  const result = spawnSync("install_name_tool", ["-add_rpath", "@loader_path", file], {
    encoding: "utf8"
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[libmpv] install_name_tool failed: ${basename(file)}\n${result.stderr ?? ""}`);
  }
}

function parseArgs(args) {
  const parsed = {
    arch: process.arch === "x64" ? "x64" : "arm64",
    source: "/Applications/IINA.app/Contents/Frameworks",
    targetRoot: "out/libmpv"
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (["--arch", "--source", "--target"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--source") parsed.source = value;
      if (arg === "--target") parsed.targetRoot = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  if (!["x64", "arm64"].includes(parsed.arch)) {
    throw new Error(`[libmpv] unsupported macOS architecture: ${parsed.arch}`);
  }
  return parsed;
}

async function isDirectory(path) {
  try {
    return (await stat(path)).isDirectory();
  } catch {
    return false;
  }
}

async function isFile(path) {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}
