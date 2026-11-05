//! Headless driver: runs the conductor with fixture audio, scripted engines
//! and a virtual clock, deterministically. The core's own tests use it, and so
//! can engine crates.
//!
//! Time model: audio arrives in 10 ms callbacks, the conductor is polled every
//! 100 ms, a recognizer call takes no virtual time, and each translation takes
//! the scenario's `mt_cost`.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::capture::{channel, CaptureWriter, Format};
use crate::conductor::{
    AsrIn, AsrOut, Command, Conductor, Input, MtOut, Output, RecordingTarget, StartSession, POLL,
};
use crate::domain::{HostNanos, LaneId, LaneRole, Lang, LanguagePolicy, Pcm16k, Spoken, StreamMs};
use crate::engine::{AsrEngine, AsrStep, MtEngine, MtRequest, Recognized};
use crate::event::Event;
use crate::glossary::Glossary;

const CALLBACK: Duration = Duration::from_millis(10);
const FIXTURE_RATE: u32 = 48_000;
/// Capture clocks never start at zero; neither does this one.
const HOST_EPOCH: u64 = 1_000_000_000;
/// A scenario that has not gone idle this long after Stop is a bug.
const STOP_GRACE: Duration = Duration::from_secs(60);

/// Audio one lane delivers: a constant level (not digital silence, which
/// would read as a withheld permission) for `len`, from `start`.
#[derive(Clone)]
pub struct Fixture {
    start: Duration,
    len: Duration,
    level: f32,
}

impl Fixture {
    pub fn tone(len: Duration) -> Self {
        Self {
            start: Duration::ZERO,
            len,
            level: 0.1,
        }
    }

    pub fn silence(len: Duration) -> Self {
        Self {
            start: Duration::ZERO,
            len,
            level: 0.0,
        }
    }

    /// The lane's first callback arrives this far into the session.
    pub fn starting_at(mut self, at: Duration) -> Self {
        self.start = at;
        self
    }

    fn plays_at(&self, t: Duration) -> bool {
        t >= self.start && t < self.start + self.len
    }
}

/// A recognizer that "hears" what the script says, timed in the lane's own
/// stream ms. It grows a line's text as volatile while the audio passes
/// through it and commits it once the audio has passed its end.
#[derive(Default)]
pub struct ScriptedAsr {
    lines: Vec<ScriptLine>,
    pushed_ms: HashMap<LaneId, u64>,
}

struct ScriptLine {
    lane: LaneId,
    ms: Range<u64>,
    lang: Lang,
    text: String,
    done: bool,
}

impl ScriptedAsr {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn say(mut self, lane: LaneId, ms: Range<u64>, lang: &str, text: &str) -> Self {
        self.lines.push(ScriptLine {
            lane,
            ms,
            lang: Lang::parse(lang).expect("valid language code"),
            text: text.into(),
            done: false,
        });
        self
    }

    fn finished(line: &mut ScriptLine) -> AsrStep {
        line.done = true;
        AsrStep {
            committed_delta: line.text.clone(),
            volatile: String::new(),
            utterance: Some(Recognized {
                text: line.text.clone(),
                start: StreamMs(line.ms.start),
                end: StreamMs(line.ms.end),
                lang: Some(line.lang.clone()),
            }),
        }
    }
}

