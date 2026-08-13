#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, cp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

export const RIFE_SIDECAR_SOURCE = Object.freeze({
  repository: "https://github.com/nihui/rife-ncnn-vulkan.git",
  commit: "a7532fc3f9f008cd6eecd6f2ffe2a9698e0cf7",
  modelId: "rife-v4.6",
  backend: "ncnn-vulkan",
  files: Object.freeze([
    Object.freeze({
      name: "flownet.bin",
      sha256: "f334ed2260149ce0188a6dcf049844e8b0cdd912e01cbcfb63553157d2508958"
    }),
    Object.freeze({
      name: "flownet.param",
      sha256: "724569596bcd1e7b9fa50455c604777ebed99746d2ef40aa86e31b5725f1053c"
    })
  ])
});

const MODEL_CDN = `https://cdn.jsdelivr.net/gh/nihui/rife-ncnn-vulkan@${RIFE_SIDECAR_SOURCE.commit}/models/${RIFE_SIDECAR_SOURCE.modelId}`;

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}

async function main(args) {
  const options = parseArgs(args);
  const targetKey = `${options.platform}-${options.arch}`;
  const targetDirectory = resolve(options.targetRoot, targetKey);
  if (options.verifyOnly) {
    await verifyBundle(targetDirectory);
    console.log(`[rife-sidecar] verified ${targetKey}`);
    return;
  }

  const cacheRoot = resolve(options.cacheRoot);
  const sourceDirectory = join(cacheRoot, `source-${RIFE_SIDECAR_SOURCE.commit}`);
  const modelCacheDirectory = join(cacheRoot, "models", RIFE_SIDECAR_SOURCE.modelId);
  const buildDirectory = join(cacheRoot, `build-${targetKey}`);
  await ensureSource(sourceDirectory, options.offline);
  await ensureModelFiles(modelCacheDirectory, options.offline);
  await configureAndBuild(sourceDirectory, buildDirectory, options);
  const executable = await resolveBuiltExecutable(buildDirectory, options.platform);

  await rm(targetDirectory, { recursive: true, force: true });
  const modelDirectory = join(targetDirectory, "models", RIFE_SIDECAR_SOURCE.modelId);
  const licenseDirectory = join(targetDirectory, "licenses");
  await mkdir(modelDirectory, { recursive: true });
  await mkdir(licenseDirectory, { recursive: true });
  const executableName = options.platform === "win32"
    ? "ani-rife-model-sidecar.exe"
    : "ani-rife-model-sidecar";
  const stagedExecutable = join(targetDirectory, executableName);
  await cp(executable, stagedExecutable);
  if (options.platform !== "win32") await chmod(stagedExecutable, 0o755);
  for (const file of RIFE_SIDECAR_SOURCE.files) {
    await cp(join(modelCacheDirectory, file.name), join(modelDirectory, file.name));
  }
  await cp(join(sourceDirectory, "LICENSE"), join(licenseDirectory, "rife-ncnn-vulkan-MIT.txt"));

  const manifest = await createBundleManifest(targetDirectory, executableName, targetKey);
  await writeFile(join(targetDirectory, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  await writeFile(
    join(targetDirectory, "SOURCE.json"),
    `${JSON.stringify({
      repository: RIFE_SIDECAR_SOURCE.repository,
      commit: RIFE_SIDECAR_SOURCE.commit,
      modelId: RIFE_SIDECAR_SOURCE.modelId,
      backend: RIFE_SIDECAR_SOURCE.backend,
      target: targetKey,
      modelFiles: RIFE_SIDECAR_SOURCE.files
    }, null, 2)}\n`,
    "utf8"
  );
  await verifyBundle(targetDirectory);
  console.log(`[rife-sidecar] bundle ready: ${targetDirectory}`);
}

export async function createBundleManifest(targetDirectory, executableName, targetKey) {
  const files = [];
  for (const file of RIFE_SIDECAR_SOURCE.files) {
    const relativePath = `models/${RIFE_SIDECAR_SOURCE.modelId}/${file.name}`;
    const path = join(targetDirectory, ...relativePath.split("/"));
    files.push({ path: relativePath, sha256: await sha256(path) });
  }
  return {
    schemaVersion: 1,
    protocolVersion: 1,
    target: targetKey,
    executable: executableName,
    executableSha256: await sha256(join(targetDirectory, executableName)),
    model: {
      modelId: RIFE_SIDECAR_SOURCE.modelId,
      backend: RIFE_SIDECAR_SOURCE.backend,
      directory: `models/${RIFE_SIDECAR_SOURCE.modelId}`,
      inputWidth: 320,
      inputHeight: 180,
      requiredVramBytes: 1_073_741_824,
      estimatedFrameTimeMs: 16
    },
    files
  };
}

export async function verifyBundle(targetDirectory) {
  const manifest = JSON.parse(await readFile(join(targetDirectory, "manifest.json"), "utf8"));
  if (
    manifest.schemaVersion !== 1
    || manifest.protocolVersion !== 1
    || manifest.model?.modelId !== RIFE_SIDECAR_SOURCE.modelId
    || manifest.model?.backend !== RIFE_SIDECAR_SOURCE.backend
    || !Array.isArray(manifest.files)
  ) {
    throw new Error(`[rife-sidecar] invalid manifest: ${targetDirectory}`);
  }
  await verifyFile(join(targetDirectory, manifest.executable), manifest.executableSha256);
  for (const expected of RIFE_SIDECAR_SOURCE.files) {
    const relativePath = `models/${RIFE_SIDECAR_SOURCE.modelId}/${expected.name}`;
    const declared = manifest.files.find((file) => file.path === relativePath);
    if (!declared || declared.sha256 !== expected.sha256) {
      throw new Error(`[rife-sidecar] model manifest mismatch: ${relativePath}`);
    }
    await verifyFile(join(targetDirectory, ...relativePath.split("/")), expected.sha256);
  }
  await stat(join(targetDirectory, "licenses", "rife-ncnn-vulkan-MIT.txt"));
  return manifest;
}

async function ensureSource(directory, offline) {
  if (await isDirectory(join(directory, ".git"))) {
    const commit = run("git", ["rev-parse", "HEAD"], { cwd: directory, capture: true }).trim();
    if (commit === RIFE_SIDECAR_SOURCE.commit) return;
    if (offline) throw new Error(`[rife-sidecar] cached source commit mismatch: ${commit}`);
  }
  if (offline) throw new Error(`[rife-sidecar] offline source missing: ${directory}`);
  await rm(directory, { recursive: true, force: true });
  await mkdir(resolve(directory, ".."), { recursive: true });
  run("git", ["clone", "--filter=blob:none", "--no-checkout", RIFE_SIDECAR_SOURCE.repository, directory]);
  run("git", ["checkout", "--detach", RIFE_SIDECAR_SOURCE.commit], { cwd: directory });
  run("git", ["submodule", "update", "--init", "--recursive", "--depth", "1"], { cwd: directory });
}

async function ensureModelFiles(directory, offline) {
  await mkdir(directory, { recursive: true });
  for (const file of RIFE_SIDECAR_SOURCE.files) {
    const path = join(directory, file.name);
    if (await fileMatches(path, file.sha256)) continue;
    if (offline) throw new Error(`[rife-sidecar] offline model missing: ${path}`);
    await downloadWithRetries(`${MODEL_CDN}/${file.name}`, path);
    await verifyFile(path, file.sha256);
  }
}

async function configureAndBuild(sourceDirectory, buildDirectory, options) {
  await rm(buildDirectory, { recursive: true, force: true });
  const cmakeArgs = [
    "-S", resolve("native/rife-model-sidecar"),
    "-B", buildDirectory,
    `-DRIFE_SOURCE_DIR=${sourceDirectory}`,
    "-DCMAKE_BUILD_TYPE=Release"
  ];
  if (options.platform === "darwin") {
    const moltenVk = await resolveMacosMoltenVk();
    cmakeArgs.push(
      "-DUSE_STATIC_MOLTENVK=ON",
      `-DCMAKE_OSX_ARCHITECTURES=${options.arch === "x64" ? "x86_64" : "arm64"}`,
      `-DVulkan_INCLUDE_DIR=${moltenVk.includeDirectory}`,
      `-DVulkan_LIBRARY=${moltenVk.library}`
    );
  }
  if (options.platform === "win32") cmakeArgs.push("-A", "x64");
  run("cmake", cmakeArgs);
  run("cmake", ["--build", buildDirectory, "--config", "Release", "--parallel", String(options.jobs)]);
}

async function resolveMacosMoltenVk() {
  const sdk = process.env.VULKAN_SDK;
  if (!sdk) throw new Error("[rife-sidecar] VULKAN_SDK is required for macOS MoltenVK builds");
  const includeCandidates = [join(sdk, "include"), join(sdk, "MoltenVK", "include")];
  const libraryCandidates = [
    join(sdk, "MoltenVK", "MoltenVK.xcframework", "macos-arm64_x86_64", "libMoltenVK.a"),
    join(sdk, "lib", "libMoltenVK.a")
  ];
  const includeDirectory = await firstDirectory(includeCandidates);
  const library = await firstFile(libraryCandidates);
  if (!includeDirectory || !library) {
    throw new Error(`[rife-sidecar] static MoltenVK is missing from VULKAN_SDK: ${sdk}`);
  }
  return { includeDirectory, library };
}

async function resolveBuiltExecutable(buildDirectory, platform) {
  const name = platform === "win32" ? "ani-rife-model-sidecar.exe" : "ani-rife-model-sidecar";
  for (const candidate of [join(buildDirectory, name), join(buildDirectory, "Release", name)]) {
    if (await isFile(candidate)) return candidate;
  }
  throw new Error(`[rife-sidecar] built executable missing: ${buildDirectory}`);
}

async function downloadWithRetries(url, target) {
  let lastError;
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(180_000) });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      await writeFile(target, Buffer.from(await response.arrayBuffer()));
      return;
    } catch (error) {
      lastError = error;
      if (attempt < 5) await new Promise((resolveDelay) => setTimeout(resolveDelay, attempt * 1_000));
    }
  }
  throw new Error(`[rife-sidecar] download failed: ${url}: ${lastError}`);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? process.cwd(),
    env: process.env,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit"
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`[rife-sidecar] command failed: ${command} ${args.join(" ")}`);
  return result.stdout ?? "";
}

