#!/usr/bin/env node

/** 终版播放器实机验收矩阵；CI 只校验目标项完整，不伪造硬件通过。 */
export const PLAYER_ENHANCEMENT_MATRIX = [
  { platform: "windows", arch: "x64", gpuVendors: ["nvidia", "amd", "intel"], renderer: "d3d11", decoder: "d3d11va" },
  // macOS libVLC remains the production transport; future libmpv render API uses OpenGL/CGL.
  { platform: "macos", arch: "arm64", gpuVendors: ["apple"], renderer: "opengl-cgl", decoder: "videotoolbox" },
  { platform: "macos", arch: "x64", gpuVendors: ["intel", "amd"], renderer: "opengl-cgl", decoder: "videotoolbox" },
  { platform: "linux", arch: "x64", gpuVendors: ["amd", "intel", "nvidia"], renderer: "vulkan", decoder: "vaapi" }
];

export function validateEnhancementMatrix(matrix = PLAYER_ENHANCEMENT_MATRIX) {
  const required = new Set(["windows:x64", "macos:arm64", "macos:x64", "linux:x64"]);
  const actual = new Set(matrix.map((entry) => `${entry.platform}:${entry.arch}`));
  const missing = [...required].filter((target) => !actual.has(target));
  if (missing.length) throw new Error(`播放器实机矩阵缺少目标：${missing.join(", ")}`);
  for (const entry of matrix) {
    if (!entry.gpuVendors?.length || !entry.renderer || !entry.decoder) {
      throw new Error(`播放器实机矩阵条目不完整：${entry.platform}:${entry.arch}`);
    }
  }
  return matrix;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  validateEnhancementMatrix();
  console.log(`[player-matrix] ${PLAYER_ENHANCEMENT_MATRIX.length} 个桌面目标已登记`);
}
