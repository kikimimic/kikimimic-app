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
struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> Self {
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
        Self(root)
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
