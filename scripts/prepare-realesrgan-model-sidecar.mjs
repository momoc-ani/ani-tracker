#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { chmod, cp, mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { pipeline } from "node:stream/promises";

export const REALESRGAN_SIDECAR_SOURCE = Object.freeze({
  repository: "https://github.com/xinntao/Real-ESRGAN-ncnn-vulkan.git",
  commit: "37026f49824c5cf84062e7c6a5dd71445dcf610f",
  submodules: Object.freeze({
    ncnn: "6125c9f47cd14b589de0521350668cf9d3d37e3c",
    libwebp: "8ea81561d2fdd382da60f57958741a7c23a18eb6",
    glslang: "4afd69177258d0636f78d2c4efb823ab6382a187"
  }),
  modelId: "realesr-animevideov3-x2",
  backend: "ncnn-vulkan",
  modelArchive: Object.freeze({
    repository: "https://github.com/xinntao/Real-ESRGAN",
    release: "v0.2.5.0",
    asset: "realesrgan-ncnn-vulkan-20220424-windows.zip",
    size: 45_474_481,
    sha256: "abc02804e17982a3be33675e4d471e91ea374e65b70167abc09e31acb412802d"
  }),
  files: Object.freeze([
    Object.freeze({
      name: "realesr-animevideov3-x2.bin",
      sha256: "548a36f9c3f4ab8da56cd3b13badf23968bee207b396dad14d04b830e5f2ab2d"
    }),
    Object.freeze({
      name: "realesr-animevideov3-x2.param",
      sha256: "b88ff4f00ebf019a7fdac17fdd45a7fd3665d37509efc5baf2e4da2e24420a04"
    })
  ])
});

const MODEL_ARCHIVE_URL = `${REALESRGAN_SIDECAR_SOURCE.modelArchive.repository}/releases/download/${REALESRGAN_SIDECAR_SOURCE.modelArchive.release}/${REALESRGAN_SIDECAR_SOURCE.modelArchive.asset}`;

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}

