//! The capture boundary. OS backends only push samples with timestamps into a
//! channel the core provides; interpreting those timestamps (dropouts, the
//! shared session clock) is core logic, tested once for every platform.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::HostNanos;

/// The device's native format, as the backend delivers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
}

/// Where a run of frames was captured: frame index since the stream began,
/// and the host time of that frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub frame: u64,
    pub nanos: HostNanos,
}

/// Room for this many anchors; one per OS callback, read every 100 ms.
const ANCHOR_RING: usize = 1024;

/// A capture channel: the writer goes into the OS callback, the stream to
/// the core.
pub fn channel(format: Format, ring: Duration) -> (CaptureWriter, CaptureStream) {
    let samples = (format.rate as f64 * format.channels as f64 * ring.as_secs_f64()) as usize;
    let (audio_tx, audio_rx) = rtrb::RingBuffer::new(samples.max(1));
    let (times_tx, times_rx) = rtrb::RingBuffer::new(ANCHOR_RING);
    let dropped = Arc::new(AtomicUsize::new(0));
    let fault = Arc::new(Mutex::new(None));
    (
        CaptureWriter {
            audio: audio_tx,
            times: times_tx,
            frames: 0,
            channels: format.channels,
            dropped: Arc::clone(&dropped),
            fault: Arc::clone(&fault),
        },
        CaptureStream {
            audio: audio_rx,
            times: times_rx,
            format,
            dropped,
            fault,
        },
    )
}

/// Real-time safe: no allocation, no blocking lock on the hot path.
pub struct CaptureWriter {
    audio: rtrb::Producer<f32>,
    times: rtrb::Producer<Anchor>,
    frames: u64,
    channels: u16,
    dropped: Arc<AtomicUsize>,
    fault: Arc<Mutex<Option<String>>>,
}

impl CaptureWriter {
    /// Interleaved samples whose first frame was captured at `captured_at`.
    /// What does not fit is counted as dropped, never blocked on.
    pub fn push(&mut self, interleaved: &[f32], captured_at: HostNanos) {
        let channels = self.channels as usize;
        let room = self.audio.slots() / channels * channels;
        let take = interleaved.len().min(room) / channels * channels;
        if take > 0 {
            let _ = self.times.push(Anchor {
                frame: self.frames,
                nanos: captured_at,
            });
            if let Ok(chunk) = self.audio.write_chunk_uninit(take) {
                chunk.fill_from_iter(interleaved[..take].iter().copied());
            }
            self.frames += (take / channels) as u64;
        }
        if take < interleaved.len() {
            self.dropped
                .fetch_add(interleaved.len() - take, Ordering::Relaxed);
        }
    }

    /// The device failed (unplugged). The session stops listening to it.
    pub fn fail(&self, reason: &str) {
        if let Ok(mut slot) = self.fault.try_lock() {
            slot.get_or_insert_with(|| reason.to_string());
        }
    }
}

/// The core's read side. Consumed only by the conductor.
pub struct CaptureStream {
    pub(crate) audio: rtrb::Consumer<f32>,
    pub(crate) times: rtrb::Consumer<Anchor>,
    pub(crate) format: Format,
    pub(crate) dropped: Arc<AtomicUsize>,
    pub(crate) fault: Arc<Mutex<Option<String>>>,
}

impl CaptureStream {
    pub fn format(&self) -> Format {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_that_does_not_fit_is_counted_not_blocked_on() {
        let fmt = Format {
            rate: 10,
            channels: 2,
        };
        let (mut w, s) = channel(fmt, Duration::from_secs(1)); // 20 samples
        w.push(&[0.5; 30], HostNanos(0));
        assert_eq!(s.audio.slots(), 20);
        assert_eq!(s.dropped.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn each_push_is_anchored_at_its_first_frame() {
        let fmt = Format {
            rate: 100,
            channels: 1,
        };
        let (mut w, mut s) = channel(fmt, Duration::from_secs(1));
        w.push(&[0.0; 10], HostNanos(1_000));
        w.push(&[0.0; 10], HostNanos(2_000));
        assert_eq!(
            s.times.pop().unwrap(),
            Anchor {
                frame: 0,
                nanos: HostNanos(1_000)
            }
        );
        assert_eq!(
            s.times.pop().unwrap(),
            Anchor {
                frame: 10,
                nanos: HostNanos(2_000)
            }
        );
    }
}
