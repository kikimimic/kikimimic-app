//! The subtitle timeline (mock 23): every cue as a block on one time axis,
//! one row per speaker, so cue lengths and gaps are seen rather than read.
//! Boundaries are still split and joined in the transcript rows below it.

use kikimimic_core::word_timing::TimedLine;

use super::*;
use crate::theme;

/// Pixels per second on the timeline.
const SCALE: f32 = 40.;
/// Subtitle conventions: longer is hard to read, shorter flashes by.
const MAX_CHARS: usize = 42;
const MIN_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Block {
    pub row: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Too long to read, or too short to see.
    pub warn: bool,
}

/// Rows (speakers, or lanes when unnamed, in order of appearance) and the
/// cue blocks on them. Lines without timing have no blocks.
pub(super) fn layout(lines: &[library::Line], timed: &[TimedLine]) -> (Vec<String>, Vec<Block>) {
    let mut rows: Vec<String> = Vec::new();
    let mut blocks = Vec::new();
    for (line, timed) in lines.iter().zip(timed) {
        let who = line.speaker.clone().unwrap_or_else(|| line.lane.clone());
        let row = match rows.iter().position(|r| *r == who) {
            Some(i) => i,
            None => {
                rows.push(who);
                rows.len() - 1
            }
        };
        for cue in timed.cues().unwrap_or_default() {
            let warn = cue.text.chars().count() > MAX_CHARS
                || cue.end_ms.saturating_sub(cue.start_ms) < MIN_MS;
            blocks.push(Block {
                row,
                start_ms: cue.start_ms,
                end_ms: cue.end_ms,
                text: cue.text,
                warn,
            });
        }
    }
    (rows, blocks)
}

impl MainWindow {
    /// The timeline strip; clicking a cue plays it.
    pub(super) fn subtitle_timeline(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let data = self.words.data.as_ref()?;
        let (_, session) = self.open_recording.as_ref()?;
        let (rows, blocks) = layout(&session.lines, &data.lines);
        if blocks.is_empty() {
            return None;
        }
        let end = blocks.iter().map(|b| b.end_ms).max().unwrap_or(0);
        let x = |ms: u64| px(ms as f32 / 1000. * SCALE);
        const ROW: f32 = 30.;
        const LABEL: f32 = 80.;
        let mut lanes = div()
            .relative()
            .w(px(LABEL) + x(end) + px(40.))
            .h(px(18. + ROW * rows.len() as f32));
        // A tick every ten seconds.
        for t in (0..=end).step_by(10_000) {
            lanes = lanes.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(LABEL) + x(t))
                    .text_size(px(10.5))
                    .text_color(theme::muted())
                    .child(mm_ss(t)),
            );
        }
        for (r, name) in rows.iter().enumerate() {
            lanes = lanes.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(18. + ROW * r as f32 + 7.))
                    .w(px(LABEL - 6.))
                    .truncate()
                    .text_size(px(11.))
                    .child(name.clone()),
            );
        }
        for (i, b) in blocks.into_iter().enumerate() {
            let start = b.start_ms;
            lanes = lanes.child(
                div()
                    .id(("cue", i))
                    .absolute()
                    .left(px(LABEL) + x(b.start_ms))
                    .top(px(18. + ROW * b.row as f32 + 3.))
                    .w(x(b.end_ms.saturating_sub(b.start_ms)).max(px(6.)))
                    .h(px(ROW - 6.))
                    .px_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if b.warn {
                        theme::warn()
                    } else {
                        theme::border()
                    })
                    .bg(gpui::white())
                    .text_size(px(11.))
                    .truncate()
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::fg()))
                    .child(b.text)
                    .on_click(cx.listener(move |this, _, _, cx| this.play_recording(start, cx))),
            );
        }
        if let Some(p) = &self.words.playback {
            lanes = lanes.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(LABEL) + x(p.position_ms))
                    .w(px(1.))
                    .bg(theme::danger()),
            );
        }
        Some(
            div()
                .id("subtitle-timeline")
                .debug_selector(|| "subtitle-timeline".into())
                .w_full()
                .overflow_x_scroll()
                .border_1()
                .border_color(theme::border())
                .rounded(px(6.))
                .p_2()
                .child(lanes)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kikimimic_core::word_timing::{TimeRange, Word};

    fn word(text: &str, start_ms: u64, end_ms: u64) -> Word {
        Word {
            text: text.into(),
            timing: Some(TimeRange { start_ms, end_ms }),
        }
    }

    fn line(id: u64, speaker: Option<&str>, lane: &str) -> library::Line {
        library::Line {
            id,
            lane: lane.into(),
            start_ms: 0,
            end_ms: 0,
            lang: None,
            text: String::new(),
            translation: None,
            translation_lang: None,
            speaker: speaker.map(Into::into),
        }
    }

    #[test]
    fn cues_land_on_their_speakers_row_at_their_times() {
        let mut first = TimedLine {
            words: vec![word("では", 0, 400), word("始めます", 400, 2000)],
            breaks: Default::default(),
        };
        first.toggle_break(1).unwrap();
        let second = TimedLine {
            words: vec![word(
                "この字幕はとても長くて一度に読み切れないほど続いていく文章になっています。さらに続きます",
                2500,
                9000,
            )],
            breaks: Default::default(),
        };
        let (rows, blocks) = layout(
            &[line(1, Some("自分"), "mic"), line(2, None, "speaker")],
            &[first, second],
        );
        assert_eq!(rows, ["自分", "speaker"]);
        assert_eq!(
            blocks
                .iter()
                .map(|b| (b.row, b.start_ms, b.end_ms, b.warn))
                .collect::<Vec<_>>(),
            [
                (0, 0, 400, true),
                (0, 400, 2000, false),
                (1, 2500, 9000, true)
            ],
            "a 0.4 s cue and a 45-character cue are flagged"
        );
    }
}
