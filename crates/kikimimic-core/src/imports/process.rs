use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{atomic_json, audio, AudioDecoder, ImportEngines, ImportJob, ImportMode};
use crate::domain::Pcm16k;

#[derive(Serialize, Deserialize)]
struct Manifest {
    key: String,
    sources: Vec<PathBuf>,
    settings: crate::settings::Settings,
    artifacts: Vec<(String, String)>,
}

fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut sha = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
    }
    Ok(hex(&sha.finalize()))
}

fn key(job: &ImportJob) -> Result<String> {
    let sources = job
        .inputs
        .iter()
        .map(|p| Ok((hash(p)?, p.extension().map(|e| e.to_ascii_lowercase()))))
        .collect::<Result<Vec<_>>>()?;
    let bytes = serde_json::to_vec(&("kikimimic-import-v1-30s", job.mode, &job.settings, sources))?;
    Ok(hex(&Sha256::digest(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn valid(dir: &Path, key: &str) -> Result<bool> {
    let path = dir.join("import.json");
    if !path.is_file() {
        return Ok(false);
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
    if manifest.key != key {
        return Ok(false);
    }
    anyhow::ensure!(manifest.artifacts.len() == 3, "incomplete import manifest");
    for name in ["mic.wav", "mix.wav", "transcript.jsonl"] {
        let expected = manifest
            .artifacts
            .iter()
            .find(|(n, _)| n == name)
            .context("missing imported artifact")?;
        anyhow::ensure!(
            hash(&dir.join(name))? == expected.1,
            "completed import was modified: {}",
            dir.display()
        );
    }
    Ok(true)
}

pub(super) fn import(
    base: &Path,
    job: &ImportJob,
    decoder: &mut dyn AudioDecoder,
    engines: &mut dyn ImportEngines,
    mut prepared: impl FnMut(&str, &Path) -> Result<()>,
) -> Result<(PathBuf, bool)> {
    anyhow::ensure!(
        !job.inputs.is_empty() && job.inputs.iter().all(|p| p.is_absolute()),
        "import queue contains invalid input paths"
    );
    anyhow::ensure!(
        job.stamp.len() == 14 && job.stamp.bytes().all(|c| c.is_ascii_digit()),
        "import queue contains an invalid recording date"
    );
    if let (Some(identity), Some(destination)) = (&job.fingerprint, &job.output) {
        anyhow::ensure!(
            identity.len() == 64 && identity.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid persisted import fingerprint"
        );
        anyhow::ensure!(
            destination.parent() == Some(base)
                && destination.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .ends_with(&format!("-import-{identity}"))),
            "persisted import destination is outside its recording base"
        );
        if valid(destination, identity)? {
            return Ok((destination.clone(), true));
        }
    }
    // Validate join semantics before invoking a decoder/model or creating output.
    if job.mode == ImportMode::JoinWav {
        audio::preflight_join(&job.inputs)?;
    }
    let fingerprint = key(job)?;
    std::fs::create_dir_all(base)?;
    for entry in std::fs::read_dir(base)?.flatten() {
        let dir = entry.path();
        if dir
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(&fingerprint))
            && valid(&dir, &fingerprint)?
        {
            anyhow::ensure!(
                key(job)? == fingerprint,
                "input changed while checking completed import; retry the file"
            );
            prepared(&fingerprint, &dir)?;
            return Ok((dir, true));
        }
    }
    let destination = base.join(format!("{}-import-{fingerprint}", job.stamp));
    anyhow::ensure!(
        !destination.exists(),
        "import destination already exists; preserving it"
    );
    prepared(&fingerprint, &destination)?;
    let scratch = base.join(".imports");
    anyhow::ensure!(!scratch.is_symlink(), "import staging cannot be a symlink");
    std::fs::create_dir_all(&scratch)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stage = scratch.join(format!(
        "job-{}-{}-{}",
        job.id,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // Never reuse/delete a previous process's directory: it could still be
    // active, or the user may have selected one of its files as an input.
    std::fs::create_dir(&stage)?;
    let result = build(&stage, job, decoder, engines).and_then(|()| {
        // Detect replacements/edits during decoding before publishing a result
        // associated with the previous bytes.
        anyhow::ensure!(
            key(job)? == fingerprint,
            "input changed during import; retry the file"
        );
        let artifacts = ["mic.wav", "mix.wav", "transcript.jsonl"]
            .iter()
            .map(|n| Ok(((*n).into(), hash(&stage.join(n))?)))
            .collect::<Result<Vec<_>>>()?;
        atomic_json(
            &stage.join("import.json"),
            &Manifest {
                key: fingerprint,
                sources: job.inputs.clone(),
                settings: job.settings.clone(),
                artifacts,
            },
        )?;
        // Metadata is optional, the stable directory identifier remains intact.
        let title = job.inputs[0]
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect::<String>();
        atomic_json(
            &stage.join("metadata.json"),
            &serde_json::json!({ "title": title }),
        )?;
        for name in ["mic.wav", "mix.wav", "transcript.jsonl"] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(stage.join(name))?
                .sync_all()?;
        }
        std::fs::remove_dir_all(stage.join("decoded"))?;
        std::fs::rename(&stage, &destination).context("publish completed import")?;
        Ok((destination, false))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}

fn build(
    stage: &Path,
    job: &ImportJob,
    decoder: &mut dyn AudioDecoder,
    engines: &mut dyn ImportEngines,
) -> Result<()> {
    let decoded = stage.join("decoded");
    std::fs::create_dir(&decoded)?;
    let mut inputs = Vec::new();
    for (i, path) in job.inputs.iter().enumerate() {
        let temp = decoded.join(format!("{i}.wav"));
        inputs.push(decoder.decode(path, &temp)?);
    }
    let mic = stage.join("mic.wav");
    audio::normalize(&inputs, &mic)?;
    engines.prepare(&job.settings)?;
    std::fs::copy(&mic, stage.join("mix.wav"))?;
    let mut transcript = BufWriter::new(File::create(stage.join("transcript.jsonl"))?);
    let mut id = 0u64;
    audio::clips(&mic, |samples, offset| {
        let duration_ms = samples.len() as u64 * 1_000 / 16_000;
        // Exact digital silence needs no recognition; keeps empty recordings
        // useful and avoids hallucinated text on a completely silent clip.
        if samples.iter().all(|s| s.abs() < f32::EPSILON) {
            return Ok(());
        }
        let heard = engines.recognize(Pcm16k::assume_16k(samples), &job.settings)?;
        for utterance in heard {
            anyhow::ensure!(
                utterance.start.0 <= utterance.end.0 && utterance.end.0 <= duration_ms,
                "speech engine returned timestamps outside the input clip"
            );
            let text = job.settings.glossary.apply(&utterance.text);
            if text.trim().is_empty() {
                continue;
            }
            id += 1;
            let start = offset * 1_000 / 16_000 + utterance.start.0;
            let end = offset * 1_000 / 16_000 + utterance.end.0;
            writeln!(
                transcript,
                "{}",
                serde_json::json!({"type": "utterance", "id": id, "lane": "mic", "startMs": start, "endMs": end, "lang": utterance.lang.as_ref().map(|l| l.as_str()), "text": text})
            )?;
            if let Some(target) = job.settings.languages.target_for(utterance.lang.as_ref()) {
                let translated =
                    engines.translate(&text, utterance.lang.as_ref(), target, &job.settings)?;
                writeln!(
                    transcript,
                    "{}",
                    serde_json::json!({"type": "translation", "id": id, "lang": target.as_str(), "text": translated})
                )?;
            }
        }
        Ok(())
    })?;
    transcript.flush()?;
    Ok(())
}
