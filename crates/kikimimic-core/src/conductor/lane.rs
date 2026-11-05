//! One capture lane inside a session: draining its ring, placing its audio on
//! the session's timeline, and the bookkeeping that keeps the UI honest.
//! Ported from v2's `src-tauri/src/pipeline.rs` with its tests.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::notices::{NoticeKey, Notices};
use super::record::{SessionRecorder, RECORD_RATE};
use crate::capture::{Anchor, CaptureStream};
use crate::domain::{HostNanos, LaneId, LaneLabel, LaneRole, Pcm16k, StreamMs, UtteranceId};
use crate::engine::AsrStep;
use crate::event::Notice;
use crate::resample::StreamResampler;

/// Two anchors further apart than this from what the device rate predicts
/// mark a real hole in the lane's audio, not timestamp jitter.
const ANCHOR_JUMP_NANOS: u64 = 30_000_000;

/// How long an overrun notice stays up after the last overrun.
const OVERRUN_NOTICE: Duration = Duration::from_secs(5);

/// How long a system lane may deliver nothing but digital silence before we
/// call it broken. Long enough that a quiet stretch in a meeting doesn't trip it.
pub(crate) const SILENCE_GRACE: Duration = Duration::from_secs(20);

/// Below this, a change in a lane's stream-to-recording offset is clock
/// wobble, not a dropout: the 16 kHz resampler holds back up to ~21 ms.
const TIMELINE_SLACK_MS: u64 = 30;

pub(crate) struct Lane {
    pub id: LaneId,
    pub label: LaneLabel,
    stream: CaptureStream,
    /// Frames taken from the ring so far: what the capture anchors count in.
    frames_read: u64,
    trail: AnchorTrail,
    /// Where this lane's audio is expected to continue, for the rare poll
    /// with no anchor to go on.
    next_position: usize,
    resampler: StreamResampler,
    mono: Vec<f32>,
    reported_drops: usize,
    overrun_until: Option<Duration>,
    pub gate: EmitGate,
    /// Audio handed to the recognizer so far, in 16 kHz samples.
    stream_samples: usize,
    pub timeline: LaneTimeline,
    /// When this lane is next due to step. Per lane, so one lane's slow
    /// decode delays only itself.
    pub next_step: Duration,
    /// A step is out with the recognizer; don't send another.
    pub stepping: bool,
    silence: Option<SilenceWatch>,
    /// The utterance currently being spoken on this lane, assigned at its
    /// first text and cleared when it finishes.
    pub current: Option<UtteranceId>,
}

impl Lane {
    pub fn new(
        id: LaneId,
        label: LaneLabel,
        role: LaneRole,
        stream: CaptureStream,
        now: Duration,
    ) -> anyhow::Result<Self> {
        let resampler = StreamResampler::new(stream.format.rate)?;
        Ok(Self {
            id,
            label,
            stream,
            frames_read: 0,
            trail: AnchorTrail::default(),
            next_position: 0,
            resampler,
            mono: Vec::new(),
            reported_drops: 0,
            overrun_until: None,
            gate: EmitGate::default(),
            stream_samples: 0,
            timeline: LaneTimeline::default(),
            next_step: now,
            stepping: false,
            silence: matches!(role, LaneRole::System).then(SilenceWatch::default),
            current: None,
        })
    }

    pub fn src_rate(&self) -> u32 {
        self.stream.format.rate
    }

    fn stream_ms(&self) -> u64 {
        self.stream_samples as u64 * 1000 / Pcm16k::RATE as u64
    }

