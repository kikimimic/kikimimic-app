//! Offline speaker labels for a completed recording.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use tauri::Manager;

#[derive(Debug, PartialEq)]
struct Turn {
    start_ms: f64,
    end_ms: f64,
    speaker: String,
}

fn read_rttm(path: &Path) -> anyhow::Result<Vec<Turn>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read diarization {}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            anyhow::ensure!(
                fields.len() == 10 && fields[0] == "SPEAKER",
                "invalid RTTM line"
            );
            let start: f64 = fields[3].parse()?;
            let duration: f64 = fields[4].parse()?;
            anyhow::ensure!(
                start.is_finite() && start >= 0.0 && duration.is_finite() && duration > 0.0,
                "invalid RTTM time"
            );
            anyhow::ensure!(
                fields[7]
                    .strip_prefix("speaker_")
                    .and_then(|n| n.parse::<u8>().ok())
                    .is_some(),
                "invalid Nemotron speaker"
            );
            Ok(Turn {
                start_ms: start * 1000.0,
                end_ms: (start + duration) * 1000.0,
                speaker: fields[7].to_string(),
            })
        })
        .collect()
}

fn speaker_for(start_ms: u64, end_ms: u64, turns: &[Turn]) -> Option<String> {
    // ponytail: scan all turns per line; index by time if long sessions open slowly.
    let mut overlap = BTreeMap::<&str, f64>::new();
    for turn in turns {
        let shared = (end_ms as f64).min(turn.end_ms) - (start_ms as f64).max(turn.start_ms);
        if shared > 0.0 {
            *overlap.entry(&turn.speaker).or_default() += shared;
        }
    }
    overlap
        .into_iter()
        .max_by(|(a, x), (b, y)| x.total_cmp(y).then_with(|| b.cmp(a)))
        .map(|(speaker, _)| speaker.to_string())
}

/// Where [`split`] writes the transcript it re-recognised, beside the
/// untouched live one.
pub const TRANSCRIPT: &str = "postprocess/transcript.jsonl";

/// A turn shorter than this is not given its own line: whisper hears too
/// little of it to be trusted, and it is usually a backchannel ("うん").
const MIN_PIECE_MS: u64 = 500;
/// Audio either side of a piece, so a word clipped by the turn boundary is
/// still heard whole.
const CROP_PAD_MS: u64 = 100;
/// whisper.cpp returns nothing for less than a second of input, so a short
/// crop is topped up with silence to just past that.
const MIN_CROP_SAMPLES: usize = 17_600;

/// The stretch of one line one speaker holds.
#[derive(Debug, PartialEq)]
struct Piece {
    start_ms: u64,
    end_ms: u64,
    speaker: String,
}

/// Where a line changes hands, or nothing when one speaker holds all of it.
/// `turns` must be in start order.
fn pieces(start_ms: u64, end_ms: u64, turns: &[Turn]) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    for turn in turns {
        let start = (turn.start_ms.max(start_ms as f64)) as u64;
        let end = (turn.end_ms.min(end_ms as f64)) as u64;
        if end < start + MIN_PIECE_MS {
            continue;
        }
        match pieces.last_mut() {
            Some(last) if last.speaker == turn.speaker => last.end_ms = last.end_ms.max(end),
            // Talked over: both voices are in the same stretch of audio, and
            // no crop can give either one alone.
            Some(last) if end <= last.end_ms => {}
            _ => pieces.push(Piece {
                start_ms: start,
                end_ms: end,
                speaker: turn.speaker.clone(),
            }),
        }
    }
    if pieces.len() < 2 {
        return Vec::new();
    }
    // Cover the whole line so nothing recognised live falls between pieces.
    pieces[0].start_ms = start_ms;
    pieces.last_mut().unwrap().end_ms = end_ms;
    for i in 1..pieces.len() {
        if pieces[i - 1].end_ms < pieces[i].start_ms {
            let middle = (pieces[i - 1].end_ms + pieces[i].start_ms) / 2;
            pieces[i - 1].end_ms = middle;
            pieces[i].start_ms = middle;
        }
    }
    pieces
}

