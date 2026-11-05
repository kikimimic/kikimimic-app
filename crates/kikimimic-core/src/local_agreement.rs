//! LocalAgreement-2: a hypothesis prefix is committed once two consecutive
//! hypotheses agree on it. Committed text never retracts.
//!
//! Char-level agreement so it works for Japanese (no word boundaries) and
//! English alike.
//!
//! Known limitation: when a hypothesis contradicts text that is already
//! committed (the engine re-read earlier audio), no alignment of its tail
//! against the committed text exists. The volatile tail is hidden for such
//! feeds, but if the new reading persists, later commits splice at a
//! char-count offset and can leave one garbled seam. Retraction would be
//! the only real fix and is ruled out by design.

/// Result of feeding one hypothesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreement {
    /// Newly committed text (delta since the previous feed).
    pub committed_delta: String,
    /// Unstable tail of the current hypothesis (may change next feed).
    pub volatile: String,
}

#[derive(Debug, Default)]
pub struct LocalAgreement {
    prev: Option<String>,
    committed_chars: usize,
}

impl LocalAgreement {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a fresh agreement context (call when the audio window slides).
    pub fn reset(&mut self) {
        self.prev = None;
        self.committed_chars = 0;
    }

    /// Commit and return the pending volatile tail (the last hypothesis
    /// beyond the committed point). Call when the hypothesis is known to be
    /// stable: at an utterance boundary or when capture stops.
    pub fn flush(&mut self) -> String {
        let Some(prev) = &self.prev else {
            return String::new();
        };
        let tail: String = prev.chars().skip(self.committed_chars).collect();
        self.committed_chars += tail.chars().count();
        tail
    }

    pub fn feed(&mut self, hypothesis: &str) -> Agreement {
        let agreed_chars = match &self.prev {
            Some(prev) => common_prefix_chars(prev, hypothesis),
            None => 0,
        };
        // Disagreement inside the committed region means this hypothesis is a
        // different reading of audio we already committed; its tail would
        // garble the display if spliced after the committed text.
        let diverged = self.prev.is_some() && agreed_chars < self.committed_chars;
        let commit_to = agreed_chars.max(self.committed_chars);
        let chars: Vec<char> = hypothesis.chars().collect();
        let delta_from = self.committed_chars.min(chars.len());
        let delta_to = commit_to.min(chars.len());
        let committed_delta: String = chars[delta_from..delta_to].iter().collect();
        let volatile: String = if diverged {
            String::new()
        } else {
            chars[delta_to..].iter().collect()
        };
        self.committed_chars = commit_to;
        self.prev = Some(hypothesis.to_string());
        Agreement {
            committed_delta,
            volatile,
        }
    }
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_hypothesis_commits_nothing() {
        let mut la = LocalAgreement::new();
        let out = la.feed("こんにちは");
        assert_eq!(out.committed_delta, "");
        assert_eq!(out.volatile, "こんにちは");
    }

    #[test]
    fn reset_starts_a_fresh_agreement_context() {
        let mut la = LocalAgreement::new();
        la.feed("こんにちは");
        la.feed("こんにちは"); // fully committed
        la.reset();
        let out = la.feed("次の文章");
        assert_eq!(out.committed_delta, "");
        assert_eq!(out.volatile, "次の文章");
    }

    #[test]
    fn shorter_hypothesis_than_committed_yields_nothing() {
        let mut la = LocalAgreement::new();
        la.feed("こんにちは、世界");
        la.feed("こんにちは、世界"); // fully committed
        let out = la.feed("こんに"); // hypothesis shrank below the committed point
        assert_eq!(out.committed_delta, "");
        assert_eq!(out.volatile, "");
    }

    #[test]
    fn flush_commits_and_returns_the_pending_volatile_tail() {
        let mut la = LocalAgreement::new();
        la.feed("こんにちは、せ");
        la.feed("こんにちは、世界"); // committed: こんにちは、 volatile: 世界
        assert_eq!(la.flush(), "世界");
        // the flushed tail is now committed: a second flush has nothing left
        assert_eq!(la.flush(), "");
    }

    #[test]
    fn hypothesis_diverging_before_committed_point_suppresses_volatile() {
        let mut la = LocalAgreement::new();
        la.feed("こんにちは、せ");
        la.feed("こんにちは、世界"); // committed: こんにちは、
                                     // Longer hypothesis that contradicts the committed prefix: splicing
                                     // its tail after the committed text would garble the display.
        let out = la.feed("今日は天気が良いですね");
        assert_eq!(out.committed_delta, "");
        assert_eq!(out.volatile, "");
    }

    #[test]
    fn flush_before_any_hypothesis_returns_nothing() {
        let mut la = LocalAgreement::new();
        assert_eq!(la.flush(), "");
    }

    #[test]
    fn agreeing_prefix_of_two_hypotheses_is_committed() {
        let mut la = LocalAgreement::new();
        la.feed("こんにちは、せ");
        let out = la.feed("こんにちは、世界");
        assert_eq!(out.committed_delta, "こんにちは、");
        assert_eq!(out.volatile, "世界");
    }
}
