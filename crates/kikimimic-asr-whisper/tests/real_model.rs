//! The v3 engine on the real model and real speech. Needs the model files
//! and a 16 kHz mono recording, so it is opt-in:
//!
//! ```sh
//! KKM_MODELS_DIR=/path/to/models KKM_TEST_AUDIO=/path/to/assets \
//!   cargo test -p kikimimic-asr-whisper --test real_model -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use kikimimic_core::domain::{Lang, LaneId, Pcm16k, Spoken};
use kikimimic_core::engine::AsrEngine;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

#[test]
#[ignore = "needs the Whisper model and test audio"]
fn english_speech_comes_out_as_english_text() {
    let models = env_path("KKM_MODELS_DIR");
    let mut engine = kikimimic_asr_whisper::load(
        models.join("ggml-large-v3-turbo-q8_0.bin").to_str().unwrap(),
        models.join("ggml-silero-v5.1.2.bin").to_str().unwrap(),
    )
    .unwrap();
    let mut reader = hound::WavReader::open(env_path("KKM_TEST_AUDIO").join("test-en.wav")).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    let samples: Vec<f32> =
        reader.samples::<i16>().map(|s| s.unwrap() as f32 / i16::MAX as f32).collect();
    let langs = Spoken::Two(Lang::parse("ja").unwrap(), Lang::parse("en").unwrap());
    let lane = LaneId(0);
    engine.open_lane(lane);
    let mut text = String::new();
    let mut utterances = Vec::new();
    for second in samples.chunks(16_000) {
        engine.push(lane, &Pcm16k::assume_16k(second.to_vec()));
        if let Some(step) = engine.step(lane, &langs).unwrap() {
            text.push_str(&step.committed_delta);
            utterances.extend(step.utterance);
        }
    }
    for step in engine.finish(lane, &langs).unwrap() {
        text.push_str(&step.committed_delta);
        utterances.extend(step.utterance);
    }
    println!("committed: {text}\nutterances: {utterances:#?}");
    assert!(!utterances.is_empty(), "nothing was recognized");
    assert!(utterances.iter().any(|u| u.lang == Some(Lang::parse("en").unwrap())));
    assert!(text.to_lowercase().contains("search"), "{text}");
}