    /// Move captured audio to the recorder and out for recognition, and
    /// report overruns. Fails when the stream itself failed (device
    /// unplugged): otherwise the session would listen to nothing forever.
    pub fn pump(
        &mut self,
        now: Duration,
        index: usize,
        clock: &mut SessionClock,
        mut rec: Option<&mut SessionRecorder>,
        notices: &mut Notices,
    ) -> anyhow::Result<Option<Pcm16k>> {
        if let Some(msg) = self.stream.fault.lock().unwrap().take() {
            anyhow::bail!("input stream failed: {msg}");
        }
        let mut out = Vec::new();
        // One pass per contiguous run: drain() stops at a time jump, and the
        // next pass places what follows by its own anchor.
        loop {
            let captured = self.drain();
            if self.mono.is_empty() {
                break;
            }
            if let Some(watch) = self.silence.as_mut() {
                match watch.observe(&self.mono, now) {
                    Some(SilenceEvent::Broken) => notices.set(
                        NoticeKey::Silent(self.id),
                        Some(Notice::SystemAudioSilent {
                            lane: self.label.clone(),
                        }),
                    ),
                    Some(SilenceEvent::Recovered) => notices.set(NoticeKey::Silent(self.id), None),
                    None => false,
                };
            }
            let at = match captured {
                Some(nanos) => clock.position(nanos),
                None => self.next_position,
            };
            self.next_position = at + self.record_samples(self.mono.len());
            // Recorded ahead of any VAD: the files hold what the device heard.
            if let Some(rec) = rec.as_deref_mut() {
                rec.write_at(index, &self.mono, at);
            }
            let resampled = self.resampler.process(&self.mono)?;
            let at_ms = at as u64 * 1000 / RECORD_RATE as u64;
            self.timeline
                .observe(self.stream_ms(), at_ms.saturating_sub(self.stream_ms()));
            self.stream_samples += resampled.len();
            out.extend_from_slice(&resampled);
            self.mono.clear();
        }
        self.report_overruns(now, notices);
        Ok((!out.is_empty()).then(|| Pcm16k::assume_16k(out)))
    }

    /// Stop: what the resampler still holds.
    pub fn flush(&mut self) -> anyhow::Result<Option<Pcm16k>> {
        let tail = self.resampler.flush()?;
        self.stream_samples += tail.len();
        Ok((!tail.is_empty()).then(|| Pcm16k::assume_16k(tail)))
    }

    fn report_overruns(&mut self, now: Duration, notices: &mut Notices) {
        let drops = self.stream.dropped.load(Ordering::Relaxed);
        if drops > self.reported_drops {
            let fmt = self.stream.format;
            let ms = drops as u64 * 1000 / (fmt.channels as u64 * fmt.rate as u64);
            notices.set(
                NoticeKey::Overrun(self.id),
                Some(Notice::Overrun {
                    lane: self.label.clone(),
                    dropped_ms: ms,
                }),
            );
            self.reported_drops = drops;
            self.overrun_until = Some(now + OVERRUN_NOTICE);
        } else if self.overrun_until.is_some_and(|until| now >= until) {
            notices.set(NoticeKey::Overrun(self.id), None);
            self.overrun_until = None;
        }
    }

    fn record_samples(&self, samples: usize) -> usize {
        samples * RECORD_RATE as usize / self.stream.format.rate as usize
    }

    /// Move the ring's next contiguous run of audio into `mono`, downmixed,
    /// and report when its first frame was captured. Stops at a time jump:
    /// read past one, the hole would collapse and everything after it would
    /// sit early in the timeline.
    fn drain(&mut self) -> Option<HostNanos> {
        let channels = self.stream.format.channels as usize;
        let mut frames = self.stream.audio.slots() / channels;
        if frames == 0 {
            return None;
        }
        let rate = self.stream.format.rate;
        let captured = self
            .trail
            .nanos_at(&mut self.stream.times, self.frames_read, rate);
        let limit = self.frames_read + frames as u64;
        if let Some(jump) = self.trail.jump_before(&mut self.stream.times, limit, rate) {
            frames = (jump - self.frames_read) as usize;
        }
        let n = frames * channels;
        let Ok(chunk) = self.stream.audio.read_chunk(n) else {
            return None;
        };
        let (a, b) = chunk.as_slices();
        let mut samples = a.iter().chain(b.iter());
        self.mono.reserve(frames);
        for _ in 0..frames {
            let mut frame = 0.0f32;
            for _ in 0..channels {
                frame += samples.next().expect("n is a multiple of channels");
            }
            self.mono.push(frame / channels as f32);
        }
        chunk.commit_all();
        self.frames_read += frames as u64;
        captured
    }
}

/// The session's zero: the first captured moment shown to it. Every position
/// in the recording and the transcript is measured from it.
#[derive(Default)]
pub(crate) struct SessionClock {
    zero: Option<HostNanos>,
}

impl SessionClock {
    /// Where a captured moment sits in the recording, in record-rate samples.
    pub fn position(&mut self, at: HostNanos) -> usize {
        let zero = *self.zero.get_or_insert(at);
        let since = at.0.saturating_sub(zero.0) as u128;
        (since * RECORD_RATE as u128 / 1_000_000_000) as usize
    }
}

/// Follows a lane's capture anchors as its audio is read, so any frame in
/// the ring can be given the time the OS says it was captured.
#[derive(Default)]
struct AnchorTrail {
    current: Option<Anchor>,
}

