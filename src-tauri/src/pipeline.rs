//! The pipeline worker: one long-lived thread that owns the ASR models, the
//! per-lane recognisers, and the capture-session lifecycle.
//!
//! Start/Stop arrive on a command channel and are handled strictly in
//! order, so a stale session can never overlap a new one, and the models
//! load exactly once for the app's lifetime instead of once per Record.
//!
//! How audio becomes text is not decided here: a lane holds a
//! [`Recognizer`] and this file only feeds it and delivers what comes back.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::capture;
use crate::record;
use crate::translate::{Outcome, Status, TranslateLane};
use kikimimic_core::language::LanguagePolicy;
use kikimimic_core::models::{Model, ModelManager};
use kikimimic_core::{Lane, Recognizer};

/// Decode cadence, measured step-start to step-start: a slow decode eats
/// into the following idle time instead of stacking on top of it.
const STEP_INTERVAL: Duration = Duration::from_millis(1000);
const SONIOX_STEP_INTERVAL: Duration = Duration::from_millis(100);
/// How often the worker drains the ring buffer and checks for commands;
/// also the worst-case extra latency for Stop.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long an overrun notice stays up after the last drop; long enough to
/// read, gone once the trouble has passed.
const OVERRUN_NOTICE: Duration = Duration::from_secs(5);
/// A pending anchor within this much of where the current clock predicts it
/// is the same continuous stream: capture timestamps wobble a little, and a
/// hole this small is absorbed by the recorder's snap anyway. Beyond it, the
/// audio either side belongs to different moments and must be placed
/// separately (see [`LaneRuntime::drain`]).
const ANCHOR_JUMP_NANOS: u64 = 30_000_000;

pub enum Cmd {
    Start {
        mimi: bool,
        rules: Vec<GlossaryRule>,
    },
    Stop,
    RespondOutputDevice {
        prompt_id: u64,
        switch_device: bool,
    },
    /// Re-recognise a diarized recording's shared lines (postprocess::split)
    /// with the models this worker already holds. Idle only.
    Split {
        dir: PathBuf,
        reply: std::sync::mpsc::Sender<anyhow::Result<()>>,
    },
}

#[derive(Clone, serde::Deserialize)]
pub struct GlossaryRule {
    from: String,
    to: String,
}

pub fn validate_glossary(rules: &[GlossaryRule]) -> Result<(), String> {
    if rules.len() > 50
        || rules.iter().any(|r| {
            r.from.trim().is_empty()
                || r.to.trim().is_empty()
                || r.from.chars().count() > 80
                || r.to.chars().count() > 80
        })
    {
        return Err("辞書は50件以内、各表記は1〜80文字で登録してください".into());
    }
    Ok(())
}

fn apply_glossary(text: &str, rules: &[GlossaryRule]) -> String {
    rules.iter().fold(text.to_string(), |text, rule| {
        text.replace(&rule.from, &rule.to)
    })
}

const OUTPUT_DEVICE_WINDOW: &str = "output-device-change";
const OUTPUT_DEVICE_EVENT: &str = "output-device-prompt";
static NEXT_OUTPUT_DEVICE_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevicePrompt {
    id: u64,
    previous_name: String,
    detected_name: String,
}

pub type OutputDevicePromptStore = Arc<Mutex<Option<OutputDevicePrompt>>>;

pub fn new_output_device_prompt_store() -> OutputDevicePromptStore {
    Arc::new(Mutex::new(None))
}

#[derive(Default)]
struct PendingOutputDevice {
    prompt: Option<OutputDevicePrompt>,
    expected_uid: Option<String>,
}

impl PendingOutputDevice {
    fn observe(
        &mut self,
        capturing: &capture::OutputDevice,
        detected: &capture::OutputDevice,
    ) -> bool {
        if capturing.uid == detected.uid {
            return self.clear();
        }
        if self.expected_uid.as_deref() == Some(detected.uid.as_str()) {
            return false;
        }
        self.expected_uid = Some(detected.uid.clone());
        self.prompt = Some(OutputDevicePrompt {
            id: NEXT_OUTPUT_DEVICE_PROMPT_ID.fetch_add(1, Ordering::Relaxed),
            previous_name: capturing.name.clone(),
            detected_name: detected.name.clone(),
        });
        true
    }

    fn decide(
        &mut self,
        prompt_id: u64,
        switch_device: bool,
    ) -> Option<capture::OutputDeviceSwitchDecision> {
        if self.prompt.as_ref()?.id != prompt_id {
            return None;
        }
        let expected_uid = self.expected_uid.take()?;
        self.prompt = None;
        Some(if switch_device {
            capture::OutputDeviceSwitchDecision::Switch { expected_uid }
        } else {
            capture::OutputDeviceSwitchDecision::Cancel { expected_uid }
        })
    }

    fn clear(&mut self) -> bool {
        let changed = self.prompt.take().is_some();
        self.expected_uid = None;
        changed
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptPayload {
    /// Which audio lane the text belongs to ("mic" or "speaker").
    lane: &'static str,
    committed_delta: String,
    volatile: String,
    utterance_final: Option<UtterancePayload>,
}

/// A finished utterance, timed against this session's recording so the two
/// can be lined up afterwards.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UtterancePayload {
    /// Session-unique; the translation arriving later names it.
    id: u64,
    text: String,
    start_ms: u64,
    end_ms: u64,
    /// What it was recognised as, when detection was confident.
    lang: Option<String>,
    /// Whether a translation is on its way, so the UI knows to leave room for
    /// it rather than showing a placeholder that never resolves.
    translating: bool,
}

/// A translation, or the news that there will not be one.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TranslationPayload {
    id: u64,
    lane: &'static str,
    /// `None` means this utterance will never get a translation (the queue
    /// overflowed, or the backend failed); the UI takes its placeholder down.
    text: Option<String>,
    lang: Option<String>,
}

/// `state` drives the UI (idle/loading/listening/error); `message` is
/// human-readable detail, e.g. a non-fatal decode error while listening.
/// `recording_dir` is where this session's audio is being written — it
/// outlives the session so the user can still find the files after stopping.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    pub(crate) state: &'static str,
    message: Option<String>,
    pub(crate) recording_dir: Option<String>,
}

/// Latest status snapshot for the `get_status` command. Status events only
/// fire on change, so a webview that (re)loads mid-session would otherwise
/// show "idle" until the next transition.
pub type StatusStore = Arc<Mutex<StatusPayload>>;

pub fn new_status_store() -> StatusStore {
    Arc::new(Mutex::new(StatusPayload {
        state: "idle",
        message: None,
        recording_dir: None,
    }))
}

/// The language settings, shared with the UI commands. Read at each utterance
/// rather than at session start, so a change takes effect from the next
/// sentence without stopping the recording: `allowed_lang_ids` is a field the
/// engine reads per decode, not something baked into the loaded model.
pub type PolicyStore = Arc<Mutex<LanguagePolicy>>;

pub fn new_policy_store() -> PolicyStore {
    Arc::new(Mutex::new(policy_from_env()))
}

/// The startup default, still honouring the environment variables the
/// pipeline grew up with: `KKM_LANG` pins one spoken language, `KKM_LANGS`
/// lists the candidates, `KKM_TARGET` (or `none`) sets the translation target.
fn policy_from_env() -> LanguagePolicy {
    let mut policy = LanguagePolicy::default();
    if let Ok(langs) = std::env::var("KKM_LANGS") {
        let spoken: Vec<String> = langs
            .split(',')
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !spoken.is_empty() {
            policy.spoken = spoken;
        }
    }
    if let Ok(lang) = std::env::var("KKM_LANG") {
        policy.spoken = vec![lang];
    }
    if let Ok(target) = std::env::var("KKM_TARGET") {
        policy.target = (target != "none" && !target.is_empty()).then_some(target);
    }
    policy
}

/// The worker's channel back to the frontend: events plus the shared
/// status snapshot, updated together.
pub struct Ui {
    pub app: AppHandle,
    pub status: StatusStore,
    pub output_device_prompt: OutputDevicePromptStore,
}

/// The loaded ASR models, shared by the lanes of a session and kept between
/// sessions. Each lane recognises through its own [`Recognizer`] over this.
type Models = Rc<RefCell<kikimimic_asr_whisper::WhisperModels>>;

enum AsrBackend {
    Whisper(Models),
    Soniox {
        api_key: String,
        spoken: Vec<String>,
        rules: Vec<GlossaryRule>,
    },
}