/// One lane's audio from `start_ms` to `end_ms`, at whisper's rate.
fn crop(path: &Path, start_ms: u64, end_ms: u64) -> anyhow::Result<Vec<f32>> {
    let mut wav =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = wav.spec();
    anyhow::ensure!(
        spec.channels == 1 && spec.bits_per_sample == 16,
        "{} is not 16-bit mono",
        path.display()
    );
    let rate = u64::from(spec.sample_rate);
    let from = (start_ms.saturating_sub(CROP_PAD_MS) * rate / 1000).min(wav.duration().into());
    let to = ((end_ms + CROP_PAD_MS) * rate / 1000).min(wav.duration().into());
    wav.seek(from as u32)?;
    let raw = wav
        .samples::<i16>()
        .take((to - from) as usize)
        .map(|s| s.map(|s| f32::from(s) / 32_768.0))
        .collect::<Result<Vec<_>, _>>()?;
    let mut resampler = kikimimic_core::resample::StreamResampler::new(spec.sample_rate)?;
    let mut samples = resampler.process(&raw)?;
    samples.extend(resampler.flush()?);
    samples.resize(samples.len().max(MIN_CROP_SAMPLES), 0.0);
    Ok(samples)
}

/// Re-recognise each line two or more speakers share, one piece per speaker,
/// and write the result as [`TRANSCRIPT`]. Lines one speaker holds keep their
/// live text and translation. A split piece is translated into whatever its
/// line was, so a session that was not translating stays untranslated.
pub fn split(
    dir: &Path,
    mut transcribe: impl FnMut(&[f32], Option<&str>) -> anyhow::Result<kikimimic_core::Hypothesis>,
    mut translate: impl FnMut(&str, Option<&str>, &str) -> Option<String>,
) -> anyhow::Result<()> {
    let mut turns = read_rttm(&dir.join("postprocess/nemotron.rttm"))?;
    turns.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
    let mut out = Vec::new();
    for line in crate::library::transcript_lines(&dir.join("transcript.jsonl")) {
        let audio = dir.join(format!("{}.wav", line.lane));
        let mut split = Vec::new();
        for piece in pieces(line.start_ms, line.end_ms, &turns) {
            // ponytail: pinned to the line's language; a line that switches
            // language mid-way needs per-piece detection.
            let heard = transcribe(
                &crop(&audio, piece.start_ms, piece.end_ms)?,
                line.lang.as_deref(),
            )?;
            if heard.text.is_empty() {
                continue;
            }
            let lang = heard.lang.or_else(|| line.lang.clone());
            let translation = line
                .translation_lang
                .as_deref()
                .and_then(|target| translate(&heard.text, lang.as_deref(), target));
            split.push(crate::library::Line {
                id: 0,
                lane: line.lane.clone(),
                start_ms: piece.start_ms,
                end_ms: piece.end_ms,
                lang,
                text: heard.text,
                translation_lang: translation.as_ref().and(line.translation_lang.clone()),
                translation,
                speaker: Some(piece.speaker),
            });
        }
        if split.is_empty() {
            let speaker = speaker_for(line.start_ms, line.end_ms, &turns);
            out.push(crate::library::Line { speaker, ..line });
        } else {
            out.extend(split);
        }
    }
    write_transcript(dir, &out)
}

