//! Spike S2: translation latency while recognition keeps the GPU busy, the
//! way a meeting does (two lanes decoding every second). Opt-in:
//!
//! ```sh
//! cargo build --release -p kikimimic-mt-llama-dylib
//! KKM_TRANSLATE_DYLIB="$PWD/target/release/libkikimimic_mt_llama_dylib.dylib" \
//! KKM_MODELS_DIR=/path/to/whisper/models KKM_TEST_AUDIO=/path/to/assets \
//! KKM_QWEN_GGUF=... KKM_PLAMO_GGUF=... \
//!   cargo test --release -p kikimimic-mt-llama --test concurrent -- --ignored --nocapture --test-threads=1
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kikimimic_core::domain::{LaneId, Lang, Pcm16k, Spoken};
use kikimimic_core::engine::{AsrEngine, MtEngine, MtRequest};
use kikimimic_mt_llama::{LlamaMt, PromptFormat};

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

const SENTENCES: &[&str] = &[
    "The implementation of the new search feature was completed on schedule.",
    "Next week we plan to start working on performance improvements.",
    "Do you have any questions about the schedule?",
    "We may need to revisit the priorities depending on customer feedback.",
];

fn audio() -> Vec<f32> {
    let mut r = hound::WavReader::open(env_path("KKM_TEST_AUDIO").join("test-en.wav")).unwrap();
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32)
        .collect()
}

/// Two lanes stepping once per second of audio, as fast as the GPU allows,
/// until `stop`. Returns each step's duration.
fn recognize_until(stop: Arc<AtomicBool>) -> Vec<Duration> {
    let models = env_path("KKM_MODELS_DIR");
    let mut asr = kikimimic_asr_whisper::load(
        models
            .join("ggml-large-v3-turbo-q8_0.bin")
            .to_str()
            .unwrap(),
        models.join("ggml-silero-v5.1.2.bin").to_str().unwrap(),
    )
    .unwrap();
    let langs = Spoken::Two(Lang::parse("ja").unwrap(), Lang::parse("en").unwrap());
    let samples = audio();
    let mut steps = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        for lane in [LaneId(0), LaneId(1)] {
            asr.open_lane(lane);
        }
        for second in samples.chunks(16_000) {
            for lane in [LaneId(0), LaneId(1)] {
                asr.push(lane, &Pcm16k::assume_16k(second.to_vec()));
                let started = Instant::now();
                asr.step(lane, &langs).unwrap();
                steps.push(started.elapsed());
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        for lane in [LaneId(0), LaneId(1)] {
            asr.finish(lane, &langs).unwrap();
        }
    }
    steps
}

fn translate_all(mt: &mut LlamaMt) -> Vec<Duration> {
    let (en, ja) = (Lang::parse("en").unwrap(), Lang::parse("ja").unwrap());
    SENTENCES
        .iter()
        .map(|text| {
            let started = Instant::now();
            mt.translate(&MtRequest {
                text,
                source: Some(&en),
                target: &ja,
                context: &[],
                terms: &[],
            })
            .unwrap();
            started.elapsed()
        })
        .collect()
}

fn ms(d: &[Duration]) -> String {
    let mut v: Vec<u128> = d.iter().map(|d| d.as_millis()).collect();
    v.sort();
    let avg = v.iter().sum::<u128>() / v.len().max(1) as u128;
    format!(
        "avg {avg} ms, max {} ms (n={})",
        v.last().copied().unwrap_or(0),
        v.len()
    )
}

/// Recognition on its own for `secs`, as the baseline for its step times.
fn recognize_alone(secs: u64) -> Vec<Duration> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&stop);
    let asr = std::thread::spawn(move || recognize_until(s));
    std::thread::sleep(Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);
    asr.join().unwrap()
}

fn measure(env: &str, format: PromptFormat) {
    println!(
        "  ASR step alone:                {}",
        ms(&recognize_alone(20))
    );
    let mut mt = LlamaMt::load(&env_path(env), format).unwrap();
    translate_all(&mut mt); // warm up
    let alone = translate_all(&mut mt);

    let stop = Arc::new(AtomicBool::new(false));
    let asr_steps = Arc::new(Mutex::new(Vec::new()));
    let (s, out) = (Arc::clone(&stop), Arc::clone(&asr_steps));
    let asr = std::thread::spawn(move || *out.lock().unwrap() = recognize_until(s));
    std::thread::sleep(Duration::from_secs(5)); // let recognition reach steady state
    let mut loaded = Vec::new();
    for _ in 0..3 {
        loaded.extend(translate_all(&mut mt));
    }
    stop.store(true, Ordering::Relaxed);
    asr.join().unwrap();
    let steps = asr_steps.lock().unwrap().clone();

    println!("{env}");
    println!("  translation alone:            {}", ms(&alone));
    println!("  translation during 2-lane ASR: {}", ms(&loaded));
    println!("  ASR step during translation:   {}", ms(&steps));
}

#[test]
#[ignore = "needs Whisper, a GGUF and the test audio"]
fn plamo_under_recognition_load() {
    measure("KKM_PLAMO_GGUF", PromptFormat::Plamo3);
}

#[test]
#[ignore = "needs Whisper, a GGUF and the test audio"]
fn qwen_under_recognition_load() {
    measure("KKM_QWEN_GGUF", PromptFormat::ChatMl);
}