/// The recogniser a new lane gets. One place to change when there is more
/// than one ASR to choose from (plan.md Step 6).
fn recognizer(backend: &AsrBackend) -> Box<dyn Recognizer> {
    match backend {
        AsrBackend::Whisper(models) => Box::new(kikimimic_asr_whisper::WhisperRecognizer::new(
            Rc::clone(models),
        )),
        AsrBackend::Soniox {
            api_key,
            spoken,
            rules,
        } => Box::new(crate::soniox::SonioxRecognizer::new(
            api_key.clone(),
            spoken.clone(),
            rules.iter().map(|rule| rule.to.clone()).collect(),
        )),
    }
}

/// Where models are looked for. The download command fills the first
/// directory; this only asks.
fn models(app: &AppHandle) -> ModelManager {
    crate::model_files::manager(app)
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Mic => "mic",
        Lane::Speaker => "speaker",
    }
}

fn emit_status(ui: &Ui, state: &'static str, message: Option<String>) {
    let payload = {
        let mut store = ui.status.lock().unwrap();
        store.state = state;
        store.message = message;
        store.clone()
    };
    let _ = ui.app.emit("status", payload);
}

/// Announce where the session is being recorded (or, with `None`, that it is
/// not), leaving the rest of the status alone.
fn emit_recording_dir(ui: &Ui, dir: Option<String>) {
    let payload = {
        let mut store = ui.status.lock().unwrap();
        store.recording_dir = dir;
        store.clone()
    };
    let _ = ui.app.emit("status", payload);
}

/// Unconditional emit: pass outputs through an [`EmitGate`] first.
fn emit_step(
    ui: &Ui,
    lane: Lane,
    out: kikimimic_core::scheduler::StepOutput,
    utterance_final: Option<UtterancePayload>,
) {
    let _ = ui.app.emit(
        "transcript",
        TranscriptPayload {
            lane: lane_name(lane),
            committed_delta: out.committed_delta,
            volatile: out.volatile,
            utterance_final,
        },
    );
}

/// A translation, or — with `text: None` — the news that this utterance is
/// not getting one after all.
fn emit_translation(ui: &Ui, lane: Lane, id: u64, text: Option<String>, lang: Option<String>) {
    let _ = ui.app.emit(
        "translation",
        TranslationPayload {
            id,
            lane: lane_name(lane),
            text,
            lang,
        },
    );
}

fn present_output_device_prompt(ui: &Ui, prompt: Option<OutputDevicePrompt>) {
    *ui.output_device_prompt.lock().unwrap() = prompt.clone();
    let _ = ui
        .app
        .emit_to(OUTPUT_DEVICE_WINDOW, OUTPUT_DEVICE_EVENT, prompt.clone());
    let Some(window) = ui.app.get_webview_window(OUTPUT_DEVICE_WINDOW) else {
        return;
    };
    if prompt.is_some() {
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        let _ = window.hide();
    }
}

fn output_device_events(lanes: &[LaneRuntime]) -> Vec<capture::OutputDeviceSwitchEvent> {
    let mut events = Vec::new();
    for lane in lanes {
        let Some(control) = lane.session.output_device_switch.as_ref() else {
            continue;
        };
        let Ok(receiver) = control.event_receiver().lock() else {
            continue;
        };
        events.extend(receiver.try_iter());
    }
    events
}

fn send_output_device_decision(
    lanes: &[LaneRuntime],
    decision: capture::OutputDeviceSwitchDecision,
) -> Result<(), String> {
    let control = lanes
        .iter()
        .find_map(|lane| lane.session.output_device_switch.as_ref())
        .ok_or_else(|| "speaker capture is not available".to_string())?;
    control
        .decision_sender()
        .send(decision)
        .map_err(|_| "speaker capture is no longer running".to_string())
}

fn apply_output_device_event(
    ui: &Ui,
    pending: &mut PendingOutputDevice,
    notices: &mut Notices,
    event: capture::OutputDeviceSwitchEvent,
) {
    match event {
        capture::OutputDeviceSwitchEvent::Detected {
            capturing,
            detected,
        } => {
            if pending.observe(&capturing, &detected) {
                present_output_device_prompt(ui, pending.prompt.clone());
            }
        }
        capture::OutputDeviceSwitchEvent::Switched { current, .. } => {
            pending.clear();
            present_output_device_prompt(ui, None);
            if notices.set(NoticeKey::Speaker, None) {
                emit_status(ui, "listening", notices.message());
            }
            let _ = ui
                .app
                .emit_to("main", "output-device-switched", current.name);
        }
        capture::OutputDeviceSwitchEvent::SwitchFailed {
            capturing,
            detected,
            error,
        } => {
            eprintln!("output device switch failed: {error}");
            pending.clear();
            pending.observe(&capturing, &detected);
            present_output_device_prompt(ui, pending.prompt.clone());
            let _ = ui.app.emit_to(
                OUTPUT_DEVICE_WINDOW,
                "output-device-switch-error",
                "出力先を切り替えられませんでした。接続を確認して、もう一度お試しください。",
            );
            if notices.set(
                NoticeKey::Speaker,
                Some("スピーカー録音の出力先を切り替えられませんでした".to_string()),
            ) {
                emit_status(ui, "listening", notices.message());
            }
        }
        capture::OutputDeviceSwitchEvent::Cancelled { .. } => {
            // The prompt was already cleared when its matching decision was
            // accepted. A stale decision can arrive after a newer Detected
            // event, and must not dismiss that newer prompt.
        }
    }
}

pub fn run(cmd_rx: Receiver<Cmd>, ui: Ui, policy: PolicyStore) {
    let mut engines: Option<Models> = None;
    // Spawned once for the app: the translation model is expensive to load
    // and, like the ASR engines, is kept between sessions. Its own model path
    // is resolved lazily inside the worker, so a missing translation model
    // costs the translation lane and not the transcript.
    let mut translations = Translations::new(
        TranslateLane::spawn(
            models(&ui.app)
                .resolve(Model::Translator)
                .unwrap_or_else(|_| PathBuf::from(Model::Translator.default_file())),
        ),
        policy,
    );
    emit_status(&ui, "idle", None);
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::Stop => {}                       // Stop while idle
            Cmd::RespondOutputDevice { .. } => {} // no prompt while idle
            Cmd::Split { dir, reply } => {
                let _ = reply.send(split(&ui, &mut engines, &mut translations, &dir));
            }
            Cmd::Start { mimi, rules } => {
                match run_session(&cmd_rx, &ui, &mut engines, &mut translations, mimi, &rules) {
                    Ok(()) => emit_status(&ui, "idle", None),
                    Err(e) => emit_status(&ui, "error", Some(format!("{e:#}"))),
                }
            }
        }
    }
}

fn load_engines(
    ui: &Ui,
    engines: &mut Option<Models>,
    spoken: &[String],
) -> anyhow::Result<Models> {
    if engines.is_none() {
        emit_status(ui, "loading", None);
        let paths = models(&ui.app);
        let loaded = kikimimic_asr_whisper::WhisperModels::load(
            &paths.resolve(Model::Asr)?.to_string_lossy(),
            &paths.resolve(Model::Vad)?.to_string_lossy(),
            spoken,
        )?;
        *engines = Some(Rc::new(RefCell::new(loaded)));
    }
    Ok(Rc::clone(engines.as_ref().expect("engines just ensured")))
}

/// A translation may wait on the model loading first.
const SPLIT_TRANSLATE_TIMEOUT: Duration = Duration::from_secs(120);

fn split(
    ui: &Ui,
    engines: &mut Option<Models>,
    translations: &mut Translations,
    dir: &std::path::Path,
) -> anyhow::Result<()> {
    let loading = engines.is_none();
    let models = load_engines(ui, engines, &translations.policy().spoken)?;
    if loading {
        emit_status(ui, "idle", None);
    }
    let mut transcribe = |samples: &[f32], lang: Option<&str>| {
        kikimimic_core::AsrEngine::transcribe(&mut models.borrow_mut().engine, samples, lang)
    };
    let mut translate = |text: &str, source: Option<&str>, target: &str| {
        let id = translations.next_id;
        translations.next_id += 1;
        translations.lane.submit(kikimimic_core::translate::Job {
            id,
            lane: Lane::Mic,
            text: text.to_string(),
            source: source.map(str::to_string),
            target: target.to_string(),
        });
        // A late result from an earlier session may still be in the channel.
        loop {
            let outcome = translations.lane.wait(SPLIT_TRANSLATE_TIMEOUT)?;
            if outcome.id == id {
                return match outcome.status {
                    Status::Done(text) => Some(text),
                    Status::Failed(e) => {
                        eprintln!("split translation: {e}");
                        None
                    }
                };
            }
        }
    };
    crate::postprocess::split(dir, &mut transcribe, &mut translate)
}

