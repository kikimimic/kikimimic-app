//! Manual speaker corrections, separate from automatic transcript evidence.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library::Line;

pub const FILE: &str = "annotations.json";

/// Exact evidence on the original recording clock. Numeric line IDs are not stable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub lane: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

impl From<&Line> for Anchor {
    fn from(line: &Line) -> Self {
        Self {
            lane: line.lane.clone(),
            start_ms: line.start_ms,
            end_ms: line.end_ms,
            text: line.text.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerName {
    pub source: String,
    pub speaker: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub anchor: Anchor,
    pub source: String,
    pub automatic_speaker: Option<String>,
    /// None explicitly leaves this utterance unassigned. Review is independent.
    pub speaker: Option<String>,
    pub reviewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotations {
    version: u32,
    pub names: Vec<SpeakerName>,
    pub corrections: Vec<Correction>,
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            version: 1,
            names: Vec::new(),
            corrections: Vec::new(),
        }
    }
}

/// Applied selection stays present even when `reviewed` is false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewState {
    pub speaker: Option<String>,
    pub reviewed: bool,
    pub needs_reconfirmation: bool,
    pub pending_speaker: Option<String>,
}

#[derive(Debug)]
pub struct Reconciliation {
    pub rows: Vec<ReviewState>,
    /// Indices into `Annotations::corrections`; never deleted automatically.
    pub unapplied: Vec<usize>,
    pub inactive_names: Vec<usize>,
}

impl Annotations {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == 1,
            "unsupported annotations version {}",
            self.version
        );
        let mut names = BTreeSet::new();
        for name in &self.names {
            valid_label(&name.speaker)?;
            valid_label(&name.name)?;
            anyhow::ensure!(!name.source.is_empty(), "missing speaker source");
            anyhow::ensure!(
                names.insert((&name.source, &name.speaker)),
                "duplicate speaker name"
            );
        }
        let mut anchors = BTreeSet::new();
        for correction in &self.corrections {
            anyhow::ensure!(
                anchors.insert(&correction.anchor),
                "duplicate correction anchor"
            );
            anyhow::ensure!(
                correction.anchor.end_ms >= correction.anchor.start_ms,
                "invalid correction range"
            );
            anyhow::ensure!(
                !correction.anchor.lane.is_empty() && !correction.source.is_empty(),
                "missing correction evidence"
            );
            if let Some(speaker) = &correction.speaker {
                valid_label(speaker)?;
            }
        }
        Ok(())
    }

    /// Empty name removes the current mapping; identical names keep separate IDs.
    pub fn rename(&mut self, source: &str, speaker: &str, name: &str) -> anyhow::Result<()> {
        valid_label(speaker)?;
        let name = name.trim();
        if !name.is_empty() {
            valid_label(name)?;
        }
        if name.is_empty() {
            self.names
                .retain(|n| n.source != source || n.speaker != speaker);
        } else if let Some(existing) = self
            .names
            .iter_mut()
            .find(|n| n.source == source && n.speaker == speaker)
        {
            existing.name = name.into();
        } else {
            self.names.push(SpeakerName {
                source: source.into(),
                speaker: speaker.into(),
                name: name.into(),
            });
        }
        Ok(())
    }

    pub fn display_name<'a>(&'a self, source: &str, speaker: &'a str) -> &'a str {
        self.names
            .iter()
            .find(|n| n.source == source && n.speaker == speaker)
            .map(|n| n.name.as_str())
            .unwrap_or(speaker)
    }

    /// Replace only the correction for this exact utterance. Explicitly calling
    /// this after a source change confirms the new namespace for this one row.
    pub fn correct(&mut self, line: &Line, source: &str, speaker: Option<String>, reviewed: bool) {
        let anchor = Anchor::from(line);
        let correction = Correction {
            anchor,
            source: source.into(),
            automatic_speaker: line.speaker.clone(),
            speaker,
            reviewed,
        };
        if let Some(existing) = self
            .corrections
            .iter_mut()
            .find(|c| c.anchor == correction.anchor)
        {
            *existing = correction;
        } else {
            self.corrections.push(correction);
        }
    }

    pub fn reconcile(&self, lines: &[Line], source: &str) -> Reconciliation {
        let mut counts = BTreeMap::new();
        for line in lines {
            *counts.entry(Anchor::from(line)).or_insert(0usize) += 1;
        }
        let mut applied = BTreeSet::new();
        let rows = lines
            .iter()
            .map(|line| {
                let anchor = Anchor::from(line);
                let correction = self
                    .corrections
                    .iter()
                    .enumerate()
                    .find(|(_, c)| c.anchor == anchor);
                if let Some((i, c)) = correction {
                    let valid = counts[&anchor] == 1
                        && c.source == source
                        && c.automatic_speaker == line.speaker;
                    if valid {
                        applied.insert(i);
                        return ReviewState {
                            speaker: c.speaker.clone(),
                            reviewed: c.reviewed,
                            needs_reconfirmation: false,
                            pending_speaker: None,
                        };
                    }
                    return ReviewState {
                        speaker: line.speaker.clone(),
                        reviewed: false,
                        needs_reconfirmation: true,
                        pending_speaker: c.speaker.clone(),
                    };
                }
                ReviewState {
                    speaker: line.speaker.clone(),
                    reviewed: false,
                    needs_reconfirmation: false,
                    pending_speaker: None,
                }
            })
            .collect();
        Reconciliation {
            rows,
            unapplied: (0..self.corrections.len())
                .filter(|i| !applied.contains(i))
                .collect(),
            inactive_names: self
                .names
                .iter()
                .enumerate()
                .filter(|(_, n)| n.source != source)
                .map(|(i, _)| i)
                .collect(),
        }
    }

    pub fn apply(&self, lines: &mut [Line], source: &str) -> Reconciliation {
        let report = self.reconcile(lines, source);
        for (line, state) in lines.iter_mut().zip(&report.rows) {
            line.speaker = state
                .speaker
                .as_deref()
                .map(|id| self.display_name(source, id).to_string());
        }
        report
    }
}

