//! Mixes the capture lanes into one timeline for `mix.wav`.
//!
//! The lanes are independent audio devices: they start at different moments,
//! run on their own clocks, and the speaker tap delivers *nothing at all*
//! while nothing is playing (docs/step0-tap-results.md). So the mix cannot be
//! "add up whatever arrived this poll" — a lane going quiet would shorten the
//! timeline and desync the other lane a little more every time.
//!
//! Instead the caller says where each chunk belongs, in samples from the
//! session's zero, worked out from the capture timestamps the OS puts on every
//! buffer. This mixer only adds them up and decides when a moment is settled
//! enough to write out.

/// Accumulates the lanes into a single mono stream.
pub struct Mixer {
    /// Absolute position of `buf[0]`: everything before it is already out.
    written: usize,
    /// Sums for the region that is still open to contributions.
    buf: Vec<f32>,
    /// Where each lane's audio has reached — its last chunk's end.
    cursors: Vec<usize>,
}

impl Mixer {
    pub fn new(lanes: usize) -> Self {
        Self {
            written: 0,
            buf: Vec::new(),
            cursors: vec![0; lanes],
        }
    }

    /// Place one lane's audio at `at`, and report the silence that leaves in
    /// front of it — the lane's own recording has to be padded by the same
    /// amount to stay in step with the mix.
    ///
    /// Audio for a moment already written out can only go in at the head: the
    /// alternative is dropping it. The drain's lag is what keeps that rare.
    pub fn push_at(&mut self, lane: usize, samples: &[f32], at: usize) -> usize {
        if samples.is_empty() {
            return 0;
        }
        let start = at.max(self.written);
        let skipped = start.saturating_sub(self.cursors[lane]);
        let offset = start - self.written;
        if self.buf.len() < offset + samples.len() {
            self.buf.resize(offset + samples.len(), 0.0);
        }
        for (slot, s) in self.buf[offset..].iter_mut().zip(samples) {
            *slot += s;
        }
        self.cursors[lane] = start + samples.len();
        skipped
    }

    /// Emit the mixed audio, holding back `lag` samples behind the furthest
    /// lane. The lag has to cover the worst case of one lane being polled
    /// well after another (a decode runs between their polls), or that lane's
    /// audio would arrive for a moment already written.
    pub fn drain(&mut self, lag: usize) -> Vec<f32> {
        let head = self.cursors.iter().copied().max().unwrap_or(self.written);
        let upto = head.saturating_sub(lag).max(self.written);
        let n = (upto - self.written).min(self.buf.len());
        self.written += n;
        self.buf.drain(..n).collect()
    }

    /// Emit everything left, holding nothing back (the session ended).
    pub fn finish(&mut self) -> Vec<f32> {
        self.drain(0)
    }

    /// How much has been emitted — the length of the mixed timeline so far.
    pub fn position(&self) -> usize {
        self.written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_the_lanes_sample_by_sample() {
        let mut mix = Mixer::new(2);
        mix.push_at(0, &[0.1, 0.2, 0.3], 0);
        mix.push_at(1, &[0.5, 0.5, 0.5], 0);
        assert_eq!(mix.finish(), vec![0.6, 0.7, 0.8]);
    }

    #[test]
    fn a_lane_that_opened_late_starts_where_its_audio_was_captured() {
        // The speaker tap's first sample is not the session's first sample:
        // its capture timestamp says how much later it began.
        let mut mix = Mixer::new(2);
        mix.push_at(0, &[0.1, 0.1, 0.1, 0.1], 0);
        mix.push_at(1, &[0.5, 0.5], 2);
        assert_eq!(mix.finish(), vec![0.1, 0.1, 0.6, 0.6]);
    }

    #[test]
    fn a_gap_in_a_lane_is_silence_the_others_still_fill() {
        // The tap runs no IO cycle while nothing plays. The mix keeps pace
        // with the mic rather than waiting for audio that never comes.
        let mut mix = Mixer::new(2);
        mix.push_at(0, &[0.1, 0.2, 0.3], 0);
        mix.push_at(1, &[1.0], 2);
        assert_eq!(mix.finish(), vec![0.1, 0.2, 1.3]);
    }

    #[test]
    fn push_reports_the_silence_it_leaves_in_front_of_a_lane() {
        // The lane's own file has to be padded with the same silence, or it
        // ends up shorter than the mix and no longer lines up with it.
        let mut mix = Mixer::new(2);
        assert_eq!(mix.push_at(0, &[0.1, 0.2], 0), 0);
        assert_eq!(mix.push_at(0, &[0.3], 5), 3, "3 samples of dropout");
        assert_eq!(mix.push_at(1, &[0.5], 4), 4, "lane 1 opened 4 samples in");
    }

    #[test]
    fn lag_holds_back_the_tail_for_audio_still_in_flight() {
        // Lane 1 has not been polled yet when the drain happens; the held-back
        // tail is what lets its audio still land in the right place.
        let mut mix = Mixer::new(2);
        mix.push_at(0, &[0.1, 0.2, 0.3, 0.4], 0);
        assert_eq!(mix.drain(2), vec![0.1, 0.2]);
        mix.push_at(1, &[0.5, 0.5], 2);
        assert_eq!(mix.finish(), vec![0.8, 0.9]);
    }

    #[test]
    fn a_burst_after_a_quiet_stretch_does_not_stretch_the_timeline() {
        // A lane that hands over three samples in one poll and none in the
        // next stays exactly as long as the audio it carries.
        let mut mix = Mixer::new(2);
        let mut out = Vec::new();
        mix.push_at(0, &[0.1], 0);
        mix.push_at(1, &[0.5, 0.5, 0.5], 0);
        out.extend(mix.drain(2));
        mix.push_at(0, &[0.1], 1);
        out.extend(mix.drain(2));
        mix.push_at(0, &[0.1], 2);
        mix.push_at(1, &[0.5], 3);
        out.extend(mix.finish());
        assert_eq!(out.len(), 4, "as long as the furthest lane, no longer");
        assert_eq!(out, vec![0.6, 0.6, 0.6, 0.5]);
    }

    #[test]
    fn audio_for_a_moment_already_written_lands_at_the_head() {
        // Later than the lag allowed for: the only alternatives are dropping
        // it or rewriting audio already on disk, so it plays a touch late.
        let mut mix = Mixer::new(2);
        mix.push_at(0, &[0.1, 0.2], 0);
        assert_eq!(mix.drain(0), vec![0.1, 0.2]);
        mix.push_at(1, &[0.5, 0.5], 0);
        assert_eq!(mix.finish(), vec![0.5, 0.5]);
    }

    #[test]
    fn drain_emits_nothing_when_no_lane_has_advanced() {
        let mut mix = Mixer::new(2);
        assert!(mix.drain(0).is_empty());
        assert!(mix.finish().is_empty());
        assert_eq!(mix.position(), 0);
    }

    #[test]
    fn position_is_how_much_has_been_written_out() {
        let mut mix = Mixer::new(1);
        mix.push_at(0, &[0.1; 10], 0);
        mix.drain(4);
        assert_eq!(mix.position(), 6);
        mix.finish();
        assert_eq!(mix.position(), 10);
    }
}
