//! The threaded runtime on the real clock: only order-insensitive outcomes,
//! since thread timing varies. Behaviour is pinned in tests/meeting.rs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kikimimic_core::capture::{channel, CaptureStream, Format, SourceDecision, SourceEvent};
use kikimimic_core::catalog::Catalog;
use kikimimic_core::domain::{HostNanos, LaneId, LaneRole};
use kikimimic_core::engine::{AsrEngine, Diarizer, EngineRegistry, LoadRequest};
use kikimimic_core::engine::{MtEngine, MtRequest};
use kikimimic_core::event::{Event, Phase, PostProcessState, TranslationOutcome};
use kikimimic_core::sim::ScriptedAsr;
use kikimimic_core::{CaptureBackend, CaptureHandle, Config, Events, Kikimimic, SessionMode};

/// Delivers a steady tone in 10 ms callbacks until its handle is dropped.
struct Tone;
struct Running(
    Arc<AtomicBool>,
    Vec<SourceEvent>,
    Arc<std::sync::Mutex<Vec<SourceDecision>>>,
);
impl CaptureHandle for Running {
    fn poll_event(&mut self) -> Option<SourceEvent> {
        self.1.pop()
    }
    fn decide(&mut self, decision: SourceDecision) {
        self.2.lock().unwrap().push(decision);
    }
}

static DECISIONS: std::sync::OnceLock<Arc<std::sync::Mutex<Vec<SourceDecision>>>> =
    std::sync::OnceLock::new();
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
        let proposal = SourceEvent::OutputSwitchProposed {
            capturing: "Speakers".into(),
            detected: "AirPods".into(),
            uid: "airpods".into(),
        };
        let decisions = Arc::clone(DECISIONS.get_or_init(Default::default));
        Ok((s, Box::new(Running(running, vec![proposal], decisions))))
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

/// Says two speakers took turns in the first two seconds.
struct TwoSpeakers;
impl Diarizer for TwoSpeakers {
    fn diarize(&mut self, _mix: &std::path::Path, rttm: &std::path::Path) -> anyhow::Result<()> {
        std::fs::write(
            rttm,
            "SPEAKER mix 1 0.000 1.000 <NA> <NA> speaker_0 <NA> <NA>\n\
             SPEAKER mix 1 1.000 1.000 <NA> <NA> speaker_1 <NA> <NA>\n",
        )?;
        Ok(())
    }
}

fn start(label: &str, install: bool) -> (Kikimimic, Events) {
    start_with(label, install, None)
}

fn start_with(
    label: &str,
    install: bool,
    diarizer: Option<Box<dyn Diarizer>>,
) -> (Kikimimic, Events) {
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
        diarizer,
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

#[test]
fn a_source_question_reaches_the_ui_and_its_answer_reaches_the_source() {
    let (kkm, events) = start("source", true);
    kkm.start_session(SessionMode::FaceToFace);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut asked = None;
    while Instant::now() < deadline && asked.is_none() {
        if let Some(Event::Source(SourceEvent::OutputSwitchProposed { uid, .. })) =
            events.recv_timeout(Duration::from_millis(100))
        {
            asked = Some(uid);
        }
    }
    let uid = asked.expect("the proposal was forwarded");
    kkm.decide_source(SourceDecision::Follow { uid: uid.clone() });
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if DECISIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .contains(&SourceDecision::Follow { uid: uid.clone() })
        {
            kkm.stop_session();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the decision never reached the source");
}

/// A finished recording on disk: two seconds of silence in each wav and one
/// line that both speakers share.
fn recording(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    for name in ["mic", "mix"] {
        let mut w = hound::WavWriter::create(dir.join(format!("{name}.wav")), spec).unwrap();
        for _ in 0..96_000 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
    }
    std::fs::write(
        dir.join("transcript.jsonl"),
        r#"{"type":"utterance","id":1,"lane":"mic","startMs":0,"endMs":2000,"lang":"en","text":"hello"}"#,
    )
    .unwrap();
}

fn post_process_outcome(events: &Events) -> PostProcessState {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(Event::PostProcess { state, .. }) =
            events.recv_timeout(Duration::from_millis(100))
        {
            if state != PostProcessState::Running {
                return state;
            }
        }
    }
    panic!("post-processing never finished");
}

#[test]
fn a_finished_recording_gets_speaker_labels() {
    let (kkm, events) = start_with("diarize", true, Some(Box::new(TwoSpeakers)));
    let dir = std::env::temp_dir().join(format!("kikimimic-diarize-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    recording(&dir);
    kkm.diarize(dir.clone());
    assert_eq!(post_process_outcome(&events), PostProcessState::Done);
    assert!(dir.join("postprocess/transcript.jsonl").exists());
    assert!(dir.join("postprocess/nemotron.rttm").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_a_diarizer_labelling_is_refused_with_a_reason() {
    let (kkm, events) = start("no-diarizer", true);
    kkm.diarize(std::env::temp_dir());
    assert!(matches!(
        post_process_outcome(&events),
        PostProcessState::Failed { .. }
    ));
}
