//! Downloading the three model files the pipeline resolves.
//!
//! Nothing here loads weights. A command streams bytes into
//! `app_data_dir()/models` (created when a download starts) under a
//! `.partial` name, and [`kikimimic_core::models::install_model_file`] renames that
//! onto the real filename only after the body has arrived. The resolver
//! searches that directory first, then the checkout's `models/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kikimimic_core::models::{self, Model, ModelManager};
use tauri::{AppHandle, Emitter, Manager, State};

/// Whisper large-v3-turbo, q8_0. The upstream whisper.cpp model repository
/// publishes this exact filename.
/// https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin
const WHISPER_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin";

/// Silero VAD in whisper.cpp's GGML packaging. `download-vad-model.sh` in
/// ggml-org/whisper.cpp fetches this exact file from `ggml-org/whisper-vad`.
/// https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin
const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

/// Qwen3-4B-Instruct-2507 at Q4_K_M. unsloth publishes this exact filename,
/// which is also the file llama.cpp requests from that repo. bartowski's
/// widely used quant is named `Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf`
/// (a `Qwen_` prefix), so it would not match the resolver.
/// https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf
const LLM_URL: &str = "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf";

const DOWNLOAD_EVENT: &str = "model-download";

struct Spec {
    id: &'static str,
    model: Model,
    url: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        id: "asr",
        model: Model::Asr,
        url: WHISPER_URL,
    },
    Spec {
        id: "vad",
        model: Model::Vad,
        url: VAD_URL,
    },
    Spec {
        id: "translator",
        model: Model::Translator,
        url: LLM_URL,
    },
];

struct Active {
    received: u64,
    total: Option<u64>,
}

/// Downloads currently streaming. The UI reads this from [`model_status`] so
/// a screen opened mid-download still shows progress.
#[derive(Clone)]
pub struct Downloads {
    inner: Arc<Mutex<HashMap<String, Active>>>,
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Downloads {
    fn begin(&self, id: &str) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(id) {
            return Err("このモデルはすでにダウンロード中です".into());
        }
        map.insert(
            id.to_string(),
            Active {
                received: 0,
                total: None,
            },
        );
        Ok(())
    }

    fn end(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    fn tick(&self, id: &str, received: u64, total: Option<u64>) {
        if let Some(active) = self.inner.lock().unwrap().get_mut(id) {
            active.received = received;
            active.total = total;
        }
    }
}

/// Clears the in-progress flag when the download task ends, including when
/// the command future is dropped while the file is still streaming.
struct FinishDownload(Downloads, String);

impl Drop for FinishDownload {
    fn drop(&mut self) {
        self.0.end(&self.1);
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFileStatus {
    pub id: String,
    pub file_name: String,
    /// `missing`, `ready`, or `downloading`.
    pub state: String,
    pub path: Option<String>,
    pub bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// False when the file is already resolved, a download is running, or an
    /// environment variable points at a path this download cannot replace.
    pub can_download: bool,
    pub message: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadEvent {
    id: String,
    received_bytes: u64,
    total_bytes: Option<u64>,
}

/// Where models are looked for, in order: what a download put in the app's
/// own directory first, then the checkout's `models/` so `cargo run` works
/// without one. The latter is a build-machine path and only ever a fallback.
pub fn manager(app: &AppHandle) -> ModelManager {
    let mut dirs = Vec::new();
    if let Ok(dir) = app_models_dir(app) {
        dirs.push(dir);
    }
    dirs.push(checkout_models_dir());
    ModelManager::new(dirs)
}

fn checkout_models_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../models"))
}

fn app_models_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("models"))
        .map_err(|err| format!("アプリデータの場所を取得できませんでした: {err}"))
}

fn spec_by_id(id: &str) -> Result<&'static Spec, String> {
    SPECS
        .iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| format!("不明なモデルです: {id}"))
}

/// One row per required model. Filesystem only — model weights stay unloaded.
#[tauri::command]
pub fn model_status(app: AppHandle, downloads: State<'_, Downloads>) -> Vec<ModelFileStatus> {
    let manager = manager(&app);
    let active = downloads.inner.lock().unwrap();
    SPECS
        .iter()
        .map(|spec| {
            classify(
                spec,
                manager.resolve(spec.model).ok(),
                std::env::var(spec.model.env_var()).ok(),
                active.get(spec.id),
            )
        })
        .collect()
}

