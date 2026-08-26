import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import test from "node:test";

const workflow = await readFile(".github/workflows/tauri-release-desktop.yml", "utf8");
const desktopWorkflow = await readFile(".github/workflows/tauri-desktop.yml", "utf8");
const torrentCorePrepare = await readFile("scripts/prepare-desktop-torrent-core-dev.mjs", "utf8");
const qbittorrentUnixBuild = await readFile("scripts/build-qbittorrent-nox-unix.sh", "utf8");
const rifeSidecarPrepare = await readFile("scripts/prepare-rife-model-sidecar.mjs", "utf8");
const realesrganSidecarPrepare = await readFile("scripts/prepare-realesrgan-model-sidecar.mjs", "utf8");
const rifeSidecarCmake = await readFile("native/rife-model-sidecar/CMakeLists.txt", "utf8");
const realesrganSidecarCmake = await readFile("native/realesrgan-model-sidecar/CMakeLists.txt", "utf8");
const windowsSigningImport = await readFile("scripts/import-windows-signing-certificate.ps1", "utf8");
const windowsSignatureVerification = await readFile("scripts/verify-windows-self-signature.ps1", "utf8");
const windowsConfig = JSON.parse(await readFile("src-tauri/tauri.windows.conf.json", "utf8"));
const macosConfig = JSON.parse(await readFile("src-tauri/tauri.macos.conf.json", "utf8"));
const linuxConfig = JSON.parse(await readFile("src-tauri/tauri.linux.conf.json", "utf8"));

test("桌面原生依赖统一使用 libmpv 且不再准备 VLC", () => {
  assert.match(workflow, /name: Prepare Windows libmpv[\s\S]*?if: matrix\.platform == 'win32'[\s\S]*?shell: pwsh/);
  assert.match(workflow, /pnpm run prepare:tauri:win-libmpv/);
  assert.match(desktopWorkflow, /name: Prepare Windows Tauri libmpv runtime[\s\S]*?pnpm run prepare:tauri:win-libmpv/);
  assert.match(desktopWorkflow, /Install Linux Tauri dependencies[\s\S]*?libmpv1/);
  assert.match(workflow, /Install Linux desktop dependencies[\s\S]*?libmpv1/);
  assert.match(workflow, /name: Prepare macOS libmpv[\s\S]*?if: matrix\.platform == 'darwin'[\s\S]*?--pinned/);
  assert.match(desktopWorkflow, /name: Prepare macOS Tauri libmpv runtime[\s\S]*?--pinned/);
  assert.match(workflow, /name: Verify desktop libmpv runtime[\s\S]*?--require-pinned/);
  assert.match(desktopWorkflow, /name: Verify desktop libmpv runtime[\s\S]*?--require-pinned/);
  assert.doesNotMatch(workflow, /prepare:tauri:.*libvlc|\bvlc\b/i);
  assert.doesNotMatch(desktopWorkflow, /prepare:tauri:.*libvlc|\bvlc\b/i);
  assert.equal(windowsConfig.bundle.resources["../out/libmpv/win32-x64/"], "libmpv/win32-x64/");
  assert.equal(macosConfig.bundle.resources["../out/libmpv/"], "libmpv/");
  assert.equal(linuxConfig.bundle.linux.deb.depends.includes("libmpv1"), true);
  assert.equal(linuxConfig.bundle.linux.deb.depends.some((value) => /vlc/i.test(value)), false);
  assert.match(workflow, /name: Build Windows torrent-core[\s\S]*?shell: pwsh/);
  assert.match(workflow, /name: Build macOS torrent-core[\s\S]*?shell: bash/);
  assert.match(workflow, /name: Build Linux torrent-core[\s\S]*?shell: bash/);
  assert.match(workflow, /name: Build Windows managed qBittorrent[\s\S]*?shell: pwsh/);
  assert.match(workflow, /name: Build macOS managed qBittorrent[\s\S]*?shell: bash/);
  assert.match(workflow, /name: Build Linux managed qBittorrent[\s\S]*?shell: bash/);
});

