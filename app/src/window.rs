//! The main window: a status bar, the meeting timeline, one screen in a
//! width-limited column, and a status line (the v4 layout).

mod ask;
mod detail;
mod imports;
mod live;
mod recordings;
mod settings;
mod shell;
mod subtitles;

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

/// The screen in the main column; the timeline beside it is always there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Live,
    Recording,
    Imports,
    Settings,
}

pub struct MainWindow {
    kkm: Kikimimic,
    state: Entity<AppState>,
    view: View,
    /// The mode of the session in progress, for the status bar.
    live_mode: Option<SessionMode>,
    search: Entity<TextInput>,
    /// The query `hits` answer, so a new query starts one search.
    searched: String,
    hits: Vec<kikimimic_core::search::Hit>,
    list_filter: shell::ListFilter,
    /// Whether a session was running at the last state change.
    was_running: bool,
    /// Import jobs seen waiting or running in this run.
    imports_watched: std::collections::HashSet<u64>,
    section: settings::Section,
    detail_tab: detail::DetailTab,
    detail: detail::DetailCache,
    ask: ask::AskState,
    advice: live::Advice,
    /// Automatic recording of calendar meetings (#35), and whether the
    /// session in progress is one it started.
    policy: kikimimic_core::meeting::MeetingPolicy,
    auto_session: bool,
    /// The policy counts monotonic seconds from here.
    started_at: std::time::Instant,
    /// How an agent is called; tests put a fake here.
    run_agent: fn(kikimimic_core::agent::Agent, &str) -> anyhow::Result<String>,
    dest_selected: usize,
    dest_name: Entity<TextInput>,
    dest_folder: Entity<TextInput>,
    dest_file: Entity<TextInput>,
    show_older: bool,
    /// What each recording still waits for, keyed by its directory.
    stages: std::collections::HashMap<std::path::PathBuf, Option<kikimimic_core::timeline::Stage>>,
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
    /// The recording the timeline's right-click menu is open for, and where.
    timeline_menu: Option<(std::path::PathBuf, Point<Pixels>)>,
    pub(crate) ui: Ui,
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
            // A session that just ended is a finished recording.
            let running = matches!(state.read(cx).phase, Phase::Listening | Phase::Stopping);
            if this.was_running && !running {
                this.live_mode = None;
                this.policy.stopped(false);
                this.auto_session = false;
                if let Some(dir) = state.read(cx).recording_dir.clone() {
                    this.auto_export(dir, kikimimic_core::destinations::When::AfterRecording, cx);
                }
                this.refresh_recordings(cx);
            }
            this.was_running = running;
            // So is an import that finished while we watched. Jobs already
            // done when the app started were handled by the run that did them.
            let jobs = state.read(cx).imports.jobs.clone();
            for job in &jobs {
                match (&job.state, &job.output) {
                    (kikimimic_core::imports::ImportState::Pending | kikimimic_core::imports::ImportState::Running, _) => {
                        this.imports_watched.insert(job.id);
                    }
                    (kikimimic_core::imports::ImportState::Done, Some(dir)) if this.imports_watched.remove(&job.id) => {
                        let dir = dir.clone();
                        this.auto_export(dir, kikimimic_core::destinations::When::AfterRecording, cx);
                    }
                    _ => {}
                }
            }
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
                this.refresh_recordings(cx);
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
        let ui = Ui::new(kkm.settings().ui_language.as_deref());
        let wrong = cx.new(|cx| TextInput::new(ui.misrecognized_as(), cx));
        let right = cx.new(|cx| TextInput::new(ui.correct_spelling(), cx));
        let evaluation = cx.new(crate::evaluation::EvaluationPanel::new);
        let search = cx.new(|cx| TextInput::new(ui.search_meetings(), cx));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &evaluation,
            |this, _, seek: &crate::evaluation::Seek, cx| {
                this.play_from(&seek.dir, seek.position_ms, cx);
            },
        )
        .detach();
        let mut this = Self {
            kkm,
            state,
            view: View::Recording,
            live_mode: None,
            search,
            searched: String::new(),
            hits: Vec::new(),
            list_filter: shell::ListFilter::All,
            was_running: false,
            imports_watched: Default::default(),
            section: settings::Section::General,
            detail_tab: detail::DetailTab::Text,
            detail: Default::default(),
            ask: ask::AskState::new(ui, cx),
            advice: Default::default(),
            policy: Default::default(),
            auto_session: false,
            started_at: std::time::Instant::now(),
            run_agent: kikimimic_core::agent::run,
            dest_selected: 0,
            dest_name: cx.new(|cx| TextInput::new("", cx)),
            dest_folder: cx.new(|cx| TextInput::new("", cx)),
            dest_file: cx.new(|cx| TextInput::new("", cx)),
            show_older: false,
            stages: Default::default(),
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
            timeline_menu: None,
            ui,
        };
        this.refresh_recordings(cx);
        this.load_destination_fields(cx);
        this.watch_calendar_meetings(cx);
        this
    }

    fn show(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    /// Search what was said once per new query, off the UI thread.
    fn search_if_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).text().trim().to_string();
        if query == self.searched {
            return;
        }
        self.searched = query.clone();
        if query.is_empty() {
            self.hits.clear();
            return;
        }
        let folders: Vec<std::path::PathBuf> = Vec::new();
        cx.spawn(async move |this, cx| {
            let base = crate::paths::recordings_dir();
            let q = query.clone();
            let hits = cx
                .background_spawn(async move {
                    kikimimic_core::search::search(&base, &folders, &q).unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.searched == query {
                    this.hits = hits;
                    cx.notify();
                }
            });
        })
        .detach();
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
        let ui = Ui::new(self.kkm.settings().ui_language.as_deref());
        if ui != self.ui {
            self.relabel(ui, cx);
        }
        cx.notify();
    }

    /// Everything that keeps its wording instead of asking `self.ui` on
    /// each render: input placeholders and the menus.
    fn relabel(&mut self, ui: Ui, cx: &mut Context<Self>) {
        self.ui = ui;
        for (input, text) in [
            (&self.wrong, ui.misrecognized_as()),
            (&self.right, ui.correct_spelling()),
            (&self.search, ui.search_meetings()),
            (&self.ask.input, ui.ask_placeholder()),
        ] {
            input.update(cx, |i, cx| {
                i.placeholder = text.into();
                cx.notify();
            });
        }
        crate::menus::relabel(ui, cx);
    }

    /// Start recording from outside the window (the menu bar, the Dock).
    pub(crate) fn record(&mut self, mode: SessionMode, cx: &mut Context<Self>) {
        if !self.state.read(cx).is_listening() {
            self.start(mode, cx);
        }
    }

    pub(crate) fn toggle_auto_record(&mut self, cx: &mut Context<Self>) {
        self.update_settings(|s| s.auto_record = !s.auto_record, cx);
    }

    pub(crate) fn toggle_captions(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).is_listening() {
            if let Err(error) = crate::captions::toggle(self.state.clone(), cx) {
                self.error = Some(error.to_string());
            }
            cx.notify();
        }
    }

    /// A recording the user started: automatic recording leaves it alone.
    fn start(&mut self, mode: SessionMode, cx: &mut Context<Self>) {
        self.policy.manual_started();
        self.begin(mode, cx);
    }

    /// Stopped by the user: an automatic recording does not come back for
    /// the same meeting.
    pub(crate) fn stop_by_user(&mut self, cx: &mut Context<Self>) {
        self.policy.stopped(true);
        self.auto_session = false;
        self.kkm.stop_session();
        cx.notify();
    }

    /// One step of automatic recording: `present` is whether a calendar
    /// meeting is in progress, `now` monotonic seconds.
    fn auto_record_tick(&mut self, present: bool, now: u64, cx: &mut Context<Self>) {
        use kikimimic_core::meeting::Action;
        self.policy.enabled = self.kkm.settings().auto_record;
        // A session started elsewhere (the calendar card) counts as manual,
        // so the policy never starts a second one beside it.
        if self.state.read(cx).is_listening() && !self.auto_session {
            self.policy.manual_started();
        }
        match self.policy.poll(present, now) {
            Some(Action::Start) => {
                self.auto_session = true;
                self.begin(SessionMode::Meeting, cx);
            }
            Some(Action::Stop) => {
                self.auto_session = false;
                self.kkm.stop_session();
            }
            None => {}
        }
        cx.notify();
    }

    fn watch_calendar_meetings(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    let present = this.calendar.read(cx).in_meeting(chrono::Utc::now());
                    let now = this.started_at.elapsed().as_secs();
                    this.auto_record_tick(present, now, cx);
                }) else {
                    return;
                };
            }
        })
        .detach();
    }

    fn begin(&mut self, mode: SessionMode, cx: &mut Context<Self>) {
        self.stop_playback();
        if self.kkm.settings().clear_on_start {
            self.state.update(cx, |s, _| s.lines.clear());
        }
        self.live_mode = Some(mode);
        // One advisor per meeting: dismissals and a stop do not carry over.
        self.advice = Default::default();
        self.view = View::Live;
        self.kkm.start_session(mode);
        cx.notify();
    }
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.search_if_changed(cx);
        let body = match self.view {
            View::Live => self.session(cx),
            View::Recording => self.recording_detail(cx),
            View::Imports => div()
                .id("imports")
                .size_full()
                .overflow_y_scroll()
                .child(shell::column().child(self.imports_view(cx)))
                .into_any_element(),
            View::Settings => self.settings_view(cx),
        };
        let mut main = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        if let Some(e) = &self.error {
            main = main.child(
                div()
                    .px_4()
                    .py_1()
                    .bg(rgb(0xffebe9))
                    .text_color(rgb(0xcf222e))
                    .child(e.clone()),
            );
        }
        let main = main.child(div().flex_1().min_h_0().child(body));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x1f2328))
            .text_size(px(14.))
            .child(self.top_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(cx))
                    .child(main),
            )
            .child(self.footer(cx))
    }
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        self.stop_playback();
    }
}

#[cfg(test)]
mod tests;
