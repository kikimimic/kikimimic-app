//! Recording word review and independent subtitle boundaries.
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, rgb};
use kikimimic_core::event::PostProcessState;
use kikimimic_core::library;
use kikimimic_core::word_timing::{self, WordTiming};

use super::{MainWindow, button, gray, mm_ss};

#[derive(Default)]
pub(crate) struct WordsView {
    pub data: Option<Arc<WordTiming>>,
    pub error: Option<String>,
    pub pending: bool,
    pub loading: bool,
    pub saving: bool,
    load_generation: u64,
    pub revision: u64,
    pub editing: bool,
    pub playback: Option<Playback>,
}

#[derive(Clone, serde::Deserialize)]
pub(crate) struct Playback {
    pub position_ms: u64,
    pub playing: bool,
    pub lane: String,
}

impl Playback {
    pub(crate) fn active(&self, lane: &str, word: &word_timing::Word) -> bool {
        self.playing
            && self.lane == lane
            && word
                .timing
                .is_some_and(|t| self.position_ms >= t.start_ms && self.position_ms < t.end_ms)
    }
}

impl MainWindow {
    pub(super) fn load_words(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.words.data = None;
        self.words.error = None;
        self.words.pending = false;
        self.words.loading = true;
        self.words.load_generation = self.words.load_generation.wrapping_add(1);
        let generation = self.words.load_generation;
        cx.spawn(async move |this, cx| {
            let path = dir.clone();
            let result = cx
                .background_spawn(async move { word_timing::load(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.words.load_generation != generation
                    || !this
                        .open_recording
                        .as_ref()
                        .is_some_and(|(path, _)| path == &dir)
                {
                    return;
                }
                this.words.loading = false;
                match result {
                    Ok(data) => this.words.data = data.map(Arc::new),
                    Err(e) => this.words.error = Some(format!("{e:#}")),
                }
                this.sync_word_editor(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn sync_word_editor(&self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.speaker_editor {
            let data = self.words.data.clone();
            let editing = self.words.editing;
            let playback = self.words.playback.clone();
            editor.update(cx, |editor, cx| {
                editor.set_word_timing(data, editing, cx);
                editor.set_word_playback(playback, cx);
            });
        }
    }

    pub(super) fn word_toolbar(&self, dir: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let running = self.words.pending
            || self.words.loading
            || self.words.saving
            || matches!(&self.state.read(cx).word_timing,
            Some((path, PostProcessState::Running)) if path == &dir);
        let mut col = div().flex().flex_col().gap_2();
        let mut bar = div().flex().gap_2().items_center();
        if running {
            bar = bar.child(div().text_color(gray()).child(if self.words.loading {
                "単語時刻を読み込み中…"
            } else if self.words.saving {
                "字幕を保存中…"
            } else {
                "単語時刻を生成中…"
            }));
        } else {
            let path = dir.clone();
            bar = bar.child(
                button("generate-words", "単語時刻を生成 / 再生成", false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.words.pending = true;
                        this.words.error = None;
                        this.kkm.generate_word_timing(path.clone());
                        cx.notify();
                    },
                )),
            );
        }
        if self.words.data.is_some() {
            bar = bar.child(
                button("edit-word-cues", "字幕の分割 / 結合", self.words.editing).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.words.editing = !this.words.editing;
                        this.sync_word_editor(cx);
                        cx.notify();
                    }),
                ),
            );
            bar = bar.child(
                button("export-word-srt", "再認識字幕 words.srt", false).on_click(cx.listener(
                    move |_, _, _, cx| {
                        // Re-check fingerprints at export, including changes outside UI.
                        let path = dir.clone();
                        let recording = dir.clone();
                        cx.spawn(async move |this, cx| {
                            let result = cx
                                .background_spawn(async move { word_timing::export_srt(&path) })
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if !this
                                    .open_recording
                                    .as_ref()
                                    .is_some_and(|(open, _)| open == &recording)
                                {
                                    return;
                                }
                                this.words.error = result.err().map(|e| format!("{e:#}"));
                                cx.notify();
                            });
                        })
                        .detach();
                        cx.notify();
                    },
                )),
            );
        }
        col = col.child(bar).child(div().text_color(gray()).child("単語の位置は再認識による推定です。元の本文・訳文は変更しません。日本語は文字や語の一部で区切られることがあります。単語を押すと聞き直せます。"));
        if let Some(data) = &self.words.data {
            let title = kikimimic_core::catalog::Catalog::builtin()
                .get(&data.model)
                .map_or(data.model.as_str(), |model| model.title.as_str());
            col = col.child(
                div()
                    .text_color(gray())
                    .child(format!("時刻の生成: {title}")),
            );
        }
        if let Some(error) = &self.words.error {
            col = col.child(div().text_color(rgb(0xcf222e)).child(error.clone()));
        }
        col.into_any_element()
    }

