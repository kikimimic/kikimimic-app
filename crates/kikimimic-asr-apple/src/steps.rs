//! Turns what the recognizer reports into the core's [`AsrStep`]s.

use std::collections::VecDeque;

use kikimimic_core::domain::{Lang, StreamMs};
use kikimimic_core::engine::{AsrStep, Recognized};

/// One result from SpeechTranscriber, times relative to the lane's start.
#[derive(Clone, Debug, PartialEq)]
pub enum Heard {
    /// The current guess for the audio after the last final result.
    Volatile(String),
    /// Text that will not change: one phrase, ending an utterance.
    Final {
        text: String,
        start_ms: u64,
        end_ms: u64,
    },
}

/// Results heard but not yet handed to the conductor.
#[derive(Debug, Default)]
pub struct Pending {
    finals: VecDeque<(String, Recognized)>,
    volatile: String,
    volatile_changed: bool,
}

impl Pending {
    pub fn hear(&mut self, heard: Heard, lang: Option<&Lang>) {
        match heard {
            Heard::Volatile(text) => self.volatile = text,
            Heard::Final {
                text,
                start_ms,
                end_ms,
            } => {
                let utterance = Recognized {
                    text: text.trim().to_string(),
                    start: StreamMs(start_ms),
                    end: StreamMs(end_ms),
                    lang: lang.cloned(),
                };
                self.finals.push_back((text, utterance));
                self.volatile.clear();
            }
        }
        self.volatile_changed = true;
    }

    /// One step: at most one utterance, since a step carries one.
    pub fn next_step(&mut self) -> Option<AsrStep> {
        if let Some((committed_delta, utterance)) = self.finals.pop_front() {
            let volatile = if self.finals.is_empty() {
                self.volatile_changed = false;
                self.volatile.clone()
            } else {
                String::new()
            };
            return Some(AsrStep {
                committed_delta,
                volatile,
                utterance: Some(utterance),
            });
        }
        if !std::mem::take(&mut self.volatile_changed) {
            return None;
        }
        Some(AsrStep {
            committed_delta: String::new(),
            volatile: self.volatile.clone(),
            utterance: None,
        })
    }

    /// Everything left, as steps.
    pub fn drain(&mut self) -> Vec<AsrStep> {
        std::iter::from_fn(|| self.next_step()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fin(text: &str, start_ms: u64, end_ms: u64) -> Heard {
        Heard::Final {
            text: text.into(),
            start_ms,
            end_ms,
        }
    }

    fn ja() -> Lang {
        Lang::parse("ja").unwrap()
    }

    #[test]
    fn nothing_heard_is_no_step() {
        assert_eq!(Pending::default().next_step(), None);
    }

    #[test]
    fn a_guess_shows_as_volatile_once() {
        let mut p = Pending::default();
        p.hear(Heard::Volatile("こんに".into()), None);
        p.hear(Heard::Volatile("こんにちは".into()), None);
        let step = p.next_step().unwrap();
        assert_eq!(step.volatile, "こんにちは");
        assert_eq!(step.committed_delta, "");
        assert_eq!(step.utterance, None);
        assert_eq!(p.next_step(), None, "unchanged since the last step");
    }

    #[test]
    fn a_final_result_commits_and_ends_an_utterance() {
        let mut p = Pending::default();
        p.hear(Heard::Volatile("こんにち".into()), Some(&ja()));
        p.hear(fin(" こんにちは。", 300, 1200), Some(&ja()));
        let step = p.next_step().unwrap();
        assert_eq!(step.committed_delta, " こんにちは。");
        assert_eq!(step.volatile, "", "the guess it replaced is gone");
        let u = step.utterance.unwrap();
        assert_eq!(u.text, "こんにちは。");
        assert_eq!((u.start, u.end), (StreamMs(300), StreamMs(1200)));
        assert_eq!(u.lang, Some(ja()));
    }

    #[test]
    fn finals_heard_together_come_out_one_per_step_in_order() {
        let mut p = Pending::default();
        p.hear(fin("one.", 0, 500), None);
        p.hear(fin(" two.", 500, 900), None);
        p.hear(Heard::Volatile(" thr".into()), None);
        let steps = p.drain();
        let texts: Vec<_> = steps
            .iter()
            .map(|s| s.utterance.as_ref().unwrap().text.as_str())
            .collect();
        assert_eq!(texts, ["one.", "two."]);
        assert_eq!(steps[0].volatile, "", "the guess is not after one.");
        assert_eq!(steps[1].volatile, " thr");
    }
}
