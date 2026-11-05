//! Step 2 risk spike: whisper-rs statically linked into this binary while
//! llama-cpp-2 lives in a dlopen'd cdylib (kikimimic-mt-llama-dylib).
//!
//! Success criterion: windowed ASR decodes and sentence translations run
//! concurrently with correct output and no ggml symbol-collision SIGABRT
//! (the failure mode documented in docs/step0-results.md).
//!
//! Usage:
//!   cargo build --release -p kikimimic-mt-llama-dylib
//!   cargo run --release -p spike --bin cdylib-check \
//!     --no-default-features --features asr [-- path/to/libkikimimic_mt_llama_dylib.dylib]

use std::ffi::{c_char, c_void, CStr, CString};
use std::time::Instant;

use anyhow::{Context, Result};
use kikimimic_core::AsrEngine;

const WHISPER_MODEL: &str = "models/ggml-large-v3-turbo-q8_0.bin";
const LLM_MODEL: &str = "models/Qwen3-4B-Instruct-2507-Q4_K_M.gguf";
const AUDIO_JA: &str = "assets/test-ja.wav";
const SAMPLE_RATE: usize = 16_000;

const SENTENCES: [&str; 4] = [
    "Thank you all for joining today.",
    "The implementation of the new search feature was completed on schedule.",
    "先週の進捗を共有します。",
    "新しい検索機能の実装は予定どおり完了しました。",
];

type InitFn = unsafe extern "C" fn(*const c_char, u32) -> *mut c_void;
type TranslateFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);
type ShutdownFn = unsafe extern "C" fn(*mut c_void);

fn main() -> Result<()> {
    let dylib_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/release/libkikimimic_mt_llama_dylib.dylib".into());

    // ASR under load on its own thread, like the app's pipeline worker:
    // re-decode a growing window for every second of audio, no idle time.
    let asr = std::thread::spawn(|| -> Result<(String, u128)> {
        let audio = read_wav(AUDIO_JA)?;
        let mut engine =
            kikimimic_asr_whisper::WhisperEngine::load(WHISPER_MODEL, &["ja".into(), "en".into()])?;
        let mut worst = 0u128;
        let mut text = String::new();
        for end_s in 1..=audio.len().div_ceil(SAMPLE_RATE) {
            let end = (end_s * SAMPLE_RATE).min(audio.len());
            let t = Instant::now();
            let hyp = engine.transcribe(&audio[..end], Some("ja"))?;
            worst = worst.max(t.elapsed().as_millis());
            text = hyp.text;
        }
        Ok((text, worst))
    });

    let gpu_layers: u32 = if std::env::var("KKM_SPIKE_CPU").is_ok() {
        0
    } else {
        1_000_000
    };
    let mut translations = Vec::new();
    unsafe {
        let lib = libloading::Library::new(&dylib_path)
            .with_context(|| format!("dlopen {dylib_path}"))?;
        let init: libloading::Symbol<InitFn> = lib.get(b"kkm_translate_init")?;
        let translate: libloading::Symbol<TranslateFn> = lib.get(b"kkm_translate")?;
        let free: libloading::Symbol<FreeFn> = lib.get(b"kkm_translate_free")?;
        let shutdown: libloading::Symbol<ShutdownFn> = lib.get(b"kkm_translate_shutdown")?;

        let model = CString::new(LLM_MODEL)?;
        let handle = init(model.as_ptr(), gpu_layers);
        anyhow::ensure!(!handle.is_null(), "kkm_translate_init failed");

        for sentence in SENTENCES {
            let (source, target) = if sentence.is_ascii() {
                ("English", "Japanese")
            } else {
                ("Japanese", "English")
            };
            let s = CString::new(sentence)?;
            let src = CString::new(source)?;
            let tgt = CString::new(target)?;
            let t = Instant::now();
            let out = translate(handle, s.as_ptr(), src.as_ptr(), tgt.as_ptr());
            anyhow::ensure!(!out.is_null(), "translation failed for: {sentence}");
            let translated = CStr::from_ptr(out).to_string_lossy().into_owned();
            free(out);
            println!(
                "translate {:>5} ms: {sentence} -> {translated}",
                t.elapsed().as_millis()
            );
            translations.push(translated);
        }
        shutdown(handle);
    }

    let (asr_text, asr_worst) = asr.join().expect("asr thread panicked")?;
    println!("asr final text: {asr_text}");
    println!("asr worst window decode under load: {asr_worst} ms");

    anyhow::ensure!(!asr_text.is_empty(), "ASR produced no text");
    anyhow::ensure!(
        translations.iter().all(|t| !t.is_empty()),
        "empty translation"
    );
    println!("peak RSS: {} MB", peak_rss_mb());
    println!("OK: no ggml symbol collision — cdylib isolation works");
    Ok(())
}

fn read_wav(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {path}"))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate == 16_000 && spec.channels == 1,
        "expected 16kHz mono, got {}Hz {}ch",
        spec.sample_rate,
        spec.channels
    );
    Ok(reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<std::result::Result<Vec<_>, _>>()?)
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
