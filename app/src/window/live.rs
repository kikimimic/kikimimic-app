//! The live session: controls, notices and the growing transcript.

use super::*;

impl MainWindow {
    pub(super) fn session(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let captions_open = crate::captions::is_open(cx);
        controls = controls.child(div().flex_1()).child(
            button(
                "captions-switch",
                if captions_open {
                    "字幕  ● オン"
                } else {
                    "字幕  ○ オフ"
                },
                captions_open,
            )
            .opacity(if listening { 1. } else { 0.45 })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.state.read(cx).is_listening() {
                    if let Err(error) = crate::captions::toggle(this.state.clone(), cx) {
                        this.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            })),
        );
        col = col.child(
            div()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(0xd0d7de))
                .child(controls),
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
}
