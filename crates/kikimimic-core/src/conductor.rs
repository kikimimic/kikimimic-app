//! The single owner of a session's domain state, as a state machine:
//! `handle(now, input) -> outputs`. It never calls an engine or sleeps;
//! whoever drives it (the threaded runtime, or `sim`) executes the `Asr`/`Mt`
//! outputs and feeds the answers back. That is what makes a whole session
//! reproducible in a test.

pub(crate) mod lane;
pub(crate) mod ledger;
pub(crate) mod notices;
pub(crate) mod record;

use std::path::PathBuf;
use std::time::Duration;

use lane::{Lane, SessionClock};
use ledger::{Ledger, MtJob};
use notices::{NoticeKey, Notices};
use record::SessionRecorder;

use crate::capture::CaptureStream;
use crate::domain::{lane_labels, LaneId, LaneRole, LanguagePolicy, Pcm16k, RecordingMs, Spoken};
use crate::engine::AsrStep;
use crate::event::{Event, Notice, Phase, StatusView, UtteranceView};
use crate::glossary::Glossary;

/// How often a live session is polled.
pub(crate) const POLL: Duration = Duration::from_millis(100);
/// How long Stop waits for outstanding translations.
pub(crate) const TRANSLATE_DRAIN: Duration = Duration::from_secs(10);

pub(crate) struct StartSession {
    pub lanes: Vec<(LaneRole, CaptureStream)>,
    pub recording: Option<RecordingTarget>,
    /// The ASR engine's step cadence.
    pub cadence: Duration,
}

/// Where a session's files go: `<base>/<name>[-n]/`.
#[derive(Clone, Debug)]
pub struct RecordingTarget {
    pub base: PathBuf,
    pub name: String,
}

pub(crate) enum Command {
    Start(StartSession),
    Stop,
    SetPolicy(LanguagePolicy),
    SetGlossary(Glossary),
}

pub(crate) enum Input {
    Command(Command),
    Asr(AsrOut),
    Mt(MtOut),
    Tick,
}

#[derive(Debug)]
pub(crate) enum Output {
    Emit(Event),
    Asr(AsrIn),
    Mt(MtJob),
    /// The session no longer reads its capture streams; stop the devices.
    ReleaseCapture,
}

#[derive(Debug)]
pub(crate) enum AsrIn {
    Open(LaneId),
    Audio(LaneId, Pcm16k),
    Step(LaneId, Spoken),
    Finish(LaneId, Spoken),
}

pub(crate) enum AsrOut {
    Stepped(LaneId, Result<Option<AsrStep>, String>),
    Finished(LaneId, Result<Vec<AsrStep>, String>),
}

pub(crate) struct MtOut {
    pub id: crate::domain::UtteranceId,
    pub result: Result<String, String>,
}

struct Live {
    lanes: Vec<Lane>,
    clock: SessionClock,
    recorder: Option<SessionRecorder>,
    recording_dir: Option<PathBuf>,
    cadence: Duration,
}

enum SessionPhase {
    Idle,
    Listening(Live),
    /// Lanes flushed; waiting for the recognizer to finish them.
    Finishing {
        live: Live,
        pending: Vec<LaneId>,
    },
    /// Waiting for owed translations until the deadline.
    Draining {
        live: Live,
        deadline: Duration,
    },
}

pub(crate) struct Conductor {
    policy: LanguagePolicy,
    glossary: Glossary,
    ledger: Ledger,
    notices: Notices,
    phase: SessionPhase,
    /// A lane's stream failed: the session stops, and reports this once idle.
    error_after_stop: Option<String>,
}

impl Conductor {
    pub fn new(policy: LanguagePolicy, glossary: Glossary) -> Self {
        Self {
            policy,
            glossary,
            ledger: Ledger::default(),
            notices: Notices::default(),
            phase: SessionPhase::Idle,
            error_after_stop: None,
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.phase, SessionPhase::Idle)
    }

