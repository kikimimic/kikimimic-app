//! One recording: where its post-processing stands, a player on top (a word
//! in the text plays from that word), and tabs for the transcript, minutes,
//! subtitles, export destinations and conversation statistics.

use std::path::{Path, PathBuf};

use kikimimic_core::destinations::{self, ExportState};
use kikimimic_core::minutes::{Kind, Minutes};
use kikimimic_core::timeline::Stage;

use super::*;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DetailTab {
    #[default]
    Text,
    Minutes,
    Subtitles,
    Export,
    Stats,
}

/// What the minutes and export tabs show, read once per recording (and
/// again after a write) rather than on every redraw — playback redraws the
/// window several times a second.
#[derive(Default)]
pub(super) struct DetailCache {
    pub dir: Option<PathBuf>,
    pub minutes: Option<Minutes>,
    /// One per destination, in settings order.
    pub exports: Vec<ExportRow>,
    pub selected: usize,
}

pub(super) struct ExportRow {
    pub state: ExportState,
    pub path: PathBuf,
    pub preview: String,
}

/// Read minutes and every destination's rendering for `dir`.
pub(super) fn read_cache(dir: &Path, dests: &[destinations::Destination]) -> DetailCache {
    let minutes = kikimimic_core::minutes::load(dir).ok().flatten();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let exports = match library::read(dir) {
        Ok(session) => dests
            .iter()
            .map(|d| ExportRow {
                state: destinations::state(d, &session, minutes.as_ref(), dir),
                path: destinations::path(d, &session, &home),
                preview: destinations::render(d, &session, minutes.as_ref()),
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    DetailCache {
        dir: Some(dir.to_path_buf()),
        minutes,
        exports,
        selected: 0,
    }
}

fn stage_steps(ui: Ui) -> [(Stage, &'static str); 4] {
    [
        (Stage::Transcribe, ui.step_transcript()),
        (Stage::Speakers, ui.step_speakers()),
        (Stage::Minutes, ui.step_minutes()),
        (Stage::Export, ui.step_export()),
    ]
}

impl MainWindow {
    /// Reload what the minutes and export tabs show, off the UI thread.
    pub(super) fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let Some((dir, _)) = &self.open_recording else {
            return;
        };
        let dir = dir.clone();
        let dests = self.kkm.settings().destinations;
        let selected = self.detail.selected;
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let cache = cx
                .background_spawn(async move { read_cache(&d, &dests) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .open_recording
                    .as_ref()
                    .is_some_and(|(open, _)| open == &dir)
                {
                    this.detail = DetailCache {
                        selected: selected.min(cache.exports.len().saturating_sub(1)),
                        ..cache
                    };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        self.detail_tab = tab;
        // Subtitle boundaries are edited in the same rows the transcript shows.
        self.words.editing = tab == DetailTab::Subtitles && self.words.data.is_some();
        self.sync_word_editor(cx);
        if matches!(tab, DetailTab::Export | DetailTab::Minutes) {
            self.refresh_detail(cx);
        }
        cx.notify();
    }

    /// Write every destination that writes by itself, then re-read states.
    fn export_pending(&mut self, cx: &mut Context<Self>) {
        let Some((dir, _)) = &self.open_recording else {
            return;
        };
        let dir = dir.clone();
        let dests: Vec<_> = self
            .kkm
            .settings()
            .destinations
            .into_iter()
            .filter(|d| d.enabled && d.when != destinations::When::Manual)
            .collect();
        self.write_destinations(dir, dests, cx);
    }

    /// Write the destinations waiting for `when`, off the UI thread.
    pub(super) fn auto_export(
        &mut self,
        dir: PathBuf,
        when: destinations::When,
        cx: &mut Context<Self>,
    ) {
        let dests = destinations::triggered(&self.kkm.settings().destinations, when);
        if dests.is_empty() {
            return;
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let session = library::read(&dir)?;
                    let minutes = kikimimic_core::minutes::load(&dir)?;
                    for d in &dests {
                        destinations::write(d, &session, minutes.as_ref(), &dir, &home)?;
                    }
                    anyhow::Ok(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(format!("{e:#}"));
                }
                this.refresh_detail(cx);
                this.refresh_recordings(cx);
            });
        })
        .detach();
    }

    pub(super) fn write_destinations(
        &mut self,
        dir: PathBuf,
        dests: Vec<destinations::Destination>,
        cx: &mut Context<Self>,
    ) {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let result = library::read(&dir).and_then(|session| {
            let minutes = kikimimic_core::minutes::load(&dir)?;
            for d in &dests {
                destinations::write(d, &session, minutes.as_ref(), &dir, &home)?;
            }
            Ok(())
        });
        self.error = result.err().map(|e| format!("{e:#}"));
        self.refresh_detail(cx);
        self.refresh_recordings(cx);
    }

    pub(super) fn recording_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let Some((dir, session)) = &self.open_recording else {
            return div()
                .size_full()
                .child(
                    super::shell::column()
                        .child(div().pt_6().text_color(gray()).child(ui.pick_a_recording())),
                )
                .into_any_element();
        };
        let dir = dir.clone();
        // Called the same as in the timeline, so the two read as one thing.
        let title = self
            .recordings
            .iter()
            .find(|r| Path::new(&r.dir) == dir)
            .map(|r| super::shell::row_title(ui, r))
            .unwrap_or_else(|| session.name.clone());
        let when = session
            .started
            .as_deref()
            .filter(|s| s.len() >= 12)
            .map(|s| {
                format!(
                    "{}-{}-{} {}:{}",
                    &s[..4],
                    &s[4..6],
                    &s[6..8],
                    &s[8..10],
                    &s[10..12]
                )
            })
            .unwrap_or_default();
        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        button("ask-this", ui.ask_this_meeting(), self.ask_open)
                            .flex_none()
                            .debug_selector(|| "ask-this".into())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_ask(cx))),
                    ),
            )
            .child(self.pipeline(&dir, format!("{when} · {}", mm_ss(session.duration_ms)), cx))
            .child(self.player(session.duration_ms, cx))
            .child(self.detail_tabs(cx));
        let body: AnyElement = match self.detail_tab {
            DetailTab::Text => self.text_tab(&dir, cx),
            DetailTab::Subtitles => self.subtitles_tab(&dir, cx),
            DetailTab::Minutes => self.minutes_tab(session, cx),
            DetailTab::Export => self.export_tab(&dir, cx),
            DetailTab::Stats => stats_tab(ui, session),
        };
        div()
            .id("recording")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(super::shell::column().pb_3().child(header)),
            )
            .child(super::shell::column().child(body))
            .into_any_element()
    }

    /// The four post-processing steps, the current one highlighted, and the
    /// button that does it.
    fn pipeline(&self, dir: &Path, lead: String, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let stage = self.stages.get(dir).copied().flatten();
        let diarizing = matches!(&self.state.read(cx).post_process, Some((d, PostProcessState::Running)) if d == dir);
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(theme::muted())
            .child(div().mr_2().child(lead));
        for (i, (s, label)) in stage_steps(ui).into_iter().enumerate() {
            if i > 0 {
                row = row.child("›");
            }
            let done = stage.is_none_or(|current| s < current);
            let current = stage == Some(s);
            row = row.child(
                div()
                    .text_color(if current {
                        theme::warn()
                    } else if done {
                        theme::fg()
                    } else {
                        theme::muted()
                    })
                    .when(current, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(if done {
                        format!("✓ {label}")
                    } else {
                        label.to_string()
                    }),
            );
        }
        row = row.child(div().flex_1());
        let action = match stage {
            _ if diarizing => Some(div().child(ui.identifying()).into_any_element()),
            Some(Stage::Speakers) => {
                let d = dir.to_path_buf();
                Some(
                    small_button("step-speakers", ui.identify_speakers(), true)
                        .on_click(cx.listener(move |this, _, _, _| this.kkm.diarize(d.clone())))
                        .into_any_element(),
                )
            }
            Some(Stage::Export) => Some(
                small_button("step-export", ui.write_pending(), true)
                    .on_click(cx.listener(|this, _, _, cx| this.export_pending(cx)))
                    .into_any_element(),
            ),
            Some(Stage::Minutes) => Some(div().child(ui.minutes_not_made()).into_any_element()),
            Some(Stage::Transcribe) => {
                Some(div().child(ui.not_transcribed_yet()).into_any_element())
            }
            None => Some(
                div()
                    .text_color(theme::ok())
                    .child(ui.all_done())
                    .into_any_element(),
            ),
        };
        if let Some(action) = action {
            row = row.child(action);
        }
        if let Some((d, PostProcessState::Failed { message })) = &self.state.read(cx).post_process
            && d == dir
        {
            row = row.child(div().text_color(theme::danger()).child(message.clone()));
        }
        row
    }

    /// Play / stop and where playback is. Clicking a word in the transcript
    /// plays from that word.
    fn player(&self, duration_ms: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let playing = self.words.playback.as_ref().is_some_and(|p| p.playing);
        let position = self.words.playback.as_ref().map_or(0, |p| p.position_ms);
        let fraction = if duration_ms == 0 {
            0.
        } else {
            (position as f32 / duration_ms as f32).clamp(0., 1.)
        };
        div()
            .debug_selector(|| "detail-player".into())
            .flex()
            .items_center()
            .gap_3()
            .child(if playing {
                button("player", ui.pause(), true)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stop_playback();
                        cx.notify();
                    }))
                    .into_any_element()
            } else {
                button("player", ui.play(), false)
                    .on_click(cx.listener(move |this, _, _, cx| this.play_recording(position, cx)))
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .h(px(6.))
                    .rounded(px(3.))
                    .bg(theme::subtle())
                    .child(
                        div()
                            .h_full()
                            .rounded(px(3.))
                            .bg(theme::fg())
                            .w(gpui::relative(fraction)),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(format!("{} / {}", mm_ss(position), mm_ss(duration_ms))),
            )
    }

    fn detail_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let mut tabs = Vec::new();
        for (tab, id, label) in [
            (DetailTab::Text, "tab-text", ui.tab_text()),
            (DetailTab::Minutes, "tab-minutes", ui.step_minutes()),
            (DetailTab::Subtitles, "tab-subtitles", ui.tab_subtitles()),
            (DetailTab::Export, "tab-export", ui.step_export()),
            (DetailTab::Stats, "tab-stats", ui.tab_stats()),
        ] {
            tabs.push(
                button(id, label, self.detail_tab == tab)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_detail_tab(tab, cx))),
            );
        }
        div().flex().child(segmented(tabs))
    }

    fn text_tab(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().flex().flex_col().gap_2();
        if self.words.data.is_none() {
            col = col.child(self.word_maker(dir.to_path_buf(), cx));
        }
        if let Some(editor) = &self.speaker_editor {
            col = col.child(editor.clone());
        }
        col.into_any_element()
    }

    fn subtitles_tab(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.word_toolbar(dir.to_path_buf(), cx));
        if let Some(strip) = self.subtitle_timeline(cx) {
            col = col.child(strip);
        }
        if let Some(editor) = &self.speaker_editor {
            col = col.child(editor.clone());
        }
        col.into_any_element()
    }

    fn minutes_tab(&self, session: &library::Session, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let dir = PathBuf::from(&session.dir);
        let making = self.ask.making_minutes.as_deref() == Some(dir.as_path());
        let mut col = div().flex().flex_col().gap_3().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().text_color(theme::muted()).child(
                    if self.detail.minutes.is_some() {
                        ui.minutes_from_transcript()
                    } else {
                        ui.minutes_not_made()
                    },
                ))
                .child(if making {
                    div()
                        .text_color(theme::muted())
                        .child(ui.making_minutes())
                        .into_any_element()
                } else {
                    let d = dir.clone();
                    button(
                        "make-minutes",
                        if self.detail.minutes.is_some() {
                            ui.remake_minutes()
                        } else {
                            ui.make_minutes()
                        },
                        self.detail.minutes.is_none(),
                    )
                    .debug_selector(|| "make-minutes".into())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.request_minutes(d.clone(), cx)),
                    )
                    .into_any_element()
                }),
        );
        if let Some(row) = self.consent_row_for_minutes(cx) {
            col = col.child(row);
        }
        let Some(minutes) = &self.detail.minutes else {
            return col.into_any_element();
        };
        for (kind, heading) in [
            (Kind::Summary, ui.minutes_summary()),
            (Kind::Decision, ui.minutes_decisions()),
            (Kind::Action, ui.minutes_actions()),
            (Kind::OpenQuestion, ui.minutes_open()),
        ] {
            let items: Vec<_> = minutes.items.iter().filter(|i| i.kind == kind).collect();
            let mut section = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(heading));
            if items.is_empty() {
                section = section.child(div().text_color(theme::muted()).child(ui.none()));
            }
            for (n, item) in items.into_iter().enumerate() {
                let mut row = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(item.text.clone());
                for (e, id) in item.evidence.iter().enumerate() {
                    let Some(line) = session.lines.iter().find(|l| l.id == *id) else {
                        continue;
                    };
                    let at = line.start_ms;
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("ev-{kind:?}-{n}-{e}")))
                            .text_size(px(12.))
                            .text_color(rgb(0x0969da))
                            .cursor_pointer()
                            .child(mm_ss(at))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.play_recording(at, cx)),
                            ),
                    );
                }
                section = section.child(row);
            }
            col = col.child(section);
        }
        col.into_any_element()
    }

    fn export_tab(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let dests = self.kkm.settings().destinations;
        if self.detail.dir.as_deref() != Some(dir) {
            return div()
                .text_color(theme::muted())
                .child(ui.loading())
                .into_any_element();
        }
        let mut list = div().flex().flex_col().gap_1().w(px(300.)).flex_none();
        for (i, (d, row)) in dests.iter().zip(&self.detail.exports).enumerate() {
            let (label, color) = match row.state {
                ExportState::Fresh => (ui.export_fresh(), theme::ok()),
                ExportState::Stale => (ui.export_stale(), theme::warn()),
                ExportState::NotWritten => (ui.export_not_written(), theme::muted()),
            };
            let selected = self.detail.selected == i;
            list = list.child(
                div()
                    .id(("dest", i))
                    .flex()
                    .flex_col()
                    .p_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::subtle()))
                    .when(selected, |s| s.bg(theme::subtle()))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(d.name.clone()),
                            )
                            .child(div().text_size(px(12.)).text_color(color).child(label)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::muted())
                            .truncate()
                            .child(row.path.display().to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.detail.selected = i;
                        cx.notify();
                    })),
            );
        }
        list = list.child(
            button("edit-destinations", ui.edit_destinations(), false)
                .mt_2()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.section = super::settings::Section::Export;
                    this.show(View::Settings, cx);
                })),
        );
        let Some(row) = self.detail.exports.get(self.detail.selected) else {
            return list.into_any_element();
        };
        let dest = dests.get(self.detail.selected).cloned();
        let dir = dir.to_path_buf();
        let preview = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .p_3()
                    .rounded(px(6.))
                    .bg(theme::subtle())
                    .text_size(px(12.))
                    .font_family("Menlo")
                    .child(row.preview.clone()),
            )
            .child(
                div().flex().justify_end().child(
                    button(
                        "write-dest",
                        if row.state == ExportState::NotWritten {
                            ui.write()
                        } else {
                            ui.rewrite()
                        },
                        true,
                    )
                    .debug_selector(|| "write-dest".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = dest.clone() {
                            this.write_destinations(dir.clone(), vec![d], cx);
                        }
                    })),
                ),
            );
        div()
            .flex()
            .gap_3()
            .child(list)
            .child(preview)
            .into_any_element()
    }
}

fn stats_tab(ui: Ui, session: &library::Session) -> AnyElement {
    let m = kikimimic_core::metrics::calculate(&session.lines);
    let mut col = div().flex().flex_col().gap_3();
    for (name, lane) in [(ui.lane_me(), &m.mic), (ui.lane_them(), &m.speaker)] {
        col =
            col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(
                                div()
                                    .text_color(theme::muted())
                                    .child(ui.speech_share(lane.share * 100.)),
                            ),
                    )
                    .child(
                        div().h(px(6.)).rounded(px(3.)).bg(theme::subtle()).child(
                            div()
                                .h_full()
                                .rounded(px(3.))
                                .bg(theme::fg())
                                .w(gpui::relative(lane.share as f32)),
                        ),
                    )
                    .child(div().text_size(px(12.)).text_color(theme::muted()).child(
                        ui.lane_metrics(
                            lane.longest_ms as f64 / 1000.,
                            lane.characters_per_minute,
                            lane.questions,
                        ),
                    )),
            );
    }
    col.child(
        div()
            .text_size(px(12.))
            .text_color(theme::muted())
            .child(ui.overlaps(m.overlaps)),
    )
    .into_any_element()
}
