//! Durable, sequential external-audio imports. OS decoding is supplied by the
//! shell; the core publishes only complete, ordinary recording directories.

mod audio;
mod process;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domain::{Lang, Pcm16k};
use crate::engine::Recognized;
use crate::settings::Settings;

/// Decode compressed audio into a temporary WAV. Must never modify `source`.
/// Called on the import worker, never the UI or conductor thread.
pub trait AudioDecoder: Send {
    fn decode(&mut self, source: &Path, temporary: &Path) -> Result<PathBuf>;
}

/// Portable WAV-only decoder; macOS supplies AVFoundation for M4A/MP3.
pub struct WavDecoder;
impl AudioDecoder for WavDecoder {
    fn decode(&mut self, source: &Path, _temporary: &Path) -> Result<PathBuf> {
        anyhow::ensure!(is_wav(source), "this decoder supports WAV only");
        Ok(source.to_path_buf())
    }
}

fn is_wav(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportMode {
    /// One recording for every input, in selected order.
    Separate,
    /// One recording, concatenating matching WAVs in selected order.
    JoinWav,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportState {
    Pending,
    Running,
    Done,
    Failed { message: String },
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportJob {
    pub id: u64,
    pub inputs: Vec<PathBuf>,
    pub mode: ImportMode,
    pub settings: Settings,
    pub state: ImportState,
    pub output: Option<PathBuf>,
    /// Local wall clock supplied by the shell, preserved across retries.
    stamp: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportQueue {
    pub jobs: Vec<ImportJob>,
    /// Transient worker state, deliberately reset after restart.
    #[serde(skip)]
    pub running: bool,
    pub error: Option<String>,
}

/// Shared durable queue. Mutations are serialized; audio/model work happens
/// outside the lock. A corrupt queue is surfaced, never silently overwritten.
pub(crate) struct Queue {
    pub view: ImportQueue,
    path: PathBuf,
    pub stop: Arc<AtomicBool>,
    pub readable: bool,
}

impl Queue {
    pub fn load(path: PathBuf) -> Self {
        let result = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<ImportQueue>(&bytes).map_err(Into::into),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ImportQueue::default()),
            Err(e) => Err(anyhow::Error::from(e)),
        };
        let readable = result.is_ok();
        let mut view = result.unwrap_or_else(|e| ImportQueue {
            error: Some(format!("cannot read import queue: {e:#}")),
            ..ImportQueue::default()
        });
        for job in &mut view.jobs {
            if job.state == ImportState::Running {
                job.state = ImportState::Pending;
            }
            if matches!(job.state, ImportState::Done | ImportState::Skipped)
                && !job.output.as_ref().is_some_and(|dir| {
                    ["mic.wav", "mix.wav", "transcript.jsonl", "import.json"]
                        .iter()
                        .all(|name| dir.join(name).is_file())
                })
            {
                job.state = ImportState::Failed {
                    message: "completed import files are missing; restore them or remove the incomplete recording folder before retrying".into(),
                };
            }
        }
        Self {
            view,
            path,
            stop: Arc::new(AtomicBool::new(false)),
            readable,
        }
    }

    pub fn save(&self) -> Result<()> {
        atomic_json(&self.path, &self.view)
    }

    pub fn add(
        &mut self,
        inputs: Vec<PathBuf>,
        mode: ImportMode,
        mut settings: Settings,
        stamp: String,
    ) -> Result<()> {
        anyhow::ensure!(
            self.readable,
            "resolve the import queue error before adding files"
        );
        anyhow::ensure!(!inputs.is_empty(), "select at least one audio file");
        anyhow::ensure!(
            stamp.len() == 14 && stamp.bytes().all(|c| c.is_ascii_digit()),
            "invalid import date"
        );
        for path in &inputs {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            anyhow::ensure!(
                matches!(ext.as_str(), "wav" | "m4a" | "mp3"),
                "select WAV, M4A or MP3: {}",
                path.display()
            );
            anyhow::ensure!(path.is_absolute(), "import paths must be absolute");
        }
        settings.clear_on_start = false; // Not an inference setting.
        let groups = if mode == ImportMode::JoinWav {
            vec![inputs]
        } else {
            inputs.into_iter().map(|p| vec![p]).collect()
        };
        let old = self.view.clone();
        let mut id = self.view.jobs.iter().map(|j| j.id).max().unwrap_or(0);
        for inputs in groups {
            id = id.checked_add(1).context("too many import jobs")?;
            self.view.jobs.push(ImportJob {
                id,
                inputs,
                mode,
                settings: settings.clone(),
                state: ImportState::Pending,
                output: None,
                stamp: stamp.clone(),
            });
        }
        if let Err(e) = self.save() {
            self.view = old;
            return Err(e);
        }
        Ok(())
    }

    pub fn retry_failed(&mut self) -> Result<()> {
        anyhow::ensure!(self.readable, "cannot read import queue");
        let old = self.view.clone();
        for job in &mut self.view.jobs {
            if matches!(job.state, ImportState::Failed { .. }) {
                job.state = ImportState::Pending;
            }
        }
        if let Err(e) = self.save() {
            self.view = old;
            return Err(e);
        }
        Ok(())
    }
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("missing parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Worker adapter to the runtime's existing model-owner threads. The snapshot
/// accompanies every request, so settings edits cannot alter an in-flight job.
pub(crate) trait ImportEngines {
    fn prepare(&mut self, settings: &Settings) -> Result<()>;
    fn recognize(&mut self, audio: Pcm16k, settings: &Settings) -> Result<Vec<Recognized>>;
    fn translate(
        &mut self,
        text: &str,
        source: Option<&Lang>,
        target: &Lang,
        settings: &Settings,
    ) -> Result<String>;
}

pub(crate) fn run(
    queue: &Arc<Mutex<Queue>>,
    base: &Path,
    decoder: &mut dyn AudioDecoder,
    engines: &mut dyn ImportEngines,
    mut changed: impl FnMut(ImportQueue),
) {
    loop {
        let job = {
            let mut q = queue.lock().unwrap();
            if q.stop.load(Ordering::SeqCst) {
                break;
            }
            let Some(job) = q
                .view
                .jobs
                .iter_mut()
                .find(|j| j.state == ImportState::Pending)
            else {
                break;
            };
            job.state = ImportState::Running;
            let job = job.clone();
            if let Err(e) = q.save() {
                q.view.error = Some(format!("cannot save import queue: {e:#}"));
                break;
            }
            changed(q.view.clone());
            job
        };
        let result = process::import(base, &job, decoder, engines);
        let mut q = queue.lock().unwrap();
        let row = q
            .view
            .jobs
            .iter_mut()
            .find(|j| j.id == job.id)
            .expect("queue retains jobs");
        match result {
            Ok((path, skipped)) => {
                row.output = Some(path);
                row.state = if skipped {
                    ImportState::Skipped
                } else {
                    ImportState::Done
                };
            }
            Err(e) => {
                row.state = ImportState::Failed {
                    message: format!("{e:#}"),
                }
            }
        }
        if let Err(e) = q.save() {
            q.view.error = Some(format!("cannot save import queue: {e:#}"));
            break;
        }
        changed(q.view.clone());
    }
    let mut q = queue.lock().unwrap();
    q.view.running = false;
    changed(q.view.clone());
}

#[cfg(test)]
mod tests;
