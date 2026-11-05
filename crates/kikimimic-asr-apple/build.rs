//! Compiles swift/SpeechBridge.swift into a static library. SpeechAnalyzer is
//! only in the macOS 26 SDK, so this uses the system SDK even when SDKROOT
//! names an older one (the nix dev shell points SDKROOT and DEVELOPER_DIR at
//! apple-sdk 14.4).

use std::path::PathBuf;
use std::process::Command;

const BRIDGE: &str = "swift/SpeechBridge.swift";

fn xcrun(args: &[&str]) -> String {
    let out = Command::new("/usr/bin/xcrun")
        .env_remove("SDKROOT")
        .env_remove("DEVELOPER_DIR")
        .args(args)
        .output()
        .expect("run xcrun");
    assert!(
        out.status.success(),
        "xcrun {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn main() {
    println!("cargo:rerun-if-changed={BRIDGE}");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "arm64",
        other => panic!("unsupported arch {other}"),
    };
    // The app's LSMinimumSystemVersion; the bridge checks for macOS 26 itself.
    let target = format!("{arch}-apple-macosx14.2");
    let sdk = xcrun(&["--sdk", "macosx", "--show-sdk-path"]);
    let lib = out.join("libkkm_speech.a");
    xcrun(&[
        "swiftc",
        "-parse-as-library",
        "-emit-library",
        "-static",
        "-O",
        "-swift-version",
        "5",
        "-module-name",
        "KkmSpeech",
        "-target",
        &target,
        "-sdk",
        &sdk,
        "-o",
        lib.to_str().unwrap(),
        BRIDGE,
    ]);
    let swiftc = PathBuf::from(xcrun(&["--find", "swiftc"]));
    let toolchain_lib = swiftc
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("lib/swift/macosx");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=kkm_speech");
    // The SDK first: the toolchain directory also holds back-deployment
    // dylibs with @rpath install names, which the app could not load. It is
    // only searched for the static compatibility libraries.
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-search=native={}", toolchain_lib.display());
    println!("cargo:rustc-link-search=framework={sdk}/System/Library/Frameworks");
    for framework in ["Speech", "AVFoundation", "CoreMedia", "Foundation"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
