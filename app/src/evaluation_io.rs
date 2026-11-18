//! Local adapters for the Settings evaluation panel. Never writes source data.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use kikimimic_core::diarization_evaluation::{self as evaluation, Options, Report};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub schema: u32,
    pub recording_dir: PathBuf,
    pub reference_path: PathBuf,
    pub hypothesis_path: PathBuf,
    pub report: Report,
}

fn read_rttm(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    ensure!(file.metadata()?.is_file(), "RTTM must be a regular file");
    let mut text = String::new();
    file.take(evaluation::MAX_INPUT_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("Cannot read UTF-8 RTTM {}", path.display()))?;
    ensure!(
        text.len() <= evaluation::MAX_INPUT_BYTES,
        "RTTM exceeds 8 MiB"
    );
    Ok(text)
}

pub fn run(
    recording_dir: PathBuf,
    reference_path: PathBuf,
    hypothesis_path: PathBuf,
    options: Options,
) -> Result<Artifact> {
    let reference = read_rttm(&reference_path)?;
    let hypothesis = read_rttm(&hypothesis_path)?;
    let report = evaluation::evaluate(&reference, &hypothesis, options)?;
    Ok(Artifact {
        schema: 1,
        recording_dir,
        reference_path,
        hypothesis_path,
        report,
    })
}

/// Fresh destination on every explicit save. Validate the temporary JSON and
/// publish via hard link (atomic, no replacement), then remove the temporary.
pub fn save(artifact: &Artifact) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = artifact.recording_dir.join("postprocess/evaluations");
    fs::create_dir_all(&dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = format!(
        "evaluation-{stamp}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let target = dir.join(format!("{name}.json"));
    let temporary = dir.join(format!(".{name}.tmp"));
    let bytes = serde_json::to_vec_pretty(artifact)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        let checked: Artifact = serde_json::from_slice(&fs::read(&temporary)?)?;
        ensure!(checked == *artifact, "Evaluation JSON validation failed");
        fs::hard_link(&temporary, &target)
            .context("Cannot publish evaluation; existing results were preserved")?;
        Ok(target)
    })();
    drop(file);
    let _ = fs::remove_file(&temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use kikimimic_core::diarization_evaluation::Mode;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "kkm-evaluation-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture(scratch: &Scratch) -> Artifact {
        let r = scratch.0.join("human.rttm");
        let h = scratch.0.join("postprocess/nemotron.rttm");
        fs::create_dir_all(h.parent().unwrap()).unwrap();
        fs::write(&r, "SPEAKER audio 1 0 3 <NA> <NA> A <NA> <NA>\n").unwrap();
        fs::write(&h, "SPEAKER audio 1 0 2 <NA> <NA> x <NA> <NA>\n").unwrap();
        fs::write(scratch.0.join("mix.wav"), b"original audio").unwrap();
        fs::write(scratch.0.join("transcript.jsonl"), b"original transcript").unwrap();
        run(
            scratch.0.clone(),
            r,
            h,
            Options {
                mode: Mode::HumanReference,
                recording_duration: 5.,
                uem: vec![],
                collar: 0.,
                skip_overlap: false,
            },
        )
        .unwrap()
    }
    #[test]
    fn saves_reloadable_hashes_and_conditions_without_changing_sources() {
        let scratch = Scratch::new();
        let artifact = fixture(&scratch);
        let before_r = fs::read(&artifact.reference_path).unwrap();
        let before_h = fs::read(&artifact.hypothesis_path).unwrap();
        let first = save(&artifact).unwrap();
        let first_bytes = fs::read(&first).unwrap();
        let restored: Artifact = serde_json::from_slice(&first_bytes).unwrap();
        assert_eq!(restored, artifact);
        let second = save(&artifact).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), first_bytes);
        assert_eq!(fs::read(&artifact.reference_path).unwrap(), before_r);
        assert_eq!(fs::read(&artifact.hypothesis_path).unwrap(), before_h);
        assert_eq!(
            fs::read(scratch.0.join("mix.wav")).unwrap(),
            b"original audio"
        );
        assert_eq!(
            fs::read(scratch.0.join("transcript.jsonl")).unwrap(),
            b"original transcript"
        );
        assert_eq!(fs::read_dir(first.parent().unwrap()).unwrap().count(), 2);
    }
    #[test]
    fn missing_invalid_and_save_failure_are_errors_and_do_not_destroy_completed_results() {
        let scratch = Scratch::new();
        let mut artifact = fixture(&scratch);
        let saved = save(&artifact).unwrap();
        let bytes = fs::read(&saved).unwrap();
        let options = artifact.report.options.clone();
        assert!(
            run(
                scratch.0.clone(),
                scratch.0.join("missing.rttm"),
                artifact.hypothesis_path.clone(),
                options.clone()
            )
            .is_err()
        );
        fs::write(&artifact.reference_path, b"broken").unwrap();
        assert!(
            run(
                scratch.0.clone(),
                artifact.reference_path.clone(),
                artifact.hypothesis_path.clone(),
                options
            )
            .is_err()
        );
        let blocked = scratch.0.join("blocked");
        fs::write(&blocked, b"file not directory").unwrap();
        artifact.recording_dir = blocked;
        assert!(save(&artifact).is_err());
        assert_eq!(fs::read(saved).unwrap(), bytes);
    }
}
