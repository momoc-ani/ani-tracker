#!/usr/bin/env node

/** 终版播放器实机验收矩阵；CI 只校验目标项完整，不伪造硬件通过。 */
export const PLAYER_ENHANCEMENT_MATRIX = [
  {
    platform: "windows",
    arch: "x64",
    gpuVendors: ["nvidia", "amd", "intel"],
    renderer: "d3d11",
    decoder: "d3d11va",
    localShader: "implemented",
    localModel: "pending",
    remoteModel: "implemented"
  },
  {
    platform: "macos",
    arch: "arm64",
    gpuVendors: ["apple"],
    renderer: "opengl-cgl",
    decoder: "videotoolbox",
    localShader: "implemented",
    localModel: "pending",
    remoteModel: "implemented"
  },
  {
    platform: "macos",
    arch: "x64",
    gpuVendors: ["intel", "amd"],
    renderer: "opengl-cgl",
    decoder: "videotoolbox",
    localShader: "implemented",
    localModel: "pending",
    remoteModel: "implemented"
  },
  {
    platform: "linux",
    arch: "x64",
    gpuVendors: ["amd", "intel", "nvidia"],
    renderer: "vulkan",
    decoder: "vaapi",
    localShader: "implemented",
    localModel: "pending",
    remoteModel: "implemented"
  }
];

export const MODEL_SIDECAR_MATRIX = [
  { id: "rife-v4.6", operation: "interpolate", backend: "ncnn-vulkan", outputScale: 1 },
  { id: "realesr-animevideov3-x2", operation: "enhance", backend: "ncnn-vulkan", outputScale: 2 }
];

export const REMOTE_ENCODER_MATRIX = [
  { platform: "windows", gpuVendor: "nvidia", encoder: "nvenc" },
  { platform: "windows", gpuVendor: "amd", encoder: "amf" },
  { platform: "windows", gpuVendor: "intel", encoder: "qsv" },
  { platform: "all", gpuVendor: "software-fallback", encoder: "libx264" }
];

export const FINAL_RELEASE_GATES = Object.freeze({
  requiredStableVersions: 2,
  evidenceStates: ["implemented", "release-runner", "device-passed", "stable-version"],
  requiredEvidenceFields: [
    "gitSha",
    "packageSha256",
    "systemVersion",
    "gpuModel",
    "driverVersion",
    "renderer",
    "decoder",
    "encoder",
    "modelBackend",
    "firstFrameP95Ms",
    "droppedFrames",
    "degradationReason"
  ]
});

export function validateEnhancementMatrix(matrix = PLAYER_ENHANCEMENT_MATRIX) {
  const required = new Set(["windows:x64", "macos:arm64", "macos:x64", "linux:x64"]);
  const actual = new Set(matrix.map((entry) => `${entry.platform}:${entry.arch}`));
  const missing = [...required].filter((target) => !actual.has(target));
  if (missing.length) throw new Error(`播放器实机矩阵缺少目标：${missing.join(", ")}`);
  for (const entry of matrix) {
    if (!entry.gpuVendors?.length || !entry.renderer || !entry.decoder
      || !entry.localShader || !entry.localModel || !entry.remoteModel) {
      throw new Error(`播放器实机矩阵条目不完整：${entry.platform}:${entry.arch}`);
    }
  }
  validateModelSidecars();
  validateRemoteEncoders();
  validateReleaseGates();
  return matrix;
}

function validateModelSidecars() {
  const operations = new Set(MODEL_SIDECAR_MATRIX.map((entry) => entry.operation));
  if (!operations.has("interpolate") || !operations.has("enhance")) {
    throw new Error("播放器模型矩阵必须同时登记插帧与单帧增强");
  }
  for (const entry of MODEL_SIDECAR_MATRIX) {
    if (!entry.id || entry.backend !== "ncnn-vulkan" || !Number.isInteger(entry.outputScale) || entry.outputScale < 1) {
      throw new Error(`播放器模型矩阵条目不完整：${entry.id || "unknown"}`);
    }
  }
}

function validateRemoteEncoders() {
  const required = new Set(["windows:nvidia:nvenc", "windows:amd:amf", "windows:intel:qsv", "all:software-fallback:libx264"]);
  const actual = new Set(REMOTE_ENCODER_MATRIX.map((entry) => `${entry.platform}:${entry.gpuVendor}:${entry.encoder}`));
  const missing = [...required].filter((target) => !actual.has(target));
  if (missing.length) throw new Error(`远程编码矩阵缺少目标：${missing.join(", ")}`);
}

function validateReleaseGates() {
  if (FINAL_RELEASE_GATES.requiredStableVersions < 2) {
    throw new Error("桌面 libmpv 至少需要两个稳定版本完成真机验收");
  }
  if (!FINAL_RELEASE_GATES.evidenceStates.includes("device-passed")
    || FINAL_RELEASE_GATES.requiredEvidenceFields.length < 10) {
    throw new Error("播放器发布证据字段不完整");
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  validateEnhancementMatrix();
  console.log(`[player-matrix] ${PLAYER_ENHANCEMENT_MATRIX.length} 个桌面目标已登记`);
}
