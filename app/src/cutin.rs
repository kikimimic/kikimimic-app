//! The card that slides in at the top centre shortly before a meeting.

use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{
    Context, Entity, FontWeight, IntoElement, Pixels, SharedString, Window, div, prelude::*, px,
    rgb, size,
};
use kikimimic_core::{Kikimimic, SessionMode};

use crate::calendar::{Calendar, Meeting};
use crate::i18n::Ui;
use crate::state::AppState;

pub const WIDTH: f32 = 440.;
const PAD: f32 = 12.;
const GAP: f32 = 8.;
const ROW: f32 = 76.;
const FOOTER: f32 = 30.;

pub fn height(rows: usize) -> Pixels {
    px(2. * PAD + rows as f32 * (ROW + GAP) + FOOTER)
}

pub struct Cutin {
    calendar: Entity<Calendar>,
    kkm: Kikimimic,
    state: Entity<AppState>,
    meetings: Vec<Meeting>,
    ui: Ui,
}

/// `m:ss`, for a countdown.
fn clock(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn countdown(ui: Ui, start: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (start - now).num_seconds();
    if seconds >= 0 {
        ui.starts_in(&clock(seconds))
    } else {
        ui.started_ago(&clock(-seconds))
    }
}

fn card_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(13.))
        .bg(if primary {
            rgb(0x2f81f7)
        } else {
            rgb(0x373e47)
        })
        .text_color(rgb(0xffffff))
        .hover(|s| s.opacity(0.85))
        .child(label.into())
}

impl Cutin {
    pub fn new(
        calendar: Entity<Calendar>,
        kkm: Kikimimic,
        state: Entity<AppState>,
        meetings: Vec<Meeting>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
        Self {
            calendar,
            kkm,
            state,
            meetings,
            ui: Ui::detect(),
        }
    }

    pub fn set_meetings(
        &mut self,
        meetings: Vec<Meeting>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.resize(size(px(WIDTH), height(meetings.len())));
        self.meetings = meetings;
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.meetings.iter().map(|m| m.key.clone()).collect();
        self.calendar.update(cx, |c, cx| c.dismiss(keys, cx));
        window.remove_window();
    }

    fn record(&mut self, join_url: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = join_url {
            cx.open_url(url);
        }
        self.kkm.start_session(SessionMode::Meeting);
        self.close(window, cx);
        let main = self.calendar.read(cx).main_window;
        cx.activate(true);
        if let Some(main) = main {
            let _ = main.update(cx, |_, window, _| window.activate_window());
        }
    }
}

impl Render for Cutin {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let now = Utc::now();
        let listening = self.state.read(cx).is_listening();
        let rows = self.meetings.iter().enumerate().map(|(i, m)| {
            let local = |t: DateTime<Utc>| t.with_timezone(&Local).format("%H:%M").to_string();
            let title = m.title.clone().unwrap_or_else(|| ui.no_title().into());
            let mut buttons = div().flex().gap_2();
            if !listening {
                if let Some(url) = m.join_url.clone() {
                    buttons = buttons.child(
                        card_button(("join", i), ui.join_and_record(), true).on_click(cx.listener(
                            move |this, _, window, cx| this.record(Some(&url), window, cx),
                        )),
                    );
                }
                buttons = buttons.child(
                    card_button(("record", i), ui.record(), m.join_url.is_none())
                        .on_click(cx.listener(|this, _, window, cx| this.record(None, window, cx))),
                );
            }
            div()
                .h(px(ROW))
                .flex()
                .flex_col()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0x9198a1))
                                .child(format!(
                                    "{}–{} · {}",
                                    local(m.start),
                                    local(m.end),
                                    countdown(ui, m.start, now)
                                )),
                        ),
                )
                .child(buttons)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .p(px(PAD))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(0x444c56))
            .bg(rgb(0x1f2328))
            .text_color(rgb(0xffffff))
            .text_size(px(14.))
            .children(rows)
            .child(
                div().h(px(FOOTER)).flex().justify_end().child(
                    card_button("dismiss", ui.dismiss(), false)
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_runs_down_to_the_start_and_then_up() {
        let start: DateTime<Utc> = "2026-11-06T01:00:00Z".parse().unwrap();
        let at = |s: i64| start + chrono::Duration::seconds(s);
        assert_eq!(countdown(Ui::En, start, at(-95)), "in 1:35");
        assert_eq!(countdown(Ui::En, start, at(40)), "started 0:40 ago");
        assert_eq!(countdown(Ui::Ja, start, at(-95)), "あと 1:35");
    }
}
