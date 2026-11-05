//! Reads back what [`record`](crate::record) wrote: the sessions sitting under
//! the recording base, as the library list shows them.
//!
//! Nothing here opens audio — the length comes from the WAV header, which is
//! four bytes and a division. A session list must stay cheap enough to build
//! on every visit to the home view.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{NaiveDateTime, TimeZone};

/// The lanes a session may have, in the order the list names them.
const LANES: [&str; 2] = ["mic", "speaker"];
const METADATA_FILE: &str = "metadata.json";
const MAX_TITLE_CHARS: usize = 100;

#[derive(serde::Deserialize, serde::Serialize)]
struct Metadata {
    title: String,
}

/// One recorded session, as the library list needs it.
#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    /// Directory name — `20260819090503`, or `…-2` for a restart within the
    /// same second.
    pub name: String,
    pub dir: String,
    /// A user-supplied display name. The directory name remains the stable
    /// recording identifier and still carries its start time.
    pub title: Option<String>,
    /// When it started, from the name's wall clock read as local time.
    /// `None` for a directory that does not follow the naming, which is still
    /// listed — it just cannot be placed on the calendar.
    pub started_at_ms: Option<i64>,
    pub duration_ms: u64,
    /// Which lanes were actually recorded: a session captured before the tap
    /// was permitted has no `speaker.wav`.
    pub lanes: Vec<String>,
    /// The session's first words, which is what tells two 40-minute meetings
    /// apart in a list.
    pub snippet: String,
    pub utterances: usize,
}

/// Every session under `base`, newest first. A missing base is not an error:
/// it just means nothing has been recorded yet.
pub fn list(base: &Path) -> Vec<Recording> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut recordings: Vec<Recording> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| read_session(&e.path()))
        .collect();
    // Newest first; the name breaks ties so the order never depends on the
    // order the filesystem happened to hand the directories over.
    recordings.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| b.name.cmp(&a.name))
    });
    recordings
}

/// `None` when the directory holds no lane audio — then it is something else
/// the user put here, not a session we wrote.
fn read_session(dir: &Path) -> Option<Recording> {
    let name = dir.file_name()?.to_str()?.to_string();
    let lanes: Vec<String> = LANES
        .iter()
        .filter(|lane| dir.join(format!("{lane}.wav")).is_file())
        .map(|lane| (*lane).to_string())
        .collect();
    if lanes.is_empty() {
        return None;
    }
    // The mix spans the whole session by construction (every lane is padded up
    // to it), so it is the length. A session that failed mid-write may not
    // have one, and then the longest lane is the best answer available.
    let duration_ms = duration_ms(&dir.join("mix.wav")).unwrap_or_else(|| {
        lanes
            .iter()
            .filter_map(|lane| duration_ms(&dir.join(format!("{lane}.wav"))))
            .max()
            .unwrap_or(0)
    });
    let (snippet, utterances) = read_transcript(&dir.join("transcript.jsonl"));
    Some(Recording {
        started_at_ms: started_at_ms(&name),
        dir: dir.display().to_string(),
        title: read_title(dir),
        name,
        duration_ms,
        lanes,
        snippet,
        utterances,
    })
}

fn read_title(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(METADATA_FILE)).ok()?;
    let metadata = serde_json::from_str::<Metadata>(&text).ok()?;
    normalize_title(&metadata.title).ok()
}

fn normalize_title(raw: &str) -> anyhow::Result<String> {
    let title = raw.trim();
    if title.is_empty() {
        anyhow::bail!("title must not be empty");
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        anyhow::bail!("title must be at most {MAX_TITLE_CHARS} characters");
    }
    if title.chars().any(char::is_control) {
        anyhow::bail!("title must not contain control characters");
    }
    Ok(title.to_string())
}