fn run_session(
    cmd_rx: &Receiver<Cmd>,
    ui: &Ui,
    engines: &mut Option<Models>,
    translations: &mut Translations,
    mimi: bool,
    rules: &[GlossaryRule],
) -> anyhow::Result<()> {
    let backend = if mimi {
        AsrBackend::Soniox {
            api_key: crate::soniox::api_key()
                .ok_or_else(|| anyhow::anyhow!("対面モードには SONIOX_API_KEY が必要です"))?,
            spoken: translations.policy().spoken,
            rules: rules.to_vec(),
        }
    } else {
        AsrBackend::Whisper(load_engines(ui, engines, &translations.policy().spoken)?)
    };
    // Load the translation model now rather than on the first sentence: the
    // load overlaps the start of the meeting instead of delaying the first
    // translation by all of it.
    translations.begin_session();
    let stop_capture = Arc::new(AtomicBool::new(false));
    let result = (|| {
        let mic = start_capture(capture::mic::spawn, &stop_capture)?;
        let mut lanes = vec![LaneRuntime::new(Lane::Mic, mic, recognizer(&backend))?];
        if mimi {
            lanes[0].next_step = Instant::now();
        }
        // The speaker lane is best-effort: it needs macOS 14.2+ and the
        // system-audio permission, and a meeting is still worth
        // transcribing from the mic alone when it is unavailable.
        let mut notices = Notices::default();
        let speaker = if mimi {
            Ok(None)
        } else {
            start_speaker(&stop_capture).and_then(|session| {
                session
                    .map(|s| LaneRuntime::new(Lane::Speaker, s, recognizer(&backend)))
                    .transpose()
            })
        };
        match speaker {
            Ok(Some(lane)) => lanes.push(lane),
            Ok(None) => {}
            Err(e) => {
                notices.set(
                    NoticeKey::Speaker,
                    Some(format!("speaker lane unavailable: {e:#}")),
                );
            }
        }
        // Recording is best-effort too: a full disk should cost the recording,
        // not the transcript.
        let mut recorder = match if mimi {
            Ok(None)
        } else {
            open_recorder(ui, &lanes).map(Some)
        } {
            Ok(rec) => {
                emit_recording_dir(ui, rec.as_ref().map(|rec| rec.dir().display().to_string()));
                rec
            }
            Err(e) => {
                emit_recording_dir(ui, None);
                notices.set(
                    NoticeKey::Recording,
                    Some(format!("recording unavailable: {e:#}")),
                );
                None
            }
        };
        let ran = run_capture_loop(
            cmd_rx,
            ui,
            &backend,
            &mut lanes,
            recorder.as_mut(),
            notices,
            translations,
        );
        // Close the files even when the session ended badly: a WAV whose
        // header never got its final size is unreadable.
        let closed = recorder.map_or(Ok(()), |rec| rec.finish());
        ran.and(closed)
    })();
    stop_capture.store(true, Ordering::SeqCst);
    present_output_device_prompt(ui, None);
    result
}

/// Where sessions are recorded: the platform's music folder by default
/// (`~/Music/kikimimic` on macOS), overridable with `KKM_RECORD_DIR`.
pub fn record_base(app: &AppHandle) -> anyhow::Result<PathBuf> {
    match std::env::var("KKM_RECORD_DIR") {
        Ok(dir) => Ok(PathBuf::from(dir)),
        Err(_) => Ok(app.path().audio_dir()?.join("kikimimic")),
    }
}

/// Open one WAV per lane plus their mix. Lane order here is the order the
/// runtimes are polled in, which is what [`Poll::index`] refers to.
fn open_recorder(ui: &Ui, lanes: &[LaneRuntime]) -> anyhow::Result<record::SessionRecorder> {
    let specs: Vec<record::LaneSpec> = lanes
        .iter()
        .map(|l| record::LaneSpec {
            name: lane_name(l.lane),
            src_rate: l.session.src_rate,
        })
        .collect();
    record::SessionRecorder::open(
        &record_base(&ui.app)?,
        &record::session_name(chrono::Local::now()),
        &specs,
    )
}

/// Spawn a capture backend and wait for it to report its device config.
fn start_capture(
    spawn: fn(Arc<AtomicBool>, std::sync::mpsc::Sender<anyhow::Result<capture::CaptureSession>>),
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<capture::CaptureSession> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    spawn(stop.clone(), ready_tx);
    wait_capture_ready(ready_rx, stop, Duration::from_secs(10))
}

fn wait_capture_ready(
    ready_rx: Receiver<anyhow::Result<capture::CaptureSession>>,
    stop: &Arc<AtomicBool>,
    timeout: Duration,
) -> anyhow::Result<capture::CaptureSession> {
    match ready_rx.recv_timeout(timeout) {
        Ok(session) => session,
        Err(RecvTimeoutError::Timeout) => {
            // ponytail: CoreAudio can strand this thread; move capture to a process if repeated hangs matter.
            stop.store(true, Ordering::SeqCst);
            anyhow::bail!(
                "capture device did not start within {} seconds",
                timeout.as_secs()
            )
        }
        Err(RecvTimeoutError::Disconnected) => {
            anyhow::bail!("capture thread died before reporting")
        }
    }
}

/// `Ok(None)` on platforms with no loopback backend yet (Windows gets
/// WASAPI in a later pass).
#[cfg(target_os = "macos")]
fn start_speaker(stop: &Arc<AtomicBool>) -> anyhow::Result<Option<capture::CaptureSession>> {
    start_capture(capture::speaker::spawn, stop).map(Some)
}

#[cfg(not(target_os = "macos"))]
fn start_speaker(_stop: &Arc<AtomicBool>) -> anyhow::Result<Option<capture::CaptureSession>> {
    Ok(None)
}

/// The session's zero: the first captured moment any lane reported. Every
/// position in the recording and in the transcript is measured from it.
///
/// The mic lane is polled first and runs continuously, so in practice the
/// zero is its first sample.
#[derive(Default)]
struct SessionClock {
    zero_nanos: Option<u64>,
}

impl SessionClock {
    /// Where a captured moment sits in the recording, in [`record::RECORD_RATE`]
    /// samples.
    fn position(&mut self, nanos: u64) -> usize {
        let zero = *self.zero_nanos.get_or_insert(nanos);
        let since = nanos.saturating_sub(zero) as u128;
        (since * record::RECORD_RATE as u128 / 1_000_000_000) as usize
    }
}

/// One source of status-line text. Each source owns one slot in [`Notices`],
/// so posting or expiring one notice never erases another's.
#[derive(Clone, Copy, PartialEq)]
enum NoticeKey {
    /// The speaker lane is delivering nothing usable: it failed to start,
    /// or macOS is withholding the system-audio permission.
    Speaker,
    /// Recording is off: the files failed to open, or a write failed.
    Recording,
    /// A lane's ring overflowed recently.
    Overrun(Lane),
    /// A lane's latest decode failed.
    Decode(Lane),
    /// The translation backend is not working; the transcript still is.
    Translation,
    /// The requested spoken languages were refused; the old set stays.
    Language,
}

/// The notices a listening session currently carries, joined oldest-first
/// into the single status message the UI shows.
#[derive(Default)]
struct Notices {
    entries: Vec<(NoticeKey, String)>,
}

impl Notices {
    /// Post (`Some`) or take down (`None`) one source's notice. Returns
    /// whether the joined message changed — the caller's cue to re-emit.
    fn set(&mut self, key: NoticeKey, msg: Option<String>) -> bool {
        let at = self.entries.iter().position(|(k, _)| *k == key);
        match (at, msg) {
            (Some(i), None) => {
                self.entries.remove(i);
                true
            }
            (Some(i), Some(msg)) => {
                let changed = self.entries[i].1 != msg;
                self.entries[i].1 = msg;
                changed
            }
            (None, Some(msg)) => {
                self.entries.push((key, msg));
                true
            }
            (None, None) => false,
        }
    }