fn classify(
    spec: &Spec,
    resolved: Option<PathBuf>,
    env_value: Option<String>,
    active: Option<&Active>,
) -> ModelFileStatus {
    let file_name = spec.model.default_file().to_string();
    if let Some(active) = active {
        return ModelFileStatus {
            id: spec.id.to_string(),
            file_name,
            state: "downloading".into(),
            path: None,
            bytes: Some(active.received),
            total_bytes: active.total,
            can_download: false,
            message: None,
        };
    }
    if let Some(path) = resolved {
        let bytes = std::fs::metadata(&path).ok().map(|meta| meta.len());
        return ModelFileStatus {
            id: spec.id.to_string(),
            file_name,
            state: "ready".into(),
            path: Some(path.display().to_string()),
            bytes,
            total_bytes: None,
            can_download: false,
            message: None,
        };
    }
    // An override is used as given and does not fall back. Saving a copy
    // into the app-data directory would not be the file that loads.
    if let Some(value) = env_value {
        let variable = spec.model.env_var();
        let message = if value.is_empty() {
            format!("環境変数 {variable} が空です。外すか、実在するファイルを指定してください。")
        } else {
            format!(
                "環境変数 {variable} が「{value}」を指していますが、そのファイルがありません。ダウンロードはアプリデータに保存されるため、この指定があるあいだは使われません。"
            )
        };
        return ModelFileStatus {
            id: spec.id.to_string(),
            file_name,
            state: "missing".into(),
            path: None,
            bytes: None,
            total_bytes: None,
            can_download: false,
            message: Some(message),
        };
    }
    ModelFileStatus {
        id: spec.id.to_string(),
        file_name,
        state: "missing".into(),
        path: None,
        bytes: None,
        total_bytes: None,
        can_download: true,
        message: None,
    }
}

/// Download one model. Progress is the `model-download` event; the file
/// appears to the resolver only after the stream finishes.
#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    downloads: State<'_, Downloads>,
    id: String,
) -> Result<(), String> {
    let spec = spec_by_id(&id)?;
    if manager(&app).resolve(spec.model).is_ok() {
        return Ok(());
    }
    if let Ok(value) = std::env::var(spec.model.env_var()) {
        return Err(classify(spec, None, Some(value), None)
            .message
            .unwrap_or_else(|| "モデルの場所を上書きする環境変数が設定されています".into()));
    }
    downloads.begin(spec.id)?;
    let hub = (*downloads).clone();
    let events = app.clone();
    let url = spec.url.to_string();
    let file_name = spec.model.default_file().to_string();
    let model_id = spec.id.to_string();
    let dir = match app_models_dir(&app) {
        Ok(dir) => dir,
        Err(err) => {
            downloads.end(spec.id);
            return Err(err);
        }
    };
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let _finish = FinishDownload(hub.clone(), model_id.clone());
        let mut last_emit = None;
        download_to_dir(&url, &dir, &file_name, |received, total| {
            hub.tick(&model_id, received, total);
            let now = Instant::now();
            if should_emit(last_emit, now, received, total) {
                last_emit = Some(now);
                emit_progress(&events, &model_id, received, total);
            }
        })
    })
    .await;
    downloads.end(spec.id);
    match outcome {
        Ok(result) => result,
        Err(err) => Err(format!("ダウンロードに失敗しました: {err}")),
    }
}

