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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::CaptureStream;
use crate::catalog::Catalog;
use crate::conductor::{
    AsrIn, AsrOut, Command, Conductor, Input, MtOut, Output, RecordingTarget, StartSession, POLL,
};
use crate::domain::LaneRole;
use crate::engine::{AsrEngine, EngineRegistry, LoadRequest, MtEngine, MtRequest};
use crate::event::{Event, Phase, StatusView};
use crate::settings::Settings;
use crate::store::ModelStore;

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

pub struct Config {
    /// `settings.toml` and the downloaded `models/` live here.
    pub data_dir: PathBuf,
    /// Also searched for model files (a checkout's `models/` in development).
    pub extra_model_dirs: Vec<PathBuf>,
    pub catalog: &'static Catalog,
    pub engines: EngineRegistry,
    pub recordings_dir: PathBuf,
    pub capture: Box<dyn CaptureBackend>,
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
    Decide(crate::capture::SourceDecision),
}

/// The app's handle. Cheap to clone; every call is a message to the
/// conductor thread, except settings, which this handle owns.
#[derive(Clone)]
pub struct Kikimimic {
    tx: Sender<Msg>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
    store: ModelStore,
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
        let settings_path = config.data_dir.join("settings.toml");
        let settings = Settings::load(&settings_path, config.catalog);
        let store = ModelStore::new(
            config.catalog,
            config.data_dir.join("models"),
            config.extra_model_dirs,
        );
        let (tx, rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let conductor = Conductor::new(settings.languages.clone(), settings.glossary.clone());
        let settings = Arc::new(Mutex::new(settings));
        let models = Models {
            store: store.clone(),
            engines: config.engines,
            settings: Arc::clone(&settings),
        };
        let asr_tx = spawn_asr(models.clone(), tx.clone());
        let mt_tx = spawn_mt(models, tx.clone());
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
        (
            Self {
                tx,
                settings,
                settings_path,
                store,
            },
            Events(events_rx),
        )
    }

    /// Ignored while a session is running. A failure (no model, no device)
    /// arrives as `Status` with `Phase::Error`.
    pub fn start_session(&self, mode: SessionMode) {
        let _ = self.tx.send(Msg::Start(mode));
    }

    /// Idempotent. `Status(Idle)` follows once translations have drained.
    pub fn stop_session(&self) {
        let _ = self.tx.send(Msg::Input(Input::Command(Command::Stop)));
    }

    /// Answer a `Event::Source` question (follow the new output device?).
    pub fn decide_source(&self, decision: crate::capture::SourceDecision) {
        let _ = self.tx.send(Msg::Decide(decision));
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Validate, persist, and apply. Languages and glossary take effect from
    /// the next utterance; a different recognition model from the next
    /// session, a different translation model from the next sentence.
    pub fn update_settings(&self, new: Settings) -> Result<(), String> {
        new.validate(self.store.catalog())?;
        new.save(&self.settings_path)?;
        let old = std::mem::replace(&mut *self.settings.lock().unwrap(), new.clone());
        if old.languages != new.languages {
            let _ = self.tx.send(Msg::Input(Input::Command(Command::SetPolicy(
                new.languages,
            ))));
        }
        if old.glossary != new.glossary {
            let _ = self.tx.send(Msg::Input(Input::Command(Command::SetGlossary(
                new.glossary,
            ))));
        }
        Ok(())
    }

    pub fn models(&self) -> &ModelStore {
        &self.store
    }
}

/// What the engine threads need to (re)load the model the settings name.
#[derive(Clone)]
struct Models {
    store: ModelStore,
    engines: EngineRegistry,
    settings: Arc<Mutex<Settings>>,
}

impl Models {
    fn wanted_asr(&self) -> String {
        self.settings.lock().unwrap().asr_model.clone()
    }

    fn wanted_mt(&self) -> Option<String> {
        self.settings.lock().unwrap().mt_model.clone()
    }

    fn load_asr(&self, id: &str) -> Result<Box<dyn AsrEngine>, String> {
        let entry = self
            .store
            .catalog()
            .get(id)
            .ok_or_else(|| format!("unknown model: {id}"))?;
        let files = self.store.resolve(id)?;
        self.engines
            .load_asr(&LoadRequest {
                entry,
                files: &files,
            })
            .map_err(|e| format!("{e:#}"))
    }

