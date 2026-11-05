//! How long one window decode takes, by window length and with or without
//! language detection. Opt-in, like real_model.rs.

use std::path::PathBuf;
use std::time::Instant;

use kikimimic_core::asr::WindowTranscriber;
use kikimimic_core::domain::{Lang, Spoken};

#[test]
#[ignore = "needs the Whisper model and test audio"]
fn decode_cost_by_window_and_detection() {
    let models = PathBuf::from(std::env::var("KKM_MODELS_DIR").unwrap());
    let mut engine = kikimimic_asr_whisper::WhisperEngine::load(
        models
            .join("ggml-large-v3-turbo-q8_0.bin")
            .to_str()
            .unwrap(),
        &[],
    )
    .unwrap();
    let mut r = hound::WavReader::open(
        PathBuf::from(std::env::var("KKM_TEST_AUDIO").unwrap()).join("test-en.wav"),
    )
    .unwrap();
    let audio: Vec<f32> = r
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32)
        .collect();
    let two = Spoken::Two(Lang::parse("ja").unwrap(), Lang::parse("en").unwrap());
    let one = Spoken::One(Lang::parse("en").unwrap());
    engine
        .transcribe(&audio[..16_000], Some("en"), &one)
        .unwrap(); // warm up
    for secs in [1usize, 3, 6, 10, 13] {
        let window = &audio[..(secs * 16_000).min(audio.len())];
        let t = Instant::now();
        engine.transcribe(window, Some("en"), &one).unwrap();
        let pinned = t.elapsed().as_millis();
        let t = Instant::now();
        engine.transcribe(window, None, &two).unwrap();
        let detected = t.elapsed().as_millis();
        println!("{secs:>2}s window: pinned {pinned:>5} ms, detecting {detected:>5} ms");
    }
}