async function main(args) {
  const options = parseArgs(args);
  const targetKey = `${options.platform}-${options.arch}`;
  const targetDirectory = resolve(options.targetRoot, targetKey);
  if (options.verifyOnly) {
    await verifyBundle(targetDirectory);
    console.log(`[realesrgan-sidecar] verified ${targetKey}`);
    return;
  }

  const cacheRoot = resolve(options.cacheRoot);
  const sourceDirectory = join(cacheRoot, `source-${REALESRGAN_SIDECAR_SOURCE.commit}`);
  const archivePath = join(cacheRoot, "downloads", REALESRGAN_SIDECAR_SOURCE.modelArchive.asset);
  const modelCacheDirectory = join(cacheRoot, "models", REALESRGAN_SIDECAR_SOURCE.modelId);
  const buildDirectory = join(cacheRoot, `build-${targetKey}`);
  await ensureSource(sourceDirectory, options.offline);
  await ensureModelFiles(archivePath, modelCacheDirectory, options.offline);
  await configureAndBuild(sourceDirectory, buildDirectory, options);
  const executable = await resolveBuiltExecutable(buildDirectory, options.platform);

  await rm(targetDirectory, { recursive: true, force: true });
  const modelDirectory = join(targetDirectory, "models", REALESRGAN_SIDECAR_SOURCE.modelId);
  const licenseDirectory = join(targetDirectory, "licenses");
  await mkdir(modelDirectory, { recursive: true });
  await mkdir(licenseDirectory, { recursive: true });
  const executableName = options.platform === "win32"
    ? "ani-realesrgan-model-sidecar.exe"
    : "ani-realesrgan-model-sidecar";
  const stagedExecutable = join(targetDirectory, executableName);
  await cp(executable, stagedExecutable);
  if (options.platform !== "win32") await chmod(stagedExecutable, 0o755);
  for (const file of REALESRGAN_SIDECAR_SOURCE.files) {
    await cp(join(modelCacheDirectory, file.name), join(modelDirectory, file.name));
  }
  await cp(
    join(sourceDirectory, "LICENSE"),
    join(licenseDirectory, "Real-ESRGAN-ncnn-vulkan-MIT.txt")
  );

  const manifest = await createBundleManifest(targetDirectory, executableName);
  await writeFile(join(targetDirectory, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  await writeFile(
    join(targetDirectory, "SOURCE.json"),
    `${JSON.stringify({
      repository: REALESRGAN_SIDECAR_SOURCE.repository,
      commit: REALESRGAN_SIDECAR_SOURCE.commit,
      submodules: REALESRGAN_SIDECAR_SOURCE.submodules,
      modelId: REALESRGAN_SIDECAR_SOURCE.modelId,
      backend: REALESRGAN_SIDECAR_SOURCE.backend,
      target: targetKey,
      modelArchive: REALESRGAN_SIDECAR_SOURCE.modelArchive,
      modelFiles: REALESRGAN_SIDECAR_SOURCE.files
    }, null, 2)}\n`,
    "utf8"
  );
  await verifyBundle(targetDirectory);
  console.log(`[realesrgan-sidecar] bundle ready: ${targetDirectory}`);
}

export async function createBundleManifest(targetDirectory, executableName) {
  const files = [];
  for (const file of REALESRGAN_SIDECAR_SOURCE.files) {
    const relativePath = `models/${REALESRGAN_SIDECAR_SOURCE.modelId}/${file.name}`;
    files.push({
      path: relativePath,
      sha256: await sha256(join(targetDirectory, ...relativePath.split("/")))
    });
  }
  return {
    schemaVersion: 1,
    protocolVersion: 1,
    executable: executableName,
    executableSha256: await sha256(join(targetDirectory, executableName)),
    model: {
      modelId: REALESRGAN_SIDECAR_SOURCE.modelId,
      backend: REALESRGAN_SIDECAR_SOURCE.backend,
      operation: "enhance",
      outputScale: 2,
      directory: `models/${REALESRGAN_SIDECAR_SOURCE.modelId}`,
      inputWidth: 320,
      inputHeight: 180,
      requiredVramBytes: 1_610_612_736,
      estimatedFrameTimeMs: 33
    },
    files
  };
}

export async function verifyBundle(targetDirectory) {
  const manifest = JSON.parse(await readFile(join(targetDirectory, "manifest.json"), "utf8"));
  if (
    manifest.schemaVersion !== 1
    || manifest.protocolVersion !== 1
    || manifest.model?.modelId !== REALESRGAN_SIDECAR_SOURCE.modelId
    || manifest.model?.backend !== REALESRGAN_SIDECAR_SOURCE.backend
    || manifest.model?.operation !== "enhance"
    || manifest.model?.outputScale !== 2
    || !Array.isArray(manifest.files)
  ) {
    throw new Error(`[realesrgan-sidecar] invalid manifest: ${targetDirectory}`);
  }
  await verifyFile(join(targetDirectory, manifest.executable), manifest.executableSha256);
  for (const expected of REALESRGAN_SIDECAR_SOURCE.files) {
    const relativePath = `models/${REALESRGAN_SIDECAR_SOURCE.modelId}/${expected.name}`;
    const declared = manifest.files.find((file) => file.path === relativePath);
    if (!declared || declared.sha256 !== expected.sha256) {
      throw new Error(`[realesrgan-sidecar] model manifest mismatch: ${relativePath}`);
    }
    await verifyFile(join(targetDirectory, ...relativePath.split("/")), expected.sha256);
  }
  await stat(join(targetDirectory, "licenses", "Real-ESRGAN-ncnn-vulkan-MIT.txt"));
  return manifest;
}

async function ensureSource(directory, offline) {
  if (await isDirectory(join(directory, ".git"))) {
    const commit = readGitHead(directory) ?? await readPinnedCommit(directory);
    const complete = await Promise.all([
      isFile(join(directory, "src", "realesrgan.cpp")),
      isFile(join(directory, "src", "ncnn", "CMakeLists.txt")),
      isFile(join(directory, "src", "libwebp", "CMakeLists.txt")),
      isFile(join(directory, "src", "ncnn", "glslang", "CMakeLists.txt")),
      pinnedSubmoduleMatches(join(directory, "src", "ncnn"), REALESRGAN_SIDECAR_SOURCE.submodules.ncnn),
      pinnedSubmoduleMatches(join(directory, "src", "libwebp"), REALESRGAN_SIDECAR_SOURCE.submodules.libwebp),
      pinnedSubmoduleMatches(join(directory, "src", "ncnn", "glslang"), REALESRGAN_SIDECAR_SOURCE.submodules.glslang)
    ]);
    if (commit === REALESRGAN_SIDECAR_SOURCE.commit && complete.every(Boolean)) return;
    if (offline) throw new Error(`[realesrgan-sidecar] cached source is incomplete or commit mismatched: ${commit ?? "missing"}`);
  }
  if (offline) throw new Error(`[realesrgan-sidecar] offline source missing: ${directory}`);
  await rm(directory, { recursive: true, force: true });
  await mkdir(resolve(directory, ".."), { recursive: true });
  run("git", ["init", directory]);
  run("git", ["remote", "add", "origin", REALESRGAN_SIDECAR_SOURCE.repository], { cwd: directory });
  run("git", ["sparse-checkout", "init", "--cone"], { cwd: directory });
  run("git", ["sparse-checkout", "set", "src"], { cwd: directory });
  run("git", ["fetch", "--depth", "1", "--filter=blob:none", "origin", REALESRGAN_SIDECAR_SOURCE.commit], { cwd: directory });
  run("git", ["switch", "--detach", "FETCH_HEAD"], { cwd: directory });
  configureGitSubmoduleUrls(directory);
  run("git", ["submodule", "update", "--init", "--recursive", "--depth", "1"], { cwd: directory });
  await writeFile(join(directory, ".ani-source-commit"), `${REALESRGAN_SIDECAR_SOURCE.commit}\n`, "utf8");
  await writeFile(join(directory, "src", "ncnn", ".ani-submodule-commit"), `${REALESRGAN_SIDECAR_SOURCE.submodules.ncnn}\n`, "utf8");
  await writeFile(join(directory, "src", "libwebp", ".ani-submodule-commit"), `${REALESRGAN_SIDECAR_SOURCE.submodules.libwebp}\n`, "utf8");
  await writeFile(join(directory, "src", "ncnn", "glslang", ".ani-submodule-commit"), `${REALESRGAN_SIDECAR_SOURCE.submodules.glslang}\n`, "utf8");
}

// 将上游子模块地址规范化为无需密钥的 HTTPS 地址。
function configureGitSubmoduleUrls(directory) {
  const modules = [
    ["submodule.src/ncnn.url", "https://github.com/Tencent/ncnn.git"],
    ["submodule.src/libwebp.url", "https://github.com/webmproject/libwebp.git"]
  ];
  for (const [key, url] of modules) {
    run("git", ["config", "-f", ".gitmodules", key, url], { cwd: directory });
  }
  run("git", ["submodule", "sync", "--recursive"], { cwd: directory });
}

async function readPinnedCommit(directory) {
  try { return (await readFile(join(directory, ".ani-source-commit"), "utf8")).trim(); } catch { return undefined; }
}

function readGitHead(directory) {
  try { return run("git", ["rev-parse", "HEAD"], { cwd: directory, capture: true }).trim(); } catch { return undefined; }
}

async function pinnedSubmoduleMatches(directory, expected) {
  try {
    const marker = (await readFile(join(directory, ".ani-submodule-commit"), "utf8")).trim();
    const topLevel = resolve(run("git", ["rev-parse", "--show-toplevel"], { cwd: directory, capture: true }).trim());
    if (topLevel !== resolve(directory)) return marker === expected;
    return readGitHead(directory) === expected;
  } catch {
    try {
      return readGitHead(directory) === expected;
    } catch { return false; }
  }
}

async function ensureModelFiles(archivePath, directory, offline) {
  const complete = await Promise.all(
    REALESRGAN_SIDECAR_SOURCE.files.map((file) => fileMatches(join(directory, file.name), file.sha256))
  );
  if (complete.every(Boolean)) return;
  if (offline) throw new Error(`[realesrgan-sidecar] offline model missing: ${directory}`);
  await mkdir(resolve(archivePath, ".."), { recursive: true });
  if (!await fileMatches(archivePath, REALESRGAN_SIDECAR_SOURCE.modelArchive.sha256)) {
    await downloadWithRetries(MODEL_ARCHIVE_URL, archivePath);
    await verifyFile(archivePath, REALESRGAN_SIDECAR_SOURCE.modelArchive.sha256);
  }
  const extractDirectory = `${directory}.extracting`;
  await rm(extractDirectory, { recursive: true, force: true });
  await mkdir(extractDirectory, { recursive: true });
  run("cmake", ["-E", "tar", "xvf", archivePath], { cwd: extractDirectory });
  await rm(directory, { recursive: true, force: true });
  await mkdir(directory, { recursive: true });
  for (const file of REALESRGAN_SIDECAR_SOURCE.files) {
    const source = join(extractDirectory, "models", file.name);
    await verifyFile(source, file.sha256);
    await cp(source, join(directory, file.name));
  }
  await rm(extractDirectory, { recursive: true, force: true });
}

async function configureAndBuild(sourceDirectory, buildDirectory, options) {
  await rm(buildDirectory, { recursive: true, force: true });
  const cmakeArgs = [
    "-S", resolve("native/realesrgan-model-sidecar"),
    "-B", buildDirectory,
    `-DREALESRGAN_SOURCE_DIR=${sourceDirectory}`,
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_POLICY_VERSION_MINIMUM=3.5"
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
  run("cmake", ["--build", buildDirectory, "--config", "Release", "--parallel", String(options.jobs), "--target", "ani-realesrgan-model-sidecar"]);
}

async function resolveMacosMoltenVk() {
  const sdk = process.env.VULKAN_SDK;
  if (!sdk) throw new Error("[realesrgan-sidecar] VULKAN_SDK is required for macOS MoltenVK builds");
  const includeDirectory = await firstDirectory([join(sdk, "include"), join(sdk, "MoltenVK", "include")]);
  const library = await firstFile([
    join(sdk, "MoltenVK", "MoltenVK.xcframework", "macos-arm64_x86_64", "libMoltenVK.a"),
    join(sdk, "lib", "MoltenVK", "MoltenVK.xcframework", "macos-arm64_x86_64", "libMoltenVK.a"),
    join(sdk, "lib", "MoltenVK.xcframework", "macos-arm64_x86_64", "libMoltenVK.a"),
    join(sdk, "lib", "libMoltenVK.a")
  ]);
  if (!includeDirectory || !library) {
    throw new Error(`[realesrgan-sidecar] static MoltenVK is missing from VULKAN_SDK: ${sdk}`);
  }
  return { includeDirectory, library };
}

async function resolveBuiltExecutable(buildDirectory, platform) {
  const name = platform === "win32"
    ? "ani-realesrgan-model-sidecar.exe"
    : "ani-realesrgan-model-sidecar";
  for (const candidate of [join(buildDirectory, name), join(buildDirectory, "Release", name)]) {
    if (await isFile(candidate)) return candidate;
  }
  throw new Error(`[realesrgan-sidecar] built executable missing: ${buildDirectory}`);
}

async function downloadWithRetries(url, target) {
  const partial = `${target}.part`;
  let lastError;
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(600_000) });
      if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
      await pipeline(response.body, createWriteStream(partial));
      const info = await stat(partial);
      if (info.size !== REALESRGAN_SIDECAR_SOURCE.modelArchive.size) {
        throw new Error(`unexpected archive size ${info.size}`);
      }
      await rename(partial, target);
      return;
    } catch (error) {
      lastError = error;
      await rm(partial, { force: true });
      if (attempt < 5) await new Promise((resolveDelay) => setTimeout(resolveDelay, attempt * 2_000));
    }
  }
  throw new Error(`[realesrgan-sidecar] download failed: ${url}: ${lastError}`);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? process.cwd(),
    env: process.env,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit"
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`[realesrgan-sidecar] command failed: ${command} ${args.join(" ")}`);
  }
  return result.stdout ?? "";
}

export function parseArgs(args) {
  const parsed = {
    platform: process.env.npm_config_platform || process.platform,
    arch: process.env.npm_config_arch || process.arch,
    cacheRoot: ".cache/realesrgan-model-sidecar",
    targetRoot: "out/realesrgan-model-sidecar",
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
  if (!["win32-x64", "darwin-x64", "darwin-arm64", "linux-x64"].includes(`${parsed.platform}-${parsed.arch}`)) {
    throw new Error(`[realesrgan-sidecar] unsupported target: ${parsed.platform}-${parsed.arch}`);
  }
  return parsed;
}

async function verifyFile(path, expectedSha256) {
  const info = await stat(path);
  if (!info.isFile() || info.size === 0) throw new Error(`[realesrgan-sidecar] expected file: ${path}`);
  const actual = await sha256(path);
  if (actual !== expectedSha256) throw new Error(`[realesrgan-sidecar] SHA-256 mismatch: ${path}`);
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
