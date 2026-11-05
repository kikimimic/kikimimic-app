//! The threaded runtime around the conductor: the only place real time and
//! threads exist in the core.
//!
//! Three threads, each the sole owner of what it touches:
//! - conductor: all session state; ticks every 100 ms while a session runs.
//! - asr: the recognizer (and so the GPU work of recognition).
//! - mt: the translator. One per process: llama's backend initializes once.
//!
//! Engine calls never happen on the conductor thread, so a slow decode or a
//! long translation never stalls capture draining or recording.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::CaptureStream;
use crate::conductor::{
    AsrIn, AsrOut, Command, Conductor, Input, MtOut, Output, RecordingTarget, StartSession, POLL,
};
use crate::domain::{LaneRole, LanguagePolicy};
use crate::engine::{AsrEngine, MtEngine, MtRequest};
use crate::event::Event;
use crate::glossary::Glossary;

/// Opens OS audio sources. Implemented per platform in `kikimimic-capture`.
pub trait CaptureBackend: Send {
    /// Start capturing `role`. Audio flows into the returned stream until the
    /// handle is dropped.
    fn open(&mut self, role: LaneRole) -> anyhow::Result<(CaptureStream, Box<dyn CaptureHandle>)>;
}

/// Keeps a source running; dropping it stops the device.
pub trait CaptureHandle: Send {
    fn poll_event(&mut self) -> Option<crate::capture::SourceEvent> {
        None
    }
    fn decide(&mut self, _decision: crate::capture::SourceDecision) {}
}

pub type AsrLoader = Box<dyn FnOnce() -> anyhow::Result<Box<dyn AsrEngine>> + Send>;
pub type MtLoader = Box<dyn FnOnce() -> anyhow::Result<Box<dyn MtEngine>> + Send>;

pub struct Config {
    pub recordings_dir: PathBuf,
    pub capture: Box<dyn CaptureBackend>,
    /// Loaded on the ASR thread, once, before the first session.
    pub asr: AsrLoader,
    /// Loaded on the MT thread on the first sentence. `None`: no translation.
    pub mt: Option<MtLoader>,
    pub policy: LanguagePolicy,
    pub glossary: Glossary,
    /// The recording directory name for a session starting now. The app
    /// supplies it so it follows the user's local clock.
    pub session_name: Box<dyn Fn() -> String + Send>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    /// Microphone and system audio, recorded.
    Meeting,
    /// Microphone only, not recorded.
    FaceToFace,
}

enum Msg {
    Input(Input),
    Start(SessionMode),
}

/// The app's handle. Cheap to clone; every call is a message to the
/// conductor thread.
#[derive(Clone)]
pub struct Kikimimic {
    tx: Sender<Msg>,
}

/// The UI's single, ordered event stream.
pub struct Events(Receiver<Event>);

impl Iterator for Events {
    type Item = Event;
    fn next(&mut self) -> Option<Event> {
        self.0.recv().ok()
    }
}

impl Events {
    /// Wait at most `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.0.recv_timeout(timeout).ok()
    }
}

