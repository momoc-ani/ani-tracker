#!/usr/bin/env node
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import process from "node:process";
import { fileURLToPath } from "node:url";

export const MACOS_LIBMPV = Object.freeze({
  source: "iina/iina",
  version: "1.4.4",
  archive: "IINA.v1.4.4.dmg",
  sha256: "dd0fc0bd4b37fb57a1c8d30d6e3201b3a64bafd29959fe56953964613237beb1",
  url: "https://github.com/iina/iina/releases/download/v1.4.4/IINA.v1.4.4.dmg"
});

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}

/** 准备固定来源、按目标架构裁剪且可重定位的 macOS libmpv 运行时。 */
async function main(args) {
  if (process.platform !== "darwin") {
    throw new Error(`[libmpv] macOS preparation requires darwin, received: ${process.platform}`);
  }
  const options = parseArgs(args);
  const targetKey = `darwin-${options.arch}`;
  const targetDirectory = resolve(options.targetRoot, targetKey);
  const stagingDirectory = resolve(
    options.targetRoot,
    `.${targetKey}-${process.pid}-${Date.now()}`
  );
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "ani-libmpv-"));
  let mountedDirectory;

  try {
    const installedFrameworks = "/Applications/IINA.app/Contents/Frameworks";
    const localSource = options.source
      ? resolve(options.source)
      : !options.pinned && await isDirectory(installedFrameworks)
        ? installedFrameworks
        : undefined;
    const sourceDirectory = localSource
      ? await requireFrameworkDirectory(localSource)
      : await mountPinnedIina(options, temporaryDirectory);
    if (!localSource) mountedDirectory = join(temporaryDirectory, "mount");

    const sourceFiles = await collectRuntimeFiles(sourceDirectory);
    if (!sourceFiles.some((file) => basename(file) === "libmpv.2.dylib")) {
      throw new Error(`[libmpv] IINA libmpv core missing: ${sourceDirectory}`);
    }

    await rm(stagingDirectory, { recursive: true, force: true });
    await mkdir(stagingDirectory, { recursive: true });
    for (const sourceFile of sourceFiles) {
      const targetFile = join(stagingDirectory, basename(sourceFile));
      thinDylib(sourceFile, targetFile, options.arch);
    }

    const runtimeFiles = await listDylibs(stagingDirectory);
    for (const runtimeFile of runtimeFiles) addLoaderRpath(runtimeFile);
    validateRuntimeClosure(runtimeFiles, options.arch);
    await writeFile(
      join(stagingDirectory, "SOURCE.json"),
      `${JSON.stringify(createSourceManifest(targetKey, runtimeFiles, Boolean(localSource)), null, 2)}\n`,
      "utf8"
    );
    smokeLoad(join(stagingDirectory, "libmpv.2.dylib"));
    await rm(targetDirectory, { recursive: true, force: true });
    await rename(stagingDirectory, targetDirectory);
    console.log(`[libmpv] macOS 运行时已准备：${targetDirectory} (${runtimeFiles.length} dylibs)`);
  } finally {
    if (mountedDirectory && await isDirectory(mountedDirectory)) detachDmg(mountedDirectory);
    await rm(stagingDirectory, { recursive: true, force: true });
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
}

/** 返回写入发布资源的固定来源清单。 */
export function createSourceManifest(target, runtimeFiles, localOverride = false) {
  return {
    source: localOverride ? "local-framework-override" : MACOS_LIBMPV.source,
    version: localOverride ? null : MACOS_LIBMPV.version,
    archive: localOverride ? null : MACOS_LIBMPV.archive,
    sha256: localOverride ? null : MACOS_LIBMPV.sha256,
    target,
    files: runtimeFiles.map((file) => basename(file)).sort((left, right) => left.localeCompare(right, "en"))
  };
}

