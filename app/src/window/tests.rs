//! The whole window, rendered headless: the timeline, opening a recording,
//! and writing it to an export destination.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::TestAppContext;
use kikimimic_core::catalog::Catalog;
use kikimimic_core::engine::EngineRegistry;
use kikimimic_core::{CaptureBackend, Config};

use super::*;

struct NoCapture;
impl CaptureBackend for NoCapture {
    fn open(
        &mut self,
        _role: kikimimic_core::domain::LaneRole,
    ) -> anyhow::Result<(
        kikimimic_core::capture::CaptureStream,
        Box<dyn kikimimic_core::CaptureHandle>,
    )> {
        anyhow::bail!("no capture in tests")
    }
}

/// A data dir with one finished recording, and a home for destinations.
/// The window finds recordings through a process-wide environment variable,
/// so window tests hold a lock while they run.
struct Fixture(
    std::path::PathBuf,
    #[allow(dead_code)] std::sync::MutexGuard<'static, ()>,
);

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Fixture {
    fn new() -> Self {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "kkm-window-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let rec = root.join("recordings").join("20261117100000");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(rec.join("mic.wav"), []).unwrap();
        std::fs::write(
            rec.join("transcript.jsonl"),
            concat!(
                "{\"type\":\"utterance\",\"id\":1,\"lane\":\"mic\",\"startMs\":100,\"endMs\":900,\"text\":\"定例を始めます\"}\n",
                "{\"type\":\"utterance\",\"id\":2,\"lane\":\"speaker\",\"startMs\":1000,\"endMs\":1800,\"text\":\"よろしくお願いします\"}\n",
            ),
        )
        .unwrap();
        Self(root, guard)
    }

    fn start(&self) -> Kikimimic {
        // The window reads the library from here (paths::recordings_dir).
        unsafe { std::env::set_var("KKM_RECORD_DIR", self.0.join("recordings")) };
        let (kkm, _events) = Kikimimic::start(Config {
            data_dir: self.0.join("data"),
            extra_model_dirs: Vec::new(),
            catalog: Catalog::builtin(),
            engines: EngineRegistry::default(),
            recordings_dir: self.0.join("recordings"),
            capture: Box::new(NoCapture),
            session_name: Box::new(|| "unused".into()),
            diarizer: None,
        });
        let mut settings = kkm.settings();
        settings.destinations[0].folder = self.0.join("notes").display().to_string();
        kkm.update_settings(settings).unwrap();
        kkm
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    cx.simulate_click(bounds.center(), Default::default());
    cx.run_until_parked();
}

#[gpui::test]
fn a_meeting_opens_from_the_timeline_and_writes_to_its_destination(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let (_window, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::default());
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state, calendar, cx)
    });
    cx.run_until_parked();

    click(cx, "timeline-20261117100000");
    assert!(
        cx.debug_bounds("detail-player").is_some(),
        "the player sits on top of the detail"
    );
    click(cx, "tab-export");
    click(cx, "write-dest");

    let note = fixture.0.join("notes").join("2026-11-17 20261117100000.md");
    let text = std::fs::read_to_string(&note).expect("the default destination was written");
    assert!(text.contains("**自分**: 定例を始めます"));
    assert!(text.contains("**相手**: よろしくお願いします"));
}

#[gpui::test]
fn a_finished_recording_is_written_to_destinations_waiting_for_it(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let mut settings = kkm.settings();
    settings.destinations[0].when = kikimimic_core::destinations::When::AfterRecording;
    kkm.update_settings(settings).unwrap();
    let state = cx.new(|_| AppState::default());
    let (_window, cx) = cx.add_window_view(|_, cx| {
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state.clone(), calendar, cx)
    });
    cx.run_until_parked();
    let note = fixture.0.join("notes").join("2026-11-17 20261117100000.md");
    assert!(!note.exists(), "nothing is written before a recording ends");

    let dir = fixture.0.join("recordings").join("20261117100000");
    for phase in [Phase::Listening, Phase::Stopping, Phase::Idle] {
        let dir = dir.clone();
        state.update(cx, |s, cx| {
            s.phase = phase;
            s.recording_dir = Some(dir);
            cx.notify();
        });
        cx.run_until_parked();
    }
    assert!(
        std::fs::read_to_string(&note)
            .unwrap()
            .contains("定例を始めます")
    );
}