impl AsrEngine for ScriptedAsr {
    fn cadence(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn open_lane(&mut self, lane: LaneId) {
        self.pushed_ms.insert(lane, 0);
    }

    fn push(&mut self, lane: LaneId, audio: &Pcm16k) {
        *self.pushed_ms.entry(lane).or_default() += audio.len_ms();
    }

    fn step(&mut self, lane: LaneId, _langs: &Spoken) -> anyhow::Result<Option<AsrStep>> {
        let heard = self.pushed_ms.get(&lane).copied().unwrap_or(0);
        let Some(line) = self.lines.iter_mut().find(|l| l.lane == lane && !l.done) else {
            return Ok(None);
        };
        if heard >= line.ms.end {
            return Ok(Some(Self::finished(line)));
        }
        if heard <= line.ms.start {
            return Ok(None);
        }
        let chars = line.text.chars().count() as u64;
        let shown = chars * (heard - line.ms.start) / (line.ms.end - line.ms.start);
        let volatile: String = line.text.chars().take(shown as usize).collect();
        Ok(Some(AsrStep {
            volatile,
            ..AsrStep::default()
        }))
    }

    fn finish(&mut self, lane: LaneId, _langs: &Spoken) -> anyhow::Result<Vec<AsrStep>> {
        let heard = self.pushed_ms.remove(&lane).unwrap_or(0);
        Ok(self
            .lines
            .iter_mut()
            .filter(|l| l.lane == lane && !l.done && l.ms.start < heard)
            .map(Self::finished)
            .collect())
    }
}

/// Translates by prefixing, after the scenario's virtual cost.
struct EchoMt {
    prefix: String,
}

impl MtEngine for EchoMt {
    fn translate(&mut self, req: &MtRequest<'_>) -> anyhow::Result<String> {
        Ok(format!("{}{}", self.prefix, req.text))
    }
}

pub struct Scenario {
    lanes: Vec<(LaneRole, Fixture)>,
    asr: ScriptedAsr,
    mt: (EchoMt, Duration),
    policy: LanguagePolicy,
    glossary: Glossary,
    stop_at: Duration,
    changes: Vec<(Duration, LanguagePolicy)>,
}

impl Default for Scenario {
    fn default() -> Self {
        Self::new()
    }
}

impl Scenario {
    pub fn new() -> Self {
        Self {
            lanes: Vec::new(),
            asr: ScriptedAsr::new(),
            mt: (
                EchoMt {
                    prefix: String::new(),
                },
                Duration::ZERO,
            ),
            policy: LanguagePolicy::default(),
            glossary: Glossary::default(),
            stop_at: Duration::from_secs(10),
            changes: Vec::new(),
        }
    }

    pub fn lane(mut self, role: LaneRole, fixture: Fixture) -> Self {
        self.lanes.push((role, fixture));
        self
    }

    pub fn asr(mut self, asr: ScriptedAsr) -> Self {
        self.asr = asr;
        self
    }

    /// Translate by prefixing; each sentence takes `cost` of virtual time.
    pub fn mt_prefix(mut self, prefix: &str, cost: Duration) -> Self {
        self.mt = (
            EchoMt {
                prefix: prefix.into(),
            },
            cost,
        );
        self
    }

