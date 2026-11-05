use std::process::Command;

// ggml-metal uses `@available(macOS 15.0, ...)`, which clang lowers to
// `__isPlatformVersionAtLeast` from compiler-rt. rustc links with
// `-nodefaultlibs`, so libclang_rt.osx.a is dropped and the cdylib fails to
// link; add it back explicitly.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = Command::new("xcrun")
        .args(["clang", "--print-resource-dir"])
        .output()
        .expect("xcrun clang --print-resource-dir");
    let dir = String::from_utf8(out.stdout).expect("utf-8 resource dir");
    println!("cargo:rustc-link-search=native={}/lib/darwin", dir.trim());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