/// Fetch `url` and install it as `file_name` under `models_dir`.
///
/// The directory is created when the download is asked for. Redirects are
/// followed (Hugging Face `resolve` URLs redirect to a CDN). The response
/// body is streamed; it is never buffered as a model.
pub fn download_to_dir(
    url: &str,
    models_dir: &Path,
    file_name: &str,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), String> {
    // The folder exists from the moment a download is asked for, including
    // when the server then refuses the file.
    std::fs::create_dir_all(models_dir)
        .map_err(|err| format!("モデル用のフォルダを作成できませんでした: {err}"))?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(concat!("kikimimic/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| format!("HTTPクライアントを用意できませんでした: {err}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|err| format!("ダウンロードを開始できませんでした: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("サーバーが {status} を返しました"));
    }
    let total = response.content_length();
    models::install_model_file(models_dir, file_name, response, total, |received, total| {
        progress(received, total);
    })
    .map_err(|err| format!("ダウンロードに失敗しました: {err:#}"))?;
    Ok(())
}

/// Emit download progress, but not on every chunk. Called from the command
/// so the webview can paint a bar without holding the file in memory.
pub fn emit_progress(app: &AppHandle, id: &str, received: u64, total: Option<u64>) {
    let _ = app.emit(
        DOWNLOAD_EVENT,
        DownloadEvent {
            id: id.to_string(),
            received_bytes: received,
            total_bytes: total,
        },
    );
}

/// Throttle used by the command. The first chunk and the completing chunk
/// always go out; the ones in between wait 200ms so a multi-gigabyte file
/// does not flood the webview.
fn should_emit(last: Option<Instant>, now: Instant, received: u64, total: Option<u64>) -> bool {
    if last.is_none() || total.is_some_and(|expected| received >= expected) {
        return true;
    }
    now.duration_since(last.unwrap_or(now)) >= Duration::from_millis(200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> &'static Spec {
        SPECS.iter().find(|spec| spec.id == id).unwrap()
    }

    #[test]
    fn a_resolved_file_is_ready_and_a_gap_is_missing() {
        let vad = spec("vad");
        let ready = classify(
            vad,
            Some(PathBuf::from("/data/ggml-silero-v5.1.2.bin")),
            None,
            None,
        );
        assert_eq!(ready.state, "ready");
        assert!(!ready.can_download);
        assert_eq!(ready.file_name, Model::Vad.default_file());

        let missing = classify(vad, None, None, None);
        assert_eq!(missing.state, "missing");
        assert!(missing.can_download);
        assert!(missing.message.is_none());
    }

    #[test]
    fn an_env_override_of_a_missing_file_cannot_be_downloaded_over() {
        let asr = spec("asr");
        let status = classify(asr, None, Some("/tmp/missing.bin".into()), None);
        assert_eq!(status.state, "missing");
        assert!(!status.can_download);
        let message = status.message.unwrap();
        assert!(message.contains("KKM_WHISPER_MODEL"), "{message}");
        assert!(message.contains("/tmp/missing.bin"), "{message}");
    }

    #[test]
    fn a_running_download_is_reported_as_downloading() {
        let translator = spec("translator");
        let status = classify(
            translator,
            None,
            None,
            Some(&Active {
                received: 42,
                total: Some(100),
            }),
        );
        assert_eq!(status.state, "downloading");
        assert_eq!(status.bytes, Some(42));
        assert_eq!(status.total_bytes, Some(100));
        assert!(!status.can_download);
    }

    #[test]
    fn progress_events_are_throttled_until_the_end() {
        let start = Instant::now();
        assert!(should_emit(None, start, 1, Some(100)));
        assert!(!should_emit(Some(start), start, 50, Some(100)));
        assert!(should_emit(
            Some(start),
            start + Duration::from_millis(200),
            80,
            Some(100)
        ));
        assert!(should_emit(Some(start), start, 100, Some(100)));
    }

    fn serve(status_line: &str, extra_headers: &str, body: &[u8]) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let status_line = status_line.to_string();
        let extra_headers = extra_headers.to_string();
        let body = body.to_vec();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let mut seen = 0usize;
            loop {
                let n = std::io::Read::read(&mut sock, &mut buf[seen..]).unwrap();
                if n == 0 {
                    break;
                }
                seen += n;
                if buf[..seen].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let header = format!(
                "HTTP/1.1 {status_line}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut sock, header.as_bytes());
            let _ = std::io::Write::write_all(&mut sock, &body);
        });
        format!("http://{addr}/model.bin")
    }

    fn scratch(label: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kkm-download-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_successful_response_is_renamed_into_place() {
        let dir = scratch("http-ok");
        let url = serve("200 OK", "", b"weights");
        let mut last = 0u64;
        download_to_dir(&url, &dir, Model::Vad.default_file(), |received, total| {
            last = received;
            assert_eq!(total, Some(7));
            assert!(!dir.join(Model::Vad.default_file()).exists());
        })
        .unwrap();
        assert_eq!(last, 7);
        assert_eq!(
            std::fs::read(dir.join(Model::Vad.default_file())).unwrap(),
            b"weights"
        );
        assert!(!dir
            .join(format!(
                "{}{}",
                Model::Vad.default_file(),
                models::DOWNLOAD_PARTIAL_SUFFIX
            ))
            .exists());
    }

    #[test]
    fn a_failed_response_does_not_leave_a_ready_file() {
        let dir = scratch("http-404");
        let url = serve("404 Not Found", "", b"nope");
        let err = download_to_dir(&url, &dir, Model::Asr.default_file(), |_, _| {}).unwrap_err();
        assert!(err.contains("404"), "{err}");
        assert!(!dir.join(Model::Asr.default_file()).exists());
        assert!(!dir
            .join(format!(
                "{}{}",
                Model::Asr.default_file(),
                models::DOWNLOAD_PARTIAL_SUFFIX
            ))
            .exists());
    }
}
