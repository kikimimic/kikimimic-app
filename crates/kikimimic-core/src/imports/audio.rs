use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{Context, Result};

use crate::resample::StreamResampler;

pub(super) const RECORD_RATE: u32 = 48_000;
pub(super) const CLIP_SAMPLES: usize = 30 * 16_000;
const BLOCK: usize = 4096;

type Reader = hound::WavReader<BufReader<std::fs::File>>;

pub(super) fn open(path: &Path) -> Result<Reader> {
    let reader =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let s = reader.spec();
    anyhow::ensure!(
        (1..=32).contains(&s.channels) && (8_000..=192_000).contains(&s.sample_rate),
        "unsupported WAV channels/rate"
    );
    anyhow::ensure!(
        matches!(
            (s.sample_format, s.bits_per_sample),
            (hound::SampleFormat::Float, 32) | (hound::SampleFormat::Int, 8 | 16 | 24 | 32)
        ),
        "unsupported WAV sample format"
    );
    anyhow::ensure!(reader.duration() > 0, "audio is empty");
    Ok(reader)
}

pub(super) fn preflight_join(paths: &[std::path::PathBuf]) -> Result<()> {
    let mut spec = None;
    for path in paths {
        anyhow::ensure!(super::is_wav(path), "joining requires WAV files");
        let current = open(path)?.spec();
        anyhow::ensure!(
            spec.is_none_or(|s| s == current),
            "joined WAV formats must match (rate, channels, sample type and bit depth)"
        );
        spec = Some(current);
    }
    Ok(())
}

/// Trim the resampler's filter delay and padded tail so original frame zero
/// and the duration survive conversion. Memory is bounded by one input block.
fn resample(
    reader: &mut Reader,
    rate: u32,
    mut write: impl FnMut(&[f32]) -> Result<()>,
) -> Result<()> {
    let spec = reader.spec();
    let target = u64::from(reader.duration()) * u64::from(rate) / u64::from(spec.sample_rate);
    let mut rs = StreamResampler::to(spec.sample_rate, rate)?;
    let mut skip = rs.output_delay();
    let mut left = target as usize;
    let mut emit = |samples: &[f32]| -> Result<()> {
        let skipped = skip.min(samples.len());
        skip -= skipped;
        let samples = &samples[skipped..];
        let count = samples.len().min(left);
        write(&samples[..count])?;
        left -= count;
        Ok(())
    };
    let mut block = Vec::with_capacity(BLOCK);
    let mut frame = 0u16;
    let mut sum = 0.0;
    let count = u64::from(reader.duration()) * u64::from(spec.channels);
    let mut samples = reader.samples::<i32>();
    // Hound has typed iterators: float needs its own reader loop.
    if spec.sample_format == hound::SampleFormat::Float {
        let mut seen = 0;
        for s in reader.samples::<f32>() {
            let value = s?;
            anyhow::ensure!(value.is_finite(), "non-finite WAV sample");
            sum += value;
            frame += 1;
            seen += 1;
            if frame == spec.channels {
                block.push(sum / f32::from(spec.channels));
                sum = 0.0;
                frame = 0;
            }
            if block.len() == BLOCK {
                emit(&rs.process(&block)?)?;
                block.clear();
            }
        }
        anyhow::ensure!(seen == count, "truncated WAV data");
    } else {
        let scale = 2f32.powi(i32::from(spec.bits_per_sample) - 1);
        let mut seen = 0;
        for s in &mut samples {
            sum += s? as f32 / scale;
            frame += 1;
            seen += 1;
            if frame == spec.channels {
                block.push(sum / f32::from(spec.channels));
                sum = 0.0;
                frame = 0;
            }
            if block.len() == BLOCK {
                emit(&rs.process(&block)?)?;
                block.clear();
            }
        }
        anyhow::ensure!(seen == count, "truncated WAV data");
    }
    anyhow::ensure!(frame == 0, "incomplete WAV frame");
    emit(&rs.process(&block)?)?;
    emit(&rs.flush()?)?;
    anyhow::ensure!(left == 0, "resampler did not produce the complete audio");
    Ok(())
}

pub(super) fn normalize(paths: &[std::path::PathBuf], output: &Path) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RECORD_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(BufWriter::new(std::fs::File::create(output)?), spec)?;
    let mut total = 0u64;
    for path in paths {
        let mut reader = open(path)?;
        total += u64::from(reader.duration()) * u64::from(RECORD_RATE)
            / u64::from(reader.spec().sample_rate);
        anyhow::ensure!(
            total < (u64::from(u32::MAX) - 64) / 2,
            "audio exceeds PCM WAV size limit; split into separate imports"
        );
        resample(&mut reader, RECORD_RATE, |samples| {
            for s in samples {
                writer.write_sample((s.clamp(-1., 1.) * i16::MAX as f32) as i16)?;
            }
            Ok(())
        })?;
    }
    writer.finalize()?;
    Ok(())
}

pub(super) fn clips(
    path: &Path,
    mut process: impl FnMut(Vec<f32>, u64) -> Result<()>,
) -> Result<()> {
    let mut reader = open(path)?;
    let mut clip = Vec::with_capacity(CLIP_SAMPLES);
    let mut offset = 0u64;
    resample(&mut reader, 16_000, |samples| {
        for &sample in samples {
            clip.push(sample);
            if clip.len() == CLIP_SAMPLES {
                process(
                    std::mem::replace(&mut clip, Vec::with_capacity(CLIP_SAMPLES)),
                    offset,
                )?;
                offset += CLIP_SAMPLES as u64;
            }
        }
        Ok(())
    })?;
    if !clip.is_empty() {
        process(clip, offset)?;
    }
    Ok(())
}
