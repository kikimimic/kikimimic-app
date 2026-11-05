//! Tauri shell: UI commands feed the pipeline worker (pipeline.rs), which
//! owns the capture sessions (capture/), the ASR engines, the scheduler, and
//! the session recording (record.rs), and emits `transcript`/`status` events
//! back to the UI.

mod capture;
mod library;
mod model_files;
mod pipeline;
mod postprocess;
mod record;
mod soniox;
mod translate;

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use kikimimic_core::language::LanguagePolicy;
use tauri::{Manager, State};

struct PipelineHandle {
    cmd_tx: Mutex<Sender<pipeline::Cmd>>,
    status: pipeline::StatusStore,
    policy: pipeline::PolicyStore,
    output_device_prompt: pipeline::OutputDevicePromptStore,
    postprocess: Arc<Mutex<()>>,
}

impl PipelineHandle {
    fn send(&self, cmd: pipeline::Cmd) -> Result<(), String> {
        self.cmd_tx
            .lock()
            .unwrap()
            .send(cmd)
            .map_err(|_| "pipeline worker is gone".to_string())
    }
}

#[tauri::command]
fn start_capture(state: State<'_, PipelineHandle>) -> Result<(), String> {
    state.send(pipeline::Cmd::Start {
        mimi: false,
        rules: Vec::new(),
    })
}

#[tauri::command]
fn start_mimi_capture(
    state: State<'_, PipelineHandle>,
    rules: Vec<pipeline::GlossaryRule>,
) -> Result<(), String> {
    pipeline::validate_glossary(&rules)?;
    state.send(pipeline::Cmd::Start { mimi: true, rules })
}

#[tauri::command]
fn stop_capture(state: State<'_, PipelineHandle>) -> Result<(), String> {
    state.send(pipeline::Cmd::Stop)
}

/// Snapshot for a webview that (re)loads mid-session: status events only
/// fire on change, so without this it would show "idle" until the next one.
#[tauri::command]
fn get_status(state: State<'_, PipelineHandle>) -> pipeline::StatusPayload {
    state.status.lock().unwrap().clone()
}

#[tauri::command]
fn get_output_device_prompt(
    state: State<'_, PipelineHandle>,
) -> Option<pipeline::OutputDevicePrompt> {
    state.output_device_prompt.lock().unwrap().clone()
}

#[tauri::command]
fn respond_output_device_change(
    state: State<'_, PipelineHandle>,
    prompt_id: u64,
    switch_device: bool,
) -> Result<(), String> {
    state.send(pipeline::Cmd::RespondOutputDevice {
        prompt_id,
        switch_device,
    })
}

/// The sessions already on disk, newest first. Read on every visit to the
/// library rather than cached: the folder is the user's to move things in and
/// out of, and a list that disagrees with Finder would be worse than a
/// directory scan.
#[tauri::command]
fn list_recordings(app: tauri::AppHandle) -> Result<Vec<library::Recording>, String> {
    let base = library::base(&app).map_err(|e| format!("{e:#}"))?;
    Ok(library::list(&base))
}

