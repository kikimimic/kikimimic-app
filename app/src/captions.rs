//! Independent subtitle window. Capture exclusion is deliberately not claimed.
use crate::state::{AppState, Translation};
use gpui::{prelude::*, *};

pub struct Captions {
    state: Entity<AppState>,
    font: f32,
    rows: usize,
    mode: usize,
    hidden: bool,
}
pub fn open(state: Entity<AppState>, cx: &mut App) -> anyhow::Result<()> {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(560.), px(260.)),
                cx,
            ))),
            focus: false,
            kind: WindowKind::Floating,
            ..Default::default()
        },
        |_, cx| {
            cx.new(|cx| {
                cx.observe(&state, |_, _, cx| cx.notify()).detach();
                Captions {
                    state,
                    font: 22.,
                    rows: 2,
                    mode: 0,
                    hidden: false,
                }
            })
        },
    )?;
    Ok(())
}
impl Render for Captions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .size_full()
            .p_2()
            .bg(rgb(0x17191c))
            .text_color(rgb(0xffffff))
            .flex()
            .flex_col()
            .gap_2();
        let mut controls = div().flex().gap_2().text_size(px(12.));
        for (id, label) in [
            (0, "原文/訳文"),
            (1, "文字サイズ"),
            (2, "表示件数"),
            (3, "隠す/再表示"),
        ] {
            controls = controls.child(div().id(id).cursor_pointer().child(label).on_click(
                cx.listener(move |this, _, _, cx| {
                    match id {
                        0 => this.mode = (this.mode + 1) % 3,
                        1 => {
                            this.font = if this.font >= 30. {
                                18.
                            } else {
                                this.font + 4.
                            }
                        }
                        2 => this.rows = this.rows % 3 + 1,
                        _ => this.hidden = !this.hidden,
                    }
                    cx.notify();
                }),
            ));
        }
        root = root.child(controls).child(
            div()
                .text_size(px(11.))
                .child("共有・録画に映る可能性があります。閉じて字幕ボタンで再表示。"),
        );
        if !self.hidden {
            let state = self.state.read(cx);
            let mut content = div()
                .id("caption-content")
                .flex_1()
                .overflow_y_scroll()
                .text_size(px(self.font));
            for line in state
                .lines
                .iter()
                .skip(state.lines.len().saturating_sub(self.rows))
            {
                if self.mode != 2 {
                    content =
                        content.child(div().child(format!("{}{}", line.committed, line.volatile)));
                }
                if self.mode != 1 {
                    let text = match &line.translation {
                        Translation::Text(t) => t.as_str(),
                        Translation::Pending => "翻訳中…",
                        Translation::Failed(_) => "翻訳失敗",
                        _ => "",
                    };
                    content = content.child(div().text_color(rgb(0x9fceff)).child(text.to_owned()));
                }
            }
            root = root.child(content);
        }
        root
    }
}
