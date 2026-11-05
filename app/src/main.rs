mod calendar;
mod cutin;
mod downloads;
mod engines;
mod google;
mod i18n;
mod paths;
mod state;
mod widgets;
mod window;

use futures::StreamExt;
use gpui::{App, AppContext, Bounds, Entity, WindowBounds, WindowOptions, px, size};
use kikimimic_core::catalog::Catalog;
use kikimimic_core::{Config, Events, Kikimimic};

use calendar::Calendar;
use state::AppState;
use widgets::text_input;
use window::MainWindow;

/// Core events arrive on a blocking iterator; a thread hands them to a
/// foreground task, which folds them into the UI state.
fn forward(events: Events, state: Entity<AppState>, cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    std::thread::Builder::new()
        .name("kikimimic-events".into())
        .spawn(move || {
            for event in events {
                if tx.unbounded_send(event).is_err() {
                    break;
                }
            }
        })
        .expect("spawn the event thread");
    cx.spawn(async move |cx| {
        while let Some(event) = rx.next().await {
            state.update(cx, |s, cx| {
                s.apply(event);
                cx.notify();
            });
        }
    })
    .detach();
}

/// Nemotron runs on Apple Silicon only.
fn diarizer() -> Option<Box<dyn kikimimic_core::engine::Diarizer>> {
    (cfg!(all(target_os = "macos", target_arch = "aarch64"))).then(|| {
        Box::new(kikimimic_diarize_nemotron::Nemotron::new(paths::cache_dir()))
            as Box<dyn kikimimic_core::engine::Diarizer>
    })
}

fn main() {
    let (kkm, events) = Kikimimic::start(Config {
        data_dir: paths::data_dir(),
        extra_model_dirs: paths::extra_model_dirs(),
        catalog: Catalog::builtin(),
        engines: engines::registry(),
        recordings_dir: paths::recordings_dir(),
        capture: kikimimic_capture::platform_backend(),
        session_name: Box::new(paths::session_name),
        diarizer: diarizer(),
    });
    gpui_platform::application().run(move |cx: &mut App| {
        text_input::bind_keys(cx);
        let state = cx.new(|_| AppState::default());
        forward(events, state.clone(), cx);
        let bounds = Bounds::centered(None, size(px(960.), px(720.)), cx);
        let calendar = cx.new(|cx| Calendar::new(kkm.clone(), state.clone(), cx));
        let main = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                {
                    let calendar = calendar.clone();
                    |_, cx| cx.new(|cx| MainWindow::new(kkm, state, calendar, cx))
                },
            )
            .expect("open the main window");
        calendar.update(cx, |c, _| c.main_window = Some(main.into()));
        cx.activate(true);
    });
}
