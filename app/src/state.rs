//! The UI's state: core events folded into what the window shows. Pure, so it
//! is tested without a window.

use std::path::PathBuf;

use kikimimic_core::capture::SourceEvent;
use kikimimic_core::domain::{LaneLabel, Lang, RecordingMs, UtteranceId};
use kikimimic_core::event::{Event, Notice, Phase, PostProcessState, TranslationOutcome};

/// Lines kept on screen; older ones scroll away (they are on disk).
const MAX_LINES: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum Translation {
    None,
    Pending,
    Text(String),
    Dropped,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: UtteranceId,
    pub lane: LaneLabel,
    pub committed: String,
    pub volatile: String,
    /// Set once the utterance has ended.
    pub start: Option<RecordingMs>,
    pub lang: Option<Lang>,
    pub translation: Translation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceMessage {
    Switched(String),
    Failed(String),
}

/// "Follow the new output device?"
#[derive(Clone, Debug, PartialEq)]
pub struct SourceQuestion {
    pub capturing: String,
    pub detected: String,
    pub uid: String,
}

#[derive(Debug)]
pub struct AppState {
    pub lines: Vec<Line>,
    pub phase: Phase,
    pub notices: Vec<Notice>,
    pub recording_dir: Option<PathBuf>,
    pub question: Option<SourceQuestion>,
    /// The last thing a source reported (switched, or failed to).
    pub source_message: Option<SourceMessage>,
    /// Speaker labelling of a recording, and how far it got.
    pub post_process: Option<(PathBuf, PostProcessState)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            phase: Phase::Idle,
            notices: Vec::new(),
            recording_dir: None,
            question: None,
            source_message: None,
            post_process: None,
        }
    }
}