/** 解析 macOS libmpv 目标、缓存和离线参数。 */
export function parseArgs(args) {
  const parsed = {
    arch: process.arch === "x64" ? "x64" : "arm64",
    cacheRoot: ".cache/libmpv",
    targetRoot: "out/libmpv",
    source: undefined,
    pinned: false,
    offline: false
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (arg === "--offline") {
      parsed.offline = true;
      continue;
    }
    if (arg === "--pinned") {
      parsed.pinned = true;
      continue;
    }
    if (["--arch", "--cache", "--target", "--source"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--cache") parsed.cacheRoot = value;
      if (arg === "--target") parsed.targetRoot = value;
      if (arg === "--source") parsed.source = value;
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  if (!["x64", "arm64"].includes(parsed.arch)) {
    throw new Error(`[libmpv] unsupported macOS architecture: ${parsed.arch}`);
  }
  return parsed;
}

/** 提取 otool 输出中应由应用一同分发的相对 dylib 依赖名。 */
export function parseRpathDependencies(output) {
  return output
    .split(/\r?\n/)
    .map((line) => line.trim().split(/\s+/)[0])
    .filter((dependency) =>
      dependency?.endsWith(".dylib")
      && (dependency.startsWith("@rpath/") || dependency.startsWith("@loader_path/"))
    )
    .map((dependency) => basename(dependency));
}

/** 兼容 lipo 对 fat 与 non-fat Mach-O 的两种架构输出格式。 */
export function parseLipoArchitectures(output) {
  return [...output.matchAll(/(?:^|\s)(x86_64|arm64)(?=\s|$)/g)].map((match) => match[1]);
}

async function mountPinnedIina(options, temporaryDirectory) {
  const cacheDirectory = resolve(options.cacheRoot, `iina-${MACOS_LIBMPV.version}`);
  const archivePath = join(cacheDirectory, MACOS_LIBMPV.archive);
  await mkdir(cacheDirectory, { recursive: true });
  await ensureArchive(archivePath, options.offline);
  const mountDirectory = join(temporaryDirectory, "mount");
  await mkdir(mountDirectory, { recursive: true });
  runCommand("hdiutil", ["attach", "-nobrowse", "-readonly", "-mountpoint", mountDirectory, archivePath]);
  return requireFrameworkDirectory(join(mountDirectory, "IINA.app", "Contents", "Frameworks"));
}

async function ensureArchive(path, offline) {
  if (await isFile(path)) {
    await verifyArchive(path);
    console.log(`[libmpv] cache hit: ${path}`);
    return;
  }
  if (offline) throw new Error(`[libmpv] offline archive missing: ${path}`);
  runCommand("curl", [
    "--fail",
    "--location",
    "--retry", "3",
    "--retry-all-errors",
    "--output", path,
    MACOS_LIBMPV.url
  ]);
  await verifyArchive(path);
}

async function verifyArchive(path) {
  const digest = createHash("sha256").update(await readFile(path)).digest("hex");
  if (digest !== MACOS_LIBMPV.sha256) {
    await rm(path, { force: true });
    throw new Error(`[libmpv] SHA-256 mismatch: ${path}`);
  }
}

async function requireFrameworkDirectory(path) {
  if (!(await isDirectory(path))) {
    throw new Error(`[libmpv] IINA Frameworks directory missing: ${path}`);
  }
  return path;
}

async function listDylibs(directory) {
  return (await readdir(directory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && entry.name.endsWith(".dylib"))
    .map((entry) => join(directory, entry.name))
    .sort((left, right) => basename(left).localeCompare(basename(right), "en"));
}

async function collectRuntimeFiles(directory) {
  const candidates = await listDylibs(directory);
  const byName = new Map(candidates.map((file) => [basename(file), file]));
  const core = byName.get("libmpv.2.dylib");
  if (!core) return [];
  const selected = new Map([[basename(core), core]]);
  const pending = [core];
  while (pending.length > 0) {
    const file = pending.shift();
    for (const dependency of parseRpathDependencies(captureCommand("otool", ["-L", file]))) {
      if (selected.has(dependency)) continue;
      const dependencyFile = byName.get(dependency);
      if (!dependencyFile) {
        throw new Error(`[libmpv] IINA Frameworks 缺少依赖：${basename(file)} -> ${dependency}`);
      }
      selected.set(dependency, dependencyFile);
      pending.push(dependencyFile);
    }
  }
  return [...selected.values()].sort((left, right) => basename(left).localeCompare(basename(right), "en"));
}

function thinDylib(source, target, arch) {
  const architecture = arch === "x64" ? "x86_64" : "arm64";
  const available = parseLipoArchitectures(captureCommand("lipo", ["-archs", source]));
  if (!available.includes(architecture)) {
    throw new Error(`[libmpv] ${basename(source)} does not contain ${architecture}`);
  }
  if (available.length === 1) {
    runCommand("ditto", [source, target]);
    return;
  }
  runCommand("lipo", [source, "-thin", architecture, "-output", target]);
}

function addLoaderRpath(file) {
  const current = captureCommand("otool", ["-l", file]);
  if (/path @loader_path \(offset/.test(current)) return;
  captureCommand("install_name_tool", ["-add_rpath", "@loader_path", file]);
}

function validateRuntimeClosure(files, arch) {
  const names = new Set(files.map((file) => basename(file)));
  const architecture = arch === "x64" ? "x86_64" : "arm64";
  for (const file of files) {
    const architectures = parseLipoArchitectures(captureCommand("lipo", ["-archs", file]));
    if (architectures.length !== 1 || architectures[0] !== architecture) {
      throw new Error(`[libmpv] unexpected architecture for ${basename(file)}: ${architectures.join(" ")}`);
    }
    const dependencies = captureCommand("otool", ["-L", file]);
    if (dependencies.includes("/Applications/IINA.app")) {
      throw new Error(`[libmpv] non-relocatable IINA dependency: ${basename(file)}`);
    }
    for (const dependency of parseRpathDependencies(dependencies)) {
      if (!names.has(dependency)) {
        throw new Error(`[libmpv] unresolved @rpath dependency: ${basename(file)} -> ${dependency}`);
      }
    }
  }
}

function smokeLoad(library) {
  runCommand("ruby", [
    "-rfiddle",
    "-e",
    "Fiddle.dlopen(ARGV.fetch(0)); puts '[libmpv] dynamic load passed'",
    library
  ]);
}

function detachDmg(mountDirectory) {
  runCommand("hdiutil", ["detach", mountDirectory]);
}

function captureCommand(command, args) {
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

function runCommand(command, args) {
  const result = spawnSync(command, args, {
    cwd: process.cwd(),
    env: process.env,
    stdio: "inherit"
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[libmpv] command failed (${result.status ?? "unknown"}): ${command}`);
  }
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
