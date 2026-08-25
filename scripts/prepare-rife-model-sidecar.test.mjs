import assert from "node:assert/strict";
import test from "node:test";
import { chmod, mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { mkdtemp } from "node:fs/promises";

import {
  RIFE_SIDECAR_SOURCE,
  createBundleManifest,
  parseArgs
} from "./prepare-rife-model-sidecar.mjs";

test("RIFE sidecar target parser covers all desktop GPU families", () => {
  assert.equal(parseArgs(["--platform", "win32", "--arch", "x64"]).platform, "win32");
  assert.equal(parseArgs(["--platform", "darwin", "--arch", "arm64"]).arch, "arm64");
  assert.equal(parseArgs(["--platform", "linux", "--arch", "x64"]).platform, "linux");
  assert.throws(() => parseArgs(["--platform", "linux", "--arch", "arm64"]), /unsupported target/);
});

test("RIFE source and native submodules are pinned to complete Git commits", () => {
  assert.match(RIFE_SIDECAR_SOURCE.commit, /^[0-9a-f]{40}$/);
  assert.deepEqual(Object.keys(RIFE_SIDECAR_SOURCE.submodules).sort(), ["glslang", "libwebp", "ncnn"]);
  for (const commit of Object.values(RIFE_SIDECAR_SOURCE.submodules)) {
    assert.match(commit, /^[0-9a-f]{40}$/);
  }
});

test("RIFE sidecar manifest binds executable and every model file digest", async () => {
  const root = await mkdtemp(join(tmpdir(), "ani-rife-manifest-"));
  const executableName = process.platform === "win32" ? "sidecar.exe" : "sidecar";
  await writeFile(join(root, executableName), "fixture executable");
  if (process.platform !== "win32") await chmod(join(root, executableName), 0o755);
  const modelRoot = join(root, "models", RIFE_SIDECAR_SOURCE.modelId);
  await mkdir(modelRoot, { recursive: true });
  for (const file of RIFE_SIDECAR_SOURCE.files) await writeFile(join(modelRoot, file.name), file.name);
  const manifest = await createBundleManifest(root, executableName);
  assert.deepEqual(
    Object.keys(manifest).sort(),
    ["executable", "executableSha256", "files", "model", "protocolVersion", "schemaVersion"]
  );
  assert.equal(manifest.protocolVersion, 1);
  assert.equal(manifest.model.backend, "ncnn-vulkan");
  assert.equal(manifest.model.operation, "interpolate");
  assert.equal(manifest.model.outputScale, 1);
  assert.equal(manifest.files.length, 2);
  assert.equal(
    manifest.executableSha256,
    createHash("sha256").update("fixture executable").digest("hex")
  );
});