impl AnchorTrail {
    /// When frame `frames_read` was captured. Passed anchors are dropped as
    /// the reader goes, which keeps the backend's ring of them from filling.
    fn nanos_at(
        &mut self,
        times: &mut rtrb::Consumer<Anchor>,
        frames_read: u64,
        rate: u32,
    ) -> Option<HostNanos> {
        while let Ok(next) = times.peek() {
            if next.frame > frames_read {
                break;
            }
            self.current = Some(*next);
            let _ = times.pop();
        }
        let anchor = self.current?;
        let ahead = frames_read.saturating_sub(anchor.frame);
        Some(HostNanos(
            anchor.nanos.0 + ahead * 1_000_000_000 / rate as u64,
        ))
    }

    /// The frame of the first pending anchor before `limit` whose time
    /// disagrees with the current clock: a real hole in the lane's audio.
    fn jump_before(
        &mut self,
        times: &mut rtrb::Consumer<Anchor>,
        limit: u64,
        rate: u32,
    ) -> Option<u64> {
        let mut current = self.current?;
        while let Ok(&next) = times.peek() {
            if next.frame >= limit {
                return None;
            }
            let expected =
                current.nanos.0 + (next.frame - current.frame) * 1_000_000_000 / rate as u64;
            if next.nanos.0.abs_diff(expected) > ANCHOR_JUMP_NANOS {
                return Some(next.frame);
            }
            current = next;
            self.current = Some(next);
            let _ = times.pop();
        }
        None
    }
}

#[derive(Debug, PartialEq)]
enum SilenceEvent {
    /// Digital silence lasted the grace period: the permission is withheld.
    Broken,
    /// Real audio arrived after a report.
    Recovered,
}

/// macOS withholds the system-audio permission by zero-filling buffers, so
/// every call succeeds and the transcript just stays empty. The tell is
/// samples that arrive and are all zero; an idle output device delivers no
/// samples at all, so an empty slice must not count.
#[derive(Default)]
struct SilenceWatch {
    silent_since: Option<Duration>,
    reported: bool,
}

impl SilenceWatch {
    fn observe(&mut self, samples: &[f32], now: Duration) -> Option<SilenceEvent> {
        if samples.is_empty() {
            return None;
        }
        if samples.iter().any(|s| *s != 0.0) {
            self.silent_since = None;
            return std::mem::take(&mut self.reported).then_some(SilenceEvent::Recovered);
        }
        let since = *self.silent_since.get_or_insert(now);
        if !self.reported && now.saturating_sub(since) >= SILENCE_GRACE {
            self.reported = true;
            return Some(SilenceEvent::Broken);
        }
        None
    }
}

/// Converts a lane's own clock (audio the recognizer was given) into the
/// recording's. They differ by the silence the recorder inserted: the lane's
/// late start, plus every stretch its device delivered nothing.
#[derive(Default)]
pub(crate) struct LaneTimeline {
    /// (stream ms from which it applies, offset ms), oldest first.
    anchors: Vec<(u64, u64)>,
}

impl LaneTimeline {
    fn observe(&mut self, stream_ms: u64, offset_ms: u64) {
        if self
            .anchors
            .last()
            .is_none_or(|(_, at)| at.abs_diff(offset_ms) > TIMELINE_SLACK_MS)
        {
            self.anchors.push((stream_ms, offset_ms));
        }
    }

    /// Where `stream` sits in the recording. Queries arrive in stream order,
    /// which is what lets spent anchors be dropped.
    pub fn recording_ms(&mut self, stream: StreamMs) -> u64 {
        let stream_ms = stream.0;
        while self.anchors.len() > 1 && self.anchors[1].0 <= stream_ms {
            self.anchors.remove(0);
        }
        let offset = match self.anchors.first() {
            Some((from, offset)) if *from <= stream_ms => *offset,
            _ => 0,
        };
        stream_ms + offset
    }
}

/// Decides which steps reach the UI. Idle steps are dropped, but an empty
/// step must still go out while a volatile tail is on screen: LocalAgreement
/// returns an empty volatile to hide a contradicted tail.
#[derive(Default)]
pub(crate) struct EmitGate {
    pub volatile_on_screen: bool,
}

