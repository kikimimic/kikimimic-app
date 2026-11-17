//! Recording word review and independent subtitle boundaries.
use std::path::{Path, PathBuf};

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, rgb};
use kikimimic_core::event::PostProcessState;
use kikimimic_core::library;
use kikimimic_core::word_timing::{self, WordTiming};

use super::{MainWindow, button, gray, mm_ss};

#[derive(Default)]
pub(super) struct WordsView {
    pub data: Option<WordTiming>,
    pub error: Option<String>,
    pub pending: bool,
    pub revision: u64,
    pub editing: bool,
    pub playback: Option<Playback>,
}

#[derive(serde::Deserialize)]
pub(super) struct Playback {
    pub position_ms: u64,
    pub playing: bool,
    pub lane: String,
}

impl Playback {
    fn active(&self, lane: &str, word: &word_timing::Word) -> bool {
        self.playing
            && self.lane == lane
            && word
                .timing
                .is_some_and(|t| self.position_ms >= t.start_ms && self.position_ms < t.end_ms)
    }
}

impl WordsView {
    pub fn load(&mut self, dir: &Path) {
        self.data = None;
        self.error = None;
        self.pending = false;
        match word_timing::load(dir) {
            Ok(data) => self.data = data,
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }
}

impl MainWindow {
    pub(super) fn word_toolbar(&self, dir: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let running = self.words.pending
            || matches!(&self.state.read(cx).word_timing,
            Some((path, PostProcessState::Running)) if path == &dir);
        let mut col = div().flex().flex_col().gap_2();
        let mut bar = div().flex().gap_2().items_center();
        if running {
            bar = bar.child(div().text_color(gray()).child("単語時刻を生成中…"));
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
                        cx.notify();
                    }),
                ),
            );
            bar = bar.child(
                button("export-word-srt", "再認識字幕 words.srt", false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        // Re-check fingerprints at export, including changes outside UI.
                        let result = word_timing::export_srt(&dir);
                        this.words.error = result.err().map(|e| format!("{e:#}"));
                        cx.notify();
                    },
                )),
            );
        }
        col = col.child(bar).child(div().text_color(gray()).child("Whisper再認識の推定時刻（実験的）。元本文の強制アラインメントではありません。日本語はモデルのトークン単位。単語を押すと元音声を再生します。"));
        if let Some(data) = &self.words.data {
            col = col.child(
                div()
                    .text_color(gray())
                    .child(format!("出典: {} / {}", data.model, data.source)),
            );
        }
        if let Some(error) = &self.words.error {
            col = col.child(div().text_color(rgb(0xcf222e)).child(error.clone()));
        }
        col.into_any_element()
    }

    pub(super) fn word_line(
        &self,
        index: usize,
        line: &library::Line,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(timed) = self
            .words
            .data
            .as_ref()
            .and_then(|data| data.lines.get(index))
        else {
            return div().into_any_element();
        };
        let mut col = div().flex().flex_col().gap_1();
        let label = if timed.text().trim() == line.text.trim() {
            "再認識の単語時刻"
        } else {
            "再認識文（元本文と異なります。上の本文・訳文は保持）"
        };
        col = col.child(div().text_color(gray()).child(label));
        let mut row = div().flex().flex_wrap().gap_1().items_center();
        for (i, word) in timed.words.iter().enumerate() {
            if self.words.editing && i > 0 {
                let split = timed.breaks.contains(&i);
                row = row.child(
                    button(
                        ("word-boundary", index * 1_000_000 + i),
                        if split { "結合" } else { "分割" },
                        split,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_word_break(index, i, cx)),
                    ),
                );
            }
            if let Some(start) = word.seek_ms() {
                let lane = line.lane.clone();
                let active = self
                    .words
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.active(&lane, word));
                row = row.child(
                    button(
                        ("word-seek", index * 1_000_000 + i),
                        word.text.clone(),
                        active,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.play_word(start, &lane, cx))),
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
        if self.words.editing {
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

    fn toggle_word_break(&mut self, line: usize, word: usize, cx: &mut Context<Self>) {
        if let Some((dir, _)) = &self.open_recording {
            let result = (|| -> anyhow::Result<WordTiming> {
                let mut data = word_timing::load(dir)?
                    .ok_or_else(|| anyhow::anyhow!("Generate word timing first"))?;
                data.lines
                    .get_mut(line)
                    .ok_or_else(|| anyhow::anyhow!("Transcript changed"))?
                    .toggle_break(word)?;
                word_timing::save(dir, &data)?;
                Ok(data)
            })();
            match result {
                Ok(data) => {
                    self.words.data = Some(data);
                    self.words.error = None;
                }
                Err(e) => self.words.error = Some(format!("{e:#}")),
            }
        }
        cx.notify();
    }

    /// Poll the player's actual AVAudioPlayer state. This timer only refreshes
    /// observations; it never advances a simulated playback position.
    pub(super) fn observe_playback(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
                    this.words.playback = if running { snapshot } else { None };
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
