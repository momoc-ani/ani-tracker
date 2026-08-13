import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [
  packageSource,
  mobileGate,
  androidRelease,
  iosRelease,
  desktopRelease,
  androidGradle,
  androidRootGradle,
  androidPlayerGradle,
  androidGradleProperties,
  iosTorrentScript,
  iosTorrentCmake,
  iosTorrentModuleMap,
  iosFrameworkVerifier,
  iosDeploymentTargetSync,
  iosConfigSource,
  playerPackage,
  torrentPackage,
  playerBuild,
  torrentBuild,
  playerError,
  mobileError,
  playerController,
  playerPlugin,
  playerScreen,
  windowCommands,
  tauriVite,
  mobilePluginCargo,
  mobilePluginGradle,
  mobilePluginAndroid,
  androidCertificateVerifier,
  androidRevocationPolicy,
  androidRevocationPolicyTest,
  mobilePluginRust,
  mobileConsumerRules,
  sourceNetwork,
  torrentRuntime,
  androidTorrentService,
  androidTorrentRecovery,
  iosTorrentPlugin
] = await Promise.all([
  readFile("package.json", "utf8"),
  readFile(".github/workflows/tauri-mobile.yml", "utf8"),
  readFile(".github/workflows/tauri-release-android.yml", "utf8"),
  readFile(".github/workflows/tauri-release-ios.yml", "utf8"),
  readFile(".github/workflows/tauri-release-desktop.yml", "utf8"),
  readFile("src-tauri/gen/android/app/build.gradle.kts", "utf8"),
  readFile("src-tauri/gen/android/build.gradle.kts", "utf8"),
  readFile("crates/tauri-plugin-ani-player/android/build.gradle.kts", "utf8"),
  readFile("src-tauri/gen/android/gradle.properties", "utf8"),
  readFile("scripts/prepare-ios-torrent-core.sh", "utf8"),
  readFile("native/torrent-core/CMakeLists.txt", "utf8"),
  readFile("native/torrent-core/apple/AniTorrentCore.modulemap", "utf8"),
  readFile("scripts/verify-ios-xcframework.sh", "utf8"),
  readFile("scripts/sync-tauri-ios-deployment-target.mjs", "utf8"),
  readFile("src-tauri/tauri.ios.conf.json", "utf8"),
  readFile("crates/tauri-plugin-ani-player/ios/Package.swift", "utf8"),
  readFile("crates/tauri-plugin-ani-torrent/ios/Package.swift", "utf8"),
  readFile("crates/tauri-plugin-ani-player/build.rs", "utf8"),
  readFile("crates/tauri-plugin-ani-torrent/build.rs", "utf8"),
  readFile("crates/tauri-plugin-ani-player/src/error.rs", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/src/error.rs", "utf8"),
  readFile("crates/tauri-plugin-ani-player/ios/Sources/MobileVLCPlayerController.swift", "utf8"),
  readFile("crates/tauri-plugin-ani-player/ios/Sources/AniPlayerPlugin.swift", "utf8"),
  readFile("crates/tauri-plugin-ani-player/ios/Sources/PlayerScreen.swift", "utf8"),
  readFile("src-tauri/src/commands/window.rs", "utf8"),
  readFile("vite.tauri.config.ts", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/Cargo.toml", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/build.gradle.kts", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/src/main/java/dev/ani/tracker/mobile/AniMobilePlugin.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/src/main/java/org/rustls/platformverifier/CertificateVerifier.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/src/main/java/org/rustls/platformverifier/AndroidCertificateRevocationPolicy.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/src/test/java/org/rustls/platformverifier/AndroidCertificateRevocationPolicyTest.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/src/android.rs", "utf8"),
  readFile("crates/tauri-plugin-ani-mobile/android/consumer-rules.pro", "utf8"),
  readFile("crates/ani-sources/src/lib.rs", "utf8"),
  readFile("native/torrent-core/src/torrent_core_runtime.cpp", "utf8"),
  readFile("crates/tauri-plugin-ani-torrent/android/src/main/java/dev/ani/tracker/torrent/TorrentDownloadService.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-torrent/android/src/main/java/dev/ani/tracker/torrent/TorrentRecoveryWorker.kt", "utf8"),
  readFile("crates/tauri-plugin-ani-torrent/ios/Sources/AniTorrentPlugin.swift", "utf8")
]);

const packageJson = JSON.parse(packageSource);
const iosConfig = JSON.parse(iosConfigSource);

test("本地 Android Debug 与 Release 打包均固定 ARM64", () => {
  assert.match(
    packageJson.scripts["package:tauri:android:debug"],
    /tauri android build --target aarch64 --debug --apk --ci/
  );
  assert.match(
    packageJson.scripts["package:tauri:android"],
    /tauri android build --target aarch64 --apk --ci/
  );
});

test("移动持续门禁真实编译两端产物并检查原生与包边界", () => {
  assert.match(mobileGate, /pull_request:/);
  assert.match(mobileGate, /tauri android build --target aarch64 --debug --apk --ci/);
  assert.match(mobileGate, /:tauri-plugin-ani-mobile:testDebugUnitTest/);
  assert.match(mobileGate, /verify:tauri:android-package/);
  assert.match(mobileGate, /tauri ios build --target aarch64 --ci --no-sign/);
  assert.match(mobileGate, /IPHONEOS_DEPLOYMENT_TARGET: "15\.0"/);
  assert.match(mobileGate, /key: mobile-ios15-arm64/);
  assert.match(mobileGate, /build-for-testing/);
  assert.match(mobileGate, /verify:tauri:ios-package -- --require-unsigned/);
  assert.match(mobileGate, /plutil -extract MinimumOSVersion raw/);
});

test("Android 正式发布同时强制长期 JKS、自签校验与原生单测", () => {
  assert.match(androidRelease, /Missing required Android self-signing secret/);
  assert.match(androidRelease, /tauri android build --target aarch64 --apk --ci/);
  assert.match(androidRelease, /:tauri-plugin-ani-mobile:testReleaseUnitTest/);
  assert.match(androidRelease, /apksigner[^\n]*verify --verbose --print-certs/);
  assert.match(androidGradle, /taskNames\.any \{ it\.contains\("release", ignoreCase = true\) \}/);
  assert.match(androidGradle, /Android Release 必须配置 ANI_ANDROID_KEYSTORE_PATH/);
});

test("Android 播放器固定已验证的 Kotlin Compose 编译器与 JVM 资源边界", () => {
  assert.match(androidRootGradle, /kotlin-gradle-plugin:2\.0\.21/);
  assert.match(androidRootGradle, /compose-compiler-gradle-plugin:2\.0\.21/);
  assert.match(androidPlayerGradle, /id\("org\.jetbrains\.kotlin\.plugin\.compose"\)/);
  assert.doesNotMatch(androidPlayerGradle, /kotlinCompilerExtensionVersion/);
  assert.match(androidGradleProperties, /org\.gradle\.jvmargs=-Xmx4g/);
  assert.match(androidGradle, /sourceCompatibility = JavaVersion\.VERSION_17/);
  assert.match(androidGradle, /jvmTarget = "17"/);
  assert.match(mobileGate, /Diagnose Android Kotlin compiler failure/);
  assert.match(mobileGate, /steps\.android_build\.outcome == 'failure'/);
  assert.match(mobileGate, /:tauri-plugin-ani-player:compileDebugKotlin/);
  assert.match(mobileGate, /kotlin\.compiler\.execution\.strategy=in-process/);
});

test("iOS 正式发布保持未签名 IPA 与用户重签边界", () => {
  assert.match(iosRelease, /tauri ios build --target aarch64 --ci --no-sign/);
  assert.match(iosRelease, /package-unsigned-ios-ipa\.sh/);
  assert.match(iosRelease, /IPHONEOS_DEPLOYMENT_TARGET: "15\.0"/);
  assert.match(iosRelease, /verify:tauri:ios-package -- --require-unsigned/);
  assert.match(iosRelease, /plutil -extract MinimumOSVersion raw/);
  assert.doesNotMatch(iosRelease, /APPLE_(?:CERTIFICATE|PROVISIONING_PROFILE)/);
});

test("iOS 15 保持真机兼容并仅在 iOS 16 强制切换方向", () => {
  assert.equal(iosConfig.bundle.iOS.minimumSystemVersion, "15.0");
  assert.match(packageJson.scripts["init:tauri:ios"], /sync:tauri:ios-target/);
  assert.match(packageJson.scripts["package:tauri:ios"], /sync:tauri:ios-target/);
  assert.match(iosDeploymentTargetSync, /IPHONEOS_DEPLOYMENT_TARGET/);
  assert.match(playerPackage, /\.iOS\(\.v15\)/);
  assert.match(playerPlugin, /if #available\(iOS 16\.0, \*\)/);
  assert.match(playerScreen, /if #available\(iOS 16\.0, \*\)/);
});

test("三端正式发布使用专用令牌与 Node 24 Release Action", () => {
  const releaseWorkflows = [androidRelease, iosRelease, desktopRelease];
  for (const releaseWorkflow of releaseWorkflows) {
    assert.match(
      releaseWorkflow,
      /softprops\/action-gh-release@5018f9ec04d67dca7353bf3f40a0933e8d7ddf24/
    );
    assert.match(releaseWorkflow, /token: \$\{\{ secrets\.RELEASE_TOKEN \}\}/);
    assert.match(releaseWorkflow, /if \[\[ -z "\$\{RELEASE_TOKEN\}" \]\]; then/);
    assert.match(releaseWorkflow, /Missing required GitHub release secret: RELEASE_TOKEN/);
    assert.doesNotMatch(releaseWorkflow, /ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION/);
  }
});

test("三端同版本发布串行执行并在成功后覆盖资产", () => {
  const releaseWorkflows = [androidRelease, iosRelease, desktopRelease];
  for (const releaseWorkflow of releaseWorkflows) {
    assert.match(releaseWorkflow, /concurrency:[\s\S]*?group: ani-release-\$\{\{ inputs\.release_version \|\| github\.ref_name \}\}[\s\S]*?cancel-in-progress: false/);
    assert.match(releaseWorkflow, /overwrite_files: true/);
    assert.match(releaseWorkflow, /fail_on_unmatched_files: true/);
    assert.doesNotMatch(releaseWorkflow, /gh release delete|git push .*--delete|git tag -d|DELETE .*releases/);
  }
});

test("iOS torrent-core 隔离设备与模拟器依赖并在构建前校验", () => {
  assert.match(iosTorrentScript, /printf '%s\/device' "\$\{dependency_root\}"/);
  assert.match(iosTorrentScript, /printf '%s\/simulator' "\$\{dependency_root\}"/);
  assert.match(iosTorrentScript, /validate_dependencies "\$\{triplet\}"/);
  assert.match(iosTorrentScript, /package_framework_slice "\$\{build_root\}\/Release-\$\{sdk\}\/AniTorrentCore\.framework"/);
  assert.match(iosTorrentScript, /Headers\/AniTorrentCore\.h/);
  assert.match(iosTorrentScript, /Modules\/module\.modulemap/);
  assert.match(iosTorrentScript, /include\/boost\/version\.hpp/);
  assert.match(iosTorrentScript, /lib\/libcrypto\.a/);
});

test("iOS 原生插件校验 XCFramework 模块并显式提供切片搜索路径", () => {
  assert.match(iosTorrentCmake, /XCODE_ATTRIBUTE_MODULEMAP_FILE/);
  assert.match(iosTorrentCmake, /XCODE_ATTRIBUTE_CLANG_ENABLE_MODULES "YES"/);
  assert.match(iosTorrentCmake, /TARGET_BUNDLE_DIR:AniTorrentCore>\/Headers\/AniTorrentCore\.h/);
  assert.match(iosTorrentCmake, /TARGET_BUNDLE_DIR:AniTorrentCore>\/Modules\/module\.modulemap/);
  assert.match(iosTorrentModuleMap, /framework module AniTorrentCore/);
  assert.match(iosTorrentModuleMap, /umbrella header "AniTorrentCore\.h"/);
  assert.match(iosFrameworkVerifier, /Modules\/module\.modulemap/);
  assert.match(iosFrameworkVerifier, /--sdk "\$\{sdk\}" swiftc/);
  assert.match(iosFrameworkVerifier, /arm64-apple-ios\$\{deployment_target\}-simulator/);
  assert.match(playerPackage, /xcframeworkSearchFlags\(named: "MobileVLCKit"\)/);
  assert.match(torrentPackage, /xcframeworkSearchFlags\(named: "AniTorrentCore"\)/);
  assert.match(playerBuild, /link_ios_xcframework\("MobileVLCKit"\)/);
  assert.match(torrentBuild, /link_ios_xcframework\("AniTorrentCore"\)/);
  for (const buildScript of [playerBuild, torrentBuild]) {
    assert.match(buildScript, /plist::Value::from_file/);
    assert.match(buildScript, /cargo:rustc-link-search=framework=/);
    assert.match(buildScript, /cargo:rustc-link-lib=framework=\{framework_name\}/);
  }
  assert.match(playerBuild, /cargo:rustc-link-lib=framework=SwiftUI/);
  assert.deepEqual(iosConfig.bundle.iOS.frameworks, [
    "SwiftUI",
    "../crates/tauri-plugin-ani-player/ios/Frameworks/MobileVLCKit.xcframework",
    "../crates/tauri-plugin-ani-torrent/ios/Frameworks/AniTorrentCore.xcframework"
  ]);
});

test("移动窗口命令不会编译桌面最小化与最大化 API", () => {
  assert.match(windowCommands, /#\[cfg\(desktop\)\][\s\S]*?\.minimize\(\)/);
  assert.match(windowCommands, /#\[cfg\(desktop\)\][\s\S]*?\.unmaximize\(\)/);
  assert.match(windowCommands, /#\[cfg\(mobile\)\][\s\S]*?window_operation_unsupported/);
});

test("移动 Renderer 注入 Tauri 平台变量且不使用无效通配符", () => {
  assert.match(tauriVite, /envPrefix:\s*\["VITE_",\s*"TAURI_ENV_"\]/);
  assert.doesNotMatch(tauriVite, /TAURI_ENV_\*/);
});

test("Android HTTPS 使用系统证书验证器并在业务请求前初始化", () => {
  assert.match(mobilePluginCargo, /jni = \{ version = "0\.22\.4", default-features = false \}/);
  assert.match(mobilePluginCargo, /rustls-platform-verifier = "0\.7"/);
  assert.doesNotMatch(androidGradle, /rustls:rustls-platform-verifier|JsonSlurper|cargo[\s\S]*metadata/);
  assert.match(mobilePluginGradle, /buildConfigField\("boolean", "TEST", "false"\)/);
  assert.match(androidCertificateVerifier, /Derived from rustls-platform-verifier v0\.7\.0 and upstream PR #179/);
  assert.match(
    androidCertificateVerifier,
    /revocationChecker\.options = AndroidCertificateRevocationPolicy\.options\(\)/
  );
  for (const option of ["SOFT_FAIL", "ONLY_END_ENTITY", "PREFER_CRLS", "NO_FALLBACK"]) {
    assert.match(androidRevocationPolicy, new RegExp(`PKIXRevocationChecker\\.Option\\.${option}`));
    assert.match(androidRevocationPolicyTest, new RegExp(`PKIXRevocationChecker\\.Option\\.${option}`));
  }
  assert.match(mobilePluginAndroid, /initializeRustlsPlatformVerifier\(activity\.applicationContext\)/);
  assert.match(mobilePluginAndroid, /private external fun initializeRustlsPlatformVerifier\(context: Context\): Boolean/);
  assert.match(mobilePluginRust, /rustls_platform_verifier::android::init_with_env\(env, context\)/);
  assert.match(mobileConsumerRules, /org\.rustls\.platformverifier\.\*\*/);
});

test("来源网络失败日志仅保留定位所需的脱敏字段", () => {
  assert.match(sourceNetwork, /Rust 来源网络请求失败：source_id=\{\}, host=\{\}, elapsed_ms=\{\}, error_category=\{\}, failure_reason=\{\}/);
  assert.doesNotMatch(sourceNetwork, /Rust 来源网络请求失败[^\n]*error=\{\}/);
});

test("Android 与 iOS 下载核心统一执行移动网络会话和任务策略", () => {
  assert.match(torrentRuntime, /command\.method == "setNetworkPolicy"/);
  assert.match(torrentRuntime, /session_\.pause\(\)/);
  assert.match(torrentRuntime, /session_\.resume\(\)/);
  assert.match(torrentRuntime, /pause_tasks_for_network_policy\(\)/);
  assert.match(torrentRuntime, /resume_tasks_from_network_policy\(\)/);
  assert.match(torrentRuntime, /network_policy_paused_tasks_\.insert\(id\)/);
  assert.match(torrentRuntime, /network_policy_paused_tasks_\.erase\(id\)/);
  assert.match(torrentRuntime, /return "waiting_network"/);
  assert.match(torrentRuntime, /network_policy_blocked_ \? 0 : state\.upload_payload_rate/);
  assert.match(torrentRuntime, /result\.put\("networkPolicyBlocked", network_policy_blocked_\)/);
  assert.match(androidTorrentService, /ConnectivityManager\.NetworkCallback/);
  assert.match(androidTorrentService, /NetworkCapabilities\.TRANSPORT_CELLULAR/);
  assert.match(androidTorrentService, /isActiveNetworkMetered/);
  assert.match(androidTorrentService, /ACTIVE_STATUSES[\s\S]*?"waiting_network"/);
  assert.match(androidTorrentRecovery, /NetworkType\.UNMETERED/);
  assert.match(iosTorrentPlugin, /NWPathMonitor\(\)/);
  assert.match(iosTorrentPlugin, /path\.isExpensive \|\| path\.isConstrained/);
  assert.match(iosTorrentPlugin, /initialNetworkPolicyBlocked: blocked/);
});

test("移动播放器注册错误支持 Tauri PluginInvokeError", () => {
  assert.match(playerError, /PluginInvoke\(#\[from\] tauri::plugin::mobile::PluginInvokeError\)/);
});

test("移动平台插件在 Android 与 iOS 都支持 PluginInvokeError", () => {
  assert.match(mobileError, /#\[cfg\(mobile\)\][\s\S]*?PluginInvoke\(#\[from\] tauri::plugin::mobile::PluginInvokeError\)/);
  assert.doesNotMatch(mobileError, /#\[cfg\(target_os = "android"\)\]/);
});

test("iOS 播放器安全处理 MobileVLCKit 可选音频对象", () => {
  assert.match(playerController, /guard let audio = mediaPlayer\.audio else/);
  assert.match(playerController, /applyAudioSnapshot\(reason: "playing"\)/);
  assert.doesNotMatch(playerController, /mediaPlayer\.audio\.(?:volume|isMuted)/);
});

test("iOS 播放器画面比例解析显式返回可选枚举", () => {
  assert.match(playerPlugin, /case "default", "fit":\s*return \.automatic/);
  assert.match(playerPlugin, /case "16:9":\s*return \.widescreen/);
  assert.match(playerPlugin, /case "4:3":\s*return \.standard/);
  assert.match(playerPlugin, /default:\s*return nil/);
});

test("iOS 播放器横竖屏复用同一视频 Surface", () => {
  assert.equal(playerScreen.match(/VLCVideoSurface\(controller: controller\)/g)?.length, 1);
  assert.match(playerScreen, /VStack\(spacing: 0\) \{\s*videoStage\(/);
  assert.doesNotMatch(playerScreen, /if landscape \{\s*videoStage\(/);
});