impl EmitGate {
    pub fn should_emit(&mut self, out: &AsrStep) -> bool {
        let has_news =
            !out.committed_delta.is_empty() || !out.volatile.is_empty() || out.utterance.is_some();
        let clears_tail = self.volatile_on_screen && out.volatile.is_empty();
        if !(has_news || clears_tail) {
            return false;
        }
        self.volatile_on_screen = !out.volatile.is_empty();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{channel, Format};
    use crate::engine::Recognized;

    const RATE: u32 = 48_000;
    const MS: u64 = 1_000_000;

    fn out(committed: &str, volatile: &str) -> AsrStep {
        AsrStep {
            committed_delta: committed.into(),
            volatile: volatile.into(),
            utterance: None,
        }
    }

    fn anchors(entries: &[(u64, u64)]) -> (rtrb::Producer<Anchor>, rtrb::Consumer<Anchor>) {
        let (mut tx, rx) = rtrb::RingBuffer::new(16);
        for (frame, nanos) in entries {
            tx.push(Anchor {
                frame: *frame,
                nanos: HostNanos(*nanos),
            })
            .unwrap();
        }
        (tx, rx)
    }

    #[test]
    fn gate_drops_idle_steps_but_passes_a_volatile_clear() {
        let mut gate = EmitGate::default();
        assert!(!gate.should_emit(&out("", "")));
        assert!(gate.should_emit(&out("", "こんにち")));
        assert!(gate.should_emit(&out("", "")));
        assert!(!gate.should_emit(&out("", "")));
    }

    #[test]
    fn gate_tracks_the_tail_across_commits() {
        let mut gate = EmitGate::default();
        assert!(gate.should_emit(&out("", "こんにち")));
        assert!(gate.should_emit(&out("こんにちは、", "せ")));
        assert!(gate.should_emit(&out("世界", "")));
        assert!(!gate.should_emit(&out("", "")));
    }

    #[test]
    fn gate_passes_an_utterance_final_even_without_text_deltas() {
        let mut gate = EmitGate::default();
        let fin = AsrStep {
            utterance: Some(Recognized {
                text: "x".into(),
                start: StreamMs(0),
                end: StreamMs(1),
                lang: None,
            }),
            ..AsrStep::default()
        };
        assert!(gate.should_emit(&fin));
    }

    #[test]
    fn digital_silence_for_long_enough_reports_a_withheld_permission() {
        let mut watch = SilenceWatch::default();
        let t0 = Duration::from_secs(1);
        assert_eq!(watch.observe(&[0.0; 16], t0), None);
        assert_eq!(
            watch.observe(&[0.0; 16], t0 + SILENCE_GRACE),
            Some(SilenceEvent::Broken)
        );
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE * 2), None);
    }

    #[test]
    fn audio_arriving_after_a_report_takes_the_notice_down() {
        let mut watch = SilenceWatch::default();
        let t0 = Duration::from_secs(1);
        watch.observe(&[0.0; 16], t0);
        assert_eq!(
            watch.observe(&[0.0; 16], t0 + SILENCE_GRACE),
            Some(SilenceEvent::Broken)
        );
        assert_eq!(
            watch.observe(&[0.1; 16], t0 + SILENCE_GRACE * 2),
            Some(SilenceEvent::Recovered)
        );
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE * 3), None);
    }

    #[test]
    fn an_idle_device_delivering_no_samples_is_not_silence() {
        let mut watch = SilenceWatch::default();
        assert_eq!(watch.observe(&[], Duration::ZERO), None);
        assert_eq!(watch.observe(&[], SILENCE_GRACE * 10), None);
    }

    #[test]
    fn audible_samples_clear_a_pending_silence_run() {
        let mut watch = SilenceWatch::default();
        let t0 = Duration::from_secs(1);
        watch.observe(&[0.0; 16], t0);
        assert_eq!(watch.observe(&[0.0, 0.2], t0 + SILENCE_GRACE / 2), None);
        assert_eq!(watch.observe(&[0.0; 16], t0 + SILENCE_GRACE), None);
    }

    #[test]
    fn a_frame_is_timed_from_the_anchor_it_arrived_under() {
        let (_tx, mut times) = anchors(&[(0, 1_000 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(
            trail.nanos_at(&mut times, 0, RATE),
            Some(HostNanos(1_000 * MS))
        );
        assert_eq!(
            trail.nanos_at(&mut times, 480, RATE),
            Some(HostNanos(1_010 * MS))
        );
    }

    #[test]
    fn a_dropout_between_callbacks_shows_up_as_a_jump() {
        let (_tx, mut times) = anchors(&[(0, 1_000 * MS), (480, 2_000 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(
            trail.nanos_at(&mut times, 0, RATE),
            Some(HostNanos(1_000 * MS))
        );
        assert_eq!(
            trail.nanos_at(&mut times, 480, RATE),
            Some(HostNanos(2_000 * MS))
        );
        assert_eq!(
            trail.nanos_at(&mut times, 960, RATE),
            Some(HostNanos(2_010 * MS))
        );
    }

    #[test]
    fn anchors_the_reader_has_passed_are_consumed() {
        let (_tx, mut times) = anchors(&[(0, 0), (480, 10 * MS), (960, 20 * MS)]);
        let mut trail = AnchorTrail::default();
        assert_eq!(
            trail.nanos_at(&mut times, 960, RATE),
            Some(HostNanos(20 * MS))
        );
        assert_eq!(times.slots(), 0);
    }

    #[test]
    fn a_lane_that_has_delivered_nothing_yet_has_no_time() {
        let (_tx, mut times) = anchors(&[]);
        assert_eq!(AnchorTrail::default().nanos_at(&mut times, 0, RATE), None);
    }

    fn test_lane() -> (crate::capture::CaptureWriter, Lane) {
        let (w, s) = channel(
            Format {
                rate: RATE,
                channels: 1,
            },
            Duration::from_secs(1),
        );
        let lane = Lane::new(
            LaneId(0),
            LaneLabel::new(LaneRole::Mic, 1),
            LaneRole::Mic,
            s,
            Duration::ZERO,
        )
        .unwrap();
        (w, lane)
    }

    #[test]
    fn a_time_jump_inside_one_drain_splits_the_read() {
        let (mut w, mut lane) = test_lane();
        w.push(&[0.1; 480], HostNanos(1_000 * MS));
        w.push(&[0.2; 480], HostNanos(2_000 * MS));
        assert_eq!(lane.drain(), Some(HostNanos(1_000 * MS)));
        assert_eq!(lane.mono.len(), 480, "only up to the jump");
        lane.mono.clear();
        assert_eq!(
            lane.drain(),
            Some(HostNanos(2_000 * MS)),
            "the hole survives"
        );
        assert_eq!(lane.mono.len(), 480);
    }

    #[test]
    fn contiguous_callbacks_drain_as_one_chunk() {
        let (mut w, mut lane) = test_lane();
        w.push(&[0.1; 480], HostNanos(1_000 * MS));
        w.push(&[0.2; 480], HostNanos(1_010 * MS));
        assert_eq!(lane.drain(), Some(HostNanos(1_000 * MS)));
        assert_eq!(lane.mono.len(), 960);
        assert_eq!(lane.stream.times.slots(), 0);
    }

    #[test]
    fn the_session_clock_starts_at_the_first_moment_it_is_shown() {
        let mut clock = SessionClock::default();
        assert_eq!(clock.position(HostNanos(5_000 * MS)), 0);
        assert_eq!(
            clock.position(HostNanos(5_500 * MS)),
            RECORD_RATE as usize / 2
        );
        assert_eq!(clock.position(HostNanos(4_000 * MS)), 0);
    }

    #[test]
    fn a_lane_that_never_drops_out_is_its_own_recording_clock() {
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 0);
        timeline.observe(100, 0);
        assert_eq!(timeline.recording_ms(StreamMs(2_000)), 2_000);
    }

    #[test]
    fn a_lanes_late_start_shifts_everything_it_says() {
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 300);
        assert_eq!(timeline.recording_ms(StreamMs(0)), 300);
        assert_eq!(timeline.recording_ms(StreamMs(5_000)), 5_300);
    }

    #[test]
    fn offset_wobble_within_the_slack_is_not_a_dropout() {
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 100);
        timeline.observe(50, 115);
        timeline.observe(100, 92);
        assert_eq!(timeline.anchors.len(), 1);
        assert_eq!(timeline.recording_ms(StreamMs(200)), 300);
    }

    #[test]
    fn audio_from_before_a_dropout_keeps_its_old_offset() {
        let mut timeline = LaneTimeline::default();
        timeline.observe(0, 0);
        timeline.observe(4_000, 10_000);
        assert_eq!(timeline.recording_ms(StreamMs(3_000)), 3_000);
        assert_eq!(timeline.recording_ms(StreamMs(4_500)), 14_500);
        assert_eq!(timeline.anchors.len(), 1);
    }
}
