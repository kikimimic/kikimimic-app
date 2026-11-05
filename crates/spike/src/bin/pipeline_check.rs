//! Headless end-to-end check of mic → resample → scheduler → whisper.
//! Play a known wav through the speakers while this runs and the committed
//! text should converge to its script.
//!
//! Usage: pipeline-check  (env: WHISPER_MODEL, KKM_LANG, CAPTURE_SECS)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use kikimimic_core::scheduler::StreamScheduler;

fn main() -> anyhow::Result<()> {
    let model = std::env::var("WHISPER_MODEL")
        .unwrap_or_else(|_| "models/ggml-large-v3-turbo-q8_0.bin".into());
    let lang = std::env::var("KKM_LANG").ok(); // unset = auto-detect per window
    let secs: u64 = std::env::var("CAPTURE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let allowed: Vec<String> = std::env::var("KKM_LANGS")
        .unwrap_or_else(|_| "ja,en".into())
        .split(',')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    eprintln!("loading {model} … (langs {allowed:?})");
    let mut engine = kikimimic_asr_whisper::WhisperEngine::load(&model, &allowed)?;
    let vad_model =
        std::env::var("KKM_VAD_MODEL").unwrap_or_else(|_| "models/ggml-silero-v5.1.2.bin".into());
    let mut vad = kikimimic_asr_whisper::SileroVad::load(&vad_model)?;
    eprintln!("capturing {secs}s from default input (lang={lang:?})");

    let pending: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(AtomicBool::new(true));
    {
        let pending = pending.clone();
        let running = running.clone();
        std::thread::spawn(move || {
            if let Err(e) = capture(&running, pending) {
                eprintln!("capture error: {e:#}");
                running.store(false, Ordering::SeqCst);
            }
        });
    }

    let mut scheduler = StreamScheduler::new();
    let mut committed = String::new();
    for _ in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
        {
            let mut queued = pending.lock().unwrap();
            let rms =
                (queued.iter().map(|s| s * s).sum::<f32>() / queued.len().max(1) as f32).sqrt();
            eprintln!("queued {} samples, rms {rms:.5}", queued.len());
            scheduler.push_audio(&queued);
            queued.clear();
        }
        if let Some(out) = scheduler.step(&mut engine, &mut vad, lang.as_deref())? {
            committed.push_str(&out.committed_delta);
            println!("committed: {committed}");
            println!("volatile : {}", out.volatile);
        }
    }
    running.store(false, Ordering::SeqCst);
    println!("FINAL: {committed}");
    Ok(())
}

fn capture(running: &AtomicBool, pending: Arc<Mutex<Vec<f32>>>) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default input device")?;
    let config = device.default_input_config()?;
    anyhow::ensure!(
        config.sample_format() == cpal::SampleFormat::F32,
        "unsupported input sample format: {:?}",
        config.sample_format()
    );
    let src_rate = config.sample_rate();
    let channels = config.channels() as usize;
    eprintln!("input: {src_rate} Hz, {channels} ch");

    let mut raw_count = 0usize;
    let mut last_report = std::time::Instant::now();
    let mut resampler = kikimimic_core::resample::StreamResampler::new(src_rate)?;
    let stream = device.build_input_stream(
        config.into(),
        move |data: &[f32], _info| {
            raw_count += data.len();
            if last_report.elapsed() >= Duration::from_secs(1) {
                eprintln!("raw input: {raw_count} samples/s");
                raw_count = 0;
                last_report = std::time::Instant::now();
            }
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect();
            match resampler.process(&mono) {
                Ok(resampled) => pending.lock().unwrap().extend_from_slice(&resampled),
                Err(e) => eprintln!("resample error: {e:#}"),
            }
        },
        |err| eprintln!("input stream error: {err}"),
        None,
    )?;
    stream.play()?;
    while running.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