/// Stands in for `claude -p`: answers by quoting line 1, or drafts minutes.
fn fake_agent(_: kikimimic_core::agent::Agent, prompt: &str) -> anyhow::Result<String> {
    Ok(if prompt.contains("議事録") {
        r#"{"items":[{"kind":"Summary","text":"定例を開始","evidence":[1],"owner":null,"deadline":null,"supersedes":null}]}"#.into()
    } else {
        r#"{"answer":"定例を始めると言いました","citations":[{"id":1,"quote":"定例を始めます"}]}"#
            .into()
    })
}

#[gpui::test]
fn a_question_goes_to_the_agent_only_after_consent_and_comes_back_quoted(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let mut settings = kkm.settings();
    settings.agent = Some(kikimimic_core::agent::Agent::ClaudeCode);
    settings.destinations[0].when = kikimimic_core::destinations::When::AfterMinutes;
    kkm.update_settings(settings).unwrap();
    let (window, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::default());
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        let mut w = MainWindow::new(kkm.clone(), state, calendar, cx);
        w.run_agent = fake_agent;
        w
    });
    cx.run_until_parked();
    click(cx, "timeline-20261117100000");
    click(cx, "tab-ask");
    window.update(cx, |w, cx| {
        w.ask
            .input
            .update(cx, |i, cx| i.set_text("何を始めた？", cx))
    });
    click(cx, "ask-send");
    window.read_with(cx, |w, _| {
        assert!(w.ask.entries.is_empty(), "nothing is sent before consent")
    });
    click(cx, "consent-send");
    window.read_with(cx, |w, _| {
        let reply = w.ask.entries[0].result.as_ref().unwrap().as_ref().unwrap();
        assert!(reply.confirmed);
        assert_eq!(reply.quotes, vec![(100, "定例を始めます".to_string())]);
    });

    // Minutes from the same agent (already consented) are saved, then the
    // destination waiting for minutes is written.
    click(cx, "tab-minutes");
    click(cx, "make-minutes");
    let dir = fixture.0.join("recordings").join("20261117100000");
    assert_eq!(
        kikimimic_core::minutes::load(&dir).unwrap().unwrap().items[0].text,
        "定例を開始"
    );
    let note = fixture.0.join("notes").join("2026-11-17 20261117100000.md");
    assert!(
        std::fs::read_to_string(note)
            .unwrap()
            .contains("- 定例を開始")
    );
}

#[gpui::test]
fn the_advisor_answers_only_when_asked_and_stays_quiet_once_dismissed_or_stopped(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let state = cx.new(|_| AppState::default());
    let (window, cx) = cx.add_window_view(|_, cx| {
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state.clone(), calendar, cx)
    });
    let said = |id: u64, text: &str| crate::state::Line {
        id: kikimimic_core::domain::UtteranceId(id),
        lane: kikimimic_core::domain::LaneLabel::new(kikimimic_core::domain::LaneRole::Mic, 1),
        committed: text.into(),
        volatile: String::new(),
        start: None,
        lang: None,
        translation: crate::state::Translation::None,
    };
    state.update(cx, |s, cx| {
        s.phase = Phase::Listening;
        s.lines = vec![
            said(1, "では始めます。"),
            said(2, "未決の論点：取り込み機能の担当"),
        ];
        cx.notify();
    });
    window.update(cx, |w, cx| w.show(View::Live, cx));
    cx.run_until_parked();

    click(cx, "advise");
    window.read_with(cx, |w, _| {
        assert_eq!(w.advice.cards.len(), 1);
        assert_eq!(w.advice.cards[0].evidence, "未決の論点：取り込み機能の担当");
    });
    click(cx, "advice-dismiss-0");
    click(cx, "advise");
    window.read_with(cx, |w, _| {
        assert!(
            w.advice.cards.is_empty(),
            "a dismissed card does not return"
        )
    });

    state.update(cx, |s, cx| {
        s.lines.push(said(3, "未回答の質問：リリース日は？"));
        cx.notify();
    });
    click(cx, "advice-stop");
    window.read_with(cx, |w, _| assert!(w.advice.stopped));
    assert!(
        cx.debug_bounds("advise").is_none(),
        "a stopped advisor offers no button"
    );
}

