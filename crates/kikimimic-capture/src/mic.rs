//! Microphone capture: owns the cpal stream on a dedicated thread (cpal
//! streams are !Send). The callback converts and copies into the core's
//! channel and nothing else: no allocation, no locks, no resampling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use kikimimic_core::capture::{channel, CaptureStream, CaptureWriter, Format};
use kikimimic_core::domain::HostNanos;
use kikimimic_core::CaptureHandle;

use crate::{Running, READY_TIMEOUT, RING};

/// Open the default input device.
pub fn open() -> Result<(CaptureStream, Box<dyn CaptureHandle>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let flag = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("kikimimic-mic".into())
        .spawn(move || {
            if let Err(e) = run(&flag, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
    let stream = ready_rx
        .recv_timeout(READY_TIMEOUT)
        .context("the microphone did not start in time")??;
    Ok((stream, Box::new(Running(stop))))
}

fn run(stop: &AtomicBool, ready: &mpsc::Sender<Result<CaptureStream>>) -> Result<()> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no default input device")?;
    let config = device.default_input_config()?;
    let format = Format {
        rate: config.sample_rate(),
        channels: config.channels(),
    };
    let (writer, stream) = channel(format, RING);
    let fault = writer.fault();
    let stream_config: cpal::StreamConfig = config.config();
    let cpal_stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, &stream_config, writer, fault),
        cpal::SampleFormat::I16 => build::<i16>(&device, &stream_config, writer, fault),
        cpal::SampleFormat::U16 => build::<u16>(&device, &stream_config, writer, fault),
        other => anyhow::bail!("unsupported input sample format: {other:?}"),
    }?;
    cpal_stream.play()?;
    let _ = ready.send(Ok(stream));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut writer: CaptureWriter,
    fault: kikimimic_core::capture::Fault,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            // `capture` is the callback's host time less the input latency:
            // when these frames were picked up, not when we woke up. On macOS
            // this is mach host time, the clock the Process Tap uses too.
            let at = HostNanos(info.timestamp().capture.as_nanos() as u64);
            writer.push_iter(data.iter().map(|s| f32::from_sample(*s)), at);
        },
        move |err| fault.fail(&err.to_string()),
        None,
    )?)
}
