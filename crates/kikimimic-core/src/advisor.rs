//! Conservative, manual-only advisor prototype. Candidates are not decisions.
//! No runtime/audio hook: evaluate the meeting-understanding dependency first.
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FollowUp,
    OpenQuestion,
    Undecided,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    /// Exact source text; no participant identity is inferred.
    pub evidence: String,
    pub reason: &'static str,
}

/// One instance per meeting. Dismissed and shown evidence stays suppressed.
/// Input must be finalized utterances, not streaming partials.
#[derive(Default)]
pub struct Advisor {
    stopped: bool,
    seen: HashSet<String>,
}

impl Advisor {
    /// Permanently stop this meeting's advisor, including manual requests.
    pub fn stop(&mut self) {
        self.stopped = true;
    }

    /// Suppress evidence even if it has not yet been requested.
    pub fn dismiss(&mut self, evidence: &str) {
        self.seen.insert(evidence.trim().to_owned());
    }

    /// Explicit unresolved cues only; at most three cards per manual request.
    /// This lexical baseline cannot resolve references across utterances.
    pub fn request(&mut self, utterances: &[&str]) -> Vec<Candidate> {
        if self.stopped {
            return Vec::new();
        }
        let mut cards = Vec::new();
        for text in utterances.iter().rev().take(100).rev() {
            let text = text.trim();
            if text.is_empty() || text.len() > 4096 || self.seen.contains(text) {
                continue;
            }
            let (kind, reason) = if text.starts_with("私は")
                && text.ends_with("持ち帰って確認します。")
            {
                (Kind::FollowUp, "発話者自身が確認を持ち帰ると明言")
            } else if text.starts_with("未回答の質問：") {
                (Kind::OpenQuestion, "未回答と明示された質問")
            } else if text.starts_with("未決の論点：") {
                (Kind::Undecided, "未決と明示された論点")
            } else {
                continue;
            };
            if text
                .split_once('：')
                .is_some_and(|(_, body)| body.trim().is_empty())
            {
                continue;
            }
            self.seen.insert(text.to_owned());
            cards.push(Candidate {
                kind,
                evidence: text.to_owned(),
                reason,
            });
            if cards.len() == 3 {
                break;
            }
        }
        cards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cues_and_abstention() {
        let cases = [
            ("私は仕様を持ち帰って確認します。", Some(Kind::FollowUp)),
            ("未回答の質問：納期はいつですか？", Some(Kind::OpenQuestion)),
            ("未決の論点：発売日", Some(Kind::Undecided)),
            ("発売日は来月に決定しました。", None),
            ("田中さんが持ち帰ると言いました。", None),
            ("どうしようかな？", None),
            ("納期はいつですか？", None),
            ("未回答の質問：  ", None),
        ];
        for (text, expected) in cases {
            let cards = Advisor::default().request(&[text]);
            assert_eq!(cards.first().map(|c| c.kind), expected, "{text}");
            if let Some(card) = cards.first() {
                assert_eq!(card.evidence, text);
            }
        }
    }

    #[test]
    fn meeting_controls_and_budget() {
        let mut advisor = Advisor::default();
        advisor.dismiss("未決の論点：A");
        assert!(advisor.request(&["未決の論点：A"]).is_empty());
        let input = [
            "未決の論点：B",
            "未決の論点：C",
            "未決の論点：D",
            "未決の論点：E",
        ];
        assert_eq!(advisor.request(&input).len(), 3);
        assert_eq!(advisor.request(&input).len(), 1);
        advisor.stop();
        assert!(advisor.request(&["未決の論点：F"]).is_empty());
    }
}