fn valid_label(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.chars().count() <= 100
            && !value.chars().any(char::is_control),
        "speaker/name must be 1–100 characters without control characters"
    );
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A new RTTM may reuse `speaker_0` for a different person. Never carry its name
/// or a selection across namespaces automatically. With no RTTM, fingerprint
/// the automatic evidence (excluding unstable numeric IDs).
pub fn source_fingerprint(dir: &Path, lines: &[Line]) -> anyhow::Result<String> {
    match std::fs::read(dir.join(crate::postprocess::RTTM)) {
        Ok(bytes) => Ok(format!("rttm:{}", digest(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut evidence = lines
                .iter()
                .map(|l| (Anchor::from(l), &l.speaker))
                .collect::<Vec<_>>();
            evidence.sort();
            Ok(format!(
                "transcript:{}",
                digest(&serde_json::to_vec(&evidence)?)
            ))
        }
        Err(e) => Err(e).context("read speaker namespace"),
    }
}

/// An optimistic file revision prevents a second editor from overwriting edits.
#[derive(Debug)]
pub struct AnnotationFile {
    pub annotations: Annotations,
    revision: Option<String>,
}

fn read_bytes(dir: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(dir.join(FILE)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("read annotations.json"),
    }
}

impl AnnotationFile {
    /// Malformed or unsupported sidecars are errors, never silently reset.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let bytes = read_bytes(dir)?;
        let annotations: Annotations = match &bytes {
            Some(bytes) => serde_json::from_slice(bytes)
                .context("invalid annotations.json; original transcript is unchanged")?,
            None => Annotations::default(),
        };
        annotations.validate()?;
        Ok(Self {
            annotations,
            revision: bytes.as_deref().map(digest),
        })
    }

    /// Validate, sync and atomically publish beside the original logs. Failed
    /// validation/conflicting edits/source changes leave the saved file intact.
    pub fn save(&mut self, dir: &Path, expected_source: &str) -> anyhow::Result<()> {
        self.annotations.validate()?;
        // Keep a stable lock inode: deleting it on unlock would let two
        // processes lock different inodes and pass the revision check together.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".annotations.lock"))?;
        lock.try_lock()
            .map_err(|e| anyhow::anyhow!("another speaker save is in progress: {e}"))?;
        let current = crate::library::read_raw(dir)?;
        anyhow::ensure!(
            source_fingerprint(dir, &current.lines)? == expected_source,
            "speaker evidence changed; reload before saving"
        );
        anyhow::ensure!(
            read_bytes(dir)?.as_deref().map(digest) == self.revision,
            "annotations changed on disk; reload before saving"
        );
        let bytes = serde_json::to_vec_pretty(&self.annotations)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = dir.join(format!(
            ".annotations-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            let validated: Annotations = serde_json::from_slice(&std::fs::read(&temporary)?)?;
            validated.validate()?;
            anyhow::ensure!(
                validated == self.annotations,
                "annotations temporary file mismatch"
            );
            anyhow::ensure!(
                read_bytes(dir)?.as_deref().map(digest) == self.revision,
                "annotations changed on disk; reload before saving"
            );
            let current = crate::library::read_raw(dir)?;
            anyhow::ensure!(
                source_fingerprint(dir, &current.lines)? == expected_source,
                "speaker evidence changed; reload before saving"
            );
            std::fs::rename(&temporary, dir.join(FILE)).context("publish annotations.json")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        self.revision = Some(digest(&bytes));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "kkm-annotations-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(dir.join("postprocess")).unwrap();
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            hound::WavWriter::create(dir.join("mic.wav"), spec)
                .unwrap()
                .finalize()
                .unwrap();
            std::fs::write(dir.join("transcript.jsonl"), concat!(
                "{\"type\":\"utterance\",\"id\":1,\"lane\":\"mic\",\"startMs\":100,\"endMs\":900,\"text\":\"first\",\"speaker\":\"speaker_0\"}\n",
                "{\"type\":\"translation\",\"id\":1,\"lang\":\"ja\",\"text\":\"最初\"}\n",
                "{\"type\":\"utterance\",\"id\":2,\"lane\":\"speaker\",\"startMs\":1000,\"endMs\":1800,\"text\":\"second\",\"speaker\":\"speaker_1\"}\n"
            )).unwrap();
            std::fs::write(dir.join(crate::postprocess::RTTM), "SPEAKER mix 1 0.1 0.8 <NA> <NA> speaker_0 <NA> <NA>\nSPEAKER mix 1 1.0 0.8 <NA> <NA> speaker_1 <NA> <NA>\n").unwrap();
            Self(dir)
        }
        fn raw(&self) -> Vec<Line> {
            crate::library::read_raw(&self.0).unwrap().lines
        }
        fn source(&self) -> String {
            source_fingerprint(&self.0, &self.raw()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saves_reloads_and_exports_without_touching_automatic_logs_or_other_rows() {
        let fixture = Fixture::new();
        let original = std::fs::read(fixture.0.join("transcript.jsonl")).unwrap();
        std::fs::write(fixture.0.join(crate::postprocess::TRANSCRIPT), &original).unwrap();
        let lines = fixture.raw();
        let source = fixture.source();
        let mut file = AnnotationFile::load(&fixture.0).unwrap();
        file.annotations.rename(&source, "speaker_0", "林").unwrap();
        file.annotations
            .rename(&source, "speaker_1", "佐藤")
            .unwrap();
        file.annotations
            .correct(&lines[0], &source, Some("speaker_1".into()), true);
        // Review is optional; clearing it must retain the selected person.
        file.annotations
            .correct(&lines[0], &source, Some("speaker_1".into()), false);
        file.save(&fixture.0, &source).unwrap();
        let loaded = AnnotationFile::load(&fixture.0).unwrap();
        let state = loaded.annotations.reconcile(&lines, &source);
        assert_eq!(state.rows[0].speaker.as_deref(), Some("speaker_1"));
        assert!(!state.rows[0].reviewed);
        assert!(state.unapplied.is_empty());
        let session = crate::library::read(&fixture.0).unwrap();
        assert_eq!(session.lines[0].speaker.as_deref(), Some("佐藤"));
        assert_eq!(session.lines[1].speaker.as_deref(), Some("佐藤"));
        assert_eq!(session.lines[1].text, "second");
        assert_eq!(loaded.annotations.corrections.len(), 1);
        assert!(crate::export::markdown(&session).contains("**[00:00] 佐藤:** first"));
        assert!(crate::export::srt(&session)
            .contains("00:00:00,100 --> 00:00:00,900\n佐藤: first\n最初"));
        assert_eq!(
            std::fs::read(fixture.0.join("transcript.jsonl")).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read(fixture.0.join(crate::postprocess::TRANSCRIPT)).unwrap(),
            original
        );
        assert_eq!(
            crate::library::read_raw(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("speaker_0")
        );
    }

    #[test]
    fn row_assignment_does_not_leak_to_another_row_of_the_same_automatic_speaker() {
        let fixture = Fixture::new();
        let mut lines = fixture.raw();
        lines[1].speaker = lines[0].speaker.clone();
        let mut doc = Annotations::default();
        doc.correct(
            &lines[0],
            &fixture.source(),
            Some("speaker_1".into()),
            false,
        );
        let report = doc.apply(&mut lines, &fixture.source());
        assert_eq!(lines[0].speaker.as_deref(), Some("speaker_1"));
        assert_eq!(lines[1].speaker.as_deref(), Some("speaker_0"));
        assert!(!report.rows[0].reviewed);
        assert!(!report.rows[1].reviewed);
    }

    #[test]
    fn saved_correction_survives_postprocess_id_reassignment_on_disk() {
        let fixture = Fixture::new();
        let lines = fixture.raw();
        let source = fixture.source();
        let mut file = AnnotationFile::load(&fixture.0).unwrap();
        file.annotations
            .correct(&lines[0], &source, Some("speaker_1".into()), true);
        file.save(&fixture.0, &source).unwrap();
        let generated = std::fs::read_to_string(fixture.0.join("transcript.jsonl"))
            .unwrap()
            .replace("\"id\":1", "\"id\":98");
        std::fs::write(fixture.0.join(crate::postprocess::TRANSCRIPT), generated).unwrap();
        let read = crate::library::read(&fixture.0).unwrap();
        assert_eq!(read.lines[0].id, 98);
        assert_eq!(read.lines[0].speaker.as_deref(), Some("speaker_1"));
        let loaded = AnnotationFile::load(&fixture.0).unwrap();
        assert!(
            loaded
                .annotations
                .reconcile(&fixture.raw(), &fixture.source())
                .rows[0]
                .reviewed
        );
    }

    #[test]
    fn renumbered_ids_match_but_changed_text_range_lane_and_duplicate_evidence_do_not() {
        let fixture = Fixture::new();
        let mut lines = fixture.raw();
        let source = fixture.source();
        let mut doc = Annotations::default();
        doc.correct(&lines[0], &source, Some("speaker_1".into()), true);
        lines[0].id = 77;
        assert!(doc.reconcile(&lines, &source).rows[0].reviewed);
        for changed in [
            Line {
                text: "recognized differently".into(),
                ..lines[0].clone()
            },
            Line {
                start_ms: 101,
                ..lines[0].clone()
            },
            Line {
                end_ms: 899,
                ..lines[0].clone()
            },
            Line {
                lane: "speaker".into(),
                ..lines[0].clone()
            },
        ] {
            let report = doc.reconcile(&[changed], &source);
            assert_eq!(report.unapplied, vec![0]);
            assert!(!report.rows[0].reviewed);
        }
        lines.push(lines[0].clone());
        let report = doc.reconcile(&lines, &source);
        assert_eq!(report.unapplied, vec![0]);
        assert!(report.rows[0].needs_reconfirmation);
        assert!(report.rows[2].needs_reconfirmation);
        assert_eq!(doc.corrections.len(), 1);
    }

    #[test]
    fn changed_diarization_namespace_never_applies_old_names_or_assignments() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let lines = fixture.raw();
        let mut file = AnnotationFile::load(&fixture.0).unwrap();
        file.annotations.rename(&source, "speaker_0", "林").unwrap();
        file.annotations
            .correct(&lines[0], &source, Some("speaker_1".into()), true);
        file.save(&fixture.0, &source).unwrap();
        std::fs::write(
            fixture.0.join(crate::postprocess::RTTM),
            "SPEAKER mix 1 0 2 <NA> <NA> speaker_0 <NA> <NA>\n",
        )
        .unwrap();
        let next = fixture.source();
        assert_ne!(source, next);
        let loaded = AnnotationFile::load(&fixture.0).unwrap();
        let report = loaded.annotations.reconcile(&fixture.raw(), &next);
        assert_eq!(report.inactive_names, vec![0]);
        assert_eq!(report.unapplied, vec![0]);
        assert_eq!(report.rows[0].speaker.as_deref(), Some("speaker_0"));
        assert_eq!(report.rows[0].pending_speaker.as_deref(), Some("speaker_1"));
        assert!(!report.rows[0].reviewed);
        assert!(report.rows[0].needs_reconfirmation);
        assert_ne!(
            crate::library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("林")
        );
        let saved = std::fs::read(fixture.0.join(FILE)).unwrap();
        assert!(file.save(&fixture.0, &source).is_err());
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), saved);
    }

    #[test]
    fn same_display_names_never_merge_ids_and_empty_name_restores_id() {
        let mut doc = Annotations::default();
        doc.rename("context", "speaker_0", "林").unwrap();
        doc.rename("context", "speaker_1", "林").unwrap();
        assert_eq!(doc.names.len(), 2);
        let unchanged = doc.clone();
        doc.rename("context", "speaker_0", "林").unwrap();
        assert_eq!(doc, unchanged);
        doc.rename("context", "speaker_0", "").unwrap();
        assert_eq!(doc.display_name("context", "speaker_0"), "speaker_0");
        assert_eq!(doc.display_name("context", "speaker_1"), "林");
        assert!(doc.rename("context", "speaker_1", "bad\nname").is_err());
        assert_eq!(doc.display_name("context", "speaker_1"), "林");
    }

    #[test]
    fn malformed_unsupported_and_conflicting_sidecars_fail_without_overwriting() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let mut stale = AnnotationFile::load(&fixture.0).unwrap();
        for bad in ["{", "{\"version\":2,\"names\":[],\"corrections\":[]}"] {
            std::fs::write(fixture.0.join(FILE), bad).unwrap();
            assert!(AnnotationFile::load(&fixture.0).is_err());
            assert!(crate::library::read(&fixture.0).is_err());
            assert!(crate::library::read_raw(&fixture.0).is_ok());
            assert!(stale.save(&fixture.0, &source).is_err());
            assert_eq!(std::fs::read_to_string(fixture.0.join(FILE)).unwrap(), bad);
        }
        std::fs::remove_file(fixture.0.join(FILE)).unwrap();
        let mut winner = AnnotationFile::load(&fixture.0).unwrap();
        winner
            .annotations
            .rename(&source, "speaker_0", "Alice")
            .unwrap();
        winner.save(&fixture.0, &source).unwrap();
        stale
            .annotations
            .rename(&source, "speaker_0", "Bob")
            .unwrap();
        assert!(stale.save(&fixture.0, &source).is_err());
        assert_eq!(
            crate::library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("Alice")
        );
        // A validation failure leaves the last complete file and revision usable.
        winner.annotations.names[0].name = "bad\nlabel".into();
        assert!(winner.save(&fixture.0, &source).is_err());
        winner.annotations.names[0].name = "Carol".into();
        winner.save(&fixture.0, &source).unwrap();
        assert_eq!(
            crate::library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("Carol")
        );
    }

    #[test]
    fn unedited_legacy_recording_and_missing_rttm_keep_working() {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.0.join(crate::postprocess::RTTM)).unwrap();
        let mut lines = fixture.raw();
        let source = source_fingerprint(&fixture.0, &lines).unwrap();
        lines[0].id = 999;
        assert_eq!(source_fingerprint(&fixture.0, &lines).unwrap(), source);
        assert_eq!(
            crate::library::read(&fixture.0).unwrap().lines,
            fixture.raw()
        );
        assert!(AnnotationFile::load(&fixture.0)
            .unwrap()
            .annotations
            .corrections
            .is_empty());
    }

    #[test]
    fn simultaneous_save_is_rejected_and_retry_keeps_the_completed_file() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let mut file = AnnotationFile::load(&fixture.0).unwrap();
        file.save(&fixture.0, &source).unwrap();
        let saved = std::fs::read(fixture.0.join(FILE)).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.0.join(".annotations.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        file.annotations.rename(&source, "speaker_0", "林").unwrap();
        assert!(file.save(&fixture.0, &source).is_err());
        assert_eq!(std::fs::read(fixture.0.join(FILE)).unwrap(), saved);
        drop(lock);
        file.save(&fixture.0, &source).unwrap();
        assert_eq!(
            crate::library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("林")
        );
    }
}