test("RIFE sidecar 使用完整 Vulkan SDK 构建校验后再进入三平台安装包", () => {
  const prepareIndex = workflow.indexOf("name: Build and verify RIFE model sidecar");
  const windowsBuildIndex = workflow.indexOf("name: Build signed Windows installers");
  const macosBuildIndex = workflow.indexOf("name: Build self-signed macOS app");
  const linuxBuildIndex = workflow.indexOf("name: Build Linux bundles");
  assert.match(workflow, /name: Install pinned Vulkan SDK[\s\S]*?uses: humbletim\/install-vulkan-sdk@v1\.2[\s\S]*?version: 1\.3\.296\.0[\s\S]*?cache: true/);
  assert.doesNotMatch(workflow, /humbletim\/setup-vulkan-sdk/);
  assert.match(workflow, /pnpm run prepare:rife-sidecar[\s\S]*?pnpm run verify:rife-sidecar/);
  assert.ok(prepareIndex >= 0 && prepareIndex < windowsBuildIndex);
  assert.ok(prepareIndex < macosBuildIndex && prepareIndex < linuxBuildIndex);
  assert.equal(
    windowsConfig.bundle.resources["../out/model-sidecar/win32-x64/"],
    "model-sidecar/win32-x64/"
  );
  assert.equal(macosConfig.bundle.resources["../out/model-sidecar/"], "model-sidecar/");
  assert.equal(
    linuxConfig.bundle.resources["../out/model-sidecar/linux-x64/"],
    "model-sidecar/linux-x64/"
  );
});

test("模型 sidecar 在 CI 中使用 HTTPS 子模块并统一 MSVC 静态运行库", () => {
  for (const source of [rifeSidecarPrepare, realesrganSidecarPrepare]) {
    assert.match(source, /git.*config.*url\.https:\/\/github\.com\/\.insteadOf/);
    assert.match(source, /git@github\.com:/);
    assert.match(source, /ssh:\/\/git@github\.com\//);
  }
  assert.match(rifeSidecarCmake, /CMAKE_MSVC_RUNTIME_LIBRARY[\s\S]*?MultiThreaded/);
  assert.match(realesrganSidecarCmake, /CMAKE_MSVC_RUNTIME_LIBRARY[\s\S]*?MultiThreaded/);
});

test("Real-ESRGAN sidecar 使用独立资源目录并在三平台打包前校验", () => {
  const prepareIndex = workflow.indexOf("name: Build and verify Real-ESRGAN model sidecar");
  const windowsBuildIndex = workflow.indexOf("name: Build signed Windows installers");
  const macosBuildIndex = workflow.indexOf("name: Build self-signed macOS app");
  const linuxBuildIndex = workflow.indexOf("name: Build Linux bundles");
  assert.match(
    workflow,
    /pnpm run prepare:realesrgan-sidecar[\s\S]*?pnpm run verify:realesrgan-sidecar/
  );
  assert.ok(prepareIndex >= 0 && prepareIndex < windowsBuildIndex);
  assert.ok(prepareIndex < macosBuildIndex && prepareIndex < linuxBuildIndex);
  assert.equal(
    windowsConfig.bundle.resources["../out/realesrgan-model-sidecar/win32-x64/"],
    "realesrgan-model-sidecar/win32-x64/"
  );
  assert.equal(
    macosConfig.bundle.resources["../out/realesrgan-model-sidecar/"],
    "realesrgan-model-sidecar/"
  );
  assert.equal(
    linuxConfig.bundle.resources["../out/realesrgan-model-sidecar/linux-x64/"],
    "realesrgan-model-sidecar/linux-x64/"
  );
});

test("桌面重发同一版本时保留旧 Release，等待全平台成功后覆盖资产", () => {
  assert.match(workflow, /concurrency:[\s\S]*?group: ani-release-\$\{\{ inputs\.release_version \|\| github\.ref_name \}\}[\s\S]*?cancel-in-progress: false/);
  assert.match(workflow, /publish:\n    needs: desktop/);
  assert.match(workflow, /overwrite_files: true/);
  assert.match(workflow, /fail_on_unmatched_files: true/);
  assert.doesNotMatch(workflow, /gh release delete|git push .*--delete|git tag -d|DELETE .*releases/);
});

test("macOS Intel 架构转换为 CMake 识别的 x86_64", () => {
  assert.match(torrentCorePrepare, /arch === "x64" \? "x86_64" : arch/);
  assert.match(torrentCorePrepare, /CMAKE_OSX_ARCHITECTURES=\$\{cmakeArchitecture\}/);
});

test("桌面发布强制 Windows 与 macOS 使用固定自签凭据", () => {
  assert.match(workflow, /required=\(WINDOWS_CERTIFICATE_BASE64 WINDOWS_CERTIFICATE_PASSWORD\)/);
  assert.match(workflow, /required=\(APPLE_CERTIFICATE_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY\)/);
  assert.match(workflow, /Missing required Windows self-signing secret/);
  assert.match(workflow, /Missing required macOS self-signing secret/);
});

test("Windows 自签证书仅导入私钥并限制执行时间", () => {
  const importStep = workflow.match(
    /name: Import Windows signing certificate[\s\S]*?(?=\n      - name: Import macOS self-signing certificate)/
  )?.[0];
  assert.ok(importStep);
  assert.match(importStep, /timeout-minutes: 2/);
  assert.match(importStep, /scripts\/import-windows-signing-certificate\.ps1/);
  assert.match(windowsSigningImport, /Import-PfxCertificate/);
  assert.match(windowsSigningImport, /Cert:\\CurrentUser\\My/);
  assert.match(windowsSigningImport, /\[windows-signing\] Windows 自签证书私钥导入完成/);
  assert.doesNotMatch(windowsSigningImport, /TrustedPeople|TrustedPublisher|StoreName\]::Root/);
  assert.doesNotMatch(windowsSigningImport, /certutil\.exe/);
  assert.doesNotMatch(windowsSigningImport, /\bImport-Certificate\b/);
  assert.match(workflow, /scripts\/verify-windows-self-signature\.ps1/);
  assert.match(windowsSignatureVerification, /SignatureStatus\]::UnknownError/);
  assert.match(windowsSignatureVerification, /X509ChainStatusFlags\]::UntrustedRoot/);
  assert.match(windowsSignatureVerification, /SignerCertificate\.Thumbprint/);
});

