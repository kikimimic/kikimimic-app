//! The live session as one stream: notices, then what is being said,
//! each line marked by who said it.

use super::*;

impl MainWindow {
    pub(super) fn session(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let state = self.state.read(cx);
        let mut col = super::shell::column().size_full().on_mouse_down(
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
        // Start/stop live in the timeline and captions in the status line;
        // here only the title row and Clear, which belongs to this screen.
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(ui.live_title()),
                )
                .child(div().text_color(gray()).child(match &state.phase {
                    Phase::Idle => ui.idle().to_string(),
                    Phase::Listening => ui.listening().to_string(),
                    Phase::Stopping => ui.finishing().to_string(),
                    Phase::Error { message } => ui.error(message),
                }))
                .child(button("clear-live", ui.clear_transcript(), false).on_click(
                    cx.listener(|this, _, _, cx| this.state.update(cx, |s, _| s.lines.clear())),
                )),
        );
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
            let mine = l.lane.as_str() == "mic";
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .py_1()
                .pl_3()
                .border_l_2()
                .border_color(if mine { rgb(0x0969da) } else { rgb(0x8250df) });
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
                    .child(if mine { ui.lane_me() } else { ui.lane_them() }),
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
        let ask = state.recording_dir.clone().filter(|_| state.is_listening());
        let col = col.child(
            div()
                .id("transcript")
                .flex()
                .flex_col()
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(lines),
        );
        match ask {
            Some(dir) => col
                .child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(rgb(0xd0d7de))
                        .child(self.ask_panel(&dir, cx)),
                )
                .into_any_element(),
            None => col.into_any_element(),
        }
    }
}
