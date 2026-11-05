//! Haas on the real model and real speech. Needs libspeech, the model files
//! and a 16 kHz mono Japanese recording, so it is opt-in:
//!
//! ```sh
//! KKM_SPEECH_DYLIB=/path/to/libspeech.4.dylib KKM_MODELS_DIR=/path/to/models \
//!   KKM_TEST_AUDIO=/path/to/assets \
//!   cargo test -p kikimimic-asr-speech --test real_model -- --ignored --nocapture
//! ```
//!
//! `KKM_MODELS_DIR` holds `Haas-616M-ja-v1.0-Q4_K.gguf` and `ggml-silero-v5.1.2.bin`.

use std::path::PathBuf;

use kikimimic_core::domain::{LaneId, Lang, Pcm16k, Spoken};
use kikimimic_core::engine::{AsrEngine, Recognized};
use kikimimic_core::glossary::Keyword;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("set {name}")))
}

/// The committed text and the utterances of `test-ja.wav`, fed a second at a time.
fn recognize(keywords: &[Keyword]) -> (String, Vec<Recognized>) {
    let models = env_path("KKM_MODELS_DIR");
    let mut engine = kikimimic_asr_speech::load(
        &models.join("Haas-616M-ja-v1.0-Q4_K.gguf"),
        models.join("ggml-silero-v5.1.2.bin").to_str().unwrap(),
    )
    .unwrap();
    engine.set_keywords(keywords);
    let mut reader =
        hound::WavReader::open(env_path("KKM_TEST_AUDIO").join("test-ja.wav")).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32)
        .collect();
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
    (text, utterances)
}

#[test]
#[ignore = "needs libspeech, the Haas model and test audio"]
fn japanese_speech_comes_out_as_japanese_text() {
    let (_, utterances) = recognize(&[]);
    assert!(!utterances.is_empty(), "nothing was recognized");
    assert!(utterances
        .iter()
        .all(|u| u.lang == Some(Lang::parse("ja").unwrap())));
}

#[test]
#[ignore = "needs libspeech, the Haas model and test audio"]
fn a_glossary_keyword_is_written_as_the_glossary_spells_it() {
    let kw = |written: &str, readings: &[&str]| Keyword {
        written: written.into(),
        readings: readings.iter().map(|r| r.to_string()).collect(),
    };
    // A Latin reading too: a glossary's "wrong" side is whatever a recognizer
    // wrote, and speech.cpp must not refuse the request over it.
    let (text, _) = recognize(&[
        kw("検索エンジン", &["検索"]),
        kw("Kikimimic", &["Kiki mimic"]),
    ]);
    assert!(text.contains("検索エンジン"), "{text}");
}