#[gpui::test]
fn a_meeting_with_word_timing_shows_its_subtitle_timeline(cx: &mut TestAppContext) {
    use kikimimic_core::word_timing::{self, TimeRange, Word};
    let fixture = Fixture::new();
    let dir = fixture.0.join("recordings").join("20261117100000");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(dir.join("mic.wav"), spec).unwrap();
    for _ in 0..32_000 {
        wav.write_sample(500i16).unwrap();
    }
    wav.finalize().unwrap();
    std::fs::copy(dir.join("mic.wav"), dir.join("speaker.wav")).unwrap();
    let mut data = word_timing::generate(&dir, "fixture", |_, _| {
        Ok(vec![Word {
            text: "定例".into(),
            timing: Some(TimeRange {
                start_ms: 0,
                end_ms: 600,
            }),
        }])
    })
    .unwrap();
    word_timing::save(&dir, &mut data).unwrap();

    let kkm = fixture.start();
    let (_window, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::default());
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state, calendar, cx)
    });
    cx.run_until_parked();
    click(cx, "timeline-20261117100000");
    click(cx, "tab-subtitles");
    assert!(cx.debug_bounds("subtitle-timeline").is_some());
}

#[gpui::test]
fn calendar_meetings_start_and_stop_automatic_recording(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let mut settings = kkm.settings();
    settings.auto_record = true;
    kkm.update_settings(settings).unwrap();
    let (window, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::default());
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state, calendar, cx)
    });
    let tick = |cx: &mut gpui::VisualTestContext, present: bool, at: u64| {
        window.update(cx, |w, cx| w.auto_record_tick(present, at, cx));
    };
    tick(cx, true, 0);
    window.read_with(cx, |w, _| {
        assert!(w.auto_session, "a meeting in progress starts a recording")
    });
    tick(cx, false, 10);
    window.read_with(cx, |w, _| {
        assert!(w.auto_session, "a short gap does not end it")
    });
    tick(cx, false, 70);
    window.read_with(cx, |w, _| {
        assert!(!w.auto_session, "60 s without a meeting stops it")
    });

    // Stopping by hand holds until the meeting is over.
    tick(cx, true, 100);
    window.update(cx, |w, cx| w.stop_by_user(cx));
    tick(cx, true, 110);
    window.read_with(cx, |w, _| {
        assert!(
            !w.auto_session,
            "a meeting stopped by hand is not restarted"
        )
    });
}

#[gpui::test]
fn a_recording_started_from_the_calendar_card_is_not_doubled(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let mut settings = kkm.settings();
    settings.auto_record = true;
    kkm.update_settings(settings).unwrap();
    let state = cx.new(|_| AppState::default());
    let (window, cx) = cx.add_window_view(|_, cx| {
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state.clone(), calendar, cx)
    });
    // The cut-in card starts sessions directly in the core.
    state.update(cx, |s, cx| {
        s.phase = Phase::Listening;
        cx.notify();
    });
    window.update(cx, |w, cx| w.auto_record_tick(true, 0, cx));
    window.read_with(cx, |w, _| assert!(!w.auto_session));
}

#[gpui::test]
fn a_recording_right_clicked_in_the_timeline_goes_to_the_trash(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let kkm = fixture.start();
    let trash = fixture.0.join("Trash");
    unsafe { std::env::set_var("KKM_TRASH_DIR", &trash) };
    let (_window, cx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::default());
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        MainWindow::new(kkm.clone(), state, calendar, cx)
    });
    cx.run_until_parked();
    click(cx, "timeline-20261117100000");

    let row = cx.debug_bounds("timeline-20261117100000").unwrap();
    cx.simulate_mouse_down(row.center(), MouseButton::Right, Default::default());
    cx.run_until_parked();
    click(cx, "timeline-trash");

    assert!(!fixture.0.join("recordings/20261117100000").exists());
    assert!(trash.join("20261117100000/transcript.jsonl").exists());
    assert!(cx.debug_bounds("timeline-20261117100000").is_none());
    assert!(
        cx.debug_bounds("detail-player").is_none(),
        "the trashed recording no longer stays open"
    );
}