    fn load_mt(&self, id: &str) -> Result<Box<dyn MtEngine>, String> {
        let entry = self
            .store
            .catalog()
            .get(id)
            .ok_or_else(|| format!("unknown model: {id}"))?;
        let files = self.store.resolve(id)?;
        self.engines
            .load_mt(&LoadRequest {
                entry,
                files: &files,
            })
            .map_err(|e| format!("{e:#}"))
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
                for handle in &mut self.handles {
                    while let Some(event) = handle.poll_event() {
                        let _ = self.events.send(Event::Source(event));
                    }
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
                Ok(Msg::Decide(decision)) => {
                    for handle in &mut self.handles {
                        handle.decide(decision.clone());
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
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = self.asr.send(AsrWork::Ensure(reply_tx));
        let cadence = match reply_rx.recv() {
            Ok(Ok(cadence)) => cadence,
            Ok(Err(message)) => return self.refuse(message),
            Err(_) => return self.refuse("the speech engine stopped".into()),
        };
        let roles: &[LaneRole] = match mode {
            SessionMode::Meeting => &[LaneRole::Mic, LaneRole::System],
            SessionMode::FaceToFace => &[LaneRole::Mic],
        };
        let mut lanes = Vec::new();
        let mut unavailable = Vec::new();
        for role in roles {
            match self.capture.open(*role) {
                Ok((stream, handle)) => {
                    lanes.push((*role, stream));
                    self.handles.push(handle);
                }
                Err(e) => unavailable.push(crate::event::Notice::SourceUnavailable {
                    role: *role,
                    reason: format!("{e:#}"),
                }),
            }
        }
        if lanes.is_empty() {
            return self.refuse("no audio source could be opened".into());
        }
        if !unavailable.is_empty() {
            let _ = self.events.send(Event::Status(StatusView {
                phase: Phase::Idle,
                notices: unavailable,
                recording_dir: None,
            }));
        }
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

    fn refuse(&mut self, message: String) -> Option<StartSession> {
        self.handles.clear();
        let _ = self.events.send(Event::Status(StatusView {
            phase: Phase::Error { message },
            notices: Vec::new(),
            recording_dir: None,
        }));
        None
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
    /// Make sure the model the settings name is loaded; reply with its cadence.
    Ensure(Sender<Result<Duration, String>>),
}

fn spawn_asr(models: Models, tx: Sender<Msg>) -> Sender<AsrWork> {
    let (work_tx, work_rx) = mpsc::channel::<AsrWork>();
    thread::Builder::new()
        .name("kikimimic-asr".into())
        .spawn(move || {
            let mut loaded: Option<(String, Box<dyn AsrEngine>)> = None;
            for work in work_rx {
                let work = match work {
                    AsrWork::Ensure(reply) => {
                        let wanted = models.wanted_asr();
                        if loaded.as_ref().is_none_or(|(id, _)| *id != wanted) {
                            loaded = None;
                            match models.load_asr(&wanted) {
                                Ok(engine) => loaded = Some((wanted, engine)),
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                    continue;
                                }
                            }
                        }
                        let cadence = loaded.as_ref().map(|(_, e)| e.cadence()).unwrap_or(POLL);
                        let _ = reply.send(Ok(cadence));
                        continue;
                    }
                    AsrWork::Run(work) => work,
                };
                // A session only starts after Ensure succeeded.
                let Some((_, engine)) = loaded.as_mut() else {
                    continue;
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

fn spawn_mt(models: Models, tx: Sender<Msg>) -> Sender<crate::conductor::ledger::MtJob> {
    let (job_tx, job_rx) = mpsc::channel::<crate::conductor::ledger::MtJob>();
    thread::Builder::new()
        .name("kikimimic-mt".into())
        .spawn(move || {
            let mut loaded: Option<(String, Box<dyn MtEngine>)> = None;
            for job in job_rx {
                let result = translate(&models, &mut loaded, &job);
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

/// Translate one job with the model the settings name, (re)loading it first
/// when the choice changed.
fn translate(
    models: &Models,
    loaded: &mut Option<(String, Box<dyn MtEngine>)>,
    job: &crate::conductor::ledger::MtJob,
) -> Result<String, String> {
    let wanted = models
        .wanted_mt()
        .ok_or("no translation model is selected")?;
    if loaded.as_ref().is_none_or(|(id, _)| *id != wanted) {
        // Drop the old model before loading the next: two may not fit in memory.
        *loaded = None;
        let engine = models
            .load_mt(&wanted)
            .map_err(|e| format!("translator unavailable: {e}"))?;
        *loaded = Some((wanted, engine));
    }
    let (_, engine) = loaded.as_mut().expect("loaded above");
    engine
        .translate(&MtRequest {
            text: &job.text,
            source: job.source.as_ref(),
            target: &job.target,
            context: &job.context,
            terms: &job.terms,
        })
        .map_err(|e| format!("{e:#}"))
}
