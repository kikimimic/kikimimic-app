//! The only import/runtime bridge: reuse the ASR and MT model-owner threads.
use super::*;
use crate::imports::{AudioDecoder, ImportEngines, ImportMode, ImportQueue};

pub(super) enum ImportCommand {
    Add {
        inputs: Vec<PathBuf>,
        mode: ImportMode,
        settings: Settings,
    },
    Resume {
        retry_failed: bool,
    },
}

impl Kikimimic {
    /// Queue selected files with the current inference settings, then run when
    /// idle. Native decoding belongs to the shell. Results use `Event::Imports`.
    pub fn import_audio(
        &self,
        inputs: Vec<PathBuf>,
        mode: ImportMode,
        decoder: Box<dyn AudioDecoder>,
    ) {
        let _ = self.tx.send(Msg::Import(
            ImportCommand::Add {
                inputs,
                mode,
                settings: self.settings(),
            },
            decoder,
        ));
    }

    /// Resume pending files; optionally retry failed jobs with their original
    /// settings. An interrupted file starts from its beginning.
    pub fn resume_imports(&self, retry_failed: bool, decoder: Box<dyn AudioDecoder>) {
        let _ = self
            .tx
            .send(Msg::Import(ImportCommand::Resume { retry_failed }, decoder));
    }

    /// Finish the current job, then stop. A joined group is one job.
    pub fn stop_imports(&self) {
        self.imports
            .lock()
            .unwrap()
            .stop
            .store(true, Ordering::SeqCst);
    }

    pub fn import_queue(&self) -> ImportQueue {
        self.imports.lock().unwrap().view.clone()
    }
}

impl Shell {
    pub(super) fn import(&mut self, command: ImportCommand, mut decoder: Box<dyn AudioDecoder>) {
        let mut q = self.imports.lock().unwrap();
        let result = match command {
            ImportCommand::Add {
                inputs,
                mode,
                settings,
            } => q.add(inputs, mode, settings, (self.session_name)()),
            ImportCommand::Resume { retry_failed: true } => q.retry_failed(),
            ImportCommand::Resume {
                retry_failed: false,
            } => Ok(()),
        };
        let result = result.and_then(|()| {
            anyhow::ensure!(
                q.readable,
                "cannot read import queue; preserve and repair imports.json"
            );
            if self.import_worker_running {
                return Ok(());
            }
            anyhow::ensure!(
                self.conductor.is_idle(),
                "stop the live session before resuming imports"
            );
            anyhow::ensure!(
                !self.postprocessing.swap(true, Ordering::SeqCst),
                "another recording operation is running; resume imports when it finishes"
            );
            q.stop.store(false, Ordering::SeqCst);
            q.view.running = true;
            Ok(())
        });
        if let Err(e) = result {
            q.view.error = Some(format!("{e:#}"));
            let _ = self.events.send(Event::Imports(q.view.clone()));
            return;
        }
        // Adding more work while running needs no second worker.
        if self.import_worker_running {
            let _ = self.events.send(Event::Imports(q.view.clone()));
            return;
        }
        q.view.error = None;
        let _ = self.events.send(Event::Imports(q.view.clone()));
        drop(q);
        self.import_worker_running = true;
        let (queue, base, events, busy, tx) = (
            Arc::clone(&self.imports),
            self.recordings_dir.clone(),
            self.events.clone(),
            Arc::clone(&self.postprocessing),
            self.tx.clone(),
        );
        let mut engines = WorkerEngines {
            asr: self.asr.clone(),
            mt: self.mt.clone(),
        };
        // Completion goes through the conductor, so releasing the shared busy
        // flag and starting the next operation are ordered with incoming work.
        let spawned = thread::Builder::new()
            .name("kikimimic-import".into())
            .spawn(move || {
                crate::imports::run(&queue, &base, decoder.as_mut(), &mut engines, |view| {
                    if view.running {
                        let _ = events.send(Event::Imports(view));
                    }
                });
                let _ = tx.send(Msg::ImportFinished(decoder));
            });
        if let Err(e) = spawned {
            self.import_worker_running = false;
            busy.store(false, Ordering::SeqCst);
            let mut q = self.imports.lock().unwrap();
            q.view.running = false;
            q.view.error = Some(format!("cannot start import worker: {e}"));
            let _ = self.events.send(Event::Imports(q.view.clone()));
        }
    }
}

struct WorkerEngines {
    asr: Sender<AsrWork>,
    mt: Sender<MtWork>,
}
impl ImportEngines for WorkerEngines {
    fn prepare(&mut self, settings: &Settings) -> anyhow::Result<()> {
        let (reply, answer) = mpsc::channel();
        self.asr
            .send(AsrWork::ImportEnsure(settings.clone(), reply))?;
        answer.recv()?.map_err(anyhow::Error::msg)
    }
    fn recognize(&mut self, audio: Pcm16k, settings: &Settings) -> anyhow::Result<Vec<Recognized>> {
        let (reply, answer) = mpsc::channel();
        self.asr
            .send(AsrWork::ImportClip(audio, settings.clone(), reply))?;
        answer.recv()?.map_err(anyhow::Error::msg)
    }
    fn translate(
        &mut self,
        text: &str,
        source: Option<&Lang>,
        target: &Lang,
        settings: &Settings,
    ) -> anyhow::Result<String> {
        let job = MtJob {
            id: crate::domain::UtteranceId(0),
            lane: crate::domain::LaneLabel::new(LaneRole::Mic, 1),
            text: text.into(),
            source: source.cloned(),
            target: target.clone(),
            context: Vec::new(),
            terms: settings.glossary.terms(),
        };
        let (reply, answer) = mpsc::channel();
        self.mt.send(MtWork::Import {
            job,
            settings: settings.clone(),
            reply,
        })?;
        answer.recv()?.map_err(anyhow::Error::msg)
    }
}

pub(super) fn ensure_import<'a>(
    models: &Models,
    loaded: &'a mut Option<Loaded>,
    settings: &Settings,
) -> Result<&'a mut Box<dyn AsrEngine>, String> {
    let wanted = &settings.asr_model;
    if loaded.as_ref().is_none_or(|l| l.id != *wanted) {
        *loaded = None;
        *loaded = Some(Loaded {
            id: wanted.clone(),
            engine: models.load_asr(wanted)?,
            keywords: Vec::new(),
        });
    }
    let current = loaded.as_mut().expect("loaded above");
    current.listen_for(settings.glossary.keywords());
    Ok(&mut current.engine)
}
