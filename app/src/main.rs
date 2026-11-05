mod widgets;

use std::time::Duration;

use gpui::{
    App, Bounds, Context, Entity, Focusable, Window, WindowBounds, WindowOptions, div, hsla,
    prelude::*, px, rgb, size,
};
use widgets::{text_input, text_input::TextInput, transcript_line};

/// Step 0.5 check screen (plan.md): an IME field and a fake partial stream.
/// Replaced by the real session view once the core emits events (Step 6).
struct MainWindow {
    glossary: Entity<TextInput>,
    lines: Vec<(String, String)>,
}

/// Fake recognizer output: each sentence grows a character at a time as
/// volatile text and commits in chunks, like LocalAgreement does.
const SCRIPT: &[&str] = &[
    "今日は新しい検索機能の進捗を共有します。",
    "The implementation was completed on schedule.",
    "来週からはパフォーマンスの改善に取り組む予定です。",
];

impl MainWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        let glossary = cx.new(|cx| TextInput::new("誤認識 → 正しい表記（IME の確認用）", cx));
        cx.spawn(async move |this, cx| {
            for sentence in SCRIPT.iter().cycle() {
                let chars: Vec<char> = sentence.chars().collect();
                let ok = this.update(cx, |w, cx| {
                    w.lines.push((String::new(), String::new()));
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
                for (i, ch) in chars.iter().enumerate() {
                    cx.background_executor()
                        .timer(Duration::from_millis(120))
                        .await;
                    let commit = (i + 1) % 6 == 0 || i + 1 == chars.len();
                    let ok = this.update(cx, |w, cx| {
                        let line = w.lines.last_mut().expect("pushed above");
                        line.1.push(*ch);
                        if commit {
                            line.0 += &std::mem::take(&mut line.1);
                        }
                        cx.notify();
                    });
                    if ok.is_err() {
                        return;
                    }
                }
            }
        })
        .detach();
        Self {
            glossary,
            lines: Vec::new(),
        }
    }
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let gray = hsla(0., 0., 0.55, 1.);
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x1f2328))
            .text_size(px(15.))
            .child(self.glossary.clone())
            .child(
                div().flex().flex_col().gap_2().children(
                    self.lines
                        .iter()
                        .rev()
                        .take(12)
                        .rev()
                        .map(|(committed, volatile)| {
                            transcript_line::render(committed, volatile, gray)
                        }),
                ),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(MainWindow::new),
            )
            .expect("open the main window");
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.glossary.focus_handle(cx), cx)
            })
            .expect("focus the input");
        cx.activate(true);
    });
}
