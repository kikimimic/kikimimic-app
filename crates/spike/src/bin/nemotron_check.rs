//! Does Nemotron Streaming keep up, and how far behind the audio does it
//! commit? Whisper cannot stream, so everything above it — the sliding
//! window, LocalAgreement, the scheduler — exists to fake it. An RNN-T
//! streams natively, and the question is whether that buys enough to be
//! worth a second ASR engine.
//!
//! transcribe.cpp vendors its own ggml, which cannot share a binary with
//! whisper-rs's (ADR-153805). Hence the exclusive feature:
//!
//!   cargo run --release -p spike --bin nemotron-check \
//!     --no-default-features --features parakeet [-- path/to/audio.wav]
//!
//! With no argument it takes mic.wav from the newest recorded session, so
//! the measurement runs on speech this app actually captured rather than on
//! a clean sample.
//!
//! Env: KKM_NEMOTRON_MODEL (gguf path), KKM_LANG (pin a language; unset =
//! let the model detect), KKM_CHUNK_MS (audio fed per call, default 320),
//! KKM_MAX_SECS (cap the audio, default 120 — sessions run to the hour),
//! KKM_VERBOSE (keep ggml's own logging).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use kikimimic_core::resample::{StreamResampler, TARGET_RATE};
use transcribe_cpp::{CommitPolicy, Model, RunOptions, StreamOptions};

/// Multiples of the encoder's 80ms frame (10ms hop x 8 subsampling). A chunk
/// is what a real capture callback would hand over, so it also sets how often
/// the UI could possibly update.
const DEFAULT_CHUNK_MS: usize = 320;

fn main() -> Result<()> {
    let wav = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => newest_mic_wav()?,
    };
    let model_path = model_path()?;
    let chunk_ms: usize = std::env::var("KKM_CHUNK_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CHUNK_MS);
    let lang = std::env::var("KKM_LANG").ok();

    println!("model: {}", model_path.display());
    println!("audio: {}", wav.display());

    if std::env::var("KKM_VERBOSE").is_err() {
        transcribe_cpp::disable_logging();
    }

    let mut pcm = read_16k_mono(&wav)?;
    // A meeting recording is an hour long and the measurement does not get
    // truer for running that long.
    let max_secs: usize = std::env::var("KKM_MAX_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    pcm.truncate(max_secs * TARGET_RATE as usize);
    let audio_ms = pcm.len() as u64 * 1000 / TARGET_RATE as u64;
    println!(
        "       {:.1}s at {}Hz mono",
        audio_ms as f64 / 1000.0,
        TARGET_RATE
    );

    let t0 = Instant::now();
    let model =
        Model::load(&model_path).with_context(|| format!("loading {}", model_path.display()))?;
    let mut session = model.session()?;
    println!(
        "load:  {:?}  arch={} variant={} backend={}",
        t0.elapsed(),
        model.arch(),
        model.variant(),
        model.backend()
    );

    let run = RunOptions {
        language: lang.clone(),
        ..RunOptions::default()
    };
    // Auto is the family's own stable-prefix rule. Whether we keep it or put
    // our LocalAgreement in front of it is the decision this spike informs,
    // so measure what the library does before replacing it.
    let stream_opts = StreamOptions {
        commit_policy: CommitPolicy::Auto,
        ..StreamOptions::default()
    };
    println!("lang:  {}", lang.as_deref().unwrap_or("(detect)"));

    let chunk = chunk_ms * TARGET_RATE as usize / 1000;
    let mut stream = session.stream(&run, &stream_opts)?;

    let mut feed_times = Vec::new();
    let mut first_text: Option<(u64, u128)> = None;
    let mut commit_lags = Vec::new();
    let mut committed_so_far = String::new();
    let wall0 = Instant::now();

    for (i, block) in pcm.chunks(chunk).enumerate() {
        let fed_ms = (i * chunk + block.len()) as u64 * 1000 / TARGET_RATE as u64;
        let t = Instant::now();
        let update = stream.feed(block)?;
        feed_times.push(t.elapsed());

        if update.result_changed && first_text.is_none() {
            first_text = Some((fed_ms, wall0.elapsed().as_millis()));
        }
        if update.committed_changed {
            // How much audio has been fed but not yet committed: the delay a
            // reader sees before a line stops being provisional.
            let lag = update.input_received_ms - update.audio_committed_ms;
            commit_lags.push(lag);
            let text = stream.text();
            if let Some(delta) = text.committed.strip_prefix(committed_so_far.as_str()) {
                if !delta.trim().is_empty() {
                    println!("  {:>6}ms  +{}ms  {}", fed_ms, lag, delta.trim());
                }
            }
            committed_so_far = text.committed;
        }
    }

    let t = Instant::now();
    stream.finalize()?;
    let finalize = t.elapsed();
    let wall = wall0.elapsed();
    let text = stream.text();

    let sum: u128 = feed_times.iter().map(|d| d.as_millis()).sum();
    let max = feed_times.iter().map(|d| d.as_millis()).max().unwrap_or(0);
    println!("\n--- {} chunks of {}ms ---", feed_times.len(), chunk_ms);
    println!(
        "feed:      mean {}ms  max {}ms  (a chunk must be under {}ms to keep up)",
        sum as usize / feed_times.len().max(1),
        max,
        chunk_ms
    );
    println!("finalize:  {finalize:?}");
    println!(
        "RTF:       {:.3}  ({:?} of wall for {:.1}s of audio)",
        wall.as_secs_f64() / (audio_ms as f64 / 1000.0),
        wall,
        audio_ms as f64 / 1000.0
    );
    if let Some((at_ms, wall_ms)) = first_text {
        println!("first text at {at_ms}ms of audio ({wall_ms}ms of wall)");
    }
    if !commit_lags.is_empty() {
        let mean = commit_lags.iter().sum::<i64>() / commit_lags.len() as i64;
        let worst = commit_lags.iter().copied().max().unwrap_or(0);
        println!("commit lag: mean {mean}ms  worst {worst}ms  (budget: final within 1500ms)");
    }
    println!("peak RSS:  {} MB", peak_rss_mb());
    println!("\n{}", text.full.trim());
    Ok(())
}

/// The newest session's mic lane. Real captured speech beats a clean sample:
/// the numbers only mean something on the audio the app actually gets.
fn newest_mic_wav() -> Result<PathBuf> {
    let base = std::env::var("KKM_RECORD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Music")
                .join("kikimimic")
        });
    let mut sessions: Vec<PathBuf> = std::fs::read_dir(&base)
        .with_context(|| format!("no recordings under {}", base.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("mic.wav").is_file())
        .collect();
    sessions.sort();
    let newest = sessions
        .pop()
        .with_context(|| format!("no session with a mic lane under {}", base.display()))?;
    Ok(newest.join("mic.wav"))
}

