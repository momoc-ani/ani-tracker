import assert from "node:assert/strict";
import test from "node:test";

import { compileOnlyResourceDirectories } from "./prepare-tauri-gate-resources.mjs";

test("返回 Windows 分架构编译资源目录", () => {
  assert.deepEqual(compileOnlyResourceDirectories("win32", "x64"), [
    "out/ffmpeg/win32-x64",
    "out/libmpv/win32-x64",
    "out/qbittorrent/win32-x64",
    "out/torrent-core/win32-x64"
  ]);
});

test("返回 macOS 通用编译资源目录", () => {
  assert.deepEqual(compileOnlyResourceDirectories("darwin", "arm64"), [
    "out/ffmpeg",
    "out/qbittorrent",
    "out/torrent-core"
  ]);
});

test("返回 Linux 分架构编译资源目录且不伪造系统 FFmpeg", () => {
  assert.deepEqual(compileOnlyResourceDirectories("linux", "x64"), [
    "out/qbittorrent/linux-x64",
    "out/torrent-core/linux-x64"
  ]);
});

test("拒绝未支持的平台", () => {
  assert.throws(
    () => compileOnlyResourceDirectories("android", "arm64"),
    /unsupported target: android-arm64/
  );
});
