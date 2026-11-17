//! Floating subtitles with a compact, live preview. Capture exclusion is not claimed.
use crate::state::{AppState, Translation};
use gpui::{prelude::*, *};

pub struct Captions {
    state: Entity<AppState>,
    font: f32,
    scroll: ScrollHandle,
    following: bool,
    animating: bool,
    mode: usize,
    compact: bool,
    hovered: bool,
    settings: bool,
}
pub fn is_open(cx: &App) -> bool {
    cx.windows()
        .iter()
        .any(|w| w.downcast::<Captions>().is_some())
}
pub fn toggle(state: Entity<AppState>, cx: &mut App) -> anyhow::Result<()> {
    for handle in cx.windows() {
        if let Some(handle) = handle.downcast::<Captions>() {
            handle.update(cx, |_, window, _| window.remove_window())?;
            return Ok(());
        }
    }
    open(state, cx)
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
                cx.observe_in(&state, window, |this: &mut Captions, state, window, cx| {
                    if !state.read(cx).is_listening() {
                        window.remove_window();
                    } else {
                        if this.following {
                            this.follow_latest(window, cx);
                        } else {
                            let anchor = this.scroll.logical_scroll_top();
                            let scroll = this.scroll.clone();
                            window.on_next_frame(move |_, _| {
                                let current = scroll.logical_scroll_top();
                                if let (Some(old), Some(new)) = (
                                    scroll.bounds_for_item(anchor.0),
                                    scroll.bounds_for_item(current.0),
                                ) {
                                    let mut offset = scroll.offset();
                                    offset.y += new.top() - old.top() + anchor.1 - current.1;
                                    scroll.set_offset(offset);
                                }
                            });
                        }
                        cx.notify();
                    }
                })
                .detach();
                cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                    .detach();
                Captions {
                    state,
                    font: 22.,
                    scroll: ScrollHandle::new(),
                    following: true,
                    animating: false,
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
    fn follow_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.reduce_motion() {
            self.scroll.scroll_to_bottom();
            cx.notify();
            return;
        }
        if self.animating {
            return;
        }
        self.animating = true;
        let view = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.animating = false;
                if !this.following || this.compact {
                    return;
                }
                if cx.reduce_motion() {
                    this.scroll.scroll_to_bottom();
                    cx.notify();
                    return;
                }
                let mut offset = this.scroll.offset();
                let remaining = -this.scroll.max_offset().y - offset.y;
                if remaining.abs() > px(0.5) {
                    offset.y += remaining * 0.35;
                    this.scroll.set_offset(offset);
                    this.follow_latest(window, cx);
                } else {
                    this.scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        });
    }

    fn at_latest(&self) -> bool {
        self.scroll.offset().y + self.scroll.max_offset().y <= px(28.)
    }

    fn toggle_compact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.compact = !self.compact;
        self.settings = false;
        window.resize(if self.compact {
            size(px(320.), px(64.))
        } else {
            size(px(560.), px(260.))
        });
        if !self.compact && self.following {
            self.follow_latest(window, cx);
        }
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
                            _ => {}
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
            .track_scroll(&self.scroll)
            .on_scroll(cx.listener(|this, _, window, cx| {
                this.following = false;
                let view = cx.entity().downgrade();
                window.on_next_frame(move |_, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.following = this.at_latest();
                        cx.notify();
                    });
                });
            }))
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
        for line in &state.lines {
            let mine = line.lane.as_str() == "mic" || line.lane.as_str().starts_with("mic-");
            let mut utterance = div()
                .id(("utterance", line.id.0))
                .when(mine, |d| d.border_dashed())
                .border_l_2()
                .border_color(if mine { secondary } else { translation })
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(secondary)
                        .child(if mine {
                            "自分 · マイク"
                        } else {
                            "相手 · PC音声"
                        }),
                )
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
        root = root.child(content);
        if !self.following {
            root = root.child(
                div()
                    .id("caption-latest")
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(translation)
                    .child("↓ 最新へ")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.following = true;
                        this.follow_latest(window, cx);
                        cx.notify();
                    })),
            );
        }
        root
    }
}
