//! The core must stay free of devices, GPUs, UI and the network (ADR-120001),
//! or it stops building and testing on every platform.

use std::process::Command;

const DENIED: &[&str] = &[
    "cpal",
    "whisper-rs",
    "llama-cpp-2",
    "gpui",
    "tauri",
    "reqwest",
    "ureq",
    "hyper",
    "tungstenite",
    "objc2",
    "windows",
];

#[test]
fn the_core_depends_on_nothing_platform_bound() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "tree",
            "-p",
            "kikimimic-core",
            "-e",
            "normal",
            "--prefix",
            "none",
            "--offline",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8(out.stdout).expect("utf-8");
    let found: Vec<&str> = tree
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| DENIED.contains(name))
        .collect();
    assert!(found.is_empty(), "kikimimic-core depends on {found:?}");
}
