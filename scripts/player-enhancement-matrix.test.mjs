import assert from "node:assert/strict";
import test from "node:test";

import {
  FINAL_RELEASE_GATES,
  MODEL_SIDECAR_MATRIX,
  PLAYER_ENHANCEMENT_MATRIX,
  REMOTE_ENCODER_MATRIX,
  validateEnhancementMatrix
} from "./player-enhancement-matrix.mjs";

test("播放器实机矩阵覆盖桌面架构和 GPU 厂商", () => {
  const matrix = validateEnhancementMatrix();
  assert.equal(matrix.length, 4);
  assert.deepEqual(matrix.find((entry) => entry.platform === "windows").gpuVendors, ["nvidia", "amd", "intel"]);
  assert.deepEqual(matrix.find((entry) => entry.platform === "linux").gpuVendors, ["amd", "intel", "nvidia"]);
  assert.equal(matrix.find((entry) => entry.platform === "macos" && entry.arch === "arm64").renderer, "opengl-cgl");
  assert.equal(matrix.filter((entry) => entry.platform === "macos").every((entry) => entry.localShader === "implemented"), true);
  assert.equal(matrix.find((entry) => entry.platform === "windows").localModel, "pending");
  assert.equal(matrix.every((entry) => entry.remoteModel === "implemented"), true);
});

test("播放器实机矩阵拒绝缺少目标或能力字段", () => {
  assert.throws(() => validateEnhancementMatrix(PLAYER_ENHANCEMENT_MATRIX.slice(1)), /缺少目标/);
  assert.throws(() => validateEnhancementMatrix(PLAYER_ENHANCEMENT_MATRIX.map((entry) => ({ ...entry, renderer: "" }))), /条目不完整/);
});

test("模型、远程编码和稳定期门禁明确区分代码与真机证据", () => {
  validateEnhancementMatrix();
  assert.deepEqual(MODEL_SIDECAR_MATRIX.map((entry) => entry.operation), ["interpolate", "enhance"]);
  assert.equal(MODEL_SIDECAR_MATRIX.find((entry) => entry.operation === "enhance").outputScale, 2);
  assert.equal(REMOTE_ENCODER_MATRIX.some((entry) => entry.encoder === "amf"), true);
  assert.equal(REMOTE_ENCODER_MATRIX.at(-1).encoder, "libx264");
  assert.equal(FINAL_RELEASE_GATES.requiredStableVersions, 2);
  assert.equal(FINAL_RELEASE_GATES.evidenceStates.includes("device-passed"), true);
  assert.equal(FINAL_RELEASE_GATES.requiredEvidenceFields.includes("degradationReason"), true);
});