/// Persist a display title without renaming the session directory. `dir` must
/// be an immediate recording child of `base`; unlike reading, this mutates the
/// filesystem and must not accept an arbitrary path from the webview.
pub fn set_title(base: &Path, dir: &Path, raw_title: &str) -> anyhow::Result<String> {
    let title = normalize_title(raw_title)?;
    let dir = resolve_recording(base, dir)?;

    let mut payload = serde_json::to_vec_pretty(&Metadata {
        title: title.clone(),
    })?;
    payload.push(b'\n');
    let target = dir.join(METADATA_FILE);
    let temporary = dir.join(format!(".{METADATA_FILE}.tmp"));
    std::fs::write(&temporary, payload)
        .with_context(|| format!("write recording title {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("save recording title {}", target.display()));
    }
    Ok(title)
}

pub fn resolve_recording(base: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let base = base
        .canonicalize()
        .with_context(|| format!("resolve recording base {}", base.display()))?;
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolve recording {}", dir.display()))?;
    if dir.parent() != Some(base.as_path()) || read_session(&dir).is_none() {
        anyhow::bail!("{} is not a recording in {}", dir.display(), base.display());
    }
    Ok(dir)
}

/// The wall clock in the directory name, as local time. The `-2` suffix a
/// same-second restart gets is not part of it.
fn started_at_ms(name: &str) -> Option<i64> {
    let stamp = name.split('-').next()?;
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S").ok()?;
    // An hour that repeats itself (DST ending) is ambiguous; either reading is
    // an hour out at worst, and the earlier one keeps the list in order.
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp_millis())
}

/// From the WAV header alone — `duration()` is the frame count the header
/// declares, not a scan of the samples.
fn duration_ms(path: &Path) -> Option<u64> {
    let reader = hound::WavReader::open(path).ok()?;
    let rate = reader.spec().sample_rate as u64;
    (rate > 0).then(|| reader.duration() as u64 * 1_000 / rate)
}

/// The first utterance and how many there are. Translation lines are skipped:
/// the snippet should read as what was said, not as what we made of it.
fn read_transcript(path: &Path) -> (String, usize) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (String::new(), 0);
    };
    let mut snippet = String::new();
    let mut count = 0;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // a half-written last line after a crash
        };
        if value["type"] != "utterance" {
            continue;
        }
        count += 1;
        if snippet.is_empty() {
            snippet = value["text"].as_str().unwrap_or_default().to_string();
        }
    }
    (snippet, count)
}

/// One utterance of a finished session, with whatever translation caught up
/// to it before the session ended.
#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub id: u64,
    pub lane: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub lang: Option<String>,
    pub text: String,
    /// `None` when nothing was translated for it — the session was not
    /// translating, or the queue dropped it (record.rs).
    pub translation: Option<String>,
    /// The language `translation` is in, kept so a re-translation matches it.
    #[serde(skip)]
    pub translation_lang: Option<String>,
    pub speaker: Option<String>,
}

/// A finished session, read back for the transcript view.
#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub name: String,
    pub dir: String,
    pub title: Option<String>,
    pub started_at_ms: Option<i64>,
    pub duration_ms: u64,
    pub lanes: Vec<String>,
    /// Both lanes in one stream, in the order they were spoken — which is
    /// how the transcript is read, and the file does not store it that way
    /// because translations are appended when they arrive.
    pub lines: Vec<Line>,
}

/// Put `transcript.jsonl` back together. The file is an append log — an
/// utterance now, its translation seconds later, the other lane in between —
/// so reading it means joining on the id, not trusting the order.
pub fn read(dir: &Path) -> anyhow::Result<Session> {
    let recording =
        read_session(dir).ok_or_else(|| anyhow::anyhow!("{} is not a recording", dir.display()))?;
    // Speaker-split lines, once a diarization has produced them; until then
    // the live transcript, labelled from whatever diarization there is.
    let split = dir.join(crate::postprocess::TRANSCRIPT);
    let lines = if split.is_file() {
        transcript_lines(&split)
    } else {
        let mut lines = transcript_lines(&dir.join("transcript.jsonl"));
        crate::postprocess::label_lines(dir, &mut lines);
        lines
    };

    Ok(Session {
        name: recording.name,
        dir: recording.dir,
        title: recording.title,
        started_at_ms: recording.started_at_ms,
        duration_ms: recording.duration_ms,
        lanes: recording.lanes,
        lines,
    })
}

/// One stream in spoken order, each utterance joined to its translation.
pub fn transcript_lines(path: &Path) -> Vec<Line> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<Line> = Vec::new();
    let mut translations: Vec<(u64, Option<String>, String)> = Vec::new();
    for raw in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let id = v["id"].as_u64().unwrap_or_default();
        let body = v["text"].as_str().unwrap_or_default().to_string();
        match v["type"].as_str() {
            Some("utterance") => lines.push(Line {
                id,
                lane: v["lane"].as_str().unwrap_or("mic").to_string(),
                start_ms: v["startMs"].as_u64().unwrap_or_default(),
                end_ms: v["endMs"].as_u64().unwrap_or_default(),
                lang: v["lang"].as_str().map(str::to_string),
                text: body,
                translation: None,
                translation_lang: None,
                speaker: v["speaker"].as_str().map(str::to_string),
            }),
            Some("translation") => {
                translations.push((id, v["lang"].as_str().map(str::to_string), body))
            }
            _ => continue,
        }
    }
    for (id, lang, body) in translations {
        // A translation whose utterance is not in the file is dropped rather
        // than shown on its own: a sentence with no original is noise.
        if let Some(line) = lines.iter_mut().find(|l| l.id == id) {
            line.translation = Some(body);
            line.translation_lang = lang;
        }
    }
    // One stream in spoken order. `start_ms` is on the recording's clock for
    // both lanes (pipeline::SessionClock), so this is the conversation.
    lines.sort_by_key(|l| (l.start_ms, l.id));
    lines
}