    pub fn handle(&mut self, now: Duration, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        match input {
            Input::Command(Command::Start(start)) => self.start(now, start, &mut out),
            Input::Command(Command::Stop) => self.stop(now, &mut out),
            Input::Command(Command::SetPolicy(p)) => self.policy = p,
            Input::Command(Command::SetGlossary(g)) => self.glossary = g,
            Input::Tick => self.tick(now, &mut out),
            Input::Asr(AsrOut::Stepped(lane, result)) => self.stepped(lane, result, &mut out),
            Input::Asr(AsrOut::Finished(lane, result)) => {
                self.finished(now, lane, result, &mut out)
            }
            Input::Mt(MtOut { id, result }) => {
                if let Some(event) = self.ledger.settle(id, result) {
                    self.journal_translation(&event);
                    out.push(Output::Emit(event));
                }
                self.dispatch_mt(&mut out);
                self.maybe_done(now, &mut out);
            }
        }
        if self.notices.take_changed() && !self.is_idle() {
            out.push(Output::Emit(self.status()));
        }
        out
    }

    fn start(&mut self, now: Duration, start: StartSession, out: &mut Vec<Output>) {
        if !self.is_idle() {
            return;
        }
        let roles: Vec<LaneRole> = start.lanes.iter().map(|(r, _)| *r).collect();
        let labels = lane_labels(&roles);
        let mut lanes = Vec::new();
        for (i, ((role, stream), label)) in start.lanes.into_iter().zip(labels).enumerate() {
            match Lane::new(LaneId(i as u8), label, role, stream, now + start.cadence) {
                Ok(lane) => lanes.push(lane),
                Err(e) => {
                    self.notices.set(
                        NoticeKey::Source(role),
                        Some(Notice::SourceUnavailable {
                            role,
                            reason: format!("{e:#}"),
                        }),
                    );
                }
            }
        }
        let recorder = start.recording.and_then(|t| {
            let specs: Vec<_> = lanes
                .iter()
                .map(|l| record::LaneSpec {
                    name: l.label.as_str().to_string(),
                    src_rate: l.src_rate(),
                })
                .collect();
            match SessionRecorder::open(&t.base, &t.name, &specs) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.notices.set(
                        NoticeKey::Recording,
                        Some(Notice::RecordingOff {
                            reason: format!("{e:#}"),
                        }),
                    );
                    None
                }
            }
        });
        for lane in &lanes {
            out.push(Output::Asr(AsrIn::Open(lane.id)));
        }
        let recording_dir = recorder.as_ref().map(|r| r.dir().to_path_buf());
        self.phase = SessionPhase::Listening(Live {
            lanes,
            clock: SessionClock::default(),
            recorder,
            recording_dir,
            cadence: start.cadence,
        });
        self.notices.take_changed();
        out.push(Output::Emit(self.status()));
    }

    fn tick(&mut self, now: Duration, out: &mut Vec<Output>) {
        let mut failed = None;
        match &mut self.phase {
            SessionPhase::Listening(live) => {
                let spoken = self.policy.spoken.clone();
                for (i, lane) in live.lanes.iter_mut().enumerate() {
                    match lane.pump(
                        now,
                        i,
                        &mut live.clock,
                        live.recorder.as_mut(),
                        &mut self.notices,
                    ) {
                        Ok(Some(audio)) => out.push(Output::Asr(AsrIn::Audio(lane.id, audio))),
                        Ok(None) => {}
                        Err(e) => failed = Some(format!("{e:#}")),
                    }
                    if !lane.stepping && now >= lane.next_step {
                        // Start to start: a slow decode eats into the next
                        // wait instead of stacking on it.
                        lane.stepping = true;
                        lane.next_step = now + live.cadence;
                        out.push(Output::Asr(AsrIn::Step(lane.id, spoken.clone())));
                    }
                }
                if let Some(rec) = live.recorder.as_mut() {
                    rec.pump_mix();
                    if let Some(reason) = rec.take_failure() {
                        self.notices
                            .set(NoticeKey::Recording, Some(Notice::RecordingOff { reason }));
                    }
                }
            }
            SessionPhase::Draining { deadline, .. } if now >= *deadline => {
                for event in self.ledger.abandon_all() {
                    out.push(Output::Emit(event));
                }
                self.maybe_done(now, out);
            }
            _ => {}
        }
        if let Some(message) = failed {
            self.error_after_stop = Some(message);
            self.stop(now, out);
        }
    }

    fn stop(&mut self, now: Duration, out: &mut Vec<Output>) {
        let SessionPhase::Listening(mut live) =
            std::mem::replace(&mut self.phase, SessionPhase::Idle)
        else {
            return;
        };
        let spoken = self.policy.spoken.clone();
        for (i, lane) in live.lanes.iter_mut().enumerate() {
            if let Ok(Some(audio)) = lane.pump(
                now,
                i,
                &mut live.clock,
                live.recorder.as_mut(),
                &mut self.notices,
            ) {
                out.push(Output::Asr(AsrIn::Audio(lane.id, audio)));
            }
            if let Ok(Some(tail)) = lane.flush() {
                out.push(Output::Asr(AsrIn::Audio(lane.id, tail)));
            }
            out.push(Output::Asr(AsrIn::Finish(lane.id, spoken.clone())));
        }
        out.push(Output::ReleaseCapture);
        let pending = live.lanes.iter().map(|l| l.id).collect();
        self.phase = SessionPhase::Finishing { live, pending };
        out.push(Output::Emit(self.status()));
        self.maybe_done(now, out);
    }

    fn stepped(
        &mut self,
        id: LaneId,
        result: Result<Option<AsrStep>, String>,
        out: &mut Vec<Output>,
    ) {
        let Some(lane_index) = self.lane_index(id) else {
            return;
        };
        match result {
            Ok(step) => {
                self.notices.set(NoticeKey::Decode(id), None);
                if let Some(step) = step {
                    self.deliver(lane_index, step, out);
                }
            }
            Err(reason) => {
                let label = self.live_mut_any().expect("lane exists").lanes[lane_index]
                    .label
                    .clone();
                self.notices.set(
                    NoticeKey::Decode(id),
                    Some(Notice::DecodeFailed {
                        lane: label,
                        reason,
                    }),
                );
            }
        }
        if let Some(live) = self.live_mut_any() {
            live.lanes[lane_index].stepping = false;
        }
        self.dispatch_mt(out);
    }

    fn finished(
        &mut self,
        now: Duration,
        id: LaneId,
        result: Result<Vec<AsrStep>, String>,
        out: &mut Vec<Output>,
    ) {
        let Some(lane_index) = self.lane_index(id) else {
            return;
        };
        match result {
            Ok(steps) => {
                for step in steps {
                    self.deliver(lane_index, step, out);
                }
            }
            Err(reason) => {
                let label = self.live_mut_any().expect("lane exists").lanes[lane_index]
                    .label
                    .clone();
                self.notices.set(
                    NoticeKey::Decode(id),
                    Some(Notice::DecodeFailed {
                        lane: label,
                        reason,
                    }),
                );
            }
        }
        let live = self.live_mut_any().expect("lane exists");
        let lane = &mut live.lanes[lane_index];
        // A tail can still be on screen when finish had nothing to commit.
        if lane.gate.volatile_on_screen {
            lane.gate.volatile_on_screen = false;
            let utterance = lane.current.take();
            if let Some(utterance) = utterance {
                out.push(Output::Emit(Event::Transcript {
                    lane: lane.label.clone(),
                    utterance,
                    committed_delta: String::new(),
                    volatile: String::new(),
                    finished: None,
                }));
            }
        }
        if let SessionPhase::Finishing { pending, .. } = &mut self.phase {
            pending.retain(|l| *l != id);
            if pending.is_empty() {
                let SessionPhase::Finishing { live, .. } =
                    std::mem::replace(&mut self.phase, SessionPhase::Idle)
                else {
                    unreachable!()
                };
                self.phase = SessionPhase::Draining {
                    live,
                    deadline: now + TRANSLATE_DRAIN,
                };
            }
        }
        self.dispatch_mt(out);
        self.maybe_done(now, out);
    }

    /// Send a step to the UI; a finished utterance also to the transcript
    /// file and the translation queue.
    fn deliver(&mut self, lane_index: usize, mut step: AsrStep, out: &mut Vec<Output>) {
        let policy = self.policy.clone();
        let terms = self.glossary.terms();
        let glossary = self.glossary.clone();
        let Some(live) = live_of(&mut self.phase) else {
            return;
        };
        let lane = &mut live.lanes[lane_index];
        if !lane.gate.should_emit(&step) {
            return;
        }
        let utterance = match lane.current {
            Some(id) => id,
            None => {
                let id = self.ledger.allocate();
                lane.current = Some(id);
                id
            }
        };
        let finished = step.utterance.take().map(|u| {
            lane.current = None;
            let text = glossary.apply(&u.text);
            let target = policy.target_for(u.lang.as_ref()).cloned();
            let translating = target.is_some();
            let start = RecordingMs(lane.timeline.recording_ms(u.start));
            let end = RecordingMs(lane.timeline.recording_ms(u.end));
            let evicted =
                self.ledger
                    .finalize(utterance, &lane.label, &text, u.lang.clone(), target, terms);
            for event in evicted {
                out.push(Output::Emit(event));
            }
            if let Some(rec) = live.recorder.as_mut() {
                rec.write_utterance(&record::Utterance {
                    id: utterance.0,
                    lane: lane.label.as_str(),
                    start_ms: start.0,
                    end_ms: end.0,
                    lang: u.lang.as_ref().map(|l| l.as_str()),
                    text: &text,
                });
            }
            UtteranceView {
                id: utterance,
                text,
                start,
                end,
                lang: u.lang,
                translating,
            }
        });
        out.push(Output::Emit(Event::Transcript {
            lane: lane.label.clone(),
            utterance,
            committed_delta: step.committed_delta,
            volatile: step.volatile,
            finished,
        }));
    }

    fn dispatch_mt(&mut self, out: &mut Vec<Output>) {
        if let Some(job) = self.ledger.next_job() {
            out.push(Output::Mt(job));
        }
    }

    fn journal_translation(&mut self, event: &Event) {
        let Event::Translation {
            id,
            outcome: crate::event::TranslationOutcome::Text { text, lang },
            ..
        } = event
        else {
            return;
        };
        if let Some(rec) = self.live_mut_any().and_then(|l| l.recorder.as_mut()) {
            rec.write_translation(id.0, lang.as_str(), text);
        }
    }

    /// Draining ends once nothing is owed; close the files and go idle.
    fn maybe_done(&mut self, _now: Duration, out: &mut Vec<Output>) {
        if !matches!(self.phase, SessionPhase::Draining { .. }) || !self.ledger.is_settled() {
            return;
        }
        let SessionPhase::Draining { live, .. } =
            std::mem::replace(&mut self.phase, SessionPhase::Idle)
        else {
            unreachable!()
        };
        let error = self.error_after_stop.take();
        if let Some(Err(e)) = live.recorder.map(SessionRecorder::finish) {
            self.notices.set(
                NoticeKey::Recording,
                Some(Notice::RecordingOff {
                    reason: format!("{e:#}"),
                }),
            );
        }
        self.notices.clear();
        self.notices.take_changed();
        out.push(Output::Emit(Event::Status(StatusView {
            phase: match error {
                Some(message) => Phase::Error { message },
                None => Phase::Idle,
            },
            notices: Vec::new(),
            recording_dir: live.recording_dir,
        })));
    }

    fn status(&self) -> Event {
        let (phase, recording_dir) = match &self.phase {
            SessionPhase::Idle => (Phase::Idle, None),
            SessionPhase::Listening(l) => (Phase::Listening, l.recording_dir.clone()),
            SessionPhase::Finishing { live, .. } | SessionPhase::Draining { live, .. } => {
                (Phase::Stopping, live.recording_dir.clone())
            }
        };
        Event::Status(StatusView {
            phase,
            notices: self.notices.list(),
            recording_dir,
        })
    }

    fn live_mut_any(&mut self) -> Option<&mut Live> {
        live_of(&mut self.phase)
    }

    fn lane_index(&mut self, id: LaneId) -> Option<usize> {
        self.live_mut_any()?.lanes.iter().position(|l| l.id == id)
    }
}

/// The live session, whatever stage of stopping it is in. A free function so
/// callers can borrow the phase and the ledger at the same time.
fn live_of(phase: &mut SessionPhase) -> Option<&mut Live> {
    match phase {
        SessionPhase::Idle => None,
        SessionPhase::Listening(live)
        | SessionPhase::Finishing { live, .. }
        | SessionPhase::Draining { live, .. } => Some(live),
    }
}
