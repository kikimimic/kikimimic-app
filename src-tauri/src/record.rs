//! Writes the session's audio to disk: one WAV per capture lane plus their
//! mix, under `<base>/<yyyyMMddHHmmss>/`.
//!
//! Fed straight from the capture drain, *before* VAD and the silence gate, so
//! the files hold what the devices actually heard rather than what the
//! recognizer chose to look at.
//!
//! All three files share one timeline, whose zero is the session's first
//! captured moment. The caller places every chunk by the capture timestamp
//! the OS stamped it with (`pipeline::SessionClock`), so a lane that opened
//! late or delivered nothing for a while gets silence where it was absent —
//! no lane is another's clock.
//!
//! Recording never takes the session down with it. A failure while writing
//! (disk full, volume ejected) stops the recording and is reported once; the
//! transcript keeps running.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kikimimic_core::mix::Mixer;
use kikimimic_core::resample::StreamResampler;

/// One rate for every file we write, so the lanes and their mix share a
/// format regardless of what each device runs at. 48 kHz is what both macOS
/// capture paths already use, making the resamplers pass-throughs in practice.
pub const RECORD_RATE: u32 = 48_000;

/// How far behind the furthest lane the mix stays. The lanes are polled one
/// after another with decodes in between, so one can be a second or two later
/// than the other in *arriving* — this is the room that leaves for its audio
/// to be written where its timestamp says it belongs.
const MIX_LAG_SAMPLES: usize = 2 * RECORD_RATE as usize;

/// Gap below which a lane is taken to be continuous. Capture timestamps
/// wobble by a sample or two and the resampler holds up to ~23ms of input
/// back, so a small discrepancy is bookkeeping, not a dropout. Absolute
/// positions mean this never accumulates: the next chunk is placed by its own
/// timestamp regardless.
const SNAP_SAMPLES: usize = 3 * RECORD_RATE as usize / 100;

/// One finished utterance as the transcript records it. `id` is what a
/// translation arriving later names to find its way back to this line.
pub struct Utterance<'a> {
    pub id: u64,
    pub lane: &'a str,
    pub start_ms: u64,
    pub end_ms: u64,
    /// What it was recognised as; absent when detection was unconfident.
    pub lang: Option<&'a str>,
    pub text: &'a str,
}

/// What the recorder needs to know about one capture lane.
pub struct LaneSpec {
    /// File stem: `mic` → `mic.wav`.
    pub name: &'static str,
    pub src_rate: u32,
}

type Wav = hound::WavWriter<BufWriter<File>>;

struct LaneWriter {
    writer: Wav,
    /// Only resamples when the device disagrees with [`RECORD_RATE`].
    resampler: StreamResampler,
    /// Samples in this lane's file, silence included.
    written: usize,
}

pub struct SessionRecorder {
    dir: PathBuf,
    lanes: Vec<LaneWriter>,
    mix: Wav,
    mixer: Mixer,
    /// `transcript.jsonl`: the utterances, on the same clock as the audio.
    transcript: BufWriter<File>,
    /// Set by the first write failure; recording stops there.
    failure: Option<String>,
    /// Cleared once the failure has been reported to the UI.
    unreported: bool,
}

/// The directory name for a session started at `now`, in local time — this is
/// what the user scans in Finder, so it follows their clock, not UTC.
pub fn session_name(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("%Y%m%d%H%M%S").to_string()
}

/// Create `<base>/<name>`, adding a `-2`, `-3`, … suffix rather than reusing a
/// directory: stopping and restarting within the same second must not
/// overwrite the recording that just ended.
fn create_session_dir(base: &Path, name: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(base)
        .with_context(|| format!("create recording directory {}", base.display()))?;
    for attempt in 1..100 {
        let dir = base.join(match attempt {
            1 => name.to_string(),
            n => format!("{name}-{n}"),
        });
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", dir.display())),
        }
    }
    anyhow::bail!("no free recording directory under {}", base.display())
}

