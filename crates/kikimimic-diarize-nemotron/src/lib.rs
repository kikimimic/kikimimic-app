//! Nemotron-3-Diarization (MLX) as a [`Diarizer`]: the bundled Python script
//! runs under `uv`, which fetches its dependencies and the model on first use.
//!
//! An optional extra (ADR-120000): a Homebrew install cannot assume `uv`, so
//! a missing `uv` is reported with how to get it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use kikimimic_core::engine::Diarizer;

const SCRIPT: &str = include_str!("../diarize_nemotron.py");

pub struct Nemotron {
    /// Where the script is written for `uv run --script`.
    work_dir: PathBuf,
}

impl Nemotron {
    pub fn new(work_dir: PathBuf) -> Self {
        Self { work_dir }
    }
}

/// `uv` from KKM_UV_BIN, PATH, or the usual install locations: an app
/// launched from Finder does not inherit the shell's PATH.
fn find_uv() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KKM_UV_BIN") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .chain(
            home.iter()
                .flat_map(|h| [h.join(".local/bin"), h.join(".cargo/bin")]),
        )
        .map(|dir| dir.join("uv"))
        .find(|p| p.is_file())
}

impl Diarizer for Nemotron {
    fn diarize(&mut self, mix_wav: &Path, rttm: &Path) -> anyhow::Result<()> {
        let uv = find_uv().context(
            "speaker labelling needs uv (https://docs.astral.sh/uv/): install it with \
             `brew install uv`, or set KKM_UV_BIN",
        )?;
        std::fs::create_dir_all(&self.work_dir)?;
        let script = self.work_dir.join("diarize_nemotron.py");
        std::fs::write(&script, SCRIPT)?;
        let out = Command::new(&uv)
            .args(["run", "--script"])
            .arg(&script)
            .arg(mix_wav)
            .arg(rttm)
            .output()
            .with_context(|| format!("run {}", uv.display()))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(rttm);
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "Nemotron failed ({}): {}",
                out.status,
                stderr.lines().last().unwrap_or("no error details")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_script_is_bundled() {
        assert!(super::SCRIPT.contains("Nemotron"));
    }
}