    pub fn languages(mut self, policy: LanguagePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Change the language settings mid-session, as the UI would.
    pub fn languages_at(mut self, at: Duration, policy: LanguagePolicy) -> Self {
        self.changes.push((at, policy));
        self
    }

    pub fn glossary(mut self, glossary: Glossary) -> Self {
        self.glossary = glossary;
        self
    }

    pub fn stop_at(mut self, at: Duration) -> Self {
        self.stop_at = at;
        self
    }

    pub fn run(mut self) -> Outcome {
        let base = scratch_dir();
        let mut conductor = Conductor::new(self.policy.clone(), self.glossary.clone());
        let mut writers: Vec<(CaptureWriter, Fixture)> = Vec::new();
        let mut streams = Vec::new();
        for (role, fixture) in &self.lanes {
            let (w, s) = channel(
                Format {
                    rate: FIXTURE_RATE,
                    channels: 1,
                },
                Duration::from_secs(8),
            );
            writers.push((w, fixture.clone()));
            streams.push((*role, s));
        }
        let mut events = Vec::new();
        let mut pending_mt: Vec<(Duration, MtOut)> = Vec::new();
        let mut capturing = true;
        let start = StartSession {
            lanes: streams,
            recording: Some(RecordingTarget {
                base: base.clone(),
                name: "session".into(),
            }),
            cadence: self.asr.cadence(),
        };
        let mut inputs = vec![Input::Command(Command::Start(start))];
        let mut t = Duration::ZERO;
        let samples = (FIXTURE_RATE as u128 * CALLBACK.as_millis() / 1000) as usize;
        loop {
            if capturing {
                for (w, fixture) in &mut writers {
                    if fixture.plays_at(t) {
                        let at = HostNanos(HOST_EPOCH + t.as_nanos() as u64);
                        w.push(&vec![fixture.level; samples], at);
                    }
                }
            }
            for (_, policy) in self.changes.iter().filter(|(at, _)| *at == t) {
                inputs.push(Input::Command(Command::SetPolicy(policy.clone())));
            }
            if t == self.stop_at {
                inputs.push(Input::Command(Command::Stop));
            }
            if t.as_millis().is_multiple_of(POLL.as_millis()) {
                inputs.push(Input::Tick);
            }
            let (due, later): (Vec<_>, Vec<_>) = pending_mt.drain(..).partition(|(at, _)| *at <= t);
            pending_mt = later;
            inputs.extend(due.into_iter().map(|(_, out)| Input::Mt(out)));

            while !inputs.is_empty() {
                for input in std::mem::take(&mut inputs) {
                    for output in conductor.handle(t, input) {
                        match output {
                            Output::Emit(e) => events.push(e),
                            Output::ReleaseCapture => capturing = false,
                            Output::Asr(work) => {
                                inputs.extend(run_asr(&mut self.asr, work).map(Input::Asr))
                            }
                            Output::Mt(job) => {
                                let (mt, cost) = &mut self.mt;
                                let req = MtRequest {
                                    text: &job.text,
                                    source: job.source.as_ref(),
                                    target: &job.target,
                                    context: &job.context,
                                    terms: &job.terms,
                                };
                                let result = mt.translate(&req).map_err(|e| format!("{e:#}"));
                                pending_mt.push((t + *cost, MtOut { id: job.id, result }));
                            }
                        }
                    }
                }
            }
            if t > self.stop_at && conductor.is_idle() {
                break;
            }
            assert!(
                t < self.stop_at + STOP_GRACE,
                "the session never went idle after Stop"
            );
            t += CALLBACK;
        }
        Outcome {
            events,
            dir: base.join("session"),
        }
    }
}

/// Only steps and finishes answer; opening a lane and pushing audio don't.
fn run_asr(engine: &mut ScriptedAsr, work: AsrIn) -> Option<AsrOut> {
    let err = |e: anyhow::Error| format!("{e:#}");
    match work {
        AsrIn::Open(lane) => {
            engine.open_lane(lane);
            None
        }
        AsrIn::Audio(lane, audio) => {
            engine.push(lane, &audio);
            None
        }
        AsrIn::Step(lane, langs) => Some(AsrOut::Stepped(
            lane,
            engine.step(lane, &langs).map_err(err),
        )),
        AsrIn::Finish(lane, langs) => Some(AsrOut::Finished(
            lane,
            engine.finish(lane, &langs).map_err(err),
        )),
    }
}

fn scratch_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kikimimic-sim-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// What a scenario produced: the UI's event stream and the session directory.
pub struct Outcome {
    pub events: Vec<Event>,
    pub dir: PathBuf,
}

impl Outcome {
    /// `transcript.jsonl`, one parsed value per line.
    pub fn journal(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.join("transcript.jsonl"))
            .expect("the session wrote a transcript")
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect()
    }

    /// Every lane's wav and the mix end at the same moment.
    pub fn assert_wavs_share_one_length(&self) {
        let lengths: Vec<(String, u32)> = std::fs::read_dir(&self.dir)
            .expect("session dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .map(|p| {
                (
                    file_stem(&p),
                    hound::WavReader::open(&p).expect("wav").duration(),
                )
            })
            .collect();
        assert!(lengths.len() >= 2, "no wavs in {}", self.dir.display());
        assert!(lengths.windows(2).all(|w| w[0].1 == w[1].1), "{lengths:?}");
    }
}

impl Drop for Outcome {
    fn drop(&mut self) {
        if let Some(base) = self.dir.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

fn file_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
