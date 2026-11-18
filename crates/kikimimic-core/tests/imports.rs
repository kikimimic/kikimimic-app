//! Real runtime/worker integration with tiny fake model files, no network/GPU.
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kikimimic_core::catalog::Catalog;
use kikimimic_core::domain::{LaneId, LaneRole, Lang, Pcm16k, Spoken, StreamMs};
use kikimimic_core::engine::{AsrEngine, AsrStep, EngineRegistry, MtEngine, MtRequest, Recognized};
use kikimimic_core::event::{Event, Phase, PostProcessState};
use kikimimic_core::imports::{AudioDecoder, ImportMode, ImportQueue, ImportState, WavDecoder};
use kikimimic_core::{CaptureBackend, CaptureHandle, Config, Events, Kikimimic, SessionMode};

struct NoCapture;
impl CaptureBackend for NoCapture {
    fn open(
        &mut self,
        _: LaneRole,
    ) -> anyhow::Result<(
        kikimimic_core::capture::CaptureStream,
        Box<dyn CaptureHandle>,
    )> {
        anyhow::bail!("capture must not open during imports")
    }
}
struct Speaker(String);
impl AsrEngine for Speaker {
    fn cadence(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn open_lane(&mut self, _: LaneId) {}
    fn push(&mut self, _: LaneId, _: &Pcm16k) {}
    fn step(&mut self, _: LaneId, _: &Spoken) -> anyhow::Result<Option<AsrStep>> {
        Ok(None)
    }
    fn finish(&mut self, _: LaneId, _: &Spoken) -> anyhow::Result<Vec<AsrStep>> {
        Ok(Vec::new())
    }
    fn transcribe_clip(&mut self, _: &Pcm16k, _: &Spoken) -> anyhow::Result<Vec<Recognized>> {
        Ok(vec![Recognized {
            text: self.0.clone(),
            start: StreamMs(125),
            end: StreamMs(600),
            lang: Some(Lang::parse("en").unwrap()),
        }])
    }
}
struct Translator(String);
impl MtEngine for Translator {
    fn translate(&mut self, req: &MtRequest<'_>) -> anyhow::Result<String> {
        Ok(format!("{}:{}", self.0, req.text))
    }
}
struct Gate {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl AudioDecoder for Gate {
    fn decode(&mut self, source: &std::path::Path, _: &std::path::Path) -> anyhow::Result<PathBuf> {
        self.entered.send(())?;
        self.release.recv_timeout(Duration::from_secs(10))?;
        Ok(source.to_path_buf())
    }
}
struct Data(PathBuf);
impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn start(label: &str, install: bool) -> (Data, Kikimimic, Events, PathBuf) {
    let data = Data(
        std::env::temp_dir().join(format!("kkm-runtime-import-{label}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&data.0);
    std::fs::create_dir_all(&data.0).unwrap();
    let mut text = "[defaults]\nasr = 'asr-a'\nmt = 'mt-a'\n".to_string();
    for (id, kind) in [
        ("asr-a", "asr"),
        ("asr-b", "asr"),
        ("mt-a", "mt"),
        ("mt-b", "mt"),
    ] {
        text.push_str(&format!("[[model]]\nid='{id}'\nkind='{kind}'\nengine='fake'\ntitle='{id}'\nlicense={{name='MIT'}}\nfiles=[{{name='{id}.bin',url='unused',sha256='2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824',bytes=5}}]\n"));
        if install {
            std::fs::create_dir_all(data.0.join("models")).unwrap();
            std::fs::write(data.0.join(format!("models/{id}.bin")), b"hello").unwrap();
        }
    }
    let catalog = Box::leak(Box::new(Catalog::parse(&text).unwrap()));
    let mut engines = EngineRegistry::default();
    engines.asr(
        "fake",
        |_| Ok(()),
        |req| Ok(Box::new(Speaker(req.entry.id.clone()))),
    );
    engines.mt(
        "fake",
        |_| Ok(()),
        |req| Ok(Box::new(Translator(req.entry.id.clone()))),
    );
    let wav = data.0.join("source.wav");
    let mut w = hound::WavWriter::create(
        &wav,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..48_000 {
        w.write_sample(4_000i16).unwrap();
    }
    w.finalize().unwrap();
    let (kkm, events) = Kikimimic::start(Config {
        data_dir: data.0.clone(),
        extra_model_dirs: Vec::new(),
        catalog,
        engines,
        recordings_dir: data.0.join("recordings"),
        capture: Box::new(NoCapture),
        session_name: Box::new(|| "20261117120000".into()),
        diarizer: Some(Box::new(NeverDiarize)),
    });
    (data, kkm, events, wav)
}
struct NeverDiarize;
impl kikimimic_core::engine::Diarizer for NeverDiarize {
    fn diarize(&mut self, _: &std::path::Path, _: &std::path::Path) -> anyhow::Result<()> {
        panic!("diarizer must not run during import")
    }
}
fn wait(events: &Events, mut predicate: impl FnMut(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for import event"
        );
        if let Some(event) = events.recv_timeout(Duration::from_millis(100)) {
            if predicate(&event) {
                return event;
            }
        }
    }
}
fn complete(event: &Event, count: usize) -> bool {
    matches!(event, Event::Imports(q) if !q.running && q.jobs.len() == count && q.jobs.iter().all(|j| matches!(j.state, ImportState::Done | ImportState::Failed { .. } | ImportState::Skipped)))
}
fn queue(event: Event) -> ImportQueue {
    match event {
        Event::Imports(q) => q,
        _ => panic!("expected import queue"),
    }
}

#[test]
fn imports_use_the_existing_workers_with_snapshots_and_exclude_live_and_diarization() {
    let (_data, kkm, events, wav) = start("snapshot", true);
    let (entered, entrance) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    kkm.import_audio(
        vec![wav.clone()],
        ImportMode::Separate,
        Box::new(Gate {
            entered,
            release: gate,
        }),
    );
    entrance.recv_timeout(Duration::from_secs(5)).unwrap();
    kkm.start_session(SessionMode::Meeting);
    let event = wait(
        &events,
        |e| matches!(e, Event::Status(s) if matches!(s.phase, Phase::Error { .. })),
    );
    assert!(
        matches!(event, Event::Status(s) if matches!(&s.phase, Phase::Error { message } if message.contains("recording operation")))
    );
    kkm.diarize(wav.parent().unwrap().join("recordings"));
    wait(&events, |e| {
        matches!(
            e,
            Event::PostProcess {
                state: PostProcessState::Failed { .. },
                ..
            }
        )
    });
    let mut settings = kkm.settings();
    settings.asr_model = "asr-b".into();
    settings.mt_model = Some("mt-b".into());
    kkm.update_settings(settings).unwrap();
    // The queued second job uses the new settings. Its decoder is not needed:
    // both inputs are WAV; the running decoder handles the additional work.
    kkm.import_audio(vec![wav], ImportMode::Separate, Box::new(WavDecoder));
    release.send(()).unwrap();
    // The worker's decoder is reused, so release its second file too.
    entrance.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    let q = queue(wait(&events, |e| complete(e, 2)));
    assert_eq!(q.jobs[0].state, ImportState::Done);
    assert_eq!(q.jobs[1].state, ImportState::Done);
    for (i, suffix) in ["a", "b"].iter().enumerate() {
        let session = kikimimic_core::library::read(q.jobs[i].output.as_ref().unwrap()).unwrap();
        assert_eq!(session.lines[0].text, format!("asr-{suffix}"));
        assert_eq!(
            session.lines[0].translation,
            Some(format!("mt-{suffix}:asr-{suffix}"))
        );
        assert_eq!(
            (session.lines[0].start_ms, session.lines[0].end_ms),
            (125, 600)
        );
    }
}

#[test]
fn missing_installed_model_is_a_failed_import_without_a_published_recording() {
    let (data, kkm, events, wav) = start("missing", false);
    kkm.import_audio(vec![wav], ImportMode::Separate, Box::new(WavDecoder));
    let q = queue(wait(&events, |e| complete(e, 1)));
    assert!(
        matches!(&q.jobs[0].state, ImportState::Failed { message } if message.contains("asr-a"))
    );
    assert!(kikimimic_core::library::list(&data.0.join("recordings")).is_empty());
}
