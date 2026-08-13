import assert from "node:assert/strict";
import test from "node:test";
import { chmod, mkdir, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { mkdtemp } from "node:fs/promises";

import {
  REALESRGAN_SIDECAR_SOURCE,
  createBundleManifest,
  parseArgs
} from "./prepare-realesrgan-model-sidecar.mjs";

test("Real-ESRGAN sidecar target parser covers desktop Vulkan targets", () => {
  assert.equal(parseArgs(["--platform", "win32", "--arch", "x64"]).platform, "win32");
  assert.equal(parseArgs(["--platform", "darwin", "--arch", "arm64"]).arch, "arm64");
  assert.equal(parseArgs(["--platform", "linux", "--arch", "x64"]).platform, "linux");
  assert.throws(() => parseArgs(["--platform", "linux", "--arch", "arm64"]), /unsupported target/);
});

test("Real-ESRGAN manifest binds 2x enhancement executable and model digests", async () => {
  const root = await mkdtemp(join(tmpdir(), "ani-realesrgan-manifest-"));
  const executableName = process.platform === "win32" ? "sidecar.exe" : "sidecar";
  await writeFile(join(root, executableName), "fixture executable");
  if (process.platform !== "win32") await chmod(join(root, executableName), 0o755);
  const modelRoot = join(root, "models", REALESRGAN_SIDECAR_SOURCE.modelId);
  await mkdir(modelRoot, { recursive: true });
  for (const file of REALESRGAN_SIDECAR_SOURCE.files) {
    await writeFile(join(modelRoot, file.name), file.name);
  }
  const manifest = await createBundleManifest(root, executableName, "test-x64");
  assert.equal(manifest.protocolVersion, 1);
  assert.equal(manifest.model.backend, "ncnn-vulkan");
  assert.equal(manifest.model.operation, "enhance");
  assert.equal(manifest.model.outputScale, 2);
  assert.equal(manifest.files.length, 2);
  assert.equal(
    manifest.executableSha256,
    createHash("sha256").update("fixture executable").digest("hex")
  );
});

test("Real-ESRGAN source and official model archive are immutable", () => {
  assert.match(REALESRGAN_SIDECAR_SOURCE.commit, /^[a-f0-9]{40}$/);
  assert.equal(REALESRGAN_SIDECAR_SOURCE.modelArchive.release, "v0.2.5.0");
  assert.equal(REALESRGAN_SIDECAR_SOURCE.modelArchive.sha256.length, 64);
  assert.deepEqual(
    REALESRGAN_SIDECAR_SOURCE.files.map((file) => file.name),
    ["realesr-animevideov3-x2.bin", "realesr-animevideov3-x2.param"]
  );
});
