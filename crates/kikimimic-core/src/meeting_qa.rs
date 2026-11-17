//! Meeting-scoped answer validation. No inference or IO is performed here.
//! A caller must retain the immutable snapshot while displaying an answer.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub utterance_id: String,
    pub start_ms: u64,
    pub original: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub meeting_id: String,
    /// Monotonically increasing revision, including late finalization/corrections.
    pub revision: u64,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub utterance_id: String,
    /// Exact original-language excerpt, never translated text.
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Unconfirmed,
    Supported {
        text: String,
        citations: Vec<Citation>,
    },
}

impl Answer {
    pub fn text(&self) -> &str {
        match self {
            Self::Unconfirmed => "会議内では確認できない",
            Self::Supported { text, .. } => text,
        }
    }
}

impl Snapshot {
    /// Resolve against this exact snapshot for source navigation.
    pub fn source(&self, id: &str) -> Option<&Source> {
        let mut matches = self.sources.iter().filter(|s| s.utterance_id == id);
        let source = matches.next()?;
        matches.next().is_none().then_some(source)
    }

    /// Reject stale/cross-meeting output and invalid quotes conservatively.
    /// This checks provenance only; semantic entailment requires evaluation.
    pub fn validate(&self, meeting_id: &str, revision: u64, answer: Answer) -> Answer {
        if meeting_id != self.meeting_id || revision != self.revision {
            return Answer::Unconfirmed;
        }
        match &answer {
            Answer::Unconfirmed => answer,
            Answer::Supported { text, citations } => {
                let valid = !text.trim().is_empty()
                    && !citations.is_empty()
                    && citations.iter().all(|c| {
                        !c.quote.trim().is_empty()
                            && self
                                .source(&c.utterance_id)
                                .is_some_and(|s| s.original.contains(&c.quote))
                    });
                if valid {
                    answer
                } else {
                    Answer::Unconfirmed
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_original_navigation_and_rejects_wrong_scope_or_translation() {
        let snapshot = Snapshot {
            meeting_id: "selected".into(),
            revision: 2,
            sources: vec![Source {
                utterance_id: "u1".into(),
                start_ms: 1200,
                original: "担当は田中、期限は金曜日。".into(),
            }],
        };
        let answer = Answer::Supported {
            text: "田中、金曜日".into(),
            citations: vec![Citation {
                utterance_id: "u1".into(),
                quote: "期限は金曜日".into(),
            }],
        };
        assert_eq!(snapshot.validate("selected", 2, answer.clone()), answer);
        assert_eq!(snapshot.source("u1").unwrap().start_ms, 1200);
        assert_eq!(
            snapshot.validate("other", 2, answer.clone()),
            Answer::Unconfirmed
        );
        assert_eq!(
            snapshot.validate("selected", 1, answer),
            Answer::Unconfirmed
        );
        for (id, quote) in [("u2", "期限"), ("u1", "Friday"), ("u1", "")] {
            assert_eq!(
                snapshot.validate(
                    "selected",
                    2,
                    Answer::Supported {
                        text: "answer".into(),
                        citations: vec![Citation {
                            utterance_id: id.into(),
                            quote: quote.into()
                        }]
                    }
                ),
                Answer::Unconfirmed
            );
        }
    }
}
