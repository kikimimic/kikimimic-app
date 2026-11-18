//! The frame around every screen: the status bar on top, the meeting
//! timeline on the left, and the status line at the bottom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::ExternalPaths;
use kikimimic_core::destinations::Destination;
use kikimimic_core::imports::{ImportMode, ImportState};
use kikimimic_core::timeline::{self, DayLabel, Stage};

use super::*;
use crate::theme;

/// Width of the meeting timeline.
const SIDEBAR: f32 = 264.;
/// The reading column never grows wider than this, however wide the window.
const COLUMN: f32 = 860.;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ListFilter {
    #[default]
    All,
    Unfinished,
}

/// What a recording is waiting for, read from its files. Slow enough (it
/// reads the transcript) that the window computes it off the UI thread.
pub(super) fn stage_of(dir: &Path, destinations: &[Destination]) -> Option<Stage> {
    let session = library::read(dir).ok()?;
    let minutes = kikimimic_core::minutes::load(dir).ok().flatten();
    timeline::next_stage(&session, dir, minutes.as_ref(), destinations)
}

fn stage_label(ui: Ui, stage: Stage) -> &'static str {
    match stage {
        Stage::Transcribe => ui.stage_transcribe(),
        Stage::Speakers => ui.stage_speakers(),
        Stage::Minutes => ui.stage_minutes(),
        Stage::Export => ui.stage_export(),
    }
}

fn day_text(ui: Ui, label: DayLabel) -> String {
    match label {
        DayLabel::Today => ui.today().into(),
        DayLabel::Yesterday => ui.yesterday().into(),
        DayLabel::Date {
            month,
            day,
            weekday,
        } => ui.day(month, day, weekday),
        DayLabel::Unknown => ui.undated().into(),
    }
}

/// `HH:MM` from a `yyyyMMddHHmmss` start.
fn clock_of(started: Option<&str>) -> String {
    started
        .and_then(|s| s.get(8..12))
        .map(|hm| format!("{}:{}", &hm[..2], &hm[2..]))
        .unwrap_or_default()
}

/// The centred, width-limited column every screen's content sits in.
pub(super) fn column() -> gpui::Div {
    div()
        .w_full()
        .max_w(px(COLUMN))
        .mx_auto()
        .px(px(28.))
        .py(px(18.))
        .flex()
        .flex_col()
        .gap_2()
}

