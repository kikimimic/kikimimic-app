//! Streaming resampler to Whisper's 16 kHz mono input rate.
//!
//! Wraps rubato's windowed-sinc resampler (anti-aliased; naive linear
//! interpolation folds everything above the target Nyquist back into the
//! band whisper reads). State carries across arbitrary-size chunks: cpal
//! hands audio over in ~10ms callbacks, and resampling each chunk
//! independently would reset the phase and drop fractional samples at
//! every chunk boundary.

use anyhow::{Context, Result};
use rubato::audioadapter_buffers::direct::SequentialSliceOfSlices;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

pub const TARGET_RATE: u32 = 16_000;

/// Input frames per rubato call; input between multiples is buffered
/// (adds at most CHUNK/src_rate ≈ 21ms at 48 kHz — below the decode step).
const CHUNK: usize = 1024;

pub struct StreamResampler {
    /// None: the source already runs at the target rate, pass through
    /// untouched.
    inner: Option<Async<f32>>,
    pending: Vec<f32>,
}

impl StreamResampler {
    /// To Whisper's input rate.
    pub fn new(src_rate: u32) -> Result<Self> {
        Self::to(src_rate, TARGET_RATE)
    }

    /// To an arbitrary rate — the recorder keeps its files at a fixed rate of
    /// its own so the lanes and their mix share one format.
    pub fn to(src_rate: u32, dst_rate: u32) -> Result<Self> {
        let inner = if src_rate == dst_rate {
            None
        } else {
            let params = SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: None, // auto: highest cutoff that keeps aliasing below the window's sidelobes
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::Blackman2,
            };
            Some(
                Async::new_sinc(
                    dst_rate as f64 / src_rate as f64,
                    1.0,
                    &params,
                    CHUNK,
                    1,
                    FixedAsync::Input,
                )
                .with_context(|| format!("resampler for {src_rate} Hz → {dst_rate} Hz"))?,
            )
        };
        Ok(Self {
            inner,
            pending: Vec::new(),
        })
    }

    /// Feed one chunk of mono samples, of any length; returns the 16 kHz
    /// audio that became available. Up to CHUNK-1 input samples stay
    /// buffered until the next call.
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let Some(inner) = &mut self.inner else {
            return Ok(samples.to_vec());
        };
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        let mut consumed = 0;
        while self.pending.len() - consumed >= CHUNK {
            let bufs = [&self.pending[consumed..consumed + CHUNK]];
            let input = SequentialSliceOfSlices::new(&bufs, 1, CHUNK)
                .map_err(|e| anyhow::anyhow!("adapter: {e:?}"))?;
            let resampled = inner.process(&input, None).context("resample chunk")?;
            out.extend_from_slice(&resampled.take_data());
            consumed += CHUNK;
        }
        self.pending.drain(..consumed);
        Ok(out)
    }

    /// Push out what is still buffered: the sub-CHUNK pending remainder and
    /// the audio inside the sinc filter's delay line, both driven through
    /// with zero-padding. Call when capture stops. The resampler stays
    /// usable afterwards, but the injected silence breaks stream
    /// continuity, so treat it as the end of the session's audio.
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        if self.inner.is_none() {
            return Ok(Vec::new());
        }
        // One extra full chunk of silence flushes the filter delay
        // (~sinc_len input samples, far below CHUNK).
        let pad = (CHUNK - self.pending.len() % CHUNK) % CHUNK + CHUNK;
        self.process(&vec![0.0; pad])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (i as f32 / rate as f32 * freq * std::f32::consts::TAU).sin())
            .collect()
    }

    #[test]
    fn same_rate_passes_through_unchanged() {
        let mut rs = StreamResampler::new(16_000).unwrap();
        let out = rs.process(&[0.1, 0.2, 0.3]).unwrap();
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn chunked_processing_equals_whole_processing() {
        // The bug this pins down: per-chunk resampling that resets phase or
        // drops fractional samples at chunk boundaries. Output must depend
        // only on the audio, not on how it was split.
        let input = sine(440.0, 44_100, 1.0);
        let mut whole = StreamResampler::new(44_100).unwrap();
        let expected = whole.process(&input).unwrap();
        for chunk_size in [480usize, 997] {
            let mut chunked = StreamResampler::new(44_100).unwrap();
            let mut out = Vec::new();
            for chunk in input.chunks(chunk_size) {
                out.extend(chunked.process(chunk).unwrap());
            }
            assert_eq!(out, expected, "chunk_size {chunk_size}");
        }
    }

    #[test]
    fn flush_pushes_out_the_buffered_tail_and_filter_delay() {
        // Stopping mid-chunk must not silently drop the pending remainder
        // (up to CHUNK-1 samples) nor the audio inside the sinc delay line.
        let mut rs = StreamResampler::new(48_000).unwrap();
        let mut out = rs.process(&vec![0.5; 1536]).unwrap();
        assert!(out.len() < 1536 / 3); // the 512-sample tail is still buffered
        let flushed = rs.flush().unwrap();
        out.extend(&flushed);
        assert!(out.len() >= 1536 / 3, "got {}", out.len());
        assert!(
            flushed.iter().any(|s| s.abs() > 0.4),
            "flush lost the tail audio"
        );
    }

    #[test]
    fn flush_at_target_rate_returns_nothing() {
        let mut rs = StreamResampler::new(16_000).unwrap();
        rs.process(&[0.1, 0.2]).unwrap();
        assert!(rs.flush().unwrap().is_empty());
    }

    #[test]
    fn output_length_tracks_the_rate_ratio() {
        let input = sine(440.0, 48_000, 3.0);
        let mut rs = StreamResampler::new(48_000).unwrap();
        let out = rs.process(&input).unwrap();
        // 144000 in → 140 full chunks consumed → ~47787 out
        let consumed = (input.len() / CHUNK) * CHUNK;
        let expected = consumed / 3;
        assert!(
            (out.len() as i64 - expected as i64).abs() < 200,
            "got {} expected ≈{expected}",
            out.len()
        );
    }

    #[test]
    fn upsamples_to_an_arbitrary_target_rate() {
        // The recorder pulls 44.1 kHz devices up to its own 48 kHz files.
        let input = sine(440.0, 44_100, 1.0);
        let mut rs = StreamResampler::to(44_100, 48_000).unwrap();
        let out = rs.process(&input).unwrap();
        let consumed = (input.len() / CHUNK) * CHUNK;
        let expected = consumed * 48_000 / 44_100;
        assert!(
            (out.len() as i64 - expected as i64).abs() < 200,
            "got {} expected ≈{expected}",
            out.len()
        );
    }

    #[test]
    fn preserves_amplitude_of_in_band_audio() {
        // 440 Hz is far below the 8 kHz output Nyquist: it must come
        // through at full amplitude (RMS of a unit sine is 1/√2).
        let input = sine(440.0, 44_100, 1.0);
        let mut rs = StreamResampler::new(44_100).unwrap();
        let out = rs.process(&input).unwrap();
        let mid = &out[2000..out.len() - 2000];
        let rms = (mid.iter().map(|s| s * s).sum::<f32>() / mid.len() as f32).sqrt();
        assert!(
            (rms - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.05,
            "rms {rms}"
        );
    }
}
