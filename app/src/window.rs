//! The main window: session, recordings, models and settings.

mod imports;
mod live;
mod recordings;
mod settings;

use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, FontWeight, Hsla, IntoElement, MouseButton, MouseDownEvent,
    Pixels, Point, ScrollHandle, SharedString, Window, anchored, deferred, div, hsla, prelude::*,
    px, rgb,
};
use kikimimic_core::capture::SourceDecision;
use kikimimic_core::catalog::ModelKind;
use kikimimic_core::domain::{Lang, LanguagePolicy, Spoken};
use kikimimic_core::event::{Notice, Phase, PostProcessState};
use kikimimic_core::glossary::GlossaryEntry;
use kikimimic_core::library::{self, Recording};
use kikimimic_core::store::ModelState;
use kikimimic_core::{Kikimimic, SessionMode};

use crate::calendar::{Calendar, Status};
use crate::downloads::{Downloads, Progress};
use crate::i18n::Ui;
use crate::speakers::{Seek, SpeakerEditor};
use crate::state::{AppState, SourceMessage, Translation};
use crate::widgets::text_input::TextInput;
use crate::widgets::transcript_line;

pub(crate) mod words;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Session,
    Recordings,
    Models,
    Settings,
}

pub struct MainWindow {
    kkm: Kikimimic,
    state: Entity<AppState>,
    tab: Tab,
    scroll: ScrollHandle,
    downloads: Downloads,
    calendar: Entity<Calendar>,
    evaluation: Entity<crate::evaluation::EvaluationPanel>,
    recordings: Vec<Recording>,
    import_draft: Vec<std::path::PathBuf>,
    imports_completed: usize,
    open_recording: Option<(std::path::PathBuf, library::Session)>,
    speaker_editor: Option<Entity<SpeakerEditor>>,
    wrong: Entity<TextInput>,
    right: Entity<TextInput>,
    error: Option<String>,
    playback: Option<std::process::Child>,
    playback_status: Option<std::path::PathBuf>,
    words: words::WordsView,
    post_process_revision: u64,
    /// Where the session screen's right-click menu is open.
    menu: Option<Point<Pixels>>,
    ui: Ui,
}

fn gray() -> Hsla {
    hsla(0., 0., 0.55, 1.)
}

pub(crate) fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(px(6.))
        .border_1()
        .border_color(rgb(0xd0d7de))
        .cursor_pointer()
        .bg(if selected {
            rgb(0x1f2328)
        } else {
            rgb(0xffffff)
        })
        .text_color(if selected {
            rgb(0xffffff)
        } else {
            rgb(0x1f2328)
        })
        .hover(|s| s.border_color(rgb(0x8c959f)))
        .child(label.into())
}

