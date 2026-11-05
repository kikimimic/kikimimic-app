//! The main window: session, recordings, models and settings.

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
use crate::state::{AppState, SourceMessage, Translation};
use crate::widgets::text_input::TextInput;
use crate::widgets::transcript_line;

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
    recordings: Vec<Recording>,
    open_recording: Option<(std::path::PathBuf, library::Session)>,
    wrong: Entity<TextInput>,
    right: Entity<TextInput>,
    error: Option<String>,
    /// Where the session screen's right-click menu is open.
    menu: Option<Point<Pixels>>,
    ui: Ui,
}

fn gray() -> Hsla {
    hsla(0., 0., 0.55, 1.)
}

fn button(
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

fn mm_ss(ms: u64) -> String {
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
            this.scroll.scroll_to_bottom();
            // Labelling finished for the open recording: show the split lines.
            let done = matches!(
                (&state.read(cx).post_process, &this.open_recording),
                (Some((dir, PostProcessState::Done)), Some((open, _))) if dir == open
            );
            if done && let Some((dir, _)) = this.open_recording.take() {
                this.open_recording = library::read(&dir).ok().map(|s| (dir, s));
            }
            cx.notify();
        })
        .detach();
        let ui = Ui::detect();
        let wrong = cx.new(|cx| TextInput::new(ui.misrecognized_as(), cx));
        let right = cx.new(|cx| TextInput::new(ui.correct_spelling(), cx));
        Self {
            kkm,
            state,
            tab: Tab::Session,
            scroll: ScrollHandle::new(),
            downloads: Downloads::default(),
            calendar,
            recordings: Vec::new(),
            open_recording: None,
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
            self.recordings = library::list(&crate::paths::recordings_dir());
            self.open_recording = None;
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

    fn session(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let state = self.state.read(cx);
        let listening = state.is_listening();
        let busy = matches!(state.phase, Phase::Stopping);
        let mut col = div()
            .flex()
            .flex_col()
            .size_full()
            .gap_2()
            .p_4()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.menu = Some(e.position);
                    cx.notify();
                }),
            );
        if let Some(at) = self.menu {
            col = col.child(deferred(
                anchored().position(at).child(
                    div()
                        .bg(rgb(0xffffff))
                        .shadow_md()
                        .rounded(px(6.))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }))
                        .child(button("clear", ui.clear_transcript(), false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                this.state.update(cx, |s, _| s.lines.clear());
                            }),
                        )),
                ),
            ));
        }
        let mut controls = div().flex().gap_2().items_center();
        if listening {
            controls = controls.child(
                button("stop", ui.stop(), true)
                    .on_click(cx.listener(|this, _, _, _| this.kkm.stop_session())),
            );
        } else if !busy {
            controls =
                controls
                    .child(button("meeting", ui.record_meeting(), false).on_click(
                        cx.listener(|this, _, _, cx| this.start(SessionMode::Meeting, cx)),
                    ))
                    .child(button("face", ui.face_to_face(), false).on_click(
                        cx.listener(|this, _, _, cx| this.start(SessionMode::FaceToFace, cx)),
                    ));
        }
        let phase = match &state.phase {
            Phase::Idle => ui.idle().to_string(),
            Phase::Listening => ui.listening().to_string(),
            Phase::Stopping => ui.finishing().to_string(),
            Phase::Error { message } => ui.error(message),
        };
        controls = controls.child(div().text_color(gray()).child(phase));
        col = col.child(controls);
        if let Some(q) = &state.question {
            let uid = q.uid.clone();
            let uid2 = q.uid.clone();
            col = col.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .p_2()
                    .bg(rgb(0xfff8c5))
                    .child(ui.output_changed(&q.detected, &q.capturing))
                    .child(button("follow", ui.follow(), false).on_click(cx.listener(
                        move |this, _, _, _| {
                            this.kkm
                                .decide_source(SourceDecision::Follow { uid: uid.clone() })
                        },
                    )))
                    .child(button("stay", ui.stay(), false).on_click(cx.listener(
                        move |this, _, _, _| {
                            this.kkm
                                .decide_source(SourceDecision::Stay { uid: uid2.clone() })
                        },
                    ))),
            );
        }
        for n in &state.notices {
            col = col.child(div().text_color(rgb(0x9a6700)).child(notice_text(ui, n)));
        }
        if let Some(m) = &state.source_message {
            let text = match m {
                SourceMessage::Switched(name) => ui.now_capturing(name),
                SourceMessage::Failed(reason) => reason.clone(),
            };
            col = col.child(div().text_color(gray()).child(text));
        }
        let lines = state.lines.iter().map(|l| {
            let mut row = div().flex().flex_col().gap_1().py_1();
            let time = l
                .start
                .map(|t| mm_ss(t.0))
                .unwrap_or_else(|| "··:··".into());
            row = row.child(
                div()
                    .flex()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(time)
                    .child(l.lane.as_str().to_string()),
            );
            row = row.child(transcript_line::render(&l.committed, &l.volatile, gray()));
            let translation = match &l.translation {
                Translation::None => None,
                Translation::Pending => Some((ui.translating().to_string(), gray())),
                Translation::Text(t) => Some((t.clone(), Hsla::from(rgb(0x0969da)))),
                Translation::Dropped => None,
                Translation::Failed(m) => {
                    Some((ui.translation_failed(m), Hsla::from(rgb(0xcf222e))))
                }
            };
            if let Some((text, color)) = translation {
                row = row.child(div().text_color(color).child(text));
            }
            row
        });
        col.child(
            div()
                .id("transcript")
                .flex()
                .flex_col()
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(lines),
        )
        .into_any_element()
    }

    fn recordings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let mut col = div()
            .id("recordings")
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .size_full()
            .overflow_y_scroll();
        if let Some((dir, session)) = &self.open_recording {
            let labelling = match &self.state.read(cx).post_process {
                Some((d, state)) if d == dir => Some(state.clone()),
                _ => None,
            };
            let dir = dir.clone();
            let mut bar = div().flex().gap_2().items_center().child(
                button("back", ui.all_recordings(), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.open_recording = None;
                        cx.notify();
                    },
                )),
            );
            bar =
                match labelling {
                    Some(PostProcessState::Running) => {
                        bar.child(div().text_color(gray()).child(ui.identifying()))
                    }
                    other => {
                        if let Some(PostProcessState::Failed { message }) = other {
                            bar = bar.child(div().text_color(rgb(0xcf222e)).child(message));
                        }
                        bar.child(button("diarize", ui.identify_speakers(), false).on_click(
                            cx.listener(move |this, _, _, _| this.kkm.diarize(dir.clone())),
                        ))
                    }
                };
            col = col.child(bar);
            for line in &session.lines {
                let mut row = div().flex().flex_col().py_1().child(
                    div()
                        .flex()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(gray())
                        .child(mm_ss(line.start_ms))
                        .child(line.speaker.clone().unwrap_or_else(|| line.lane.clone())),
                );
                row = row.child(line.text.clone());
                if let Some(t) = &line.translation {
                    row = row.child(div().text_color(rgb(0x0969da)).child(t.clone()));
                }
                col = col.child(row);
            }
            return col.into_any_element();
        }
        if self.recordings.is_empty() {
            return col
                .child(div().text_color(gray()).child(ui.no_recordings()))
                .into_any_element();
        }
        for (i, r) in self.recordings.iter().enumerate() {
            let dir = r.dir.clone();
            let title = r.title.clone().unwrap_or_else(|| r.name.clone());
            col =
                col.child(
                    div()
                        .id(("recording", i))
                        .flex()
                        .flex_col()
                        .p_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(0xf6f8fa)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(12.)).text_color(gray()).child(
                            ui.recording_summary(&mm_ss(r.duration_ms), r.utterances, &r.snippet),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let path = std::path::PathBuf::from(&dir);
                            match library::read(&path) {
                                Ok(s) => this.open_recording = Some((path, s)),
                                Err(e) => this.error = Some(format!("{e:#}")),
                            }
                            cx.notify();
                        })),
                );
        }
        col.into_any_element()
    }

    fn models_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let store = self.kkm.models().clone();
        let settings = self.kkm.settings();
        let mut col = div()
            .id("models")
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .size_full()
            .overflow_y_scroll();
        for (i, m) in store.catalog().models().iter().enumerate() {
            if m.kind == ModelKind::Vad || !crate::engines::runs_here(m) {
                continue;
            }
            let id = m.id.clone();
            let size: u64 = m.files.iter().map(|f| f.bytes).sum();
            let state = store.state(&m.id).unwrap_or(ModelState::Missing {
                partial: 0,
                total: size,
            });
            let in_use =
                settings.asr_model == m.id || settings.mt_model.as_deref() == Some(m.id.as_str());
            let mut row = div().flex().gap_3().items_center().child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "{}{}",
                        m.title,
                        if in_use { ui.in_use() } else { "" }
                    )))
                    .child(div().text_size(px(12.)).text_color(gray()).child(
                        if m.files.is_empty() {
                            format!("{:?} · {}", m.kind, ui.built_into_macos())
                        } else {
                            format!(
                                "{:?} · {:.1} GB · {}",
                                m.kind,
                                size as f64 / 1e9,
                                m.license.name
                            )
                        },
                    )),
            );
            let progress = self.downloads.get(&m.id);
            row =
                match (state, progress) {
                    (_, Some(Progress::Running { received, total })) => {
                        row.child(ui.downloading(if total == 0 {
                            0.0
                        } else {
                            received as f64 * 100.0 / total as f64
                        }))
                    }
                    (ModelState::Ready, _) => {
                        row.child(div().text_color(rgb(0x1a7f37)).child(ui.ready()))
                    }
                    (ModelState::NeedsLicense, _) => {
                        let url = m.license.url.clone().unwrap_or_default();
                        row.child(div().text_size(px(12.)).text_color(gray()).child(url))
                            .child(button(("accept", i), ui.accept_license(), false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.error = this.kkm.models().accept_license(&id).err();
                                    cx.notify();
                                }),
                            ))
                    }
                    (ModelState::Missing { .. }, failed) => {
                        if let Some(Progress::Failed(e)) = failed {
                            row = row.child(div().text_color(rgb(0xcf222e)).child(e));
                        }
                        row.child(button(("download", i), ui.download(), false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if this.downloads.start(this.kkm.models().clone(), id.clone()) {
                                    this.watch_downloads(cx);
                                }
                                cx.notify();
                            }),
                        ))
                    }
                };
            col = col.child(row);
        }
        col.into_any_element()
    }

    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let settings = self.kkm.settings();
        let policy = settings.languages.clone();
        let spoken: Vec<String> = policy
            .spoken
            .iter()
            .map(|l| l.as_str().to_string())
            .collect();
        let mut col = div()
            .id("settings")
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .size_full()
            .overflow_y_scroll();

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.spoken_languages()),
        );
        let mut row = div().flex().gap_2().flex_wrap();
        for (code, _, name) in kikimimic_core::language::NAMES {
            let on = spoken.iter().any(|s| s == code);
            let code = *code;
            row = row.child(
                button(SharedString::from(format!("spoken-{code}")), *name, on).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_settings(|s| toggle_spoken(&mut s.languages, code), cx)
                    }),
                ),
            );
        }
        col = col.child(row);

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.translate_into()),
        );
        let mut row = div().flex().gap_2().flex_wrap().child(
            button("target-none", ui.dont_translate(), policy.target.is_none()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.update_settings(|s| s.languages.target = None, cx)
                }),
            ),
        );
        for (code, _, name) in kikimimic_core::language::NAMES {
            let on = policy.target.as_ref().is_some_and(|t| t.as_str() == *code);
            let code = *code;
            row = row.child(
                button(SharedString::from(format!("target-{code}")), *name, on).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_settings(|s| s.languages.target = Lang::parse(code).ok(), cx)
                    }),
                ),
            );
        }
        col = col
            .child(row)
            .child(
                button("mutual", ui.mutual(), policy.mutual).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.update_settings(|s| s.languages.mutual = !s.languages.mutual, cx)
                    },
                )),
            );

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.recognition_model()),
        );
        let catalog = self.kkm.models().catalog();
        let mut row = div().flex().gap_2().flex_wrap();
        for (i, m) in catalog.models().iter().enumerate() {
            if m.kind != ModelKind::Asr || !crate::engines::runs_here(m) {
                continue;
            }
            let id = m.id.clone();
            let on = settings.asr_model == m.id;
            row = row.child(
                button(("asr", i), m.title.clone(), on).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = id.clone();
                        this.update_settings(|s| s.asr_model = id, cx)
                    },
                )),
            );
        }
        col = col.child(row);
        if let Some(asr) = catalog.get(&settings.asr_model) {
            let unheard: Vec<&str> = asr
                .unheard(&policy.spoken)
                .map(|l| {
                    kikimimic_core::language::NAMES
                        .iter()
                        .find(|(code, ..)| *code == l.as_str())
                        .map_or(l.as_str(), |(_, _, name)| *name)
                })
                .collect();
            if !unheard.is_empty() {
                col = col.child(div().text_color(rgb(0x9a6700)).child(format!(
                    "{}{}",
                    ui.unheard_languages(),
                    unheard.join(", ")
                )));
            }
        }

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.translation_model()),
        );
        let mut row = div().flex().gap_2().flex_wrap().child(
            button("mt-none", ui.none(), settings.mt_model.is_none()).on_click(
                cx.listener(|this, _, _, cx| this.update_settings(|s| s.mt_model = None, cx)),
            ),
        );
        for (i, m) in self.kkm.models().catalog().models().iter().enumerate() {
            if m.kind != ModelKind::Mt {
                continue;
            }
            let id = m.id.clone();
            let on = settings.mt_model.as_deref() == Some(m.id.as_str());
            row = row.child(button(("mt", i), m.title.clone(), on).on_click(cx.listener(
                move |this, _, _, cx| {
                    let id = id.clone();
                    this.update_settings(|s| s.mt_model = Some(id), cx)
                },
            )));
        }
        col = col.child(row);

        col = col
            .child(div().font_weight(FontWeight::SEMIBOLD).child(ui.glossary()))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.glossary_help()),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(220.)).child(self.wrong.clone()))
                    .child("→")
                    .child(div().w(px(220.)).child(self.right.clone()))
                    .child(button("add", ui.add(), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            let wrong = this.wrong.read(cx).text().trim().to_string();
                            let right = this.right.read(cx).text().trim().to_string();
                            if wrong.is_empty() || right.is_empty() {
                                return;
                            }
                            this.update_settings(
                                |s| s.glossary.entries.push(GlossaryEntry { wrong, right }),
                                cx,
                            );
                            this.wrong.update(cx, |i, cx| i.clear(cx));
                            this.right.update(cx, |i, cx| i.clear(cx));
                        },
                    ))),
            );
        for (i, e) in settings.glossary.entries.iter().enumerate() {
            col = col.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(format!("{} → {}", e.wrong, e.right))
                    .child(
                        button(("remove", i), ui.remove(), false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.update_settings(
                                    |s| {
                                        if i < s.glossary.entries.len() {
                                            s.glossary.entries.remove(i);
                                        }
                                    },
                                    cx,
                                )
                            },
                        )),
                    ),
            );
        }
        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.tab_session()),
        );
        col = col.child(
            button(
                "clear-on-start",
                ui.clear_on_start(),
                settings.clear_on_start,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_settings(|s| s.clear_on_start = !s.clear_on_start, cx)
            })),
        );
        col.child(self.calendar_section(cx)).into_any_element()
    }

    fn calendar_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let calendar = self.calendar.read(cx);
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(ui.google_calendar()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.calendar_description()),
            );
        let sign_in = |label: &'static str| {
            button("sign-in", label, false).on_click(
                cx.listener(|this, _, _, cx| this.calendar.update(cx, |c, cx| c.sign_in(cx))),
            )
        };
        let status = match &calendar.status {
            Status::Unavailable => div().text_color(gray()).child(ui.calendar_unavailable()),
            Status::SignedOut => div().flex().child(sign_in(ui.sign_in_google())),
            Status::NeedsSignIn { .. } => div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(0x9a6700)).child(ui.needs_sign_in()))
                .child(sign_in(ui.sign_in_google())),
            Status::SigningIn => div()
                .flex()
                .gap_2()
                .items_center()
                .child(ui.signing_in())
                .child(
                    button("cancel-sign-in", ui.cancel(), false).on_click(cx.listener(
                        |this, _, _, cx| this.calendar.update(cx, |c, cx| c.cancel_sign_in(cx)),
                    )),
                ),
            Status::SignedIn { email } => {
                let mut row = div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(ui.signed_in_as(email))
                    .child(
                        button("sign-out", ui.sign_out(), false).on_click(cx.listener(
                            |this, _, _, cx| this.calendar.update(cx, |c, cx| c.sign_out(cx)),
                        )),
                    );
                if let Some(at) = calendar.last_read {
                    row = row.child(
                        div()
                            .text_color(gray())
                            .child(ui.last_read(&at.format("%H:%M").to_string())),
                    );
                }
                row
            }
        };
        col = col.child(status);
        if let Some(error) = &calendar.last_error {
            let text = ui.google_error(error);
            if !text.is_empty() {
                col = col.child(div().text_color(rgb(0xcf222e)).child(text));
            }
        }
        col
    }
}

/// Toggle a spoken language, keeping one or two.
fn toggle_spoken(policy: &mut LanguagePolicy, code: &str) {
    let Ok(lang) = Lang::parse(code) else { return };
    let mut langs: Vec<Lang> = policy.spoken.iter().cloned().collect();
    if let Some(i) = langs.iter().position(|l| *l == lang) {
        if langs.len() > 1 {
            langs.remove(i);
        }
    } else {
        langs.push(lang);
        if langs.len() > 2 {
            langs.remove(0);
        }
    }
    if let Ok(spoken) = Spoken::try_from(langs) {
        policy.spoken = spoken;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(p: &LanguagePolicy) -> Vec<&str> {
        p.spoken.iter().map(|l| l.as_str()).collect()
    }

    #[test]
    fn toggling_keeps_one_or_two_spoken_languages() {
        let mut p = LanguagePolicy::default(); // ja, en
        toggle_spoken(&mut p, "ko");
        assert_eq!(codes(&p), ["en", "ko"], "the oldest gives way");
        toggle_spoken(&mut p, "en");
        assert_eq!(codes(&p), ["ko"]);
        toggle_spoken(&mut p, "ko");
        assert_eq!(codes(&p), ["ko"], "the last language stays");
    }
}