fn write_transcript(dir: &Path, lines: &[crate::library::Line]) -> anyhow::Result<()> {
    let mut text = String::new();
    for (id, line) in (1..).zip(lines) {
        let utterance = serde_json::json!({
            "type": "utterance",
            "id": id,
            "lane": line.lane,
            "startMs": line.start_ms,
            "endMs": line.end_ms,
            "lang": line.lang,
            "text": line.text,
            "speaker": line.speaker,
        });
        text.push_str(&format!("{utterance}\n"));
        if let Some(translation) = &line.translation {
            let entry = serde_json::json!({
                "type": "translation",
                "id": id,
                "lang": line.translation_lang,
                "text": translation,
            });
            text.push_str(&format!("{entry}\n"));
        }
    }
    let target = dir.join(TRANSCRIPT);
    let temporary = target.with_extension("jsonl.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, &target)
        .with_context(|| format!("save split transcript {}", target.display()))
}

pub fn label_lines(dir: &Path, lines: &mut [crate::library::Line]) {
    let Ok(turns) = read_rttm(&dir.join("postprocess/nemotron.rttm")) else {
        return;
    };
    for line in lines {
        line.speaker = speaker_for(line.start_ms, line.end_ms, &turns);
    }
}

pub fn run(app: &tauri::AppHandle, dir: &Path) -> anyhow::Result<()> {
    let audio = dir.join("mix.wav");
    hound::WavReader::open(&audio).with_context(|| format!("open {}", audio.display()))?;

    let cache = app.path().app_cache_dir()?;
    std::fs::create_dir_all(&cache)?;
    let script = cache.join("diarize_nemotron.py");
    std::fs::write(&script, include_str!("../../scripts/diarize_nemotron.py"))?;

    let output_dir = dir.join("postprocess");
    anyhow::ensure!(
        !output_dir.is_symlink(),
        "postprocess directory cannot be a symlink"
    );
    std::fs::create_dir_all(&output_dir)?;
    let temporary = output_dir.join(".nemotron.rttm.tmp");
    let target = output_dir.join("nemotron.rttm");
    let uv = std::env::var_os("KKM_UV_BIN").unwrap_or_else(|| "uv".into());
    let result = std::process::Command::new(&uv)
        .args(["run", "--script"])
        .arg(&script)
        .arg(&audio)
        .arg(&temporary)
        .output()
        .with_context(|| "Nemotron requires uv; set KKM_UV_BIN to its executable path")?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&temporary);
        let stderr = String::from_utf8_lossy(&result.stderr);
        anyhow::bail!(
            "Nemotron failed ({}): {}",
            result.status,
            stderr.lines().last().unwrap_or("no error details")
        );
    }
    if let Err(error) = read_rttm(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    // Lines split by the previous diarization no longer match this one.
    let _ = std::fs::remove_file(dir.join(TRANSCRIPT));
    std::fs::rename(&temporary, &target)
        .with_context(|| format!("save diarization {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_the_speaker_with_most_overlap_and_leaves_silence_unlabelled() {
        let turns = vec![
            Turn {
                start_ms: 0.0,
                end_ms: 700.0,
                speaker: "speaker_0".into(),
            },
            Turn {
                start_ms: 600.0,
                end_ms: 1500.0,
                speaker: "speaker_1".into(),
            },
        ];
        assert_eq!(speaker_for(500, 1300, &turns).as_deref(), Some("speaker_1"));
        assert_eq!(speaker_for(1500, 2000, &turns), None);
    }

    fn turn(start_ms: f64, end_ms: f64, speaker: &str) -> Turn {
        Turn {
            start_ms,
            end_ms,
            speaker: speaker.into(),
        }
    }

    fn piece(start_ms: u64, end_ms: u64, speaker: &str) -> Piece {
        Piece {
            start_ms,
            end_ms,
            speaker: speaker.into(),
        }
    }

    #[test]
    fn a_line_two_speakers_share_splits_where_the_turn_changes() {
        // The gap between the turns is split down the middle, and the ends
        // reach the line's own, so no recognised audio falls outside a piece.
        let turns = [
            turn(100.0, 1000.0, "speaker_0"),
            turn(1200.0, 1800.0, "speaker_1"),
            turn(1800.0, 2500.0, "speaker_1"),
        ];
        assert_eq!(
            pieces(0, 2400, &turns),
            vec![piece(0, 1100, "speaker_0"), piece(1100, 2400, "speaker_1")]
        );
    }

    #[test]
    fn a_line_one_speaker_holds_is_not_split() {
        assert!(pieces(0, 2000, &[turn(0.0, 2000.0, "speaker_0")]).is_empty());
    }

    #[test]
    fn a_backchannel_too_short_or_talked_over_does_not_split_a_line() {
        // Too short to recognise on its own.
        let short = [
            turn(0.0, 1000.0, "speaker_0"),
            turn(1000.0, 1300.0, "speaker_1"),
            turn(1300.0, 2000.0, "speaker_0"),
        ];
        assert!(pieces(0, 2000, &short).is_empty());
        // Said over the other speaker: no crop separates the two voices.
        let over = [
            turn(0.0, 3000.0, "speaker_0"),
            turn(1000.0, 2000.0, "speaker_1"),
        ];
        assert!(pieces(0, 3000, &over).is_empty());
    }
}