    /// The joined status message; `None` when nothing is wrong.
    fn message(&self) -> Option<String> {
        (!self.entries.is_empty()).then(|| {
            self.entries
                .iter()
                .map(|(_, msg)| msg.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
    }
}

/// The translation lane as the session sees it: utterance ids, the language
/// policy, and how many sentences are still out with the translator.
///
/// Lives for the app, not the session: the ids must stay unique across a
/// stop/start, because the UI keeps the earlier session's lines on screen and
/// a reused id would attach a translation to the wrong one.
struct Translations {
    lane: TranslateLane,
    policy: PolicyStore,
    next_id: u64,
    /// The current session's first utterance id. A translation from an
    /// earlier session can still arrive (its drain timed out) and belongs on
    /// its line, but not in this session's transcript file.
    first_id: u64,
    /// Submitted and not yet accounted for. Drives the drain at Stop.
    outstanding: usize,
}

/// What became of one utterance handed to [`Translations::submit`].
struct Submitted {
    id: u64,
    /// A translation is on its way; the UI leaves room for it.
    translating: bool,
    /// The older sentence dropped to make room, whose line is still waiting
    /// for a translation that will now never arrive.
    evicted: Option<kikimimic_core::translate::Job>,
}

impl Translations {
    fn new(lane: TranslateLane, policy: PolicyStore) -> Self {
        Self {
            lane,
            policy,
            next_id: 1,
            first_id: 1,
            outstanding: 0,
        }
    }

    fn policy(&self) -> LanguagePolicy {
        self.policy.lock().unwrap().clone()
    }

    /// A session is starting: nothing from the last one is still owed, and
    /// the model may as well start loading now.
    fn begin_session(&mut self) {
        self.outstanding = 0;
        self.first_id = self.next_id;
        self.lane.warm_up();
    }

    /// Number an utterance and queue it if the policy says it needs
    /// translating.
    fn submit(&mut self, lane: Lane, text: &str, source: Option<&str>) -> Submitted {
        let id = self.next_id;
        self.next_id += 1;
        let Some(target) = self.policy().target_for(source).map(str::to_string) else {
            return Submitted {
                id,
                translating: false,
                evicted: None,
            };
        };
        let evicted = self.lane.submit(kikimimic_core::translate::Job {
            id,
            lane,
            text: text.to_string(),
            source: source.map(str::to_string),
            target,
        });
        // An eviction is one fewer result to wait for, not one more.
        self.outstanding += usize::from(evicted.is_none());
        Submitted {
            id,
            translating: true,
            evicted,
        }
    }
}

/// What a lane needs from the session for one poll, beyond its own state.
struct Poll<'a> {
    ui: &'a Ui,
    clock: &'a mut SessionClock,
    rec: Option<&'a mut record::SessionRecorder>,
    notices: &'a mut Notices,
    /// This lane's index, in the order the recorder knows the lanes.
    index: usize,
    tr: &'a mut Translations,
    rules: &'a [GlossaryRule],
}

impl Poll<'_> {
    /// Post or take down one source's notice, re-emitting the status line
    /// only when the joined message actually changed.
    fn notice(&mut self, key: NoticeKey, msg: Option<String>) {
        if self.notices.set(key, msg) {
            emit_status(self.ui, "listening", self.notices.message());
        }
    }
}

/// Per-lane capture-to-text state: one runtime per lane, each with its own
/// [`Recognizer`] over models the lanes share.
struct LaneRuntime {
    lane: Lane,
    session: capture::CaptureSession,
    /// Frames taken from the ring so far: what the capture anchors count in.
    frames_read: u64,
    /// Follows the anchors the backend stamped that audio with.
    trail: AnchorTrail,
    /// Where this lane's audio is expected to continue, for the rare poll
    /// with no anchor to go on.
    next_position: usize,
    resampler: kikimimic_core::resample::StreamResampler,
    recognizer: Box<dyn Recognizer>,
    /// Downmix scratch buffer, reused across polls.
    mono: Vec<f32>,
    reported_drops: usize,
    /// While set, an overrun notice is on screen; cleared when it expires.
    overrun_until: Option<Instant>,
    /// Per-lane: what the UI last saw of *this* lane's volatile tail.
    gate: EmitGate,
    /// Audio handed to the recogniser so far. It times utterances against
    /// this; [`LaneTimeline`] turns that into recording time.
    stream_samples: usize,
    timeline: LaneTimeline,
    /// When this lane is next due to decode. Per-lane so one lane's slow
    /// decode delays only itself.
    next_step: Instant,
    /// Only the speaker lane: a mic that hears nothing is normal, a tap that
    /// receives digital silence means the permission was withheld.
    silence: Option<SilenceWatch>,
}

impl LaneRuntime {
    fn new(
        lane: Lane,
        session: capture::CaptureSession,
        recognizer: Box<dyn Recognizer>,
    ) -> anyhow::Result<Self> {
        let resampler = kikimimic_core::resample::StreamResampler::new(session.src_rate)?;
        Ok(Self {
            lane,
            session,
            frames_read: 0,
            trail: AnchorTrail::default(),
            next_position: 0,
            resampler,
            recognizer,
            mono: Vec::new(),
            reported_drops: 0,
            overrun_until: None,
            gate: EmitGate::default(),
            stream_samples: 0,
            timeline: LaneTimeline::default(),
            next_step: Instant::now() + STEP_INTERVAL,
            silence: matches!(lane, Lane::Speaker).then(SilenceWatch::default),
        })
    }

    /// Milliseconds of audio handed to the recogniser so far — the clock its
    /// utterances are timed against.
    fn stream_ms(&self) -> u64 {
        self.stream_samples as u64 * 1000 / kikimimic_core::resample::TARGET_RATE as u64
    }

    /// One poll of this lane: drain captured audio, then decode if its
    /// cadence came due.
    fn tick(
        &mut self,
        poll: &mut Poll<'_>,
        lang: Option<&str>,
        interval: Duration,
    ) -> anyhow::Result<()> {
        self.pump(poll)?;
        if Instant::now() < self.next_step {
            return Ok(());
        }
        // Measured step-start to step-start: a slow decode eats into the
        // following idle time instead of stacking on top of it.
        let started = Instant::now();
        match self.recognizer.step(lang) {
            Ok(Some(out)) => {
                poll.notice(NoticeKey::Decode(self.lane), None);
                self.deliver(poll, out);
            }
            Ok(None) => poll.notice(NoticeKey::Decode(self.lane), None),
            // Non-fatal: keep listening, surface the message until a
            // decode succeeds again.
            Err(e) => poll.notice(
                NoticeKey::Decode(self.lane),
                Some(format!("decode error: {e:#}")),
            ),
        }
        self.next_step = started + interval;
        Ok(())
    }

    /// Send a step's outcome to the UI, a finished utterance to the session's
    /// transcript (both timed against the recording), and the same utterance
    /// to the translation lane.
    fn deliver(&mut self, poll: &mut Poll<'_>, out: kikimimic_core::scheduler::StepOutput) {
        if !self.gate.should_emit(&out) {
            return;
        }
        let mut out = out;
        let finished = out.utterance_final.take();
        let utterance = finished.map(|u| {
            let text = apply_glossary(&u.text, poll.rules);
            let submitted = poll.tr.submit(self.lane, &text, u.lang.as_deref());
            if let Some(job) = submitted.evicted {
                // Its line has been waiting for a translation since before
                // this one; nothing is coming, so take the wait down.
                emit_translation(poll.ui, job.lane, job.id, None, None);
            }
            UtterancePayload {
                id: submitted.id,
                start_ms: self.timeline.recording_ms(u.start_ms),
                end_ms: self.timeline.recording_ms(u.end_ms),
                text,
                lang: u.lang,
                translating: submitted.translating,
            }
        });
        if let (Some(rec), Some(u)) = (poll.rec.as_deref_mut(), utterance.as_ref()) {
            rec.write_utterance(&record::Utterance {
                id: u.id,
                lane: lane_name(self.lane),
                start_ms: u.start_ms,
                end_ms: u.end_ms,
                lang: u.lang.as_deref(),
                text: &u.text,
            });
        }
        emit_step(poll.ui, self.lane, out, utterance);
    }

    /// Stop: recover the audio still in flight (ring buffer → resampler
    /// tail → whatever the recogniser has not reached) before flushing the text.
    fn finish(&mut self, poll: &mut Poll<'_>, lang: Option<&str>) -> anyhow::Result<()> {
        self.pump(poll)?;
        let tail = self.resampler.flush()?;
        self.stream_samples += tail.len();
        self.recognizer.push_audio(&tail);
        if let Some(fin) = self.recognizer.finish(lang)? {
            self.deliver(poll, fin);
        }
        while let Some(out) = self.recognizer.step(lang)? {
            self.deliver(poll, out);
        }
        // A tail can still be on screen when finish had nothing to flush: a
        // recogniser may drop its pending text rather than commit it.
        if self.gate.volatile_on_screen {
            emit_step(
                poll.ui,
                self.lane,
                kikimimic_core::scheduler::StepOutput::default(),
                None,
            );
        }
        Ok(())
    }

