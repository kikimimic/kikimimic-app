//! The translation lane: one thread draining a bounded queue of finished
//! utterances through the llama.cpp backend.
//!
//! It is deliberately one-way-ish. The pipeline submits sentences and later
//! collects results; the worker never touches the UI or the recorder. That
//! keeps [`crate::record::SessionRecorder`] single-owner (translations reach
//! the transcript from the same thread that wrote the utterance) and keeps
//! the decode loop free of translation latency: a sentence that takes four
//! seconds costs nothing but its own place in the queue.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use kikimimic_core::translate::{Job, TranslationQueue};
use kikimimic_core::{Lane, Translator};

/// Sentences that may wait at once. Sized for a burst — both lanes talking
/// over each other, or one long sentence holding the model — not for a
/// backlog: past this the oldest is dropped rather than delaying the newest.
const QUEUE_CAPACITY: usize = 8;

/// What became of one submitted sentence.
pub enum Status {
    Done(String),
    /// The backend failed; the message is for the status line.
    ///
    /// There is no "dropped" outcome: an overflow is decided at submit time
    /// and reported from there, so a dropped sentence never reaches the
    /// worker at all.
    Failed(String),
}

pub struct Outcome {
    pub id: u64,
    pub lane: Lane,
    /// The language it was being translated into, for the transcript.
    pub target: String,
    pub status: Status,
}

struct Queued {
    jobs: TranslationQueue,
    /// A session started: load the model now rather than on the first
    /// sentence, so the load overlaps the first minute of speech instead of
    /// delaying the first translation by it.
    warm: bool,
    stop: bool,
}

struct Shared {
    queued: Mutex<Queued>,
    wake: Condvar,
}

/// Handle to the translation thread. One per app run: the model is expensive
/// to load and is kept across sessions, like the ASR engines.
pub struct TranslateLane {
    shared: Arc<Shared>,
    results: Receiver<Outcome>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl TranslateLane {
    pub fn spawn(model_path: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            queued: Mutex::new(Queued {
                jobs: TranslationQueue::new(QUEUE_CAPACITY),
                warm: false,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let (results_tx, results) = std::sync::mpsc::channel();
        let worker = {
            let shared = shared.clone();
            std::thread::spawn(move || run(&shared, &results_tx, &model_path))
        };
        Self {
            shared,
            results,
            worker: Some(worker),
        }
    }

    /// Start loading the model if it is not loaded yet. Called when a session
    /// starts; a previous load failure is retried, since the user may have
    /// just put the file where it belongs.
    pub fn warm_up(&self) {
        self.shared.queued.lock().unwrap().warm = true;
        self.shared.wake.notify_all();
    }

    /// Queue a sentence. Returns the job dropped to make room, if any — its
    /// line is still on screen waiting for a translation that will never come.
    pub fn submit(&self, job: Job) -> Option<Job> {
        let evicted = self.shared.queued.lock().unwrap().jobs.push(job);
        self.shared.wake.notify_all();
        evicted
    }

    /// Everything finished since the last call. Never blocks.
    pub fn collect(&self) -> Vec<Outcome> {
        self.results.try_iter().collect()
    }

    /// Wait for one more result, for draining at the end of a session. A
    /// timeout and a dead worker are the same answer here: nothing more is
    /// coming, stop waiting.
    pub fn wait(&self, timeout: Duration) -> Option<Outcome> {
        self.results.recv_timeout(timeout).ok()
    }

    /// Abandon whatever is still waiting, for a session that ended before the
    /// backlog cleared. The in-flight sentence cannot be recalled.
    pub fn abandon(&self) -> Vec<Job> {
        self.shared.queued.lock().unwrap().jobs.drain()
    }
}

impl Drop for TranslateLane {
    fn drop(&mut self) {
        {
            let mut queued = self.shared.queued.lock().unwrap();
            queued.stop = true;
        }
        self.shared.wake.notify_all();
        // Joining is what makes the model's own Drop run before the process
        // exits — and, more to the point, keeps the dlopen'd backend alive
        // until the thread that calls into it has stopped.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// What the worker was woken for.
enum Woke {
    Job(Job),
    Warm,
    Stop,
}

fn next(shared: &Shared) -> Woke {
    let mut queued = shared.queued.lock().unwrap();
    loop {
        if queued.stop {
            return Woke::Stop;
        }
        if let Some(job) = queued.jobs.pop() {
            return Woke::Job(job);
        }
        if std::mem::take(&mut queued.warm) {
            return Woke::Warm;
        }
        queued = shared.wake.wait(queued).unwrap();
    }
}

fn run(shared: &Shared, results: &Sender<Outcome>, model_path: &std::path::Path) {
    let mut backend: Option<Box<dyn Translator>> = None;
    let mut load_error: Option<String> = None;
    loop {
        match next(shared) {
            Woke::Stop => return,
            Woke::Warm => {
                if backend.is_none() {
                    // Retry: the previous failure may have been a model file
                    // the user has since put in place.
                    load_error = None;
                    load(&mut backend, &mut load_error, model_path);
                }
            }
            Woke::Job(job) => {
                if backend.is_none() && load_error.is_none() {
                    load(&mut backend, &mut load_error, model_path);
                }
                let status = match backend.as_mut() {
                    None => Status::Failed(
                        load_error
                            .clone()
                            .unwrap_or_else(|| "translation backend unavailable".into()),
                    ),
                    Some(translator) => {
                        // An unconfident detection leaves the source empty;
                        // the backend then asks for a translation into the
                        // target without naming what it came from.
                        let source = job
                            .source
                            .as_deref()
                            .map_or("", kikimimic_core::language::language_name);
                        let target = kikimimic_core::language::language_name(&job.target);
                        match translator.translate(&job.text, source, target) {
                            Ok(text) => Status::Done(text),
                            // One bad sentence must not take the lane down:
                            // the backend catches its own panics, and a
                            // context overflow is a property of that sentence.
                            Err(e) => Status::Failed(format!("{e:#}")),
                        }
                    }
                };
                let outcome = Outcome {
                    id: job.id,
                    lane: job.lane,
                    target: job.target,
                    status,
                };
                if results.send(outcome).is_err() {
                    return; // the pipeline is gone
                }
            }
        }
    }
}

fn load(
    backend: &mut Option<Box<dyn Translator>>,
    load_error: &mut Option<String>,
    model_path: &std::path::Path,
) {
    match kikimimic_mt_llama::GgmlTranslator::load(model_path) {
        Ok(t) => *backend = Some(Box::new(t)),
        Err(e) => {
            let msg = format!("translation unavailable: {e:#}");
            eprintln!("{msg}");
            *load_error = Some(msg);
        }
    }
}
