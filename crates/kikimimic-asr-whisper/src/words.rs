//! Assemble byte tokens before UTF-8 decoding. Estimates come only from the
//! engine's token data; grouping never divides a segment's duration by text.
use anyhow::{ensure, Result};
use kikimimic_core::word_timing::{TimeRange, Word};

#[derive(Default)]
pub(crate) struct Words {
    bytes: Vec<u8>,
    timing: Option<TimeRange>,
    space: String,
    out: Vec<Word>,
}

impl Words {
    pub fn push(&mut self, bytes: &[u8], t0: i64, t1: i64) -> Result<()> {
        self.bytes.extend_from_slice(bytes);
        if t0 >= 0 && t1 > t0 {
            let next = TimeRange {
                start_ms: t0 as u64 * 10,
                end_ms: t1 as u64 * 10,
            };
            self.timing = Some(match self.timing {
                Some(t) => TimeRange {
                    start_ms: t.start_ms.min(next.start_ms),
                    end_ms: t.end_ms.max(next.end_ms),
                },
                None => next,
            });
        }
        let mut text = match std::str::from_utf8(&self.bytes) {
            Ok(text) => text.to_string(),
            Err(e) if e.error_len().is_none() => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let timing = self.timing.take();
        self.bytes.clear();
        if text.is_empty() {
            return Ok(());
        }
        if text.trim().is_empty() {
            self.space.push_str(&text);
            return Ok(());
        }
        if !self.space.is_empty() {
            text = std::mem::take(&mut self.space) + &text;
        }
        let punctuation = text
            .chars()
            .all(|c| !c.is_alphanumeric() && !c.is_whitespace());
        let continuation = text.starts_with(|c: char| c.is_ascii_alphanumeric())
            && self
                .out
                .last()
                .is_some_and(|w| w.text.ends_with(|c: char| c.is_ascii_alphanumeric()));
        if let Some(last) = self.out.last_mut().filter(|_| punctuation || continuation) {
            last.text.push_str(&text);
            // Punctuation without its own interval belongs to the preceding
            // unit. A lexical continuation without time makes it unaligned.
            last.timing = match (last.timing, timing) {
                (Some(a), Some(b)) if b.start_ms >= a.start_ms => Some(TimeRange {
                    start_ms: a.start_ms,
                    end_ms: a.end_ms.max(b.end_ms),
                }),
                (a, None) if punctuation => a,
                _ => None,
            };
        } else {
            self.out.push(Word { text, timing });
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<Word>> {
        ensure!(
            self.bytes.is_empty(),
            "Whisper returned incomplete UTF-8 bytes"
        );
        if let Some(last) = self.out.last_mut() {
            last.text.push_str(&self.space);
        }
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn japanese_byte_tokens_complete_before_display_and_english_subwords_join() {
        let mut words = Words::default();
        let bytes = "語".as_bytes();
        words.push(&bytes[..1], 1, 3).unwrap();
        words.push(&bytes[1..], 3, 8).unwrap();
        words.push("。".as_bytes(), 8, 8).unwrap();
        words.push(b" hello", 10, 20).unwrap();
        words.push(b"world", 20, 30).unwrap();
        words.push(b"!", -1, -1).unwrap();
        let result = words.finish().unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "語。");
        assert_eq!(
            result[0].timing,
            Some(TimeRange {
                start_ms: 10,
                end_ms: 80
            })
        );
        assert_eq!(result[1].text, " helloworld!");
        assert_eq!(result[1].timing.unwrap().end_ms, 300);
    }
    #[test]
    fn unaligned_and_invalid_utf8_are_not_replaced_with_fake_times_or_text() {
        let mut words = Words::default();
        words.push("未整列".as_bytes(), -1, -1).unwrap();
        assert_eq!(words.finish().unwrap()[0].timing, None);
        let mut words = Words::default();
        words.push(&[0xe8], 0, 1).unwrap();
        assert!(words.finish().is_err());
        assert!(Words::default().push(&[0xff], 0, 1).is_err());
    }

    #[test]
    fn whitespace_tokens_preserve_word_separation_without_becoming_blank_cues() {
        let mut words = Words::default();
        words.push(b"hello", 0, 10).unwrap();
        words.push(b" ", 10, 10).unwrap();
        words.push(b"world", 10, 20).unwrap();
        words.push(b" ", 20, 20).unwrap();
        let result = words.finish().unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "hello");
        assert_eq!(result[1].text, " world ");
    }
}
