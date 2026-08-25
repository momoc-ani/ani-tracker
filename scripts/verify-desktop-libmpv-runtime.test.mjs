import assert from "node:assert/strict";
import test from "node:test";

import {
  findMpvCore,
  parseArgs,
  runtimeDirectory
} from "./verify-desktop-libmpv-runtime.mjs";

test("按平台和架构定位桌面 libmpv 目录", () => {
  assert.equal(runtimeDirectory("out/libmpv", "darwin", "x64").endsWith("out/libmpv/darwin-x64"), true);
  assert.equal(runtimeDirectory("out/libmpv", "win32", "x64").endsWith("out/libmpv/win32-x64"), true);
});

test("识别 Windows 与 macOS libmpv 核心文件", () => {
  assert.equal(findMpvCore(["C:\\runtime\\mpv-2.dll"], "win32"), "C:\\runtime\\mpv-2.dll");
  assert.equal(findMpvCore(["/runtime/libmpv.2.dylib"], "darwin"), "/runtime/libmpv.2.dylib");
});

test("解析固定来源校验参数", () => {
  assert.deepEqual(parseArgs(["--platform", "darwin", "--arch", "x64", "--require-pinned"]), {
    platform: "darwin",
    arch: "x64",
    root: "out/libmpv",
    requirePinned: true
  });
  assert.throws(() => parseArgs(["--platform", "android"]), /unsupported platform/);
});