/// A past session's transcript, joined back together from its append log.
/// Takes the directory the list handed out rather than a name, so a recording
/// reached from anywhere resolves the same way.
#[tauri::command]
fn read_recording(dir: String) -> Result<library::Session, String> {
    library::read(std::path::Path::new(&dir)).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn diarize_recording(
    app: tauri::AppHandle,
    state: State<'_, PipelineHandle>,
    dir: String,
) -> Result<library::Session, String> {
    let lock = Arc::clone(&state.postprocess);
    let status = state.status.clone();
    let cmd_tx = state.cmd_tx.lock().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = lock
            .try_lock()
            .map_err(|_| "話者分離はすでに実行中です".to_string())?;
        let base = library::base(&app).map_err(|e| format!("{e:#}"))?;
        let dir = library::resolve_recording(&base, std::path::Path::new(&dir))
            .map_err(|e| format!("{e:#}"))?;
        // Splitting lines re-recognises them with the pipeline's own models,
        // which a session in progress is using.
        if matches!(
            status.lock().unwrap().state,
            "loading" | "listening" | "stopping"
        ) {
            return Err("録音の終了後に話者を分離してください".into());
        }
        postprocess::run(&app, &dir).map_err(|e| format!("{e:#}"))?;
        let (reply, answer) = std::sync::mpsc::channel();
        cmd_tx
            .send(pipeline::Cmd::Split {
                dir: dir.clone(),
                reply,
            })
            .map_err(|_| "pipeline worker is gone".to_string())?;
        answer
            .recv()
            .map_err(|_| "pipeline worker is gone".to_string())?
            .map_err(|e| format!("{e:#}"))?;
        library::read(&dir).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Change only the title users see. The timestamped directory remains the
/// session's stable identity and keeps its calendar placement intact.
#[tauri::command]
fn set_recording_title(
    app: tauri::AppHandle,
    dir: String,
    title: String,
) -> Result<String, String> {
    let base = library::base(&app).map_err(|e| format!("{e:#}"))?;
    library::set_title(&base, std::path::Path::new(&dir), &title).map_err(|e| format!("{e:#}"))
}

/// The language settings as the UI exchanges them. Kept here rather than
/// derived on [`LanguagePolicy`] so the core stays free of the shell's wire
/// format (ADR-153800).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LanguagesPayload {
    /// Whisper language codes that may be spoken; one pins detection off.
    spoken: Vec<String>,
    /// Where translations go; `None` is "don't translate".
    target: Option<String>,
    /// Also translate the target language into the other spoken one.
    mutual: bool,
}

#[tauri::command]
fn get_languages(state: State<'_, PipelineHandle>) -> LanguagesPayload {
    let policy = state.policy.lock().unwrap();
    LanguagesPayload {
        spoken: policy.spoken.clone(),
        target: policy.target.clone(),
        mutual: policy.mutual,
    }
}

/// Takes effect from the next utterance — including mid-recording, which is
/// the point: a meeting that turns out to be in the other language should not
/// need to be restarted.
#[tauri::command]
fn set_languages(
    state: State<'_, PipelineHandle>,
    languages: LanguagesPayload,
) -> Result<(), String> {
    let mut spoken = languages.spoken;
    spoken.dedup();
    if spoken.is_empty() {
        return Err("at least one spoken language is required".into());
    }
    // Beyond two, detection on a one-second window is a guess rather than a
    // decision (docs/step0-results.md), so the setting simply does not exist.
    if spoken.len() > 2 {
        return Err("at most two spoken languages".into());
    }
    *state.policy.lock().unwrap() = LanguagePolicy {
        spoken,
        target: languages.target.filter(|t| !t.is_empty()),
        mutual: languages.mutual,
    };
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = dotenvy::dotenv();
    let checkout_env = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path(checkout_env);
    tauri::Builder::default()
        .setup(|app| {
            let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
            let status = pipeline::new_status_store();
            let policy = pipeline::new_policy_store();
            let output_device_prompt = pipeline::new_output_device_prompt_store();
            let ui = pipeline::Ui {
                app: app.handle().clone(),
                status: status.clone(),
                output_device_prompt: output_device_prompt.clone(),
            };
            let worker_policy = policy.clone();
            std::thread::spawn(move || pipeline::run(cmd_rx, ui, worker_policy));
            app.manage(PipelineHandle {
                cmd_tx: Mutex::new(cmd_tx),
                status,
                policy,
                output_device_prompt,
                postprocess: Arc::new(Mutex::new(())),
            });
            app.manage(model_files::Downloads::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_capture,
            start_mimi_capture,
            stop_capture,
            get_status,
            get_output_device_prompt,
            respond_output_device_change,
            get_languages,
            set_languages,
            list_recordings,
            read_recording,
            diarize_recording,
            set_recording_title,
            model_files::model_status,
            model_files::download_model
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
