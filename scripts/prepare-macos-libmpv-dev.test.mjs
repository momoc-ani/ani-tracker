import assert from "node:assert/strict";
import test from "node:test";

import {
  architectureScopedOtoolArgs,
  createSourceManifest,
  MACOS_LIBMPV,
  macosBinaryArchitecture,
  parseArgs,
  parseLipoArchitectures,
  parseRpathDependencies
} from "./prepare-macos-libmpv-dev.mjs";

test("macOS libmpv 使用固定 IINA 归档和摘要", () => {
  const manifest = createSourceManifest("darwin-x64", [
    "/runtime/libmpv.2.dylib",
    "/runtime/libavcodec.61.dylib"
  ]);
  assert.deepEqual(manifest, {
    source: "iina/iina",
    version: MACOS_LIBMPV.version,
    archive: MACOS_LIBMPV.archive,
    sha256: MACOS_LIBMPV.sha256,
    target: "darwin-x64",
    files: ["libavcodec.61.dylib", "libmpv.2.dylib"]
  });
});

test("macOS libmpv 参数拒绝未知架构并支持离线缓存", () => {
  assert.deepEqual(
    parseArgs(["--arch", "x64", "--cache", ".cache/custom", "--target", "out/custom", "--offline"]),
    {
      arch: "x64",
      cacheRoot: ".cache/custom",
      targetRoot: "out/custom",
      source: undefined,
      pinned: false,
      offline: true
    }
  );
  assert.equal(parseArgs(["--pinned"]).pinned, true);
  assert.throws(() => parseArgs(["--arch", "riscv64"]), /unsupported macOS architecture/);
  assert.throws(() => parseArgs(["--source"]), /--source requires a value/);
});

test("仅收集需要随包分发的 @rpath dylib 依赖", () => {
  assert.deepEqual(
    parseRpathDependencies(`
/runtime/libmpv.2.dylib:
\t@rpath/libavcodec.61.dylib (compatibility version 61.0.0, current version 61.3.100)
\t@loader_path/libass.9.dylib (compatibility version 13.0.0, current version 13.1.0)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1319.0.0)
\t/System/Library/Frameworks/IOKit.framework/Versions/A/IOKit (compatibility version 1.0.0)
`),
    ["libavcodec.61.dylib", "libass.9.dylib"]
  );
});

test("兼容 fat 与 non-fat Mach-O 的 lipo 架构输出", () => {
  assert.deepEqual(parseLipoArchitectures("Architectures in the fat file: lib.dylib are: x86_64 arm64\n"), [
    "x86_64",
    "arm64"
  ]);
  assert.deepEqual(parseLipoArchitectures("Non-fat file: lib.dylib is architecture: x86_64\n"), [
    "x86_64"
  ]);
});

test("按目标切片读取依赖以排除另一架构的私有 dylib", () => {
  assert.equal(macosBinaryArchitecture("x64"), "x86_64");
  assert.equal(macosBinaryArchitecture("arm64"), "arm64");
  assert.deepEqual(
    architectureScopedOtoolArgs("/runtime/libjxl.0.10.dylib", "arm64"),
    ["-arch", "arm64", "-L", "/runtime/libjxl.0.10.dylib"]
  );
  assert.throws(() => macosBinaryArchitecture("riscv64"), /unsupported macOS architecture/);
});