/// Where [`list`] looks, so the command and the recorder agree on one place.
pub fn base(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    crate::pipeline::record_base(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kkm-library-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A session directory holding `frames` samples of silence per named lane.
    fn session(base: &Path, name: &str, lanes: &[&str], frames: u32) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for lane in lanes.iter().chain(std::iter::once(&"mix")) {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(dir.join(format!("{lane}.wav")), spec).unwrap();
            for _ in 0..frames {
                w.write_sample(0i16).unwrap();
            }
            w.finalize().unwrap();
        }
        dir
    }

    #[test]
    fn a_base_that_does_not_exist_yet_lists_nothing() {
        // Before the first recording there is no directory at all, and that is
        // the ordinary first-run state, not a failure.
        assert!(list(&scratch("missing")).is_empty());
    }

    #[test]
    fn sessions_come_back_newest_first() {
        let base = scratch("order");
        session(&base, "20260819090503", &["mic"], 1);
        session(&base, "20260817120000", &["mic"], 1);
        session(&base, "20260819235959", &["mic"], 1);

        let names: Vec<String> = list(&base).into_iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            ["20260819235959", "20260819090503", "20260817120000"]
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_directory_name_is_read_as_the_local_time_it_started() {
        let base = scratch("clock");
        session(&base, "20260819090503", &["mic"], 1);

        let started = list(&base)[0].started_at_ms.expect("a parsed start");
        let expected = chrono::Local
            .with_ymd_and_hms(2026, 8, 19, 9, 5, 3)
            .unwrap()
            .timestamp_millis();
        assert_eq!(started, expected);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_same_second_restart_keeps_the_start_of_the_second_it_names() {
        // `-2` distinguishes the directory, not the moment.
        let base = scratch("restart");
        session(&base, "20260819090503-2", &["mic"], 1);
        assert!(list(&base)[0].started_at_ms.is_some());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_directory_named_something_else_is_still_listed_just_undated() {
        // Renaming a recording to "面談" must not make it disappear.
        let base = scratch("renamed");
        session(&base, "面談", &["mic"], 1);
        let listed = list(&base);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].started_at_ms, None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_length_comes_from_the_recording_header() {
        let base = scratch("length");
        session(&base, "20260819090503", &["mic"], 48_000 * 3);
        assert_eq!(list(&base)[0].duration_ms, 3_000);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_the_lanes_that_were_recorded_are_named() {
        // A session captured before the tap was permitted has no speaker file;
        // the list says "マイク", not "マイク + スピーカー".
        let base = scratch("lanes");
        session(&base, "20260819090503", &["mic"], 1);
        session(&base, "20260819090504", &["mic", "speaker"], 1);

        let listed = list(&base);
        assert_eq!(listed[0].lanes, ["mic", "speaker"]);
        assert_eq!(listed[1].lanes, ["mic"]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_first_thing_said_stands_in_for_the_session() {
        let base = scratch("snippet");
        let dir = session(&base, "20260819090503", &["mic"], 1);
        std::fs::write(
            dir.join("transcript.jsonl"),
            [
                r#"{"type":"utterance","id":1,"lane":"mic","text":"それでは定例を始めます"}"#,
                r#"{"type":"translation","id":1,"lang":"en","text":"Let's begin."}"#,
                r#"{"type":"utterance","id":2,"lane":"speaker","text":"hello"}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let listed = list(&base);
        assert_eq!(listed[0].snippet, "それでは定例を始めます");
        // The translation is not an utterance: two people spoke, not three.
        assert_eq!(listed[0].utterances, 2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_custom_title_is_trimmed_and_used_by_the_list_and_session() {
        let base = scratch("title");
        let dir = session(&base, "20260819090503", &["mic"], 1);

        assert_eq!(
            set_title(&base, &dir, "  週次ミーティング  ").unwrap(),
            "週次ミーティング"
        );
        assert_eq!(list(&base)[0].title.as_deref(), Some("週次ミーティング"));
        assert_eq!(
            read(&dir).unwrap().title.as_deref(),
            Some("週次ミーティング")
        );
        assert_eq!(dir.file_name().unwrap(), "20260819090503");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn invalid_metadata_does_not_hide_a_recording() {
        let base = scratch("bad-metadata");
        let dir = session(&base, "20260819090503", &["mic"], 1);
        std::fs::write(dir.join(METADATA_FILE), "not json").unwrap();

        let listed = list(&base);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn titles_are_validated_before_writing() {
        let base = scratch("title-validation");
        let dir = session(&base, "20260819090503", &["mic"], 1);

        assert!(set_title(&base, &dir, "   ").is_err());
        assert!(set_title(&base, &dir, "line\nbreak").is_err());
        assert!(set_title(&base, &dir, &"長".repeat(MAX_TITLE_CHARS + 1)).is_err());
        assert!(!dir.join(METADATA_FILE).exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_title_cannot_be_written_outside_the_recording_base() {
        let base = scratch("title-base");
        let outside = scratch("title-outside");
        let dir = session(&outside, "20260819090503", &["mic"], 1);
        std::fs::create_dir_all(&base).unwrap();

        assert!(set_title(&base, &dir, "別の録音").is_err());
        assert!(!dir.join(METADATA_FILE).exists());
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn a_transcript_cut_off_mid_line_still_lists_what_it_has() {
        // A crash leaves the last line half written. Losing the session from
        // the library over it would be the worse failure.
        let base = scratch("truncated");
        let dir = session(&base, "20260819090503", &["mic"], 1);
        std::fs::write(
            dir.join("transcript.jsonl"),
            "{\"type\":\"utterance\",\"id\":1,\"text\":\"おはよう\"}\n{\"type\":\"utter",
        )
        .unwrap();

        let listed = list(&base);
        assert_eq!(listed[0].snippet, "おはよう");
        assert_eq!(listed[0].utterances, 1);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn reading_a_session_puts_both_lanes_in_one_stream_in_spoken_order() {
        // The file interleaves the lanes as they were recognised, which is not
        // the order they were spoken in: the speaker lane's decode landed
        // first even though the mic spoke earlier.
        let base = scratch("read-order");
        let dir = session(&base, "20260819090503", &["mic", "speaker"], 48_000);
        std::fs::write(
            dir.join("transcript.jsonl"),
            [
                r#"{"type":"utterance","id":2,"lane":"speaker","startMs":4000,"endMs":5000,"lang":"en","text":"hello"}"#,
                r#"{"type":"utterance","id":1,"lane":"mic","startMs":1200,"endMs":2200,"lang":"ja","text":"こんにちは"}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let read = read(&dir).unwrap();
        assert_eq!(read.lines.iter().map(|l| l.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(read.lines[0].lane, "mic");
        assert_eq!(read.lines[0].lang.as_deref(), Some("ja"));
        assert_eq!(read.duration_ms, 1_000);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_saved_nemotron_rttm_labels_only_overlapping_utterances() {
        let base = scratch("diarized");
        let dir = session(&base, "20260819090503", &["mic"], 48_000);
        std::fs::write(
            dir.join("transcript.jsonl"),
            [
                r#"{"type":"utterance","id":1,"lane":"mic","startMs":100,"endMs":900,"text":"hello"}"#,
                r#"{"type":"utterance","id":2,"lane":"mic","startMs":900,"endMs":1000,"text":"bye"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::create_dir(dir.join("postprocess")).unwrap();
        std::fs::write(
            dir.join("postprocess/nemotron.rttm"),
            "SPEAKER audio 1 0.100 0.150 <NA> <NA> speaker_0 <NA> <NA>\n\
             SPEAKER audio 1 0.300 0.550 <NA> <NA> speaker_1 <NA> <NA>\n",
        )
        .unwrap();

        let lines = read(&dir).unwrap().lines;
        assert_eq!(lines[0].speaker.as_deref(), Some("speaker_1"));
        assert_eq!(lines[1].speaker, None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_line_two_speakers_share_is_re_recognised_and_re_translated_per_speaker() {
        let base = scratch("split");
        let dir = session(&base, "20260819090503", &["mic"], 5 * 48_000);
        std::fs::write(
            dir.join("transcript.jsonl"),
            [
                r#"{"type":"utterance","id":1,"lane":"mic","startMs":0,"endMs":2000,"lang":"ja","text":"こんにちはさようなら"}"#,
                r#"{"type":"utterance","id":2,"lane":"mic","startMs":2000,"endMs":3000,"lang":"ja","text":"続き"}"#,
                r#"{"type":"utterance","id":3,"lane":"mic","startMs":3000,"endMs":5000,"lang":"ja","text":"元の文"}"#,
                r#"{"type":"translation","id":1,"lang":"en","text":"hello goodbye"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::create_dir(dir.join("postprocess")).unwrap();
        std::fs::write(
            dir.join("postprocess/nemotron.rttm"),
            "SPEAKER audio 1 0.000 1.000 <NA> <NA> speaker_0 <NA> <NA>\n\
             SPEAKER audio 1 1.000 2.000 <NA> <NA> speaker_1 <NA> <NA>\n\
             SPEAKER audio 1 3.000 1.000 <NA> <NA> speaker_0 <NA> <NA>\n\
             SPEAKER audio 1 4.000 1.000 <NA> <NA> speaker_1 <NA> <NA>\n",
        )
        .unwrap();

        // The third line's pieces come back empty, as silence or noise would:
        // the line stays as it was rather than vanish.
        let mut heard = ["こんにちは", "さようなら", "", ""].into_iter();
        crate::postprocess::split(
            &dir,
            &mut |samples: &[f32], lang: Option<&str>| {
                assert!(samples.len() >= 16_000, "whisper skips under a second");
                assert_eq!(lang, Some("ja"));
                Ok(kikimimic_core::Hypothesis {
                    text: heard.next().unwrap().into(),
                    start_ms: 0,
                    end_ms: 0,
                    lang: None,
                })
            },
            &mut |text: &str, source: Option<&str>, target: &str| {
                assert_eq!(source, Some("ja"));
                Some(format!("{target}:{text}"))
            },
        )
        .unwrap();

        let lines: Vec<_> = read(&dir)
            .unwrap()
            .lines
            .into_iter()
            .map(|l| {
                (
                    l.speaker.unwrap(),
                    l.start_ms,
                    l.end_ms,
                    l.text,
                    l.translation,
                )
            })
            .collect();
        let line = |speaker: &str, start, end, text: &str, translation: Option<&str>| {
            (
                speaker.to_string(),
                start,
                end,
                text.to_string(),
                translation.map(str::to_string),
            )
        };
        assert_eq!(
            lines,
            [
                line("speaker_0", 0, 1000, "こんにちは", Some("en:こんにちは")),
                line("speaker_1", 1000, 2000, "さようなら", Some("en:さようなら")),
                line("speaker_1", 2000, 3000, "続き", None),
                line("speaker_0", 3000, 5000, "元の文", None),
            ]
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_translation_rejoins_the_sentence_it_names() {
        // It was appended long after, with another lane's utterance between.
        let base = scratch("read-join");
        let dir = session(&base, "20260819090503", &["mic"], 1);
        std::fs::write(
            dir.join("transcript.jsonl"),
            [
                r#"{"type":"utterance","id":1,"lane":"mic","startMs":0,"text":"おはよう"}"#,
                r#"{"type":"utterance","id":2,"lane":"speaker","startMs":3000,"text":"good morning"}"#,
                r#"{"type":"translation","id":1,"lang":"en","text":"Good morning."}"#,
                r#"{"type":"translation","id":99,"lang":"ja","text":"迷子"}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let read = read(&dir).unwrap();
        assert_eq!(read.lines[0].translation.as_deref(), Some("Good morning."));
        // Nothing was translated for the second one, and the orphan does not
        // become a line of its own.
        assert_eq!(read.lines[1].translation, None);
        assert_eq!(read.lines.len(), 2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_session_recorded_without_a_transcript_reads_as_an_empty_one() {
        // Recording works without the models loaded; the audio is still a
        // session, it just has nothing to say.
        let base = scratch("read-silent");
        let dir = session(&base, "20260819090503", &["mic"], 48_000 * 2);
        let read = read(&dir).unwrap();
        assert!(read.lines.is_empty());
        assert_eq!(read.duration_ms, 2_000);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn reading_something_that_is_not_a_recording_is_an_error() {
        let base = scratch("read-stray");
        std::fs::create_dir_all(base.join("models")).unwrap();
        assert!(read(&base.join("models")).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_directory_with_no_audio_is_not_a_session() {
        let base = scratch("stray");
        std::fs::create_dir_all(base.join("models")).unwrap();
        session(&base, "20260819090503", &["mic"], 1);
        assert_eq!(list(&base).len(), 1);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