export function parseArgs(args) {
  const parsed = {
    platform: process.env.npm_config_platform || process.platform,
    arch: process.env.npm_config_arch || process.arch,
    cacheRoot: ".cache/rife-model-sidecar",
    targetRoot: "out/model-sidecar",
    jobs: 2,
    offline: false,
    verifyOnly: false
  };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") continue;
    if (arg === "--offline") { parsed.offline = true; continue; }
    if (arg === "--verify-only") { parsed.verifyOnly = true; continue; }
    if (["--platform", "--arch", "--cache", "--target", "--jobs"].includes(arg)) {
      const value = args[index + 1];
      if (!value) throw new Error(`${arg} requires a value`);
      index += 1;
      if (arg === "--platform") parsed.platform = value;
      if (arg === "--arch") parsed.arch = value;
      if (arg === "--cache") parsed.cacheRoot = value;
      if (arg === "--target") parsed.targetRoot = value;
      if (arg === "--jobs") parsed.jobs = Number(value);
      continue;
    }
    throw new Error(`Unknown argument: ${arg}`);
  }
  if (!Number.isInteger(parsed.jobs) || parsed.jobs < 1 || parsed.jobs > 32) {
    throw new Error("--jobs must be an integer between 1 and 32");
  }
  if (![
    "win32-x64", "darwin-x64", "darwin-arm64", "linux-x64"
  ].includes(`${parsed.platform}-${parsed.arch}`)) {
    throw new Error(`[rife-sidecar] unsupported target: ${parsed.platform}-${parsed.arch}`);
  }
  return parsed;
}

async function verifyFile(path, expectedSha256) {
  const info = await stat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`[rife-sidecar] expected file: ${path}`);
  const actual = await sha256(path);
  if (actual !== expectedSha256) throw new Error(`[rife-sidecar] SHA-256 mismatch: ${path}`);
}

async function fileMatches(path, expectedSha256) {
  try {
    await verifyFile(path, expectedSha256);
    return true;
  } catch {
    return false;
  }
}

async function sha256(path) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest("hex");
}

async function isFile(path) {
  try { return (await stat(path)).isFile(); } catch { return false; }
}

async function isDirectory(path) {
  try { return (await stat(path)).isDirectory(); } catch { return false; }
}

async function firstFile(paths) {
  for (const path of paths) if (await isFile(path)) return path;
  return undefined;
}

async function firstDirectory(paths) {
  for (const path of paths) if (await isDirectory(path)) return path;
  return undefined;
}