fn wav_spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: RECORD_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn create_wav(dir: &Path, stem: &str) -> Result<Wav> {
    let path = dir.join(format!("{stem}.wav"));
    hound::WavWriter::create(&path, wav_spec())
        .with_context(|| format!("create {}", path.display()))
}

/// Silence a lane owes the timeline: the stretch where it delivered nothing,
/// or the head start the lanes that opened earlier had on it.
fn write_silence(writer: &mut Wav, samples: usize) -> Result<()> {
    for _ in 0..samples {
        writer.write_sample(0i16)?;
    }
    Ok(())
}

fn write_samples(writer: &mut Wav, samples: &[f32]) -> Result<()> {
    for s in samples {
        // Lanes are summed for the mix, so this can exceed full scale when
        // both sides are loud at once; clamping distorts that moment instead
        // of wrapping it into noise.
        let clamped = s.clamp(-1.0, 1.0);
        writer.write_sample((clamped * i16::MAX as f32) as i16)?;
    }
    Ok(())
}

impl SessionRecorder {
    pub fn open(base: &Path, name: &str, specs: &[LaneSpec]) -> Result<Self> {
        let dir = create_session_dir(base, name)?;
        let mut lanes = Vec::with_capacity(specs.len());
        for spec in specs {
            lanes.push(LaneWriter {
                writer: create_wav(&dir, spec.name)?,
                resampler: StreamResampler::to(spec.src_rate, RECORD_RATE)?,
                written: 0,
            });
        }
        let transcript = File::create(dir.join("transcript.jsonl"))
            .with_context(|| format!("create transcript in {}", dir.display()))?;
        Ok(Self {
            mix: create_wav(&dir, "mix")?,
            transcript: BufWriter::new(transcript),
            dir,
            mixer: Mixer::new(lanes.len()),
            lanes,
            failure: None,
            unreported: false,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one lane's captured audio (mono, at that lane's device rate),
    /// which the OS timestamped as belonging at `at` in the recording.
    pub fn write_at(&mut self, lane: usize, samples: &[f32], at: usize) {
        if self.failure.is_some() || samples.is_empty() {
            return;
        }
        if let Err(e) = self.try_write(lane, samples, at) {
            self.fail(e);
        }
    }

    fn try_write(&mut self, lane: usize, samples: &[f32], at: usize) -> Result<()> {
        let lane_writer = &mut self.lanes[lane];
        let resampled = lane_writer.resampler.process(samples)?;
        if resampled.is_empty() {
            return Ok(());
        }
        let start = if at.saturating_sub(lane_writer.written) < SNAP_SAMPLES {
            lane_writer.written
        } else {
            at
        };
        // The mixer has the final say: it cannot go back to a moment already
        // written out. The lane's own file follows that decision, silence and
        // all, so the files stay in step with each other.
        let skipped = self.mixer.push_at(lane, &resampled, start);
        lane_writer.written += skipped + resampled.len();
        write_silence(&mut lane_writer.writer, skipped)?;
        write_samples(&mut lane_writer.writer, &resampled)
    }

    /// Append one finished utterance to `transcript.jsonl`, timed against the
    /// recording. Flushed per line: a crash should cost the last utterance,
    /// not the session's transcript.
    pub fn write_utterance(&mut self, utterance: &Utterance<'_>) {
        self.write_line(serde_json::json!({
            "type": "utterance",
            "id": utterance.id,
            "lane": utterance.lane,
            "startMs": utterance.start_ms,
            "endMs": utterance.end_ms,
            "lang": utterance.lang,
            "text": utterance.text,
        }));
    }

    /// Append a translation, naming the utterance it belongs to. It arrives
    /// seconds later than the sentence and out of order between lanes, so the
    /// file records the link rather than the position.
    pub fn write_translation(&mut self, id: u64, lang: &str, text: &str) {
        self.write_line(serde_json::json!({
            "type": "translation",
            "id": id,
            "lang": lang,
            "text": text,
        }));
    }

    fn write_line(&mut self, line: serde_json::Value) {
        if self.failure.is_some() {
            return;
        }
        if let Err(e) = writeln!(self.transcript, "{line}").and_then(|()| self.transcript.flush()) {
            self.fail(anyhow::Error::new(e).context("write transcript"));
        }
    }

    /// Write the part of the mix that no lane can still contribute to. Call
    /// once per poll, after every lane has been written.
    pub fn pump_mix(&mut self) {
        if self.failure.is_some() {
            return;
        }
        let mixed = self.mixer.drain(MIX_LAG_SAMPLES);
        if let Err(e) = write_samples(&mut self.mix, &mixed) {
            self.fail(e);
        }
    }

    /// The write failure that stopped the recording, reported once so it does
    /// not bury later status messages.
    pub fn take_failure(&mut self) -> Option<String> {
        if !self.unreported {
            return None;
        }
        self.unreported = false;
        self.failure.clone()
    }

    fn fail(&mut self, e: anyhow::Error) {
        self.failure = Some(format!("{e:#}"));
        self.unreported = true;
    }

    /// Flush what the resamplers still hold, then close every file so the WAV
    /// headers get their final sizes.
    pub fn finish(mut self) -> Result<()> {
        let flush = self.flush_tails();
        let Self { lanes, mix, .. } = self;
        let mut result = flush;
        for lane in lanes {
            let closed = lane.writer.finalize().context("close lane recording");
            result = result.and(closed);
        }
        result.and(mix.finalize().context("close mix recording"))
    }

    fn flush_tails(&mut self) -> Result<()> {
        if self.failure.is_some() {
            // Already failed — and reported — mid-session: nothing more may
            // be written, but the files still get their headers closed.
            // Bailing here would resurrect an old failure as a session error.
            return Ok(());
        }
        for (index, lane) in self.lanes.iter_mut().enumerate() {
            let tail = lane.resampler.flush()?;
            let skipped = self.mixer.push_at(index, &tail, lane.written);
            lane.written += skipped + tail.len();
            write_silence(&mut lane.writer, skipped)?;
            write_samples(&mut lane.writer, &tail)?;
        }
        let mixed = self.mixer.finish();
        write_samples(&mut self.mix, &mixed)?;
        // Bring every lane up to the mix's length. A lane that went quiet at
        // the end would otherwise stop short of it, and anything reading the
        // files side by side would have to special-case that.
        let total = self.mixer.position();
        for lane in self.lanes.iter_mut() {
            let owed = total.saturating_sub(lane.written);
            lane.written += owed;
            write_silence(&mut lane.writer, owed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Unique scratch directory; removed by the test that made it.
    fn scratch(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kkm-record-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn spec(name: &'static str, src_rate: u32) -> LaneSpec {
        LaneSpec { name, src_rate }
    }

    /// Every sample sits at `expected` full-scale, within i16 rounding.
    fn assert_level(samples: &[i16], expected: f32) {
        for s in samples {
            let level = *s as f32 / i16::MAX as f32;
            assert!(
                (level - expected).abs() < 0.001,
                "{level} is not {expected}"
            );
        }
    }

    fn read(path: &Path) -> (hound::WavSpec, Vec<i16>) {
        let mut reader = hound::WavReader::open(path).expect("open wav");
        let spec = reader.spec();
        let samples = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        (spec, samples)
    }

    #[test]
    fn session_name_is_the_local_wall_clock() {
        let t = chrono::Local
            .with_ymd_and_hms(2026, 8, 19, 9, 5, 3)
            .unwrap();
        assert_eq!(session_name(t), "20260819090503");
    }

    #[test]
    fn writes_one_wav_per_lane_plus_their_mix() {
        let base = scratch("layout");
        let mut rec = SessionRecorder::open(
            &base,
            "20260819090503",
            &[spec("mic", 48_000), spec("speaker", 48_000)],
        )
        .unwrap();
        assert_eq!(rec.dir(), base.join("20260819090503"));
        rec.write_at(0, &[0.5; 4], 0);
        rec.write_at(1, &[0.25; 4], 0);
        rec.pump_mix();
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();

        let (spec, mic) = read(&dir.join("mic.wav"));
        assert_eq!((spec.channels, spec.sample_rate), (1, RECORD_RATE));
        assert_eq!(mic.len(), 4);
        let (_, mix) = read(&dir.join("mix.wav"));
        // 0.5 + 0.25 on every sample: the mix is the lanes summed, not one of
        // them copied.
        assert_eq!(mix.len(), 4);
        assert_level(&mix, 0.75);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_lane_at_another_rate_is_recorded_at_the_record_rate() {
        let base = scratch("resample");
        let mut rec = SessionRecorder::open(&base, "s", &[spec("mic", 16_000)]).unwrap();
        rec.write_at(0, &vec![0.1; 16_000], 0); // one second
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let (spec, mic) = read(&dir.join("mic.wav"));
        assert_eq!(spec.sample_rate, RECORD_RATE);
        // A second in, a second out. The flush drives the sinc filter's delay
        // line out with silence, so the file ends with up to ~90ms of it —
        // nothing is missing, which is what this pins down.
        let extra = mic.len() as i64 - RECORD_RATE as i64;
        assert!(
            (0..RECORD_RATE as i64 / 10).contains(&extra),
            "got {} samples",
            mic.len()
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_lane_that_opened_late_starts_with_the_silence_it_missed() {
        // The tap's first buffer is timestamped half a second into the
        // session; the file's zero is the session's zero, like every other
        // file here.
        let base = scratch("offset");
        let mut rec = SessionRecorder::open(
            &base,
            "s",
            &[spec("mic", RECORD_RATE), spec("speaker", RECORD_RATE)],
        )
        .unwrap();
        let half_a_second = RECORD_RATE as usize / 2;
        rec.write_at(1, &[0.5; 10], half_a_second);
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let (_, speaker) = read(&dir.join("speaker.wav"));
        assert_level(&speaker[..half_a_second], 0.0);
        assert_level(&speaker[half_a_second..half_a_second + 10], 0.5);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_lane_delivering_nothing_is_padded_to_keep_up_with_the_others() {
        // The speaker tap goes quiet — not silent, *absent* — while nothing
        // plays. Writing its next audio straight after the last would pull the
        // whole file forward and desync it from the mic.
        let base = scratch("gap");
        let mut rec = SessionRecorder::open(
            &base,
            "s",
            &[spec("mic", RECORD_RATE), spec("speaker", RECORD_RATE)],
        )
        .unwrap();
        rec.write_at(0, &vec![0.1; 30_000], 0);
        rec.pump_mix();
        rec.write_at(1, &[0.5; 10], 6_000); // the tap finally delivers
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let (_, speaker) = read(&dir.join("speaker.wav"));
        assert_level(&speaker[..6_000], 0.0);
        assert_level(&speaker[6_000..6_010], 0.5);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn every_file_ends_at_the_same_moment() {
        // The speaker lane trails the mic by the mix's lag while it is silent;
        // that must not leave its file short at the end of the session.
        let base = scratch("lengths");
        let mut rec = SessionRecorder::open(
            &base,
            "s",
            &[spec("mic", RECORD_RATE), spec("speaker", RECORD_RATE)],
        )
        .unwrap();
        rec.write_at(0, &vec![0.1; 30_000], 0);
        rec.pump_mix();
        rec.write_at(1, &[0.5; 10], 6_000);
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let lengths: Vec<usize> = ["mic", "speaker", "mix"]
            .iter()
            .map(|stem| read(&dir.join(format!("{stem}.wav"))).1.len())
            .collect();
        assert_eq!(lengths, vec![30_000, 30_000, 30_000]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    fn utterance(
        id: u64,
        lane: &'static str,
        start_ms: u64,
        text: &'static str,
    ) -> Utterance<'static> {
        Utterance {
            id,
            lane,
            start_ms,
            end_ms: start_ms + 1_000,
            lang: Some("ja"),
            text,
        }
    }

    fn read_jsonl(dir: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.join("transcript.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect()
    }

    #[test]
    fn the_transcript_lands_next_to_the_audio() {
        let base = scratch("transcript");
        let mut rec = SessionRecorder::open(&base, "s", &[spec("mic", RECORD_RATE)]).unwrap();
        rec.write_utterance(&utterance(1, "mic", 1_200, "こんにちは"));
        rec.write_utterance(&utterance(2, "speaker", 4_000, "hello"));
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();

        let lines = read_jsonl(&dir);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "utterance");
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[0]["lane"], "mic");
        assert_eq!(lines[0]["startMs"], 1_200);
        assert_eq!(lines[0]["endMs"], 2_200);
        assert_eq!(lines[0]["lang"], "ja");
        assert_eq!(lines[0]["text"], "こんにちは");
        assert_eq!(lines[1]["lane"], "speaker");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_translation_is_appended_when_it_arrives_and_names_its_utterance() {
        // Translations land seconds after the sentence they belong to, and
        // out of order between lanes. The id is what puts them back together.
        let base = scratch("translation");
        let mut rec = SessionRecorder::open(&base, "s", &[spec("mic", RECORD_RATE)]).unwrap();
        rec.write_utterance(&utterance(1, "mic", 0, "おはようございます"));
        rec.write_utterance(&utterance(2, "speaker", 3_000, "good morning"));
        rec.write_translation(2, "ja", "おはようございます");
        rec.write_translation(1, "en", "Good morning.");
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();

        let lines = read_jsonl(&dir);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2]["type"], "translation");
        assert_eq!(lines[2]["id"], 2);
        assert_eq!(lines[2]["lang"], "ja");
        assert_eq!(lines[2]["text"], "おはようございます");
        assert_eq!(lines[3]["id"], 1);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn finish_after_a_reported_failure_closes_the_files_quietly() {
        // The failure already stopped the recording and was reported once;
        // Stop must not resurrect it as a session error — the transcript
        // that kept running was fine, and the files written so far deserve
        // readable headers.
        let base = scratch("failed");
        let mut rec = SessionRecorder::open(&base, "s", &[spec("mic", RECORD_RATE)]).unwrap();
        rec.write_at(0, &[0.5; 4], 0);
        rec.fail(anyhow::anyhow!("disk full"));
        assert_eq!(rec.take_failure().as_deref(), Some("disk full"));
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let (_, mic) = read(&dir.join("mic.wav"));
        assert_eq!(mic.len(), 4);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_restart_within_the_same_second_gets_its_own_directory() {
        let base = scratch("collision");
        let first = SessionRecorder::open(&base, "20260819090503", &[spec("mic", 48_000)]).unwrap();
        let second =
            SessionRecorder::open(&base, "20260819090503", &[spec("mic", 48_000)]).unwrap();
        assert_ne!(first.dir(), second.dir());
        assert_eq!(second.dir(), base.join("20260819090503-2"));
        first.finish().unwrap();
        second.finish().unwrap();
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_mix_holds_back_a_tail_until_the_session_ends() {
        // Otherwise a lane still delivering its share of that moment would
        // find the moment already written.
        let base = scratch("lag");
        let mut rec =
            SessionRecorder::open(&base, "s", &[spec("mic", 48_000), spec("speaker", 48_000)])
                .unwrap();
        rec.write_at(0, &vec![0.5; 1000], 0);
        rec.pump_mix(); // inside the lag: nothing may go out yet
        rec.write_at(1, &vec![0.25; 1000], 0);
        let dir = rec.dir().to_path_buf();
        rec.finish().unwrap();
        let (_, mix) = read(&dir.join("mix.wav"));
        // Had the mix gone out at pump time, lane 1 would have been appended
        // after it: 2000 samples of one lane each instead of 1000 of both.
        assert_eq!(mix.len(), 1000);
        assert_level(&mix, 0.75);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