impl AppState {
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Transcript {
                lane,
                utterance,
                committed_delta,
                volatile,
                finished,
            } => {
                let line = self.line(utterance, lane);
                line.committed.push_str(&committed_delta);
                line.volatile = volatile;
                if let Some(u) = finished {
                    // The finished text is authoritative: the glossary has
                    // been applied to it.
                    line.committed = u.text;
                    line.volatile.clear();
                    line.start = Some(u.start);
                    line.lang = u.lang;
                    line.translation = if u.translating {
                        Translation::Pending
                    } else {
                        Translation::None
                    };
                }
                if self.lines.len() > MAX_LINES {
                    self.lines.drain(..self.lines.len() - MAX_LINES);
                }
            }
            Event::Translation { id, outcome, .. } => {
                if let Some(line) = self.lines.iter_mut().rev().find(|l| l.id == id) {
                    line.translation = match outcome {
                        TranslationOutcome::Text { text, .. } => Translation::Text(text),
                        TranslationOutcome::Dropped => Translation::Dropped,
                        TranslationOutcome::Failed { message } => Translation::Failed(message),
                    };
                }
            }
            Event::Status(status) => {
                self.phase = status.phase;
                self.notices = status.notices;
                self.recording_dir = status.recording_dir.or(self.recording_dir.take());
            }
            Event::Source(SourceEvent::OutputSwitchProposed {
                capturing,
                detected,
                uid,
            }) => {
                self.question = Some(SourceQuestion {
                    capturing,
                    detected,
                    uid,
                });
            }
            Event::Source(SourceEvent::OutputSwitchWithdrawn) => self.question = None,
            Event::Source(SourceEvent::OutputSwitched { name }) => {
                self.question = None;
                self.source_message = Some(SourceMessage::Switched(name));
            }
            Event::Source(SourceEvent::OutputSwitchFailed { reason }) => {
                self.question = None;
                self.source_message = Some(SourceMessage::Failed(reason));
            }
            Event::PostProcess { recording, state } => self.post_process = Some((recording, state)),
        }
    }

    /// The line for `id`, created at the end if new. Recent lines are the
    /// ones still changing, so search from the end.
    fn line(&mut self, id: UtteranceId, lane: LaneLabel) -> &mut Line {
        match self.lines.iter().rposition(|l| l.id == id) {
            Some(i) => &mut self.lines[i],
            None => {
                self.lines.push(Line {
                    id,
                    lane,
                    committed: String::new(),
                    volatile: String::new(),
                    start: None,
                    lang: None,
                    translation: Translation::None,
                });
                self.lines.last_mut().expect("just pushed")
            }
        }
    }

    pub fn is_listening(&self) -> bool {
        self.phase == Phase::Listening
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kikimimic_core::domain::LaneRole;
    use kikimimic_core::event::{StatusView, UtteranceView};

    fn mic() -> LaneLabel {
        LaneLabel::new(LaneRole::Mic, 1)
    }

    fn step(id: u64, committed: &str, volatile: &str) -> Event {
        Event::Transcript {
            lane: mic(),
            utterance: UtteranceId(id),
            committed_delta: committed.into(),
            volatile: volatile.into(),
            finished: None,
        }
    }

    fn finished(id: u64, text: &str, translating: bool) -> Event {
        Event::Transcript {
            lane: mic(),
            utterance: UtteranceId(id),
            committed_delta: String::new(),
            volatile: String::new(),
            finished: Some(UtteranceView {
                id: UtteranceId(id),
                text: text.into(),
                start: RecordingMs(1200),
                end: RecordingMs(3400),
                lang: Some(Lang::parse("en").unwrap()),
                translating,
            }),
        }
    }

    #[test]
    fn an_utterance_grows_on_one_line() {
        let mut s = AppState::default();
        s.apply(step(1, "", "Hel"));
        s.apply(step(1, "Hello ", "the"));
        assert_eq!(s.lines.len(), 1);
        assert_eq!(
            (s.lines[0].committed.as_str(), s.lines[0].volatile.as_str()),
            ("Hello ", "the")
        );
    }

    #[test]
    fn the_finished_text_replaces_what_was_shown() {
        let mut s = AppState::default();
        s.apply(step(1, "Helo ", "ther"));
        s.apply(finished(1, "Hello there.", true));
        let l = &s.lines[0];
        assert_eq!(
            (l.committed.as_str(), l.volatile.as_str()),
            ("Hello there.", "")
        );
        assert_eq!(l.start, Some(RecordingMs(1200)));
        assert_eq!(l.translation, Translation::Pending);
    }

    #[test]
    fn a_translation_finds_its_line_by_id() {
        let mut s = AppState::default();
        s.apply(finished(1, "One.", true));
        s.apply(finished(2, "Two.", true));
        s.apply(Event::Translation {
            id: UtteranceId(1),
            lane: mic(),
            outcome: TranslationOutcome::Text {
                text: "一。".into(),
                lang: Lang::parse("ja").unwrap(),
            },
        });
        s.apply(Event::Translation {
            id: UtteranceId(2),
            lane: mic(),
            outcome: TranslationOutcome::Dropped,
        });
        assert_eq!(s.lines[0].translation, Translation::Text("一。".into()));
        assert_eq!(s.lines[1].translation, Translation::Dropped);
    }

    #[test]
    fn an_untranslated_line_waits_for_nothing() {
        let mut s = AppState::default();
        s.apply(finished(1, "One.", false));
        assert_eq!(s.lines[0].translation, Translation::None);
    }

    #[test]
    fn old_lines_scroll_away() {
        let mut s = AppState::default();
        for id in 0..MAX_LINES as u64 + 5 {
            s.apply(finished(id, "x", false));
        }
        assert_eq!(s.lines.len(), MAX_LINES);
        assert_eq!(s.lines[0].id, UtteranceId(5));
    }

    #[test]
    fn status_carries_phase_notices_and_the_recording() {
        let mut s = AppState::default();
        s.apply(Event::Status(StatusView {
            phase: Phase::Listening,
            notices: vec![Notice::RecordingOff {
                reason: "full".into(),
            }],
            recording_dir: Some("/tmp/x".into()),
        }));
        assert!(s.is_listening());
        assert_eq!(s.notices.len(), 1);
        assert_eq!(s.recording_dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn speaker_labelling_progress_is_kept_per_recording() {
        let mut s = AppState::default();
        let rec = PathBuf::from("/rec/1");
        s.apply(Event::PostProcess {
            recording: rec.clone(),
            state: PostProcessState::Running,
        });
        s.apply(Event::PostProcess {
            recording: rec.clone(),
            state: PostProcessState::Done,
        });
        assert_eq!(s.post_process, Some((rec, PostProcessState::Done)));
    }

    #[test]
    fn a_device_question_is_shown_until_answered_or_withdrawn() {
        let mut s = AppState::default();
        s.apply(Event::Source(SourceEvent::OutputSwitchProposed {
            capturing: "Speakers".into(),
            detected: "AirPods".into(),
            uid: "a".into(),
        }));
        assert_eq!(s.question.as_ref().map(|q| q.uid.as_str()), Some("a"));
        s.apply(Event::Source(SourceEvent::OutputSwitchWithdrawn));
        assert_eq!(s.question, None);
        s.apply(Event::Source(SourceEvent::OutputSwitched {
            name: "AirPods".into(),
        }));
        assert_eq!(
            s.source_message,
            Some(SourceMessage::Switched("AirPods".into()))
        );
    }
}