test("Windows 构建通过临时 JSON 文件传递动态签名配置", () => {
  const buildStep = workflow.match(
    /name: Build signed Windows installers[\s\S]*?(?=\n      - name: Build self-signed macOS app)/
  )?.[0];
  assert.ok(buildStep);
  assert.match(buildStep, /\$configPath = Join-Path \$env:RUNNER_TEMP "tauri-windows-release\.conf\.json"/);
  assert.match(
    buildStep,
    /\[IO\.File\]::WriteAllText\(\$configPath, \$config, \[Text\.UTF8Encoding\]::new\(\$false\)\)/
  );
  assert.match(buildStep, /--config "\$configPath"/);
  assert.doesNotMatch(buildStep, /--config\s+["']?\$config["']?(?:\s|$)/m);
});

test("Windows 自签证书可无交互签名且篡改文件会被拒绝", { skip: process.platform !== "win32" }, () => {
  const pwshProbe = spawnSync("pwsh", ["-NoProfile", "-Command", "exit 0"]);
  const hasPwsh = pwshProbe.error?.code !== "ENOENT";
  const powershellExecutable = hasPwsh ? "pwsh" : "powershell.exe";
  const powershellArguments = hasPwsh
    ? [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        "scripts/import-windows-signing-certificate.test.ps1"
      ]
    : [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        "$content = [IO.File]::ReadAllText('scripts/import-windows-signing-certificate.test.ps1', [Text.Encoding]::UTF8); & ([ScriptBlock]::Create($content))"
      ];
  const result = spawnSync(
    powershellExecutable,
    powershellArguments,
    {
      cwd: process.cwd(),
      encoding: "utf8",
      timeout: 30_000
    }
  );
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
});

test("macOS 发布通过临时钥匙串导入并信任自签 P12", () => {
  assert.match(workflow, /openssl pkcs12[\s\S]*?-clcerts -nokeys/);
  assert.doesNotMatch(workflow, /openssl x509[^\n]*-purpose/);
  assert.match(workflow, /certificate does not match APPLE_SIGNING_IDENTITY/);
  assert.match(workflow, /security create-keychain/);
  assert.match(workflow, /security set-key-partition-list/);
  assert.match(workflow, /current_keychains=\(\)[\s\S]*?"\$\{current_keychains\[@\]\}"/);
  assert.doesNotMatch(workflow, /security list-keychains[^\n]*\$\{current_keychains\}(?:\s|$)/);
  assert.match(workflow, /security add-trusted-cert[\s\S]*?trustRoot/);
  assert.match(workflow, /security find-identity -v -p codesigning/);
});

test("macOS 发布修复最终应用内的 qBittorrent 签名后再生成 DMG", () => {
  const appBuildIndex = workflow.indexOf("name: Build self-signed macOS app");
  const repairIndex = workflow.indexOf("name: Repair final macOS bundle signatures");
  const dmgBuildIndex = workflow.indexOf("name: Build macOS DMG from repaired app");
  assert.ok(appBuildIndex >= 0 && appBuildIndex < repairIndex && repairIndex < dmgBuildIndex);
  assert.doesNotMatch(workflow, /name: Sign staged macOS managed qBittorrent/);
  assert.match(workflow, /find out\/cargo-target\/release\/bundle\/macos[\s\S]*?-name '\*\.app'/);
  assert.match(workflow, /managed_app="\$\{final_app\}\/Contents\/Resources\/qbittorrent\/darwin-\$\{\{ matrix\.arch \}\}\/qbittorrent-nox\.app"/);
  assert.match(workflow, /rm -rf "\$\{framework\}\/Resources" "\$\{framework\}\/Versions\/Current" "\$\{framework\}\/_CodeSignature"/);
  assert.match(workflow, /ln -s A "\$\{framework\}\/Versions\/Current"/);
  assert.match(workflow, /ln -s "Versions\/Current\/\$\{framework_name\}" "\$\{framework\}\/\$\{framework_name\}"/);
  assert.match(workflow, /ln -s Versions\/Current\/Resources "\$\{framework\}\/Resources"/);
  assert.match(workflow, /find "\$\{managed_app\}\/Contents" -type f -name '\*\.dylib'/);
  assert.match(workflow, /find "\$\{managed_app\}\/Contents\/Frameworks" -type d -name '\*\.framework'/);
  assert.match(workflow, /--sign "\$\{APPLE_SIGNING_IDENTITY\}" "\$\{managed_executable\}"/);
  assert.match(workflow, /--sign "\$\{APPLE_SIGNING_IDENTITY\}" "\$\{managed_app\}"/);
  assert.match(workflow, /codesign --verify --deep --strict "\$\{managed_app\}"/);
  assert.match(workflow, /mpv_directory="\$\{final_app\}\/Contents\/Resources\/libmpv\/darwin-\$\{\{ matrix\.arch \}\}"/);
  assert.match(workflow, /find "\$\{mpv_directory\}" -type f -name '\*\.dylib'/);
  assert.match(workflow, /签名最终包 libmpv 动态库/);
  assert.match(workflow, /--sign "\$\{APPLE_SIGNING_IDENTITY\}" "\$\{final_app\}"/);
  assert.match(workflow, /codesign --verify --deep --strict "\$\{final_app\}"/);
  assert.match(workflow, /echo "ANI_FINAL_MACOS_APP=\$\{final_app\}" >> "\$\{GITHUB_ENV\}"/);
  assert.doesNotMatch(workflow, /tauri bundle --ci --bundles dmg/);
  assert.match(workflow, /find "\$\{dmg_dir\}" -maxdepth 1 -type f -name '\*\.dmg' -delete/);
  assert.match(workflow, /ditto "\$\{final_app\}" "\$\{staged_app\}"/);
  assert.match(workflow, /codesign --verify --deep --strict "\$\{staged_app\}"/);
  assert.match(workflow, /hdiutil create[\s\S]*?-srcfolder "\$\{dmg_source\}"[\s\S]*?-format UDZO/);
});

test("Unix qBittorrent 构建对 vcpkg 网络瞬时失败进行有限重试", () => {
  assert.match(qbittorrentUnixBuild, /local max_attempts=3/);
  assert.match(qbittorrentUnixBuild, /if "\$\{vcpkg_root\}\/vcpkg" install/);
  assert.match(qbittorrentUnixBuild, /retry_delay=5/);
  assert.match(qbittorrentUnixBuild, /retry_delay=15/);
  assert.match(qbittorrentUnixBuild, /return "\$\{exit_code\}"/);
});

test("macOS 应用和 DMG 内应用均拒绝 ad-hoc 并校验嵌入证书指纹", () => {
  assert.match(workflow, /codesign --verify --deep --strict/);
  assert.match(workflow, /Signature=adhoc/);
  assert.match(workflow, /codesign -d --extract-certificates/);
  assert.match(workflow, /actual_fingerprint[\s\S]*?expected_fingerprint/);
  assert.match(workflow, /hdiutil attach -nobrowse -readonly/);
  assert.match(workflow, /find "\$\{mount_dir\}" -type d -name '\*\.app'/);
  assert.match(workflow, /No macOS DMG found for embedded app signature verification/);
  assert.match(workflow, /ani-tracker-macos-self-signed\.pem/);
});

test("macOS 发布无论成功失败都会清理临时签名材料", () => {
  assert.match(workflow, /if: always\(\) && matrix\.platform == 'darwin'/);
  assert.match(workflow, /security delete-certificate/);
  assert.match(workflow, /security delete-keychain/);
});