    pub(super) fn toggle_word_break(&mut self, line: usize, word: usize, cx: &mut Context<Self>) {
        if self.words.saving || self.words.loading || self.words.pending {
            return;
        }
        if let Some((dir, _)) = &self.open_recording {
            let dir = dir.clone();
            let path = dir.clone();
            let expected = self.words.data.clone();
            let generation = self.words.load_generation;
            self.words.saving = true;
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        (|| -> anyhow::Result<WordTiming> {
                            let mut data = word_timing::load(&path)?
                                .ok_or_else(|| anyhow::anyhow!("Generate word timing first"))?;
                            anyhow::ensure!(
                                expected.as_deref() == Some(&data),
                                "字幕が更新されました。読み直してから編集してください。"
                            );
                            data.lines
                                .get_mut(line)
                                .ok_or_else(|| anyhow::anyhow!("Transcript changed"))?
                                .toggle_break(word)?;
                            word_timing::save(&path, &mut data)?;
                            Ok(data)
                        })()
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.words.saving = false;
                    cx.notify();
                    if !this
                        .open_recording
                        .as_ref()
                        .is_some_and(|(path, _)| path == &dir)
                    {
                        return;
                    }
                    if this.words.load_generation != generation {
                        let error = result.err().map(|e| format!("{e:#}"));
                        this.load_words(dir, cx);
                        this.words.error = error;
                        return;
                    }
                    match result {
                        Ok(data) => {
                            this.words.data = Some(Arc::new(data));
                            this.words.error = None;
                        }
                        Err(e) => this.words.error = Some(format!("{e:#}")),
                    }
                    this.sync_word_editor(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        self.sync_word_editor(cx);
        cx.notify();
    }

    /// Poll the player's actual AVAudioPlayer state. This timer only refreshes
    /// observations; it never advances a simulated playback position.
    pub(super) fn observe_playback(
        &mut self,
        path: PathBuf,
        recording: PathBuf,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(120))
                    .await;
                let snapshot = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Playback>(&bytes).ok());
                let Ok(running) = this.update(cx, |this, cx| {
                    if this.playback_status.as_ref() != Some(&path) {
                        return false;
                    }
                    let running = this
                        .playback
                        .as_mut()
                        .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
                    this.words.playback = if running
                        && this
                            .open_recording
                            .as_ref()
                            .is_some_and(|(open, _)| open == &recording)
                    {
                        snapshot
                    } else {
                        None
                    };
                    if let Some(editor) = &this.speaker_editor {
                        let playback = this.words.playback.clone();
                        editor.update(cx, |editor, cx| editor.set_word_playback(playback, cx));
                    }
                    cx.notify();
                    running
                }) else {
                    return;
                };
                if !running {
                    let _ = std::fs::remove_file(&path);
                    return;
                }
            }
        })
        .detach();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WordAction {
    Seek { position: u64, lane: String },
    ToggleBreak { line: usize, word: usize },
}

pub(crate) fn word_line<V: 'static + gpui::EventEmitter<WordAction>>(
    index: usize,
    line: &library::Line,
    timed: &word_timing::TimedLine,
    editing: bool,
    playback: Option<&Playback>,
    cx: &mut Context<V>,
) -> AnyElement {
    let mut col = div().flex().flex_col().gap_1();
    let label = if timed.text().trim() == line.text.trim() {
        "再認識の単語時刻"
    } else {
        "再認識文（元本文と異なります。上の本文・訳文は保持）"
    };
    col = col.child(div().text_color(gray()).child(label));
    let mut row = div().flex().flex_wrap().gap_1().items_center();
    for (i, word) in timed.words.iter().enumerate() {
        if editing && i > 0 {
            let split = timed.breaks.contains(&i);
            row = row.child(
                button(
                    ("word-boundary", index * 1_000_000 + i),
                    if split { "結合" } else { "分割" },
                    split,
                )
                .debug_selector(move || format!("word-boundary-{index}-{i}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(WordAction::ToggleBreak {
                        line: index,
                        word: i,
                    })
                })),
            );
        }
        if let Some(start) = word.seek_ms() {
            let lane = line.lane.clone();
            let active = playback.is_some_and(|p| p.active(&lane, word));
            row = row.child(
                button(
                    ("word-seek", index * 1_000_000 + i),
                    word.text.clone(),
                    active,
                )
                .debug_selector(move || format!("word-seek-{index}-{i}"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(WordAction::Seek {
                        position: start,
                        lane: lane.clone(),
                    })
                })),
            );
        } else {
            row = row.child(
                div()
                    .text_color(gray())
                    .child(format!("{} [未整列]", word.text)),
            );
        }
    }
    col = col.child(row);
    if editing {
        match timed.cues() {
            Ok(cues) => {
                for cue in cues {
                    col = col.child(div().text_color(gray()).child(format!(
                        "{}–{}  {}",
                        mm_ss(cue.start_ms),
                        mm_ss(cue.end_ms),
                        cue.text
                    )));
                }
            }
            Err(e) => col = col.child(div().text_color(gray()).child(e.to_string())),
        }
    }
    if timed.words.is_empty() {
        col = col.child(
            div()
                .text_color(gray())
                .child("この範囲では単語を検出できませんでした"),
        );
    }
    col.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn highlight_uses_reported_position_playing_and_lane_and_seek_uses_measured_start() {
        let word = word_timing::Word {
            text: "語".into(),
            timing: Some(word_timing::TimeRange {
                start_ms: 1234,
                end_ms: 1500,
            }),
        };
        assert_eq!(word.seek_ms(), Some(1234));
        let mut playback = Playback {
            position_ms: 1300,
            playing: true,
            lane: "mic".into(),
        };
        assert!(playback.active("mic", &word));
        assert!(!playback.active("speaker", &word));
        playback.playing = false;
        assert!(!playback.active("mic", &word));
        playback.playing = true;
        playback.position_ms = 1500;
        assert!(!playback.active("mic", &word));
    }
}