    /// Move captured audio into the recogniser and report overruns. Fails
    /// when the stream itself failed (device unplugged): without that the
    /// session would keep "listening" to silence forever.
    fn pump(&mut self, poll: &mut Poll<'_>) -> anyhow::Result<()> {
        if let Some(msg) = self.session.error.lock().unwrap().take() {
            anyhow::bail!("input stream failed: {msg}");
        }
        // One pass per contiguous run: drain() stops at a time jump, and the
        // next pass places what follows by its own anchor.
        loop {
            let captured = self.drain();
            if self.mono.is_empty() {
                break;
            }
            if let Some(watch) = self.silence.as_mut() {
                match watch.observe(&self.mono, Instant::now()) {
                    Some(SilenceEvent::Broken) => {
                        poll.notice(NoticeKey::Speaker, Some(SILENT_LANE_NOTICE.to_string()));
                    }
                    Some(SilenceEvent::Recovered) => poll.notice(NoticeKey::Speaker, None),
                    None => {}
                }
            }
            // Where the OS says this audio was captured. Without an anchor
            // (the backend's ring of them overflowed) assume it continues
            // where the last chunk left off.
            let at = match captured {
                Some(nanos) => poll.clock.position(nanos),
                None => self.next_position,
            };
            self.next_position = at + self.record_samples(self.mono.len());
            // Recorded here, ahead of the VAD and the silence gate: the files
            // hold what the device heard, not what the recognizer looked at.
            if let Some(rec) = poll.rec.as_deref_mut() {
                rec.write_at(poll.index, &self.mono, at);
            }
            let resampled = self.resampler.process(&self.mono)?;
            // The gap between this lane's own clock and the recording's, as
            // of the audio about to join the stream.
            let at_ms = at as u64 * 1000 / record::RECORD_RATE as u64;
            self.timeline
                .observe(self.stream_ms(), at_ms.saturating_sub(self.stream_ms()));
            self.stream_samples += resampled.len();
            self.recognizer.push_audio(&resampled);
            self.mono.clear();
        }
        let drops = self.session.dropped.load(Ordering::Relaxed);
        if drops > self.reported_drops {
            // interleaved sample count → wall-clock duration of lost audio
            let ms =
                drops as u64 * 1000 / (self.session.channels as u64 * self.session.src_rate as u64);
            poll.notice(
                NoticeKey::Overrun(self.lane),
                Some(format!("audio overrun: ~{ms}ms dropped so far")),
            );
            self.reported_drops = drops;
            self.overrun_until = Some(Instant::now() + OVERRUN_NOTICE);
        } else if self
            .overrun_until
            .is_some_and(|until| Instant::now() >= until)
        {
            // The overrun stopped a while ago; take the notice down.
            poll.notice(NoticeKey::Overrun(self.lane), None);
            self.overrun_until = None;
        }
        Ok(())
    }

    /// This lane's sample count expressed in recording samples.
    fn record_samples(&self, samples: usize) -> usize {
        samples * record::RECORD_RATE as usize / self.session.src_rate as usize
    }

    /// Move the ring's next contiguous run of audio into `mono`, downmixed,
    /// and report when the first frame of it was captured.
    ///
    /// The read stops at a time jump between callbacks (a dropout, or a tap
    /// that had nothing to deliver for a while): read past one, the hole
    /// would collapse and everything after it would sit early in the
    /// timeline. The caller drains in a loop, one pass per run.
    fn drain(&mut self) -> Option<u64> {
        let channels = self.session.channels;
        let mut frames = self.session.consumer.slots() / channels;
        if frames == 0 {
            return None;
        }
        let captured = self.captured_at();
        let limit = self.frames_read + frames as u64;
        if let Some(jump) =
            self.trail
                .jump_before(&mut self.session.times, limit, self.session.src_rate)
        {
            frames = (jump - self.frames_read) as usize;
        }
        let n = frames * channels;
        let Ok(chunk) = self.session.consumer.read_chunk(n) else {
            return None;
        };
        let (a, b) = chunk.as_slices();
        // frames may straddle the two slices, so iterate their concatenation
        let mut samples = a.iter().chain(b.iter());
        self.mono.reserve(n / channels);
        for _ in 0..n / channels {
            let mut frame = 0.0f32;
            for _ in 0..channels {
                frame += samples.next().expect("n is a multiple of channels");
            }
            self.mono.push(frame / channels as f32);
        }
        chunk.commit_all();
        self.frames_read += (n / channels) as u64;
        captured
    }

    /// Capture time of the next frame in the ring.
    fn captured_at(&mut self) -> Option<u64> {
        self.trail.nanos_at(
            &mut self.session.times,
            self.frames_read,
            self.session.src_rate,
        )
    }
}

/// Follows a lane's capture anchors as its audio is read, so any frame in the
/// ring can be given the time the OS says it was captured.
#[derive(Default)]
struct AnchorTrail {
    /// The latest anchor at or before the frames read so far.
    current: Option<capture::Anchor>,
}

impl AnchorTrail {
    /// When frame `frames_read` was captured. Anchors the reader has passed
    /// are dropped as it goes, which is also what keeps the backend's ring of
    /// them from filling up.
    ///
    /// Between anchors the device's own rate carries the time forward: frames
    /// within one callback are contiguous by construction. It is the jump
    /// *between* callbacks — a dropout, or a tap with nothing to deliver —
    /// that only the anchors can reveal.
    fn nanos_at(
        &mut self,
        times: &mut rtrb::Consumer<capture::Anchor>,
        frames_read: u64,
        rate: u32,
    ) -> Option<u64> {
        while let Ok(next) = times.peek() {
            if next.frame > frames_read {
                break;
            }
            self.current = Some(*next);
            let _ = times.pop();
        }
        let anchor = self.current?;
        let ahead = frames_read.saturating_sub(anchor.frame);
        Some(anchor.nanos + ahead * 1_000_000_000 / rate as u64)
    }

    /// The frame of the first pending anchor before `limit` whose time
    /// disagrees with the current clock — a real hole in the lane's audio.
    /// Anchors that agree (within [`ANCHOR_JUMP_NANOS`]) are absorbed as the
    /// scan passes them; they refine the clock but change nothing.
    fn jump_before(
        &mut self,
        times: &mut rtrb::Consumer<capture::Anchor>,
        limit: u64,
        rate: u32,
    ) -> Option<u64> {
        let mut current = self.current?;
        while let Ok(&next) = times.peek() {
            if next.frame >= limit {
                return None;
            }
            let expected =
                current.nanos + (next.frame - current.frame) * 1_000_000_000 / rate as u64;
            if next.nanos.abs_diff(expected) > ANCHOR_JUMP_NANOS {
                return Some(next.frame);
            }
            current = next;
            self.current = Some(next);
            let _ = times.pop();
        }
        None
    }
}

/// How long a lane may deliver nothing but digital silence before we call
/// it broken. Long enough that a quiet stretch in a meeting doesn't trip it.
const SILENCE_GRACE: Duration = Duration::from_secs(20);

const SILENT_LANE_NOTICE: &str = "speaker lane is receiving only silence — grant \
    System Settings > Privacy & Security > Screen & System Audio Recording";

/// What [`SilenceWatch`] concluded from one chunk of samples.
#[derive(Debug, PartialEq)]
enum SilenceEvent {
    /// Digital silence lasted the grace period: the permission is withheld.
    Broken,
    /// Real audio arrived after a report: the lane works after all, and the
    /// notice must not outlive the problem.
    Recovered,
}

/// Watches a lane for the one failure this pipeline cannot see as an error:
/// macOS withholds the system-audio permission by zero-filling the buffers
/// (docs/step0-tap-results.md), so every call succeeds and the transcript
/// just stays empty forever.
///
/// The tell is *samples that arrive and are all zero*. An output device with
/// nothing playing runs no IO cycle and so delivers no samples at all, which
/// is why an empty slice must not count as silence.
#[derive(Default)]
struct SilenceWatch {
    silent_since: Option<Instant>,
    reported: bool,
}

impl SilenceWatch {
    fn observe(&mut self, samples: &[f32], now: Instant) -> Option<SilenceEvent> {
        if samples.is_empty() {
            return None;
        }
        if samples.iter().any(|s| *s != 0.0) {
            self.silent_since = None;
            return std::mem::take(&mut self.reported).then_some(SilenceEvent::Recovered);
        }
        let since = *self.silent_since.get_or_insert(now);
        if !self.reported && now.duration_since(since) >= SILENCE_GRACE {
            self.reported = true;
            return Some(SilenceEvent::Broken);
        }
        None
    }
}

/// Below this, a change in a lane's stream-to-recording offset is clock
/// wobble, not a dropout: the 16 kHz resampler holds back up to ~21ms of
/// input between polls, so the offset jitters that much even on a lane that
/// never dropped a sample. Same idea as the recorder's snap on the write
/// side.
const TIMELINE_SLACK_MS: u64 = 30;

/// Converts a lane's own clock — the audio the recognizer was given — into
/// the recording's clock. The two differ by the silence the recorder had to
/// insert: the lane's late start, plus every stretch where its device
/// delivered nothing at all (a tap with nothing playing).
///
/// Only an offset change beyond [`TIMELINE_SLACK_MS`] — a real dropout —
/// records a new anchor, so a lane that never drops out keeps a single one.
/// Queries arrive in stream order, which is what lets spent anchors be
/// dropped as they are passed.
#[derive(Default)]
struct LaneTimeline {
    /// (stream ms from which it applies, offset ms), oldest first.
    anchors: Vec<(u64, u64)>,
}

impl LaneTimeline {
    /// Note the offset that applies to audio pushed at `stream_ms`.
    fn observe(&mut self, stream_ms: u64, offset_ms: u64) {
        if self
            .anchors
            .last()
            .is_none_or(|(_, at)| at.abs_diff(offset_ms) > TIMELINE_SLACK_MS)
        {
            self.anchors.push((stream_ms, offset_ms));
        }
    }