pub(crate) fn mm_ss(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn notice_text(ui: Ui, n: &Notice) -> String {
    match n {
        Notice::SourceUnavailable { role, reason } => {
            ui.source_unavailable(&format!("{role:?}"), reason)
        }
        Notice::SystemAudioSilent { .. } => ui.system_audio_silent().into(),
        Notice::RecordingOff { reason } => ui.recording_stopped(reason),
        Notice::Overrun { lane, dropped_ms } => ui.overrun(lane.as_str(), *dropped_ms),
        Notice::DecodeFailed { lane, reason } => ui.decode_failed(lane.as_str(), reason),
        Notice::TranslationUnavailable { reason } => ui.translation_unavailable(reason),
    }
}

impl MainWindow {
    pub fn new(
        kkm: Kikimimic,
        state: Entity<AppState>,
        calendar: Entity<Calendar>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&calendar, |_, _, cx| cx.notify()).detach();
        cx.observe(&state, |this: &mut Self, state, cx| {
            let completed = state
                .read(cx)
                .imports
                .jobs
                .iter()
                .filter(|j| {
                    matches!(
                        j.state,
                        kikimimic_core::imports::ImportState::Done
                            | kikimimic_core::imports::ImportState::Skipped
                    )
                })
                .count();
            if completed != this.imports_completed {
                this.imports_completed = completed;
                if this.tab == Tab::Recordings {
                    this.recordings = library::list(&crate::paths::recordings_dir());
                }
            }
            this.scroll.scroll_to_bottom();
            if state.read(cx).is_listening() {
                this.stop_playback();
            }
            // Failed reruns can also change the installed automatic evidence.
            let post_process_revision = state.read(cx).post_process_revision;
            let done = this.post_process_revision != post_process_revision
                && matches!(
                    (&state.read(cx).post_process, &this.open_recording),
                    (Some((dir, PostProcessState::Done | PostProcessState::Failed { .. })), Some((open, _))) if dir == open
                );
            this.post_process_revision = post_process_revision;
            if done && let Some((dir, _)) = this.open_recording.take() {
                if let Some(editor) = &this.speaker_editor {
                    editor.update(cx, |editor, cx| editor.refresh_evidence(cx));
                }
                this.open_recording = library::read_raw(&dir).ok().map(|s| (dir.clone(), s));
                this.load_words(dir, cx);
            }
            let word_event = state.read(cx).word_timing.clone();
            if this.words.revision != state.read(cx).word_revision
                && let Some((dir, result)) = word_event
            {
                this.words.revision = state.read(cx).word_revision;
                if this
                    .open_recording
                    .as_ref()
                    .is_some_and(|(open, _)| open == &dir)
                {
                    match result {
                        PostProcessState::Done => this.load_words(dir, cx),
                        PostProcessState::Failed { message } => {
                            this.words.pending = false;
                            this.words.error = Some(message);
                        }
                        PostProcessState::Running => this.words.pending = true,
                    }
                }
            }
            this.sync_word_editor(cx);
            cx.notify();
        })
        .detach();
        let ui = Ui::detect();
        let wrong = cx.new(|cx| TextInput::new(ui.misrecognized_as(), cx));
        let right = cx.new(|cx| TextInput::new(ui.correct_spelling(), cx));
        let evaluation = cx.new(crate::evaluation::EvaluationPanel::new);
        cx.subscribe(
            &evaluation,
            |this, _, seek: &crate::evaluation::Seek, cx| {
                this.play_from(&seek.dir, seek.position_ms, cx);
            },
        )
        .detach();
        Self {
            kkm,
            state,
            tab: Tab::Session,
            scroll: ScrollHandle::new(),
            downloads: Downloads::default(),
            calendar,
            evaluation,
            recordings: Vec::new(),
            import_draft: Vec::new(),
            imports_completed: 0,
            open_recording: None,
            speaker_editor: None,
            playback: None,
            playback_status: None,
            words: words::WordsView::default(),
            post_process_revision: 0,
            wrong,
            right,
            error: None,
            menu: None,
            ui,
        }
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == Tab::Recordings {
            self.stop_playback();
            self.sync_word_editor(cx);
            self.recordings = library::list(&crate::paths::recordings_dir());
        }
        cx.notify();
    }

    /// Redraw while downloads run, so their progress moves.
    fn watch_downloads(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let Ok(running) = this.update(cx, |w, cx| {
                    cx.notify();
                    w.downloads.any_running()
                }) else {
                    return;
                };
                if !running {
                    return;
                }
            }
        })
        .detach();
    }

    fn update_settings(
        &mut self,
        change: impl FnOnce(&mut kikimimic_core::settings::Settings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.kkm.settings();
        change(&mut settings);
        self.error = self.kkm.update_settings(settings).err();
        cx.notify();
    }

    fn start(&mut self, mode: SessionMode, cx: &mut Context<Self>) {
        self.stop_playback();
        if self.kkm.settings().clear_on_start {
            self.state.update(cx, |s, _| s.lines.clear());
        }
        self.kkm.start_session(mode);
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let tabs = [
            (Tab::Session, "session", ui.tab_session()),
            (Tab::Recordings, "recordings", ui.tab_recordings()),
            (Tab::Models, "models", ui.tab_models()),
            (Tab::Settings, "settings", ui.tab_settings()),
        ];
        div()
            .flex()
            .gap_2()
            .items_center()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(0xd0d7de))
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .mr_4()
                    .child("Kikimimic"),
            )
            .children(tabs.into_iter().map(|(tab, id, label)| {
                button(id, label, self.tab == tab)
                    .on_click(cx.listener(move |this, _, _, cx| this.select(tab, cx)))
            }))
    }
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tab {
            Tab::Session => self.session(cx),
            Tab::Recordings => self.recordings_view(cx),
            Tab::Models => self.models_view(cx),
            Tab::Settings => self.settings_view(cx),
        };
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x1f2328))
            .text_size(px(14.))
            .child(self.header(cx));
        if let Some(e) = &self.error {
            root = root.child(
                div()
                    .px_4()
                    .py_1()
                    .bg(rgb(0xffebe9))
                    .text_color(rgb(0xcf222e))
                    .child(e.clone()),
            );
        }
        root.child(div().flex_1().min_h_0().child(body))
    }
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        self.stop_playback();
    }
}
