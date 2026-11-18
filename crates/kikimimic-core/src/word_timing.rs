//! Optional re-recognition estimates, kept apart from the saved transcript.
//! Times always address the original lane WAV; no forced alignment is claimed.
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library::{self, Line, Session};

pub const SIDECAR: &str = "words.json";
const SOURCE: &str = "whisper.cpp token timestamps (re-recognition; experimental)";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    /// Includes any leading whitespace, preserving the decoded spelling.
    pub text: String,
    /// None means unaligned; neither seeking nor export invents a position.
    pub timing: Option<TimeRange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Word {
    pub fn seek_ms(&self) -> Option<u64> {
        self.timing.map(|t| t.start_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedLine {
    pub words: Vec<Word>,
    /// Word indices that start a new cue. Sorted, excludes zero and len.
    pub breaks: BTreeSet<usize>,
}

impl TimedLine {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect()
    }

    /// Split or join at the boundary immediately before `word`.
    pub fn toggle_break(&mut self, word: usize) -> Result<()> {
        ensure!(word > 0 && word < self.words.len(), "invalid word boundary");
        if !self.breaks.remove(&word) {
            self.breaks.insert(word);
        }
        Ok(())
    }

    pub fn cues(&self) -> Result<Vec<Cue>> {
        let mut cues = Vec::new();
        let mut from = 0;
        for to in self.breaks.iter().copied().chain([self.words.len()]) {
            ensure!(
                from < to && to <= self.words.len(),
                "invalid subtitle boundary"
            );
            let words = &self.words[from..to];
            ensure!(
                words.iter().all(|w| w.timing.is_some()),
                "Subtitle contains unaligned words; regenerate timing before exporting."
            );
            let start = words.first().and_then(|w| w.timing).context("empty cue")?;
            let end = words.last().and_then(|w| w.timing).context("empty cue")?;
            let text: String = words.iter().map(|w| w.text.as_str()).collect();
            cues.push(Cue {
                start_ms: start.start_ms,
                end_ms: end.end_ms,
                text: text.trim().to_string(),
            });
            from = to;
        }
        Ok(cues)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordTiming {
    #[serde(skip)]
    revision: Option<String>,
    #[serde(skip)]
    transcript_fingerprint: String,
    schema: u32,
    fingerprint: String,
    pub source: String,
    pub model: String,
    pub lines: Vec<TimedLine>,
}

fn transcript_fingerprint(lines: &[Line]) -> Result<String> {
    let evidence: Vec<_> = lines
        .iter()
        .map(|line| {
            (
                &line.lane,
                line.start_ms,
                line.end_ms,
                &line.lang,
                &line.text,
            )
        })
        .collect();
    Ok(bytes_digest(&serde_json::to_vec(&evidence)?))
}

impl WordTiming {
    /// Prevent row-index joins between different automatic transcript versions.
    pub fn matches_transcript(&self, lines: &[Line]) -> bool {
        transcript_fingerprint(lines).is_ok_and(|value| value == self.transcript_fingerprint)
    }
}

/// Hash both actual audio bytes and the effective transcript/ranges. Titles do
/// not invalidate estimates; re-recognition, lane or time edits do.
fn fingerprint(dir: &Path, session: &Session) -> Result<String> {
    let mut digest = Sha256::new();
    let evidence: Vec<_> = session
        .lines
        .iter()
        .map(|line| {
            (
                &line.lane,
                line.start_ms,
                line.end_ms,
                &line.lang,
                &line.text,
            )
        })
        .collect();
    digest.update(serde_json::to_vec(&evidence)?);
    let lanes: BTreeSet<_> = session.lines.iter().map(|l| l.lane.as_str()).collect();
    for lane in lanes {
        ensure!(
            matches!(lane, "mic" | "speaker"),
            "unsupported recording lane: {lane}"
        );
        digest.update(lane.as_bytes());
        let mut file = std::fs::File::open(dir.join(format!("{lane}.wav")))?;
        let mut buffer = [0u8; 65_536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_words(words: &[Word], start: u64, end: u64) -> Result<()> {
    let mut previous = start;
    for word in words {
        ensure!(
            !word.text.trim().is_empty() && !word.text.contains('\u{fffd}'),
            "invalid decoded word"
        );
        if let Some(t) = word.timing {
            ensure!(
                t.start_ms >= previous && t.end_ms > t.start_ms && t.end_ms <= end,
                "word timing outside its recording range or not monotonic"
            );
            previous = t.end_ms;
        }
    }
    Ok(())
}

fn validate(data: &WordTiming, session: &Session) -> Result<()> {
    ensure!(
        !data.model.trim().is_empty(),
        "word timing model is missing"
    );
    ensure!(
        data.lines
            .iter()
            .flat_map(|line| &line.words)
            .any(|word| word.timing.is_some()),
        "No aligned speech found in recording"
    );
    ensure!(
        data.schema == 1 && data.source == SOURCE,
        "unsupported words sidecar version/source"
    );
    ensure!(
        data.lines.len() == session.lines.len(),
        "word line count changed"
    );
    for (timed, line) in data.lines.iter().zip(&session.lines) {
        ensure!(
            timed.breaks.iter().all(|&b| b > 0 && b < timed.words.len()),
            "invalid subtitle boundary"
        );
        validate_words(&timed.words, line.start_ms, line.end_ms)?;
    }
    Ok(())
}

/// Missing is normal. A stale/corrupt sidecar is an explicit error, never reused.
pub fn load(dir: &Path) -> Result<Option<WordTiming>> {
    let bytes = match std::fs::read(dir.join(SIDECAR)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut data: WordTiming = serde_json::from_slice(&bytes)?;
    data.revision = Some(bytes_digest(&bytes));
    let session = library::read_raw(dir)?;
    data.transcript_fingerprint = transcript_fingerprint(&session.lines)?;
    ensure!(
        data.fingerprint == fingerprint(dir, &session)?,
        "Word timing is stale: audio or transcript changed. Generate it again."
    );
    validate(&data, &session)?;
    Ok(Some(data))
}

/// Publish only complete, validated JSON. An unsuccessful retry preserves the
/// previous file; create_new also prevents simultaneous edits sharing a temp.
fn bytes_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn revision(dir: &Path) -> Result<Option<String>> {
    match std::fs::read(dir.join(SIDECAR)) {
        Ok(bytes) => Ok(Some(bytes_digest(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save(dir: &Path, data: &mut WordTiming) -> Result<()> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".words.lock"))?;
    lock.try_lock()
        .context("another subtitle save is in progress")?;
    ensure!(
        revision(dir)? == data.revision,
        "Subtitle boundaries changed on disk; reload before saving."
    );
    let session = library::read_raw(dir)?;
    ensure!(
        data.fingerprint == fingerprint(dir, &session)?,
        "Word timing is stale: audio or transcript changed. Generate it again."
    );
    validate(data, &session)?;
    let tmp = dir.join(".words.json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .context("create words temporary file (remove a leftover .words.json.tmp after a crash)")?;
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(data)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let decoded: WordTiming = serde_json::from_slice(&std::fs::read(&tmp)?)?;
        validate(&decoded, &session)?;
        ensure!(
            revision(dir)? == data.revision,
            "Subtitle boundaries changed on disk; reload before saving."
        );
        ensure!(
            data.fingerprint == fingerprint(dir, &library::read_raw(dir)?)?,
            "Recording evidence changed before subtitle publication."
        );
        std::fs::rename(&tmp, dir.join(SIDECAR))?;
        Ok(bytes_digest(&bytes))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    data.revision = Some(result?);
    Ok(())
}

/// At most 25 seconds per engine request, read/resampled without loading a
/// whole recording into memory. No padding offset is added to recording times.
fn decode_line(
    dir: &Path,
    line: &Line,
    decode: &mut impl FnMut(&[f32], Option<&str>) -> Result<Vec<Word>>,
) -> Result<TimedLine> {
    let path = dir.join(format!("{}.wav", line.lane));
    let mut wav = hound::WavReader::open(&path)?;
    let spec = wav.spec();
    ensure!(
        spec.channels == 1
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int
            && spec.sample_rate > 0,
        "Word timing requires mono PCM16 WAV"
    );
    let rate = u64::from(spec.sample_rate);
    let duration = u64::from(wav.duration()) * 1000 / rate;
    ensure!(
        line.start_ms < line.end_ms && line.end_ms <= duration,
        "Transcript range is outside lane audio"
    );
    let mut words = Vec::new();
    let mut from = line.start_ms;
    while from < line.end_ms {
        let to = (from + 25_000).min(line.end_ms);
        let start_sample = from * rate / 1000;
        let end_sample = to * rate / 1000;
        wav.seek(u32::try_from(start_sample)?)?;
        let raw = wav
            .samples::<i16>()
            .take((end_sample - start_sample) as usize)
            .map(|s| s.map(|s| f32::from(s) / 32_768.0))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            !raw.is_empty(),
            "Recording has no audio in transcript range"
        );
        let mut resampler = crate::resample::StreamResampler::new(spec.sample_rate)?;
        let mut samples = resampler.process(&raw)?;
        samples.extend(resampler.flush()?);
        // whisper.cpp skips sub-second clips. Padding is silence after real audio;
        // estimates outside the real interval are kept explicitly unaligned.
        samples.resize(samples.len().max(17_600), 0.0);
        let mut heard = decode(&samples, line.lang.as_deref())?;
        let mut previous = from;
        for word in &mut heard {
            word.timing = word.timing.and_then(|t| {
                let range = TimeRange {
                    start_ms: from.checked_add(t.start_ms)?,
                    end_ms: from.checked_add(t.end_ms)?,
                };
                if range.start_ms < previous || range.end_ms <= range.start_ms || range.end_ms > to
                {
                    return None;
                }
                previous = range.end_ms;
                Some(range)
            });
        }
        validate_words(&heard, from, to)?;
        words.extend(heard);
        from = to;
    }
    Ok(TimedLine {
        words,
        breaks: BTreeSet::new(),
    })
}

/// Generate estimates through an engine seam, then publish only on success.
pub fn generate(
    dir: &Path,
    model: &str,
    mut decode: impl FnMut(&[f32], Option<&str>) -> Result<Vec<Word>>,
) -> Result<WordTiming> {
    let session = library::read_raw(dir)?;
    ensure!(
        !session.lines.is_empty(),
        "No transcript ranges to re-recognize"
    );
    let expected = fingerprint(dir, &session)?;
    let expected_revision = revision(dir)?;
    let lines = session
        .lines
        .iter()
        .map(|line| decode_line(dir, line, &mut decode))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        lines
            .iter()
            .flat_map(|l| &l.words)
            .any(|w| w.timing.is_some()),
        "No aligned speech found in recording"
    );
    let mut data = WordTiming {
        revision: expected_revision,
        transcript_fingerprint: transcript_fingerprint(&session.lines)?,
        schema: 1,
        fingerprint: expected,
        source: SOURCE.into(),
        model: model.into(),
        lines,
    };
    save(dir, &mut data)?;
    Ok(data)
}

/// Separate subtitle export from the re-recognized words, never translations.
pub fn export_srt(dir: &Path) -> Result<()> {
    let data = load(dir)?.context("Generate word timing first")?;
    let text = srt(&data)?;
    let tmp = dir.join(".words.srt.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    let result = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        ensure!(
            std::fs::read_to_string(&tmp)? == text,
            "Subtitle temporary file changed"
        );
        std::fs::rename(&tmp, dir.join("words.srt"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Format in-memory subtitle boundaries (use `export_srt` to recheck sources).
pub fn srt(data: &WordTiming) -> Result<String> {
    let mut cues = data
        .lines
        .iter()
        .filter(|l| !l.words.is_empty())
        .map(TimedLine::cues)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    cues.sort_by_key(|c| (c.start_ms, c.end_ms));
    let clock = |ms: u64| {
        format!(
            "{:02}:{:02}:{:02},{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    };
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            clock(cue.start_ms),
            clock(cue.end_ms),
            cue.text.lines().collect::<Vec<_>>().join(" ")
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Recording(std::path::PathBuf);
    impl Drop for Recording {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn recording() -> Recording {
        let dir = std::env::temp_dir().join(format!(
            "kikimimic-words-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(dir.join("mic.wav"), spec).unwrap();
        for _ in 0..144_000 {
            wav.write_sample(500i16).unwrap();
        }
        wav.finalize().unwrap();
        std::fs::write(dir.join("transcript.jsonl"), "{\"type\":\"utterance\",\"id\":1,\"lane\":\"mic\",\"startMs\":1000,\"endMs\":2500,\"lang\":\"ja\",\"text\":\"元の本文\"}\n{\"type\":\"translation\",\"id\":1,\"lang\":\"en\",\"text\":\"original text\"}\n").unwrap();
        Recording(dir)
    }
    fn word(text: &str, start: u64, end: u64) -> Word {
        Word {
            text: text.into(),
            timing: Some(TimeRange {
                start_ms: start,
                end_ms: end,
            }),
        }
    }
    fn heard(_: &[f32], lang: Option<&str>) -> Result<Vec<Word>> {
        assert_eq!(lang, Some("ja"));
        Ok(vec![word("再認識", 100, 300), word("の文。", 400, 800)])
    }

    #[test]
    fn stale_subtitle_writer_cannot_erase_a_saved_boundary() {
        let dir = recording();
        generate(&dir.0, "whisper", heard).unwrap();
        let mut a = load(&dir.0).unwrap().unwrap();
        let mut b = load(&dir.0).unwrap().unwrap();
        a.lines[0].toggle_break(1).unwrap();
        save(&dir.0, &mut a).unwrap();
        let saved = std::fs::read(dir.0.join(SIDECAR)).unwrap();
        assert!(save(&dir.0, &mut b).is_err());
        assert_eq!(std::fs::read(dir.0.join(SIDECAR)).unwrap(), saved);
        assert!(load(&dir.0).unwrap().unwrap().lines[0].breaks.contains(&1));
        a.lines[0].toggle_break(1).unwrap();
        save(&dir.0, &mut a).unwrap();
    }

    #[test]
    fn regeneration_does_not_overwrite_subtitle_changes_saved_while_decoding() {
        let dir = recording();
        generate(&dir.0, "whisper", heard).unwrap();
        let result = generate(&dir.0, "whisper", |samples, lang| {
            let mut other = load(&dir.0)?.unwrap();
            other.lines[0].toggle_break(1)?;
            save(&dir.0, &mut other)?;
            heard(samples, lang)
        });
        assert!(result.is_err());
        assert!(load(&dir.0).unwrap().unwrap().lines[0].breaks.contains(&1));
    }

    #[test]
    fn speaker_edits_and_renumbering_keep_times_but_changed_evidence_invalidates_them() {
        let dir = recording();
        let data = generate(&dir.0, "installed-whisper", heard).unwrap();
        let raw = library::read_raw(&dir.0).unwrap();
        let source = crate::annotations::source_fingerprint(&dir.0, &raw.lines).unwrap();
        let mut edits = crate::annotations::AnnotationFile::load(&dir.0).unwrap();
        edits
            .annotations
            .rename(&source, "speaker_0", "田中")
            .unwrap();
        edits
            .annotations
            .correct(&raw.lines[0], &source, Some("speaker_0".into()), true);
        edits.save(&dir.0, &source).unwrap();
        assert_eq!(
            library::read(&dir.0).unwrap().lines[0].speaker.as_deref(),
            Some("田中")
        );
        assert_eq!(load(&dir.0).unwrap().unwrap(), data);
        let transcript = dir.0.join("transcript.jsonl");
        let bytes = std::fs::read_to_string(&transcript).unwrap();
        std::fs::write(&transcript, bytes.replace("\"id\":1", "\"id\":98")).unwrap();
        assert_eq!(load(&dir.0).unwrap().unwrap(), data);
        export_srt(&dir.0).unwrap();
        std::fs::write(&transcript, bytes.replace("元の本文", "別の本文")).unwrap();
        assert!(load(&dir.0).is_err());
    }
    #[test]
    fn generates_recording_times_preserves_original_and_reloads_saved_subtitle_edits() {
        let dir = recording();
        let original = library::read(&dir.0).unwrap();
        let audio = std::fs::read(dir.0.join("mic.wav")).unwrap();
        let transcript = std::fs::read(dir.0.join("transcript.jsonl")).unwrap();
        assert!(load(&dir.0).unwrap().is_none());
        let mut data = generate(&dir.0, "installed-whisper", heard).unwrap();
        assert_eq!(data.lines[0].words[0].seek_ms(), Some(1100));
        assert_eq!(data.lines[0].words[1].timing.unwrap().end_ms, 1800);
        assert_eq!(data.lines[0].text(), "再認識の文。");
        assert_eq!(data, load(&dir.0).unwrap().unwrap());
        data.lines[0].toggle_break(1).unwrap();
        save(&dir.0, &mut data).unwrap();
        let mut loaded = load(&dir.0).unwrap().unwrap();
        assert_eq!(loaded.lines[0].cues().unwrap().len(), 2);
        assert_eq!(srt(&loaded).unwrap(), "1\n00:00:01,100 --> 00:00:01,300\n再認識\n\n2\n00:00:01,400 --> 00:00:01,800\nの文。\n\n");
        loaded.lines[0].toggle_break(1).unwrap();
        save(&dir.0, &mut loaded).unwrap();
        assert_eq!(
            load(&dir.0).unwrap().unwrap().lines[0]
                .cues()
                .unwrap()
                .len(),
            1
        );
        export_srt(&dir.0).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.0.join("words.srt")).unwrap(),
            srt(&loaded).unwrap()
        );
        assert_eq!(original, library::read(&dir.0).unwrap());
        assert_eq!(
            transcript,
            std::fs::read(dir.0.join("transcript.jsonl")).unwrap()
        );
        assert_eq!(audio, std::fs::read(dir.0.join("mic.wav")).unwrap());
    }
    #[test]
    fn rejects_stale_audio_text_and_ranges() {
        for change in ["audio", "text", "range"] {
            let dir = recording();
            let mut data = generate(&dir.0, "whisper", heard).unwrap();
            if change == "audio" {
                let mut bytes = std::fs::read(dir.0.join("mic.wav")).unwrap();
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(dir.0.join("mic.wav"), bytes).unwrap();
            } else {
                let text = std::fs::read_to_string(dir.0.join("transcript.jsonl")).unwrap();
                let text = if change == "text" {
                    text.replace("元の本文", "変わった本文")
                } else {
                    text.replace("2500", "2400")
                };
                std::fs::write(dir.0.join("transcript.jsonl"), text).unwrap();
            }
            assert!(load(&dir.0).unwrap_err().to_string().contains("stale"));
            assert!(save(&dir.0, &mut data).is_err());
        }
    }
    #[test]
    fn failed_retry_empty_speech_or_invalid_utf8_preserves_complete_sidecar() {
        let dir = recording();
        generate(&dir.0, "whisper", heard).unwrap();
        let previous = std::fs::read(dir.0.join(SIDECAR)).unwrap();
        assert!(generate(&dir.0, "whisper", |_, _| anyhow::bail!("engine failed")).is_err());
        assert!(generate(&dir.0, "whisper", |_, _| Ok(vec![])).is_err());
        assert!(generate(&dir.0, "whisper", |_, _| Ok(vec![word("\u{fffd}", 0, 100)])).is_err());
        assert_eq!(previous, std::fs::read(dir.0.join(SIDECAR)).unwrap());
        assert!(!dir.0.join(".words.json.tmp").exists());
    }
    #[test]
    fn invalid_estimates_remain_unaligned_and_cannot_seek_or_export() {
        let dir = recording();
        let data = generate(&dir.0, "whisper", |_, _| {
            Ok(vec![
                word("valid", 50, 100),
                word("backward", 20, 80),
                word("outside", 500, 2000),
            ])
        })
        .unwrap();
        assert_eq!(data.lines[0].words[0].seek_ms(), Some(1050));
        assert_eq!(data.lines[0].words[1].seek_ms(), None);
        assert_eq!(data.lines[0].words[2].seek_ms(), None);
        assert!(srt(&data).is_err());
        assert!(validate_words(&[word("a", 0, 100), word("b", 50, 200)], 0, 1000).is_err());
        assert!(validate_words(&[word("a", 0, 1001)], 0, 1000).is_err());
    }
    #[test]
    fn corrupt_sidecar_and_invalid_boundaries_do_not_replace_previous() {
        let dir = recording();
        let mut data = generate(&dir.0, "whisper", heard).unwrap();
        let previous = std::fs::read(dir.0.join(SIDECAR)).unwrap();
        assert!(data.lines[0].toggle_break(0).is_err());
        assert!(data.lines[0].toggle_break(2).is_err());
        data.lines[0].breaks.insert(3);
        assert!(save(&dir.0, &mut data).is_err());
        assert_eq!(previous, std::fs::read(dir.0.join(SIDECAR)).unwrap());
        std::fs::write(dir.0.join(SIDECAR), "{truncated").unwrap();
        assert!(load(&dir.0).is_err());
    }
    #[test]
    fn chunks_long_ranges_on_original_timeline_and_pads_short_clips_only_at_end() {
        let dir = recording();
        let mut wav = hound::WavWriter::create(
            dir.0.join("mic.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..16_000 * 33 {
            wav.write_sample(500i16).unwrap();
        }
        wav.finalize().unwrap();
        let path = dir.0.join("transcript.jsonl");
        std::fs::write(
            &path,
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("2500", "31500"),
        )
        .unwrap();
        let mut lengths = Vec::new();
        let data = generate(&dir.0, "whisper", |samples, _| {
            lengths.push(samples.len());
            Ok(vec![word("word", 100, 300)])
        })
        .unwrap();
        assert_eq!(lengths, [400_000, 88_000]);
        assert_eq!(
            data.lines[0]
                .words
                .iter()
                .map(Word::seek_ms)
                .collect::<Vec<_>>(),
            [Some(1100), Some(26100)]
        );
        std::fs::write(
            &path,
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("31500", "1500"),
        )
        .unwrap();
        generate(&dir.0, "whisper", |samples, _| {
            assert_eq!(samples.len(), 17_600);
            assert!(samples[8000..].iter().all(|&s| s == 0.0));
            Ok(vec![word("short", 100, 300)])
        })
        .unwrap();
    }

    #[test]
    fn export_and_publish_failures_preserve_complete_outputs() {
        let dir = recording();
        let mut data = generate(&dir.0, "whisper", heard).unwrap();
        export_srt(&dir.0).unwrap();
        let old_subtitles = std::fs::read(dir.0.join("words.srt")).unwrap();
        let old_data = std::fs::read(dir.0.join(SIDECAR)).unwrap();
        std::fs::write(dir.0.join(".words.json.tmp"), "another writer").unwrap();
        assert!(save(&dir.0, &mut data).is_err());
        assert_eq!(old_data, std::fs::read(dir.0.join(SIDECAR)).unwrap());
        std::fs::write(dir.0.join("transcript.jsonl"), "changed").unwrap();
        assert!(export_srt(&dir.0).is_err());
        assert_eq!(
            old_subtitles,
            std::fs::read(dir.0.join("words.srt")).unwrap()
        );
    }

    #[test]
    fn absent_transcript_and_empty_or_out_of_range_audio_fail_clearly() {
        let dir = recording();
        std::fs::write(dir.0.join("transcript.jsonl"), "").unwrap();
        assert!(generate(&dir.0, "whisper", heard)
            .unwrap_err()
            .to_string()
            .contains("No transcript"));
        let dir = recording();
        let wav = hound::WavWriter::create(
            dir.0.join("mic.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        wav.finalize().unwrap();
        assert!(generate(&dir.0, "whisper", heard)
            .unwrap_err()
            .to_string()
            .contains("outside lane audio"));
    }
}