    /// Where `stream_ms` sits in the recording.
    fn recording_ms(&mut self, stream_ms: u64) -> u64 {
        while self.anchors.len() > 1 && self.anchors[1].0 <= stream_ms {
            self.anchors.remove(0);
        }
        let offset = match self.anchors.first() {
            Some((from, offset)) if *from <= stream_ms => *offset,
            // Before the first anchor there is nothing to correct for.
            _ => 0,
        };
        stream_ms + offset
    }
}

/// Decides which step outputs reach the UI. Idle steps are dropped, but an
/// all-empty output must still go out while a volatile tail is on screen:
/// LocalAgreement returns an empty volatile to *hide* a contradicted tail,
/// and swallowing that event would leave ghost text in the UI.
#[derive(Default)]
struct EmitGate {
    volatile_on_screen: bool,
}

impl EmitGate {
    fn should_emit(&mut self, out: &kikimimic_core::scheduler::StepOutput) -> bool {
        let has_news = !out.committed_delta.is_empty()
            || !out.volatile.is_empty()
            || out.utterance_final.is_some();
        let clears_tail = self.volatile_on_screen && out.volatile.is_empty();
        if !(has_news || clears_tail) {
            return false;
        }
        self.volatile_on_screen = !out.volatile.is_empty();
        true
    }
}

fn run_capture_loop(
    cmd_rx: &Receiver<Cmd>,
    ui: &Ui,
    backend: &AsrBackend,
    lanes: &mut [LaneRuntime],
    mut recorder: Option<&mut record::SessionRecorder>,
    mut notices: Notices,
    translations: &mut Translations,
) -> anyhow::Result<()> {
    let rules: &[GlossaryRule] = match backend {
        AsrBackend::Soniox { rules, .. } => rules,
        AsrBackend::Whisper(_) => &[],
    };
    let mut clock = SessionClock::default();
    let mut pending_output_device = PendingOutputDevice::default();
    // The setting may have been changed while idle, after the engines were
    // loaded — sync before the first decode, not just on later changes.
    if let AsrBackend::Whisper(models) = backend {
        sync_langs(ui, models, &translations.policy(), &mut notices);
    }
    emit_status(ui, "listening", notices.message());
    let interval = if matches!(backend, AsrBackend::Soniox { .. }) {
        SONIOX_STEP_INTERVAL
    } else {
        STEP_INTERVAL
    };
    loop {
        match cmd_rx.recv_timeout(POLL_INTERVAL) {
            Ok(Cmd::Stop) => break,
            Ok(Cmd::Start { .. }) => {} // already running
            Ok(Cmd::Split { reply, .. }) => {
                let _ = reply.send(Err(anyhow::anyhow!("録音の終了後に話者を分離してください")));
            }
            Ok(Cmd::RespondOutputDevice {
                prompt_id,
                switch_device,
            }) => {
                if let Some(decision) = pending_output_device.decide(prompt_id, switch_device) {
                    present_output_device_prompt(ui, None);
                    if let Err(error) = send_output_device_decision(lanes, decision) {
                        notices.set(
                            NoticeKey::Speaker,
                            Some(format!("output device decision failed: {error}")),
                        );
                        emit_status(ui, "listening", notices.message());
                    }
                } else {
                    // The prompt webview can receive an event before its
                    // initial snapshot resolves. Re-emit the authoritative
                    // prompt so a stale response cannot leave it disabled.
                    present_output_device_prompt(ui, pending_output_device.prompt.clone());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break, // app shutdown
        }
        for event in output_device_events(lanes) {
            apply_output_device_event(ui, &mut pending_output_device, &mut notices, event);
        }
        let policy = translations.policy();
        if let AsrBackend::Whisper(models) = backend {
            sync_langs(ui, models, &policy, &mut notices);
        }
        // A single spoken language is pinned, which skips detection outright;
        // two leave the choice to the engine, restricted to those two.
        let lang = policy.pinned_lang().map(str::to_string);
        for (index, lane) in lanes.iter_mut().enumerate() {
            let mut poll = Poll {
                ui,
                clock: &mut clock,
                rec: recorder.as_deref_mut(),
                notices: &mut notices,
                index,
                tr: translations,
                rules,
            };
            lane.tick(&mut poll, lang.as_deref(), interval)?;
        }
        collect_translations(
            ui,
            &mut notices,
            recorder.as_deref_mut(),
            translations,
            "listening",
        );
        // After every lane has had its say about this moment.
        if let Some(rec) = recorder.as_deref_mut() {
            rec.pump_mix();
            report_recording_failure(ui, &mut notices, rec);
        }
    }
    // Every lane gets to flush its tail even when another failed: the
    // speaker's last utterance must not vanish because the mic died at Stop.
    let lang = translations.policy().pinned_lang().map(str::to_string);
    let mut result = Ok(());
    for (index, lane) in lanes.iter_mut().enumerate() {
        let mut poll = Poll {
            ui,
            clock: &mut clock,
            rec: recorder.as_deref_mut(),
            notices: &mut notices,
            index,
            tr: translations,
            rules,
        };
        let finished = lane.finish(&mut poll, lang.as_deref());
        result = result.and(finished);
    }
    drain_translations(ui, &mut notices, recorder.as_deref_mut(), translations);
    // The finish writes above can be the ones that fail; a failure here has
    // no later poll to report it.
    if let Some(rec) = recorder {
        report_recording_failure(ui, &mut notices, rec);
    }
    result
}

/// Point the recogniser's restricted detection at the languages the policy
/// now names. No model reload is involved — `transcribe` reads the set per
/// decode — which is what lets this happen mid-recording.
fn sync_langs(ui: &Ui, models: &Models, policy: &LanguagePolicy, notices: &mut Notices) {
    let models = &mut *models.borrow_mut();
    if policy.spoken == models.spoken {
        return;
    }
    let notice = match models.engine.set_allowed_langs(&policy.spoken) {
        Ok(()) => {
            models.spoken = policy.spoken.clone();
            None
        }
        // The old set stays in force; the session keeps running in it rather
        // than falling back to whisper's unrestricted detection.
        Err(e) => Some(format!("language setting ignored: {e:#}")),
    };
    if notices.set(NoticeKey::Language, notice) {
        emit_status(ui, "listening", notices.message());
    }
}

/// Hand every finished translation to the UI and the transcript.
fn collect_translations(
    ui: &Ui,
    notices: &mut Notices,
    mut recorder: Option<&mut record::SessionRecorder>,
    translations: &mut Translations,
    state: &'static str,
) {
    let first_id = translations.first_id;
    for outcome in translations.lane.collect() {
        translations.outstanding = translations.outstanding.saturating_sub(1);
        apply_translation(
            ui,
            notices,
            recorder.as_deref_mut(),
            outcome,
            state,
            first_id,
        );
    }
}

/// `state` is the status the session is in while this runs — a notice raised
/// during the Stop drain must not put "listening" back on screen.
///
/// `first_id` is the session's first utterance: a translation from a session
/// that ended before its backlog cleared still belongs on its line (the UI
/// keeps it), but not in *this* session's transcript file, which has no such
/// utterance in it.
fn apply_translation(
    ui: &Ui,
    notices: &mut Notices,
    recorder: Option<&mut record::SessionRecorder>,
    outcome: Outcome,
    state: &'static str,
    first_id: u64,
) {
    let (text, notice) = match outcome.status {
        Status::Done(text) => (Some(text), None),
        // The line simply stops waiting; the sentence is still transcribed.
        Status::Failed(msg) => (None, Some(msg)),
    };
    if let (Some(rec), Some(text)) = (recorder, text.as_deref()) {
        if outcome.id >= first_id {
            rec.write_translation(outcome.id, &outcome.target, text);
        }
    }
    emit_translation(ui, outcome.lane, outcome.id, text, Some(outcome.target));
    // A success takes the previous failure's notice down, so a transient
    // error does not sit on the status line for the rest of the meeting.
    if notices.set(NoticeKey::Translation, notice) {
        emit_status(ui, state, notices.message());
    }
}

/// How long Stop waits for the sentences still with the translator. Long
/// enough for a queue's worth at the measured ~1s a sentence, short enough
/// that a wedged backend cannot hold the session open.
const TRANSLATE_DRAIN: Duration = Duration::from_secs(10);

/// Collect what the translator still owes before the session closes: the last
/// sentences of a meeting are the ones most likely to matter, and the
/// transcript file is about to be closed.
fn drain_translations(
    ui: &Ui,
    notices: &mut Notices,
    mut recorder: Option<&mut record::SessionRecorder>,
    translations: &mut Translations,
) {
    collect_translations(
        ui,
        notices,
        recorder.as_deref_mut(),
        translations,
        "stopping",
    );
    if translations.outstanding == 0 {
        return;
    }
    // Not "listening" any more, but not done either — the UI's Record button
    // must already read as stopped while this finishes.
    emit_status(
        ui,
        "stopping",
        Some(format!(
            "{} sentence(s) still being translated",
            translations.outstanding
        )),
    );
    let deadline = Instant::now() + TRANSLATE_DRAIN;
    while translations.outstanding > 0 {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let Some(outcome) = translations.lane.wait(remaining) else {
            break;
        };
        translations.outstanding -= 1;
        apply_translation(
            ui,
            notices,
            recorder.as_deref_mut(),
            outcome,
            "stopping",
            translations.first_id,
        );
    }
    // Whatever is left is not coming: clear the queue and let those lines
    // stop waiting rather than leaving them mid-translation forever.
    for job in translations.lane.abandon() {
        emit_translation(ui, job.lane, job.id, None, None);
    }
    translations.outstanding = 0;
}

/// A write failure stops the recording but not the session, so the UI has to
/// be told — once — that the files it was pointed at are no longer growing.
fn report_recording_failure(ui: &Ui, notices: &mut Notices, rec: &mut record::SessionRecorder) {
    if let Some(msg) = rec.take_failure() {
        emit_recording_dir(ui, None);
        if notices.set(
            NoticeKey::Recording,
            Some(format!("recording stopped: {msg}")),
        ) {
            emit_status(ui, "listening", notices.message());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kikimimic_core::scheduler::StepOutput;

    #[test]
    fn glossary_corrects_final_text_before_translation() {
        let rules = vec![GlossaryRule {
            from: "クロードコード".into(),
            to: "Claude Code".into(),
        }];
        assert!(validate_glossary(&rules).is_ok());
        assert_eq!(
            apply_glossary("クロードコードとクロードコード", &rules),
            "Claude CodeとClaude Code"
        );
        assert_eq!(apply_glossary("別の名前", &rules), "別の名前");
        assert!(validate_glossary(&[GlossaryRule {
            from: String::new(),
            to: "x".into()
        }])
        .is_err());
    }

    #[test]
    fn capture_start_timeout_releases_the_pipeline() {
        let (_ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let error = wait_capture_ready(ready_rx, &stop, Duration::from_millis(1))
            .err()
            .expect("capture should time out");
        assert!(error.to_string().contains("did not start"));
        assert!(stop.load(Ordering::SeqCst));
    }

    fn out(committed: &str, volatile: &str) -> StepOutput {
        StepOutput {
            committed_delta: committed.into(),
            volatile: volatile.into(),
            utterance_final: None,
        }
    }

    fn output_device(uid: &str, name: &str) -> capture::OutputDevice {
        capture::OutputDevice {
            uid: uid.into(),
            name: name.into(),
        }
    }

    #[test]
    fn output_device_prompt_coalesces_duplicates_and_replaces_newer_devices() {
        let built_in = output_device("built-in", "MacBookのスピーカー");
        let headphones = output_device("headphones", "ヘッドフォン");
        let display = output_device("display", "ディスプレイ");
        let mut pending = PendingOutputDevice::default();

        assert!(pending.observe(&built_in, &headphones));
        let first_id = pending.prompt.as_ref().unwrap().id;
        assert!(!pending.observe(&built_in, &headphones));
        assert_eq!(pending.prompt.as_ref().unwrap().id, first_id);

        assert!(pending.observe(&built_in, &display));
        let latest_id = pending.prompt.as_ref().unwrap().id;
        assert_ne!(latest_id, first_id);
        assert_eq!(
            pending.prompt.as_ref().unwrap().detected_name,
            "ディスプレイ"
        );

        assert!(pending.decide(first_id, true).is_none());
        assert!(matches!(
            pending.decide(latest_id, false),
            Some(capture::OutputDeviceSwitchDecision::Cancel { expected_uid })
                if expected_uid == "display"
        ));
        assert!(pending.prompt.is_none());
    }

    #[test]
    fn returning_to_the_captured_output_dismisses_the_prompt() {
        let built_in = output_device("built-in", "MacBookのスピーカー");
        let headphones = output_device("headphones", "ヘッドフォン");
        let mut pending = PendingOutputDevice::default();

        assert!(pending.observe(&built_in, &headphones));
        assert!(pending.observe(&built_in, &built_in));
        assert!(pending.prompt.is_none());
        assert!(pending.expected_uid.is_none());
    }

    #[test]
    fn gate_drops_idle_steps_but_passes_a_volatile_clear() {
        let mut gate = EmitGate::default();
        assert!(!gate.should_emit(&out("", ""))); // nothing shown yet: idle
        assert!(gate.should_emit(&out("", "こんにち"))); // tail appears
                                                         // agreement hid the contradicted tail: the UI must be told
        assert!(gate.should_emit(&out("", "")));
        assert!(!gate.should_emit(&out("", ""))); // already clear: idle again
    }

    #[test]
    fn gate_tracks_the_tail_across_commits() {
        let mut gate = EmitGate::default();
        assert!(gate.should_emit(&out("", "こんにち")));
        assert!(gate.should_emit(&out("こんにちは、", "せ")));
        // a boundary flush ends with an empty tail; nothing stays on screen
        assert!(gate.should_emit(&out("世界", "")));
        assert!(!gate.should_emit(&out("", "")));
    }

    #[test]
    fn digital_silence_for_long_enough_reports_a_withheld_permission() {
        let mut watch = SilenceWatch::default();
        let t0 = Instant::now();
        // Callbacks arriving full of zeros: the lane is running, so this is
        // not an idle device — it is the permission being withheld.
        assert_eq!(watch.observe(&[0.0; 16], t0), None);
        assert_eq!(
            watch.observe(&[0.0; 16], t0 + SILENCE_GRACE),
            Some(SilenceEvent::Broken)
        );
        // Said once: repeating it every poll would bury the real status.
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE * 2), None);
    }

    #[test]
    fn audio_arriving_after_a_report_takes_the_notice_down() {
        // Permission granted mid-session: the tap starts delivering real
        // audio, and the warning must not outlive the problem.
        let mut watch = SilenceWatch::default();
        let t0 = Instant::now();
        assert_eq!(watch.observe(&[0.0; 16], t0), None);
        assert_eq!(
            watch.observe(&[0.0; 16], t0 + SILENCE_GRACE),
            Some(SilenceEvent::Broken)
        );
        assert_eq!(
            watch.observe(&[0.1; 16], t0 + SILENCE_GRACE * 2),
            Some(SilenceEvent::Recovered)
        );
        // and silence must again last the grace period before a new report
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE * 3), None);
    }

    #[test]
    fn one_sources_notice_does_not_erase_anothers() {
        let mut notices = Notices::default();
        assert!(notices.set(NoticeKey::Speaker, Some("speaker gone".into())));
        assert!(notices.set(NoticeKey::Overrun(Lane::Mic), Some("overrun".into())));
        assert_eq!(notices.message().as_deref(), Some("speaker gone / overrun"));
        // the overrun expiring takes down only its own line
        assert!(notices.set(NoticeKey::Overrun(Lane::Mic), None));
        assert_eq!(notices.message().as_deref(), Some("speaker gone"));
    }

    #[test]
    fn an_unchanged_notice_is_not_worth_a_re_emit() {
        let mut notices = Notices::default();
        assert!(notices.set(NoticeKey::Recording, Some("stopped".into())));
        assert!(!notices.set(NoticeKey::Recording, Some("stopped".into())));
        // clearing a notice that was never up changes nothing either
        assert!(!notices.set(NoticeKey::Decode(Lane::Mic), None));
        assert_eq!(notices.message().as_deref(), Some("stopped"));
    }

    #[test]
    fn no_notices_means_no_message() {
        assert_eq!(Notices::default().message(), None);
    }

    #[test]
    fn an_idle_device_delivering_no_samples_is_not_silence() {
        // Nothing playing means no IO cycle and no samples — a working lane
        // waiting for audio, which must not be reported as broken.
        let mut watch = SilenceWatch::default();
        let t0 = Instant::now();
        assert_eq!(watch.observe(&[], t0), None);
        assert_eq!(watch.observe(&[], t0 + SILENCE_GRACE * 10), None);
    }

    #[test]
    fn audible_samples_clear_a_pending_silence_run() {
        let mut watch = SilenceWatch::default();
        let t0 = Instant::now();
        assert_eq!(watch.observe(&[0.0; 16], t0), None);
        assert_eq!(watch.observe(&[0.0, 0.2], t0 + SILENCE_GRACE / 2), None);
        // the clock restarts from the audible sample, not from t0
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE), None);
    }

