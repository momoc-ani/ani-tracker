#!/usr/bin/env node
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { cp, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

export const WINDOWS_LIBMPV = {
  version: "2026-08-12-f4d13e1c2c",
  archive: "mpv-dev-lgpl-x86_64-20260812-git-f4d13e1c2c.7z",
  sha256: "20dffed429610b52dbb9e3d5b4124145b2a954ef3e6e8fe319cc249a5a794c51",
  baseUrl: "https://github.com/zhongfly/mpv-winbuild/releases/download/2026-08-12-f4d13e1c2c"
};

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}

async function main() {
  if (process.platform !== "win32") {
    throw new Error(`[libmpv] Windows preparation requires win32, received: ${process.platform}`);
  }

  const options = parseArgs(process.argv.slice(2));
  if (options.arch !== "x64") {
    throw new Error(`[libmpv] unsupported Windows architecture: ${options.arch}`);
  }
  const targetKey = `win32-${options.arch}`;
  const cacheDirectory = resolve(options.cacheRoot, WINDOWS_LIBMPV.version);
  const archivePath = join(cacheDirectory, WINDOWS_LIBMPV.archive);
  const targetDirectory = resolve(options.targetRoot, targetKey);
  const extractDirectory = resolve(options.cacheRoot, WINDOWS_LIBMPV.version, "extracted-x64");

  await mkdir(cacheDirectory, { recursive: true });
  await ensureArchive(archivePath, options.offline);
  await rm(extractDirectory, { recursive: true, force: true });
  await mkdir(extractDirectory, { recursive: true });
  run7Zip(archivePath, extractDirectory);

  const runtimeFiles = await findRuntimeFiles(extractDirectory);
  if (runtimeFiles.length === 0) {
    throw new Error(`[libmpv] archive does not contain a libmpv DLL: ${archivePath}`);
  }
  await rm(targetDirectory, { recursive: true, force: true });
  await mkdir(targetDirectory, { recursive: true });
  for (const file of runtimeFiles) {
    await cp(file, join(targetDirectory, runtimeFileName(file)));
  }
  await writeFile(
    join(targetDirectory, "SOURCE.json"),
    `${JSON.stringify(createSourceManifest(targetKey, runtimeFiles), null, 2)}\n`,
    "utf8"
  );

  console.log(`[libmpv] Windows 运行时已准备：${targetDirectory}`);
}

export function createSourceManifest(targetKey, runtimeFiles) {
  return {
    source: "zhongfly/mpv-winbuild",
    version: WINDOWS_LIBMPV.version,
    archive: WINDOWS_LIBMPV.archive,
    sha256: WINDOWS_LIBMPV.sha256,
    target: targetKey,
    files: runtimeFiles.map((file) => runtimeFileName(file))
  };
}

function runtimeFileName(file) {
  return file.split(/[\\/]/).at(-1);
}

async function ensureArchive(path, offline) {
  if (await isFile(path)) {
    await verifySha256(path);
    return;
  }
  if (offline) throw new Error(`[libmpv] offline archive missing: ${path}`);
  const response = await fetch(`${WINDOWS_LIBMPV.baseUrl}/${WINDOWS_LIBMPV.archive}`);
  if (!response.ok) throw new Error(`[libmpv] download failed: HTTP ${response.status}`);
  await writeFile(path, Buffer.from(await response.arrayBuffer()));
  await verifySha256(path);
}

async function verifySha256(path) {
  const digest = createHash("sha256").update(await readFile(path)).digest("hex");
  if (digest !== WINDOWS_LIBMPV.sha256) {
    throw new Error(`[libmpv] SHA-256 mismatch: ${path}`);
  }
}

function run7Zip(archive, destination) {
  const candidates = ["7z.exe", "7z", "7zz.exe", "7zz"];
  for (const command of candidates) {
    const result = spawnSync(command, ["x", archive, `-o${destination}`, "-y"], {
      cwd: process.cwd(),
      env: process.env,
      encoding: "utf8"
    });
    if (result.error?.code === "ENOENT") continue;
    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`[libmpv] 7-Zip extraction failed: ${archive}`);
    return;
  }
  throw new Error("[libmpv] 7-Zip executable is required on Windows");
}

export async function findRuntimeFiles(root) {
  const files = await listFiles(root);
  const core = files.filter((path) => /(?:^|[\\/])(?:lib)?mpv(?:-\d+)?\.dll$/i.test(path));
  const directory = core[0] ? resolve(core[0], "..") : undefined;
  if (!directory) return [];
  return (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".dll"))
    .map((entry) => join(directory, entry.name))
    .sort((left, right) => runtimeFileName(left).localeCompare(runtimeFileName(right), "en"));
}

async function listFiles(root) {
  const entries = await readdir(root, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(root, entry.name);
    return entry.isDirectory() ? listFiles(path) : [path];
  }));
  return nested.flat();
}

export function parseArgs(args) {
  const parsed = {
    arch: process.arch === "x64" ? "x64" : process.arch,
    cacheRoot: ".cache/libmpv",
    targetRoot: "out/libmpv",
    offline: false
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (arg === "--offline") {
      parsed.offline = true;
      continue;
    }
    if (["--arch", "--cache", "--target"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--cache") parsed.cacheRoot = value;
      if (arg === "--target") parsed.targetRoot = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  return parsed;
}

async function isFile(path) {
  try {
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}
