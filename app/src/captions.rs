//! Floating subtitles with a compact, live preview. Capture exclusion is not claimed.
use crate::state::{AppState, Translation};
use gpui::{prelude::*, *};

pub struct Captions {
    state: Entity<AppState>,
    font: f32,
    rows: usize,
    mode: usize,
    compact: bool,
    hovered: bool,
    settings: bool,
}
pub fn open(state: Entity<AppState>, cx: &mut App) -> anyhow::Result<()> {
    if !state.read(cx).is_listening() {
        return Ok(());
    }
    for handle in cx.windows() {
        if let Some(handle) = handle.downcast::<Captions>() {
            handle.update(cx, |_, window, _| window.activate_window())?;
            return Ok(());
        }
    }
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(560.), px(260.)),
                cx,
            ))),
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            focus: false,
            kind: WindowKind::Floating,
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                cx.observe_in(&state, window, |_, state, window, cx| {
                    if !state.read(cx).is_listening() {
                        window.remove_window();
                    } else {
                        cx.notify();
                    }
                })
                .detach();
                cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                    .detach();
                Captions {
                    state,
                    font: 22.,
                    rows: 2,
                    mode: 0,
                    compact: false,
                    hovered: false,
                    settings: false,
                }
            })
        },
    )?;
    Ok(())
}
impl Captions {
    fn toggle_compact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.compact = !self.compact;
        self.settings = false;
        window.resize(if self.compact {
            size(px(320.), px(64.))
        } else {
            size(px(560.), px(260.))
        });
        cx.notify();
    }
}
impl Render for Captions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let surface = if dark { rgb(0x202124) } else { rgb(0xf5f5f7) };
        let panel_bg = if dark { rgb(0x2c2c2e) } else { rgb(0xe8e8ed) };
        let foreground = if dark { rgb(0xf5f5f7) } else { rgb(0x1d1d1f) };
        let secondary = if dark { rgb(0xaeb1b8) } else { rgb(0x62656c) };
        let translation = if dark { rgb(0xb8d8ff) } else { rgb(0x0969da) };
        let mut root = div()
            .id("captions")
            .size_full()
            .p_2()
            .bg(surface)
            .text_color(foreground)
            .flex()
            .flex_col()
            .gap_2()
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                this.hovered = *over;
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.settings = !this.settings;
                    cx.notify();
                }),
            );
        if self.compact {
            let state = self.state.read(cx);
            let latest = state
                .lines
                .last()
                .map(|line| {
                    if self.mode != 1
                        && let Translation::Text(text) = &line.translation
                    {
                        return text.clone();
                    }
                    format!("{}{}", line.committed, line.volatile)
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "字幕を待っています…".into());
            return root.child(
                div()
                    .id("restore-captions")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(18.))
                    .bg(panel_bg)
                    .border_1()
                    .border_color(rgba(0xffffff24))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_compact(window, cx)))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_size(px(14.))
                            .child(latest),
                    )
                    .child(div().text_size(px(12.)).child("↗")),
            );
        }
        let visible = self.hovered || self.settings;
        root = root.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .h(px(26.))
                .child(
                    div()
                        .id("move-captions")
                        .flex_1()
                        .h_full()
                        .cursor_move()
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                        .when(visible, |d| {
                            d.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(secondary)
                                    .child("字幕 · ドラッグで移動"),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .opacity(if visible { 1. } else { 0. })
                        .child(
                            div()
                                .id("caption-settings")
                                .px_2()
                                .rounded_md()
                                .bg(panel_bg)
                                .cursor_pointer()
                                .child("…")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.settings = !this.settings;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("caption-minimize")
                                .px_2()
                                .rounded_md()
                                .bg(panel_bg)
                                .cursor_pointer()
                                .child("−")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_compact(window, cx)
                                })),
                        ),
                ),
        );
        if self.settings {
            let mut panel = div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .bg(panel_bg)
                .text_size(px(12.));
            for (id, label) in [
                (
                    0,
                    format!("表示: {}", ["原文と訳文", "原文", "訳文"][self.mode]),
                ),
                (1, format!("文字サイズ: {} pt", self.font)),
                (2, format!("表示件数: {} 件", self.rows)),
            ] {
                panel = panel.child(div().id(id).cursor_pointer().child(label).on_click(
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
                            _ => this.rows = this.rows % 3 + 1,
                        }
                        cx.notify();
                    }),
                ));
            }
            root = root.child(
                panel
                    .child(
                        div()
                            .id("close-captions")
                            .cursor_pointer()
                            .child("字幕を閉じる")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(secondary)
                            .child("共有・録画に映る可能性があります。"),
                    ),
            );
        }
        let state = self.state.read(cx);
        let mut content = div()
            .id("caption-content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(self.font));
        if state.lines.is_empty() {
            content = content.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(surface)
                    .child("字幕を待っています…"),
            );
        }
        for line in state
            .lines
            .iter()
            .skip(state.lines.len().saturating_sub(self.rows))
        {
            let mut utterance = div()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(surface);
            if self.mode != 2 {
                utterance =
                    utterance.child(div().child(format!("{}{}", line.committed, line.volatile)));
            }
            if self.mode != 1 {
                let text = match &line.translation {
                    Translation::Text(t) => t.as_str(),
                    Translation::Pending => "翻訳中…",
                    Translation::Failed(_) => "翻訳失敗",
                    _ => "",
                };
                if !text.is_empty() {
                    utterance =
                        utterance.child(div().text_color(translation).child(text.to_owned()));
                }
            }
            content = content.child(utterance);
        }
        root.child(content)
    }
}
