import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  createSourceManifest,
  findRuntimeFiles,
  parseArgs,
  WINDOWS_LIBMPV
} from "./prepare-windows-libmpv-dev.mjs";

test("从 libmpv 所在目录收集并稳定排序 Windows DLL", async () => {
  const root = await mkdtemp(join(tmpdir(), "ani-libmpv-test-"));
  const runtimeDirectory = join(root, "mpv-runtime", "bin");
  try {
    await mkdir(runtimeDirectory, { recursive: true });
    await mkdir(join(root, "unrelated"));
    await Promise.all([
      writeFile(join(runtimeDirectory, "mpv-2.dll"), "mpv"),
      writeFile(join(runtimeDirectory, "avcodec-62.dll"), "codec"),
      writeFile(join(runtimeDirectory, "README.txt"), "ignored"),
      writeFile(join(root, "unrelated", "unused.dll"), "ignored")
    ]);

    const runtimeFiles = await findRuntimeFiles(root);
    assert.deepEqual(runtimeFiles.map((file) => file.split(/[\\/]/).at(-1)), [
      "avcodec-62.dll",
      "mpv-2.dll"
    ]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("SOURCE.json 记录固定来源和纯文件名", () => {
  const manifest = createSourceManifest("win32-x64", [
    "C:\\runtime\\mpv-2.dll",
    "C:\\runtime\\avcodec-62.dll"
  ]);

  assert.deepEqual(manifest, {
    source: "zhongfly/mpv-winbuild",
    version: WINDOWS_LIBMPV.version,
    archive: WINDOWS_LIBMPV.archive,
    sha256: WINDOWS_LIBMPV.sha256,
    target: "win32-x64",
    files: ["mpv-2.dll", "avcodec-62.dll"]
  });
});

test("解析 Windows libmpv 准备参数", () => {
  assert.deepEqual(
    parseArgs(["--arch", "x64", "--cache", ".cache/custom", "--target", "out/custom", "--offline"]),
    {
      arch: "x64",
      cacheRoot: ".cache/custom",
      targetRoot: "out/custom",
      offline: true
    }
  );
  assert.throws(() => parseArgs(["--arch"]), /--arch requires a value/);
  assert.throws(() => parseArgs(["--unknown"]), /Unknown argument: --unknown/);
});
