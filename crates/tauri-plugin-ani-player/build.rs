use std::{env, path::PathBuf};

const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .ios_path("ios")
        .build();

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        link_ios_xcframework("MobileVLCKit");
        println!("cargo:rustc-link-lib=framework=SwiftUI");
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_macos_mpv_surface();
    }
}

/// 编译只负责创建 NSOpenGLView/CGL drawable 的轻量 AppKit 桥。
fn build_macos_mpv_surface() {
    println!("cargo:rerun-if-changed=native/macos_mpv_surface.m");
    cc::Build::new()
        .file("native/macos_mpv_surface.m")
        .flag("-fno-objc-arc")
        .compile("ani_mpv_macos_surface");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=OpenGL");
}

/// 选择当前 iOS 目标对应的 XCFramework 切片，并传递给 Rust 最终链接器。
fn link_ios_xcframework(framework_name: &str) {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR 构建变量"));
    let framework_root = manifest_dir
        .join("ios/Frameworks")
        .join(format!("{framework_name}.xcframework"));
    let info_path = framework_root.join("Info.plist");
    println!("cargo:rerun-if-changed={}", info_path.display());

    let info = plist::Value::from_file(&info_path)
        .unwrap_or_else(|error| panic!("无法读取 {}: {error}", info_path.display()));
    let libraries = info
        .as_dictionary()
        .and_then(|root| root.get("AvailableLibraries"))
        .and_then(plist::Value::as_array)
        .unwrap_or_else(|| panic!("{} 缺少 AvailableLibraries", info_path.display()));

    let target = env::var("TARGET").expect("缺少 TARGET 构建变量");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("缺少目标架构构建变量");
    let xcframework_arch = match target_arch.as_str() {
        "aarch64" => "arm64",
        other => other,
    };
    let expects_simulator = target.ends_with("-sim") || target == "x86_64-apple-ios";

    let (identifier, library_path) = libraries
        .iter()
        .find_map(|library| {
            let library = library.as_dictionary()?;
            if library.get("SupportedPlatform")?.as_string()? != "ios" {
                return None;
            }
            let simulator = library
                .get("SupportedPlatformVariant")
                .and_then(plist::Value::as_string)
                == Some("simulator");
            if simulator != expects_simulator {
                return None;
            }
            let supports_arch = library
                .get("SupportedArchitectures")?
                .as_array()?
                .iter()
                .filter_map(plist::Value::as_string)
                .any(|arch| arch == xcframework_arch);
            if !supports_arch {
                return None;
            }
            Some((
                library.get("LibraryIdentifier")?.as_string()?.to_owned(),
                library.get("LibraryPath")?.as_string()?.to_owned(),
            ))
        })
        .unwrap_or_else(|| {
            panic!("{framework_name} 没有匹配 {target} 架构的 iOS XCFramework 切片")
        });

    let slice_root = framework_root.join(identifier);
    let framework_path = slice_root.join(library_path);
    if !framework_path.is_dir() {
        panic!("iOS framework 切片不存在: {}", framework_path.display());
    }

    println!("cargo:rustc-link-search=framework={}", slice_root.display());
    println!("cargo:rustc-link-lib=framework={framework_name}");
}
