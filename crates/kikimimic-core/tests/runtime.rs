//! The threaded runtime on the real clock: only order-insensitive outcomes,
//! since thread timing varies. Behaviour is pinned in tests/meeting.rs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kikimimic_core::capture::{channel, CaptureStream, Format};
use kikimimic_core::domain::{HostNanos, LaneId, LaneRole, LanguagePolicy};
use kikimimic_core::engine::{MtEngine, MtRequest};
use kikimimic_core::event::{Event, Phase, TranslationOutcome};
use kikimimic_core::sim::ScriptedAsr;
use kikimimic_core::{CaptureBackend, CaptureHandle, Config, Kikimimic, SessionMode};

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

#[test]
fn a_face_to_face_session_transcribes_translates_and_goes_idle() {
    let (kkm, events) = Kikimimic::start(Config {
        recordings_dir: std::env::temp_dir().join("kikimimic-runtime-test"),
        capture: Box::new(Tone),
        asr: Box::new(|| {
            Ok(Box::new(ScriptedAsr::new().say(
                LaneId(0),
                100..600,
                "en",
                "hello",
            )))
        }),
        mt: Some(Box::new(|| Ok(Box::new(Upper)))),
        policy: LanguagePolicy::default(),
        glossary: Default::default(),
        session_name: Box::new(|| "unused".into()),
    });
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
