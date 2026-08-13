import assert from "node:assert/strict";
import test from "node:test";

import { PLAYER_ENHANCEMENT_MATRIX, validateEnhancementMatrix } from "./player-enhancement-matrix.mjs";

test("播放器实机矩阵覆盖桌面架构和 GPU 厂商", () => {
  const matrix = validateEnhancementMatrix();
  assert.equal(matrix.length, 4);
  assert.deepEqual(matrix.find((entry) => entry.platform === "windows").gpuVendors, ["nvidia", "amd", "intel"]);
  assert.deepEqual(matrix.find((entry) => entry.platform === "linux").gpuVendors, ["amd", "intel", "nvidia"]);
});

test("播放器实机矩阵拒绝缺少目标或能力字段", () => {
  assert.throws(() => validateEnhancementMatrix(PLAYER_ENHANCEMENT_MATRIX.slice(1)), /缺少目标/);
  assert.throws(() => validateEnhancementMatrix(PLAYER_ENHANCEMENT_MATRIX.map((entry) => ({ ...entry, renderer: "" }))), /条目不完整/);
});
