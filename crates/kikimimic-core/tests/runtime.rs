//! The threaded runtime on the real clock: only order-insensitive outcomes,
//! since thread timing varies. Behaviour is pinned in tests/meeting.rs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kikimimic_core::capture::{channel, CaptureStream, Format};
use kikimimic_core::catalog::Catalog;
use kikimimic_core::domain::{HostNanos, LaneId, LaneRole};
use kikimimic_core::engine::{AsrEngine, EngineRegistry, LoadRequest};
use kikimimic_core::engine::{MtEngine, MtRequest};
use kikimimic_core::event::{Event, Phase, TranslationOutcome};
use kikimimic_core::sim::ScriptedAsr;
use kikimimic_core::{CaptureBackend, CaptureHandle, Config, Events, Kikimimic, SessionMode};

/// Delivers a steady tone in 10 ms callbacks until its handle is dropped.
struct Tone;
struct Running(Arc<AtomicBool>);
impl CaptureHandle for Running {}
impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl CaptureBackend for Tone {
    fn open(&mut self, _role: LaneRole) -> anyhow::Result<(CaptureStream, Box<dyn CaptureHandle>)> {
        let (mut w, s) = channel(
            Format {
                rate: 48_000,
                channels: 1,
            },
            Duration::from_secs(8),
        );
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let epoch = Instant::now();
        std::thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                let at = HostNanos(1_000_000_000 + epoch.elapsed().as_nanos() as u64);
                w.push(&[0.1; 480], at);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Ok((s, Box::new(Running(running))))
    }
}

struct Upper;
impl MtEngine for Upper {
    fn translate(&mut self, req: &MtRequest<'_>) -> anyhow::Result<String> {
        Ok(req.text.to_uppercase())
    }
}

/// sha256("hello"): every fake model file holds those five bytes.
const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

fn catalog() -> &'static Catalog {
    let model = |id: &str, kind: &str, engine: &str| {
        format!(
            r#"[[model]]
            id = "{id}"
            kind = "{kind}"
            engine = "{engine}"
            title = "{id}"
            license = {{ name = "MIT" }}
            files = [{{ name = "{id}.bin", url = "u", sha256 = "{HELLO}", bytes = 5 }}]
            "#
        )
    };
    let text = format!(
        "[defaults]\nasr = \"asr\"\nmt = \"mt\"\n{}{}",
        model("asr", "asr", "scripted"),
        model("mt", "mt", "upper")
    );
    Box::leak(Box::new(Catalog::parse(&text).unwrap()))
}

fn engines() -> EngineRegistry {
    let mut r = EngineRegistry::default();
    r.asr(
        "scripted",
        |_| Ok(()),
        |_: &LoadRequest<'_>| {
            Ok(
                Box::new(ScriptedAsr::new().say(LaneId(0), 100..600, "en", "hello"))
                    as Box<dyn AsrEngine>,
            )
        },
    );
    r.mt(
        "upper",
        |_| Ok(()),
        |_: &LoadRequest<'_>| Ok(Box::new(Upper) as Box<dyn MtEngine>),
    );
    r
}

fn start(label: &str, install: bool) -> (Kikimimic, Events) {
    let data =
        std::env::temp_dir().join(format!("kikimimic-runtime-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    if install {
        std::fs::create_dir_all(data.join("models")).unwrap();
        for id in ["asr", "mt"] {
            std::fs::write(data.join("models").join(format!("{id}.bin")), b"hello").unwrap();
        }
    }
    Kikimimic::start(Config {
        data_dir: data.clone(),
        extra_model_dirs: Vec::new(),
        catalog: catalog(),
        engines: engines(),
        recordings_dir: data.join("recordings"),
        capture: Box::new(Tone),
        session_name: Box::new(|| "unused".into()),
    })
}

#[test]
fn a_face_to_face_session_transcribes_translates_and_goes_idle() {
    let (kkm, events) = start("ok", true);
    kkm.start_session(SessionMode::FaceToFace);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut translated = None;
    let mut stopped = false;
    let mut idle = false;
    while Instant::now() < deadline {
        let Some(event) = events.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        match event {
            Event::Translation {
                outcome: TranslationOutcome::Text { text, .. },
                ..
            } => {
                translated = Some(text);
                kkm.stop_session();
                stopped = true;
            }
            Event::Status(s) if stopped && s.phase == Phase::Idle => {
                idle = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(translated.as_deref(), Some("HELLO"));
    assert!(idle, "the session went idle after Stop");
}

#[test]
fn a_session_without_its_model_is_refused_with_a_reason() {
    let (kkm, events) = start("missing", false);
    kkm.start_session(SessionMode::FaceToFace);
    let event = events
        .recv_timeout(Duration::from_secs(10))
        .expect("an answer");
    let Event::Status(status) = event else {
        panic!("{event:?}")
    };
    assert!(
        matches!(&status.phase, Phase::Error { message } if message.contains("not downloaded")),
        "{status:?}"
    );
}