    #[test]
    fn gate_passes_an_utterance_final_even_without_text_deltas() {
        let mut gate = EmitGate::default();
        let fin = StepOutput {
            committed_delta: String::new(),
            volatile: String::new(),
            utterance_final: Some(kikimimic_core::scheduler::Utterance {
                text: "こんにちは".into(),
                start_ms: 0,
                end_ms: 1_000,
                lang: Some("ja".into()),
            }),
        };
        assert!(gate.should_emit(&fin));
    }

    const RATE: u32 = 48_000;
    const MS: u64 = 1_000_000;

    /// An anchor queue with the given callbacks already stamped into it.
    fn anchors(
        entries: &[(u64, u64)],
    ) -> (
        rtrb::Producer<capture::Anchor>,
        rtrb::Consumer<capture::Anchor>,
    ) {
        let (mut tx, rx) = rtrb::RingBuffer::new(16);
        for (frame, nanos) in entries {
            tx.push(capture::Anchor {
                frame: *frame,
                nanos: *nanos,
            })
            .expect("room for the test's anchors");
        }
        (tx, rx)
    }

    #[test]
    fn a_frame_is_timed_from_the_anchor_it_arrived_under() {
        let (_tx, mut times) = anchors(&[(0, 1_000 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(trail.nanos_at(&mut times, 0, RATE), Some(1_000 * MS));
        // 480 frames at 48 kHz is 10ms further into the stream
        assert_eq!(trail.nanos_at(&mut times, 480, RATE), Some(1_010 * MS));
    }

    #[test]
    fn a_dropout_between_callbacks_shows_up_as_a_jump() {
        // The tap stopped delivering for a second: the frames either side are
        // adjacent in the ring but a second apart in time, and only the
        // anchors say so.
        let (_tx, mut times) = anchors(&[(0, 1_000 * MS), (480, 2_000 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(trail.nanos_at(&mut times, 0, RATE), Some(1_000 * MS));
        assert_eq!(trail.nanos_at(&mut times, 480, RATE), Some(2_000 * MS));
        assert_eq!(trail.nanos_at(&mut times, 960, RATE), Some(2_010 * MS));
    }

    #[test]
    fn anchors_the_reader_has_passed_are_consumed() {
        // Otherwise the backend's ring of them fills up and the lane loses
        // its timestamps.
        let (_tx, mut times) = anchors(&[(0, 0), (480, 10 * MS), (960, 20 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(trail.nanos_at(&mut times, 960, RATE), Some(20 * MS));
        assert_eq!(times.slots(), 0, "all three were passed");
    }

    #[test]
    fn a_lane_that_has_delivered_nothing_yet_has_no_time() {
        let (_tx, mut times) = anchors(&[]);
        let mut trail = AnchorTrail::default();
        assert_eq!(trail.nanos_at(&mut times, 0, RATE), None);
    }

    /// Recognises nothing. These tests drive `drain` and the timeline, which
    /// happen before any decode — and a stub is only possible because the
    /// lane holds a [`Recognizer`] rather than a loaded model.
    struct NoRecognizer;

    impl Recognizer for NoRecognizer {
        fn push_audio(&mut self, _samples: &[f32]) {}
        fn step(&mut self, _lang: Option<&str>) -> anyhow::Result<Option<StepOutput>> {
            Ok(None)
        }
        fn finish(&mut self, _lang: Option<&str>) -> anyhow::Result<Option<StepOutput>> {
            Ok(None)
        }
    }

    /// A LaneRuntime over hand-fed rings, for driving `drain` directly.
    fn test_lane() -> (
        rtrb::Producer<f32>,
        rtrb::Producer<capture::Anchor>,
        LaneRuntime,
    ) {
        let (audio_tx, consumer) = rtrb::RingBuffer::new(RATE as usize);
        let (anchor_tx, times) = rtrb::RingBuffer::new(16);
        let session = capture::CaptureSession {
            consumer,
            times,
            src_rate: RATE,
            channels: 1,
            dropped: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: Arc::new(Mutex::new(None)),
            output_device_switch: None,
        };
        let lane =
            LaneRuntime::new(Lane::Mic, session, Box::new(NoRecognizer)).expect("48 kHz lane");
        (audio_tx, anchor_tx, lane)
    }

    fn feed(
        audio: &mut rtrb::Producer<f32>,
        anchors: &mut rtrb::Producer<capture::Anchor>,
        frame: u64,
        nanos: u64,
        samples: &[f32],
    ) {
        anchors
            .push(capture::Anchor { frame, nanos })
            .expect("room for the anchor");
        for s in samples {
            audio.push(*s).expect("room for the audio");
        }
    }

    #[test]
    fn a_time_jump_inside_one_drain_splits_the_read() {
        // Two callbacks a second apart sit in the ring together (a decode
        // stall kept the worker away). Read as one chunk, the second of
        // silence between them would collapse and the later audio would sit
        // a second early in the timeline.
        let (mut audio, mut anchors, mut lane) = test_lane();
        feed(&mut audio, &mut anchors, 0, 1_000 * MS, &[0.1; 480]);
        feed(&mut audio, &mut anchors, 480, 2_000 * MS, &[0.2; 480]);

        assert_eq!(lane.drain(), Some(1_000 * MS));
        assert_eq!(lane.mono.len(), 480, "only up to the jump");
        lane.mono.clear();
        assert_eq!(lane.drain(), Some(2_000 * MS), "the hole survives");
        assert_eq!(lane.mono.len(), 480);
    }

    #[test]
    fn contiguous_callbacks_drain_as_one_chunk() {
        // 480 frames at 48 kHz is 10ms: the second anchor sits exactly where
        // the first predicts it, so nothing splits.
        let (mut audio, mut anchors, mut lane) = test_lane();
        feed(&mut audio, &mut anchors, 0, 1_000 * MS, &[0.1; 480]);
        feed(&mut audio, &mut anchors, 480, 1_010 * MS, &[0.2; 480]);

        assert_eq!(lane.drain(), Some(1_000 * MS));
        assert_eq!(lane.mono.len(), 960);
        assert_eq!(lane.session.times.slots(), 0, "both anchors consumed");
    }

    #[test]
    fn the_session_clock_starts_at_the_first_moment_it_is_shown() {
        let mut clock = SessionClock::default();
        assert_eq!(clock.position(5_000 * MS), 0);
        // half a second later, in recording samples
        assert_eq!(clock.position(5_500 * MS), record::RECORD_RATE as usize / 2);
        // a lane whose first buffer predates the zero clamps to it rather
        // than going backwards
        assert_eq!(clock.position(4_000 * MS), 0);
    }

    #[test]
    fn a_lane_that_never_drops_out_is_its_own_recording_clock() {
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 0);
        timeline.observe(100, 0); // same offset: no second anchor
        assert_eq!(timeline.recording_ms(0), 0);
        assert_eq!(timeline.recording_ms(2_000), 2_000);
    }

    #[test]
    fn a_lanes_late_start_shifts_everything_it_says() {
        // The speaker tap opens after the mic, so its stream zero is 300ms
        // into the recording.
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 300);
        assert_eq!(timeline.recording_ms(0), 300);
        assert_eq!(timeline.recording_ms(5_000), 5_300);
    }

    #[test]
    fn offset_wobble_within_the_slack_is_not_a_dropout() {
        // The 16 kHz resampler's holdback makes the offset jitter by ~21ms
        // per poll; recording an anchor for each would bury the real
        // dropouts under bookkeeping.
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 100);
        timeline.observe(50, 115);
        timeline.observe(100, 92);
        assert_eq!(timeline.anchors.len(), 1);
        assert_eq!(timeline.recording_ms(200), 300);
    }

    #[test]
    fn audio_from_before_a_dropout_keeps_its_old_offset() {
        // The tap went quiet for 10s; the recorder padded the lane's file.
        // An utterance flushed *after* that padding but spoken *before* it
        // must not be dragged 10s later — that is what the anchors are for.
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 0);
        timeline.observe(4_000, 10_000); // 4s of audio in, 10s of silence added
        assert_eq!(timeline.recording_ms(3_000), 3_000);
        assert_eq!(timeline.recording_ms(4_500), 14_500);
        // and once the query has moved past, the spent anchor is gone
        assert_eq!(timeline.anchors.len(), 1);
    }
}
