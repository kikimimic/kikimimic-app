//! One utterance as it grows: committed text in the normal color, the volatile
//! tail in gray, wrapping as one paragraph (not two boxes side by side).

use std::ops::Range;

use gpui::{HighlightStyle, Hsla, IntoElement, SharedString, StyledText};

/// The joined text and the byte range of its volatile tail.
pub fn layout(committed: &str, volatile: &str) -> (String, Range<usize>) {
    (
        format!("{committed}{volatile}"),
        committed.len()..committed.len() + volatile.len(),
    )
}

pub fn render(committed: &str, volatile: &str, volatile_color: Hsla) -> impl IntoElement {
    let (text, tail) = layout(committed, volatile);
    let highlights = (!tail.is_empty()).then(|| {
        (
            tail,
            HighlightStyle {
                color: Some(volatile_color),
                ..Default::default()
            },
        )
    });
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volatile_tail_follows_the_committed_text() {
        let (text, tail) = layout("今日は", "いい天気");
        assert_eq!(text, "今日はいい天気");
        assert_eq!(&text[tail], "いい天気");
    }

    #[test]
    fn nothing_is_gray_once_everything_is_committed() {
        let (text, tail) = layout("Hello there.", "");
        assert_eq!(text, "Hello there.");
        assert!(tail.is_empty());
    }

    #[test]
    fn a_line_can_start_entirely_volatile() {
        let (text, tail) = layout("", "Hel");
        assert_eq!(tail, 0..text.len());
    }
}