/// Where Handy already put it. Sharing the Hugging Face cache means the file
/// is not downloaded or copied twice.
fn model_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("KKM_NEMOTRON_MODEL") {
        return Ok(PathBuf::from(p));
    }
    let hub = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".cache/huggingface/hub")
        .join("models--handy-computer--nemotron-3.5-asr-streaming-0.6b-gguf/snapshots");
    let snapshot = std::fs::read_dir(&hub)
        .with_context(|| format!("no model under {}", hub.display()))?
        .flatten()
        .map(|e| e.path())
        .next()
        .context("no snapshot directory")?;
    std::fs::read_dir(&snapshot)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
        .context("no .gguf in the snapshot")
}

/// The recordings are 48kHz 16-bit; the model wants 16kHz f32. Reuse the
/// pipeline's resampler so the spike hears what the pipeline would.
fn read_16k_mono(path: &PathBuf) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    // Lanes are written mono, but do not assume it.
    let mono: Vec<f32> = if spec.channels == 1 {
        raw
    } else {
        raw.chunks(spec.channels as usize)
            .map(|f| f.iter().sum::<f32>() / f.len() as f32)
            .collect()
    };
    if spec.sample_rate == TARGET_RATE {
        return Ok(mono);
    }
    let mut resampler = StreamResampler::new(spec.sample_rate)?;
    let mut out = resampler.process(&mono)?;
    out.extend(resampler.flush()?);
    Ok(out)
}

fn peak_rss_mb() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    // ru_maxrss is bytes on macOS, kilobytes on Linux.
    #[cfg(target_os = "macos")]
    return (usage.ru_maxrss as u64) / (1024 * 1024);
    #[cfg(not(target_os = "macos"))]
    return (usage.ru_maxrss as u64) / 1024;
}