impl MainWindow {
    /// Re-read the library and, in the background, how far each recording got.
    pub(super) fn refresh_recordings(&mut self, cx: &mut Context<Self>) {
        self.recordings = library::list(&crate::paths::recordings_dir());
        let dirs: Vec<PathBuf> = self
            .recordings
            .iter()
            .map(|r| PathBuf::from(&r.dir))
            .collect();
        let destinations = self.kkm.settings().destinations;
        cx.spawn(async move |this, cx| {
            let stages = cx
                .background_spawn(async move {
                    dirs.into_iter()
                        .map(|d| {
                            let stage = stage_of(&d, &destinations);
                            (d, stage)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.stages = stages;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn unfinished(&self) -> usize {
        self.stages.values().filter(|s| s.is_some()).count()
    }

    pub(super) fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let state = self.state.read(cx);
        let mut left = div().flex().items_center().gap_2().min_w_0();
        if state.is_listening() {
            let face = self.live_mode == Some(SessionMode::FaceToFace);
            left = left
                .child(div().size(px(8.)).rounded_full().bg(theme::danger()))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(if face {
                    ui.recording_face()
                } else {
                    ui.recording_meeting()
                }));
            if face {
                let saved = self.kkm.settings().save_face_to_face;
                left = left.child(
                    div()
                        .text_size(px(12.))
                        .text_color(if saved { theme::ok() } else { theme::warn() })
                        .child(if saved {
                            ui.face_saved()
                        } else {
                            ui.face_not_saved()
                        }),
                );
            }
            if self.auto_session {
                left = left.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::muted())
                        .child(ui.started_automatically()),
                );
            }
            if self.view != View::Live {
                left = left.child(
                    button("show-live", ui.show_live(), false)
                        .on_click(cx.listener(|this, _, _, cx| this.show(View::Live, cx))),
                );
            }
        } else if matches!(state.phase, Phase::Stopping) {
            left = left.child(div().text_color(theme::muted()).child(ui.finishing()));
        } else if let Some(m) = self.calendar.read(cx).next_meeting(chrono::Utc::now()) {
            let at = m
                .start
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            let title = m.title.clone().unwrap_or_else(|| ui.no_title().into());
            let url = m.join_url.clone();
            left = left
                .child(div().truncate().child(ui.next_meeting(&title, &at)))
                .child(
                    button("join-next", ui.join_and_record(), false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(url) = &url {
                                cx.open_url(url);
                            }
                            this.start(SessionMode::Meeting, cx);
                        },
                    )),
                );
        } else {
            left = left.child(div().text_color(theme::muted()).child(ui.idle()));
        }
        let unfinished = self.unfinished();
        let mut right = div().flex().items_center().gap_2().flex_none();
        if unfinished > 0 {
            right = right.child(
                div()
                    .id("unfinished")
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(theme::warn())
                    .child(ui.unfinished_count(unfinished))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.list_filter = ListFilter::Unfinished;
                        cx.notify();
                    })),
            );
        }
        right = right.child(
            button(
                "settings",
                ui.settings_button(),
                self.view == View::Settings,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                let next = if this.view == View::Settings {
                    View::Recording
                } else {
                    View::Settings
                };
                this.show(next, cx)
            })),
        );
        div()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(theme::border())
            .child(left)
            .child(right)
    }

    pub(super) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let state = self.state.read(cx);
        let listening = state.is_listening();
        let busy = matches!(state.phase, Phase::Stopping);

        let mut record = div().flex().gap_2();
        if listening {
            record = record.child(
                button("stop", ui.stop(), true)
                    .flex_1()
                    .on_click(cx.listener(|this, _, _, cx| this.stop_by_user(cx))),
            );
        } else if !busy {
            record = record
                .child(
                    button("meeting", ui.record_meeting(), false)
                        .flex_1()
                        .on_click(
                            cx.listener(|this, _, _, cx| this.start(SessionMode::Meeting, cx)),
                        ),
                )
                .child(button("face", ui.face_to_face(), false).on_click(
                    cx.listener(|this, _, _, cx| this.start(SessionMode::FaceToFace, cx)),
                ));
        }
        let filter = |id: &'static str, label: &'static str, f: ListFilter| {
            button(id, label, self.list_filter == f).on_click(cx.listener(move |this, _, _, cx| {
                this.list_filter = f;
                cx.notify();
            }))
        };
        let head = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme::border())
            .child(record)
            .child(self.search.clone())
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(filter("filter-all", ui.filter_all(), ListFilter::All))
                    .child(filter(
                        "filter-unfinished",
                        ui.filter_unfinished(),
                        ListFilter::Unfinished,
                    )),
            );

        div()
            .w(px(SIDEBAR))
            .flex_none()
            .flex()
            .flex_col()
            .min_h_0()
            .border_r_1()
            .border_color(theme::border())
            .child(head)
            .child(self.timeline_list(cx))
            .child(self.import_strip(cx))
    }

    fn timeline_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let query = self.search.read(cx).text().trim().to_lowercase();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let open = self.open_recording.as_ref().map(|(d, _)| d.clone());
        let mut list = div()
            .id("timeline")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .pb_2();

        if !query.is_empty() {
            list = list.child(self.search_hits(cx));
        }
        let recordings: Vec<Recording> = self
            .recordings
            .iter()
            .filter(|r| {
                (self.list_filter == ListFilter::All
                    || self
                        .stages
                        .get(Path::new(&r.dir))
                        .is_some_and(|s| s.is_some()))
                    && (query.is_empty()
                        || r.title
                            .as_deref()
                            .unwrap_or(&r.name)
                            .to_lowercase()
                            .contains(&query)
                        || r.snippet.to_lowercase().contains(&query))
            })
            .cloned()
            .collect();
        let days = timeline::group_by_day(&recordings);
        let limit = if self.show_older || !query.is_empty() || self.list_filter != ListFilter::All {
            days.len()
        } else {
            days.len().min(7)
        };
        for (d, day) in days.iter().take(limit).enumerate() {
            list = list.child(
                div()
                    .flex()
                    .justify_between()
                    .px_2()
                    .pt_2()
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .child(day_text(
                        ui,
                        timeline::day_label(day.date.as_deref(), &today),
                    ))
                    .child(day.recordings.len().to_string()),
            );
            for (i, r) in day.recordings.iter().enumerate() {
                let dir = PathBuf::from(&r.dir);
                let selected = self.view == View::Recording && open.as_ref() == Some(&dir);
                let stage = self.stages.get(&dir).copied().flatten();
                let mut row = div()
                    .id(("rec", d * 1000 + i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::subtle()))
                    .child(
                        div()
                            .w(px(34.))
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(theme::muted())
                            .child(clock_of(r.started.as_deref())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .child(r.title.clone().unwrap_or_else(|| r.name.clone())),
                    );
                if let Some(stage) = stage {
                    row = row.child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme::warn())
                            .child(stage_label(ui, stage)),
                    );
                }
                if selected {
                    row = row
                        .bg(theme::subtle())
                        .border_l_2()
                        .border_color(theme::fg());
                }
                let selector = format!("timeline-{}", r.name);
                list = list.child(row.debug_selector(move || selector.clone()).on_click(
                    cx.listener(move |this, _, _, cx| this.open_recording_dir(dir.clone(), cx)),
                ));
            }
        }
        if limit < days.len() {
            list = list.child(
                button("show-older", ui.show_older(), false)
                    .mt_2()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_older = true;
                        cx.notify();
                    })),
            );
        }
        if self.recordings.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_color(theme::muted())
                    .child(ui.no_recordings()),
            );
        }
        list
    }

    fn search_hits(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let mut col = div().flex().flex_col().child(
            div()
                .px_2()
                .pt_2()
                .text_size(px(11.5))
                .text_color(theme::muted())
                .child(ui.matches_in_speech()),
        );
        if self.hits.is_empty() {
            return col.child(
                div()
                    .px_2()
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(ui.no_matches()),
            );
        }
        for (i, hit) in self.hits.iter().take(20).enumerate() {
            let source = hit.source.clone();
            let at = hit.start_ms;
            col = col.child(
                div()
                    .id(("hit", i))
                    .flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::subtle()))
                    .child(
                        div()
                            .w(px(34.))
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(theme::muted())
                            .child(at.map(mm_ss).unwrap_or_default()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .child(hit.text.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if source.is_dir() {
                            this.open_recording_dir(source.clone(), cx);
                            if let Some(at) = at {
                                this.play_recording(at, cx);
                            }
                        } else {
                            cx.open_url(&format!("file://{}", source.display()));
                        }
                    })),
            );
        }
        col
    }

    /// Imports in progress, and the drop target that starts new ones.
    fn import_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let queue = &self.state.read(cx).imports;
        let active: Vec<_> = queue
            .jobs
            .iter()
            .filter(|j| !matches!(j.state, ImportState::Done | ImportState::Skipped))
            .collect();
        let mut col = div().flex().flex_col().gap_1().p_2();
        if !active.is_empty() {
            col = col.child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(ui.importing(active.len()))
                    .child(
                        button(
                            "import-details",
                            ui.import_details(),
                            self.view == View::Imports,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.show(View::Imports, cx))),
                    ),
            );
            for job in active.iter().take(4) {
                let name = job
                    .inputs
                    .first()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                col = col.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_size(px(12.))
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .child(
                            div()
                                .flex_none()
                                .text_color(match job.state {
                                    ImportState::Failed { .. } => theme::danger(),
                                    _ => theme::muted(),
                                })
                                .child(super::imports::state_text(ui, &job.state)),
                        ),
                );
            }
        }
        col.child(
            div()
                .id("drop-audio")
                .mt_1()
                .p_2()
                .rounded(px(6.))
                .border_1()
                .border_dashed()
                .border_color(theme::border())
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(theme::muted())
                .flex()
                .flex_col()
                .items_center()
                .child(ui.drop_audio())
                .child(div().text_size(px(11.)).child(ui.drop_audio_formats()))
                .drag_over::<ExternalPaths>(|style, _, _, _| {
                    style.bg(theme::subtle()).border_color(theme::fg())
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    if !paths.is_empty() {
                        this.kkm.import_audio(
                            paths,
                            ImportMode::Separate,
                            Box::new(crate::audio_import::NativeDecoder),
                        );
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(|this, _, _, cx| this.pick_import(false, cx))),
        )
    }

    pub(super) fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let state = self.state.read(cx);
        let settings = self.kkm.settings();
        let captions = crate::captions::is_open(cx);
        let listening = state.is_listening();
        let phase = match &state.phase {
            Phase::Idle => ui.idle().to_string(),
            Phase::Listening => ui.listening().to_string(),
            Phase::Stopping => ui.finishing().to_string(),
            Phase::Error { message } => ui.error(message),
        };
        let item = |id: &'static str, text: String| {
            div()
                .id(id)
                .cursor_pointer()
                .hover(|s| s.text_color(theme::fg()))
                .child(text)
        };
        div()
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::subtle())
            .text_size(px(11.5))
            .text_color(rgb(0x656d76))
            .child(
                div()
                    .text_color(if listening {
                        theme::danger()
                    } else {
                        rgb(0x656d76)
                    })
                    .child(phase),
            )
            .child(
                item(
                    "footer-face",
                    if settings.save_face_to_face {
                        ui.face_saved()
                    } else {
                        ui.face_not_saved()
                    }
                    .into(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(|s| s.save_face_to_face = !s.save_face_to_face, cx)
                })),
            )
            .child(
                item(
                    "footer-captions",
                    if captions {
                        ui.captions_shown()
                    } else {
                        ui.captions_hidden()
                    }
                    .into(),
                )
                .opacity(if listening { 1. } else { 0.5 })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_captions(cx))),
            )
            .child(
                item(
                    "footer-auto",
                    if settings.auto_record {
                        ui.auto_record_on()
                    } else {
                        ui.auto_record_off()
                    }
                    .into(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.update_settings(|s| s.auto_record = !s.auto_record, cx)
                })),
            )
            .child(div().flex_1())
            .child(ui.processed_locally())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_rows_show_the_start_time() {
        assert_eq!(clock_of(Some("20261117093000")), "09:30");
        assert_eq!(clock_of(Some("odd")), "");
        assert_eq!(clock_of(None), "");
    }

    #[test]
    fn day_headings_read_naturally_in_both_languages() {
        let d = DayLabel::Date {
            month: 11,
            day: 14,
            weekday: 6,
        };
        assert_eq!(day_text(Ui::Ja, d.clone()), "11/14（土）");
        assert_eq!(day_text(Ui::En, d), "Sat 11/14");
        assert_eq!(day_text(Ui::Ja, DayLabel::Today), "今日");
    }
}
