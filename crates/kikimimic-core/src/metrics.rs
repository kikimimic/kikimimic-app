//! Deterministic lane statistics. Overlaps are candidate interruptions only;
//! speaker playback leaking into a microphone invalidates that interpretation.
use crate::library::Line;

#[derive(Debug, Default, PartialEq)]
pub struct LaneMetrics {
    pub speech_ms: u64,
    pub share: f64,
    pub longest_ms: u64,
    pub characters_per_minute: f64,
    pub questions: usize,
}
#[derive(Debug, Default, PartialEq)]
pub struct Metrics {
    pub mic: LaneMetrics,
    pub speaker: LaneMetrics,
    /// Number of overlapping cross-lane utterance pairs (headphones required).
    pub overlaps: usize,
}

pub fn calculate(lines: &[Line]) -> Metrics {
    let valid: Vec<_> = lines
        .iter()
        .filter(|l| l.end_ms > l.start_ms && matches!(l.lane.as_str(), "mic" | "speaker"))
        .collect();
    let lane = |name: &str| {
        let mut rows: Vec<_> = valid.iter().copied().filter(|l| l.lane == name).collect();
        rows.sort_by_key(|l| l.start_ms);
        let mut result = LaneMetrics::default();
        let mut run: Option<(u64, u64)> = None;
        let mut chars = 0;
        for l in rows {
            result.speech_ms += l.end_ms - l.start_ms;
            chars += l.text.chars().filter(|c| !c.is_whitespace()).count();
            result.questions += usize::from(l.text.contains(['?', '？']));
            let (start, end) = match run {
                Some((s, e)) if l.start_ms.saturating_sub(e) < 2000 => (s, e.max(l.end_ms)),
                _ => (l.start_ms, l.end_ms),
            };
            result.longest_ms = result.longest_ms.max(end - start);
            run = Some((start, end));
        }
        if result.speech_ms > 0 {
            result.characters_per_minute = chars as f64 * 60000.0 / result.speech_ms as f64;
        }
        result
    };
    let mut result = Metrics {
        mic: lane("mic"),
        speaker: lane("speaker"),
        overlaps: 0,
    };
    let total = result.mic.speech_ms as f64 + result.speaker.speech_ms as f64;
    if total > 0.0 {
        result.mic.share = result.mic.speech_ms as f64 / total;
        result.speaker.share = result.speaker.speech_ms as f64 / total;
    }
    for a in valid.iter().filter(|l| l.lane == "mic") {
        result.overlaps += valid
            .iter()
            .filter(|b| b.lane == "speaker" && a.start_ms < b.end_ms && b.start_ms < a.end_ms)
            .count();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(lane: &str, start_ms: u64, end_ms: u64, text: &str) -> Line {
        Line {
            id: 1,
            lane: lane.into(),
            start_ms,
            end_ms,
            text: text.into(),
            lang: None,
            translation: None,
            translation_lang: None,
            speaker: None,
        }
    }
    #[test]
    fn japanese_and_overlap_boundaries() {
        let m = calculate(&[
            row("mic", 3000, 4000, "はい？?"),
            row("speaker", 500, 1000, "a"),
            row("mic", 0, 1000, "あ い"),
        ]);
        assert_eq!(m.mic.speech_ms, 2000);
        assert_eq!(m.mic.longest_ms, 1000);
        assert_eq!(m.mic.questions, 1);
        assert_eq!(m.mic.characters_per_minute, 180.0);
        assert_eq!(m.overlaps, 1);
        assert_eq!(m.mic.share, 0.8);
    }
    #[test]
    fn empty_invalid_and_joined() {
        assert_eq!(calculate(&[]), Metrics::default());
        let m = calculate(&[
            row("mic", 0, 1000, "a"),
            row("mic", 2999, 4000, "b"),
            row("speaker", 4000, 5000, "c"),
            row("mic", 9, 0, "?"),
        ]);
        assert_eq!(m.mic.longest_ms, 4000);
        assert_eq!(m.overlaps, 0);
    }
}