impl Kikimimic {
    pub fn start(config: Config) -> (Self, Events) {
        let (tx, rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let asr_tx = spawn_asr(config.asr, tx.clone());
        let mt_tx = spawn_mt(config.mt, tx.clone());
        let conductor = Conductor::new(config.policy, config.glossary);
        let shell = Shell {
            conductor,
            capture: config.capture,
            handles: Vec::new(),
            recordings_dir: config.recordings_dir,
            session_name: config.session_name,
            events: events_tx,
            asr: asr_tx,
            mt: mt_tx,
            epoch: Instant::now(),
        };
        thread::Builder::new()
            .name("kikimimic-conductor".into())
            .spawn(move || shell.run(rx))
            .expect("spawn the conductor thread");
        (Self { tx }, Events(events_rx))
    }

    /// Ignored while a session is running.
    pub fn start_session(&self, mode: SessionMode) {
        let _ = self.tx.send(Msg::Start(mode));
    }

    /// Idempotent. `Status(Idle)` follows once translations have drained.
    pub fn stop_session(&self) {
        let _ = self.tx.send(Msg::Input(Input::Command(Command::Stop)));
    }

    /// Applies from the next utterance; no model reload.
    pub fn set_languages(&self, policy: LanguagePolicy) {
        let _ = self
            .tx
            .send(Msg::Input(Input::Command(Command::SetPolicy(policy))));
    }
}

struct Shell {
    conductor: Conductor,
    capture: Box<dyn CaptureBackend>,
    handles: Vec<Box<dyn CaptureHandle>>,
    recordings_dir: PathBuf,
    session_name: Box<dyn Fn() -> String + Send>,
    events: Sender<Event>,
    asr: Sender<AsrWork>,
    mt: Sender<crate::conductor::ledger::MtJob>,
    epoch: Instant,
}

impl Shell {
    fn run(mut self, rx: Receiver<Msg>) {
        let mut next_tick = Instant::now() + POLL;
        loop {
            // Tick first when due, so a steady stream of engine answers can
            // never starve capture draining.
            if Instant::now() >= next_tick {
                next_tick = Instant::now() + POLL;
                if !self.conductor.is_idle() {
                    self.handle(Input::Tick);
                }
            }
            let wait = next_tick.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Msg::Input(input)) => self.handle(input),
                Ok(Msg::Start(mode)) => {
                    if let Some(start) = self.open_session(mode) {
                        self.handle(Input::Command(Command::Start(start)));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn handle(&mut self, input: Input) {
        let now = self.epoch.elapsed();
        for output in self.conductor.handle(now, input) {
            self.execute(output);
        }
    }

    fn open_session(&mut self, mode: SessionMode) -> Option<StartSession> {
        if !self.conductor.is_idle() {
            return None;
        }
        let roles: &[LaneRole] = match mode {
            SessionMode::Meeting => &[LaneRole::Mic, LaneRole::System],
            SessionMode::FaceToFace => &[LaneRole::Mic],
        };
        let mut lanes = Vec::new();
        for role in roles {
            match self.capture.open(*role) {
                Ok((stream, handle)) => {
                    lanes.push((*role, stream));
                    self.handles.push(handle);
                }
                Err(e) => {
                    let _ = self.events.send(Event::Status(crate::event::StatusView {
                        phase: crate::event::Phase::Idle,
                        notices: vec![crate::event::Notice::SourceUnavailable {
                            role: *role,
                            reason: format!("{e:#}"),
                        }],
                        recording_dir: None,
                    }));
                }
            }
        }
        if lanes.is_empty() {
            return None;
        }
        let (cadence_tx, cadence_rx) = mpsc::channel();
        let _ = self.asr.send(AsrWork::Cadence(cadence_tx));
        let cadence = cadence_rx.recv().unwrap_or(Duration::from_secs(1));
        let recording = (mode == SessionMode::Meeting).then(|| RecordingTarget {
            base: self.recordings_dir.clone(),
            name: (self.session_name)(),
        });
        Some(StartSession {
            lanes,
            recording,
            cadence,
        })
    }

    fn execute(&mut self, output: Output) {
        match output {
            Output::Emit(event) => {
                let _ = self.events.send(event);
            }
            Output::Asr(work) => {
                let _ = self.asr.send(AsrWork::Run(work));
            }
            Output::Mt(job) => {
                let _ = self.mt.send(job);
            }
            Output::ReleaseCapture => self.handles.clear(),
        }
    }
}

enum AsrWork {
    Run(AsrIn),
    Cadence(Sender<Duration>),
}

fn spawn_asr(loader: AsrLoader, tx: Sender<Msg>) -> Sender<AsrWork> {
    let (work_tx, work_rx) = mpsc::channel::<AsrWork>();
    thread::Builder::new()
        .name("kikimimic-asr".into())
        .spawn(move || {
            let mut engine = loader().map_err(|e| format!("{e:#}"));
            for work in work_rx {
                let work = match work {
                    AsrWork::Cadence(reply) => {
                        let cadence = engine.as_ref().map_or(POLL, |e| e.cadence());
                        let _ = reply.send(cadence);
                        continue;
                    }
                    AsrWork::Run(work) => work,
                };
                let engine = match engine.as_mut() {
                    Ok(engine) => engine,
                    Err(reason) => {
                        let answer = match work {
                            AsrIn::Step(lane, _) => AsrOut::Stepped(lane, Err(reason.clone())),
                            AsrIn::Finish(lane, _) => AsrOut::Finished(lane, Err(reason.clone())),
                            _ => continue,
                        };
                        let _ = tx.send(Msg::Input(Input::Asr(answer)));
                        continue;
                    }
                };
                let err = |e: anyhow::Error| format!("{e:#}");
                let answer = match work {
                    AsrIn::Open(lane) => {
                        engine.open_lane(lane);
                        continue;
                    }
                    AsrIn::Audio(lane, audio) => {
                        engine.push(lane, &audio);
                        continue;
                    }
                    AsrIn::Step(lane, langs) => {
                        AsrOut::Stepped(lane, engine.step(lane, &langs).map_err(err))
                    }
                    AsrIn::Finish(lane, langs) => {
                        AsrOut::Finished(lane, engine.finish(lane, &langs).map_err(err))
                    }
                };
                if tx.send(Msg::Input(Input::Asr(answer))).is_err() {
                    return;
                }
            }
        })
        .expect("spawn the ASR thread");
    work_tx
}

fn spawn_mt(loader: Option<MtLoader>, tx: Sender<Msg>) -> Sender<crate::conductor::ledger::MtJob> {
    let (job_tx, job_rx) = mpsc::channel::<crate::conductor::ledger::MtJob>();
    thread::Builder::new()
        .name("kikimimic-mt".into())
        .spawn(move || {
            let mut loader = loader;
            let mut engine: Option<Result<Box<dyn MtEngine>, String>> = None;
            for job in job_rx {
                let engine = engine.get_or_insert_with(|| match loader.take() {
                    Some(load) => load().map_err(|e| format!("translator failed to load: {e:#}")),
                    None => Err("no translator configured".into()),
                });
                let result = match engine {
                    Ok(engine) => engine
                        .translate(&MtRequest {
                            text: &job.text,
                            source: job.source.as_ref(),
                            target: &job.target,
                            context: &job.context,
                            terms: &job.terms,
                        })
                        .map_err(|e| format!("{e:#}")),
                    Err(reason) => Err(reason.clone()),
                };
                if tx
                    .send(Msg::Input(Input::Mt(MtOut { id: job.id, result })))
                    .is_err()
                {
                    return;
                }
            }
        })
        .expect("spawn the MT thread");
    job_tx
}
