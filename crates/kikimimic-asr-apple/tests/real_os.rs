//! The engine on the recognizer of this Mac (macOS 26 or later) and real
//! speech. The first run downloads the language's assets, so it is opt-in:
//!
//! ```sh
//! KKM_TEST_AUDIO=/path/to/assets \
//!   cargo test -p kikimimic-asr-apple --test real_os -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use kikimimic_core::domain::{LaneId, Lang, Pcm16k, Spoken};
use kikimimic_core::engine::AsrEngine;

fn hear(file: &str, langs: &Spoken) -> (String, Vec<kikimimic_core::engine::Recognized>) {
    let dir = PathBuf::from(std::env::var("KKM_TEST_AUDIO").expect("set KKM_TEST_AUDIO"));
    let mut reader = hound::WavReader::open(dir.join(file)).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / i16::MAX as f32)
        .collect();
    let mut engine = kikimimic_asr_apple::AppleSpeech::default();
    let lane = LaneId(0);
    engine.open_lane(lane);
    let mut text = String::new();
    let mut utterances = Vec::new();
    for chunk in samples.chunks(1_600) {
        engine.push(lane, &Pcm16k::assume_16k(chunk.to_vec()));
        if let Some(step) = engine.step(lane, langs).unwrap() {
            text.push_str(&step.committed_delta);
            utterances.extend(step.utterance);
        }
    }
    for step in engine.finish(lane, langs).unwrap() {
        text.push_str(&step.committed_delta);
        utterances.extend(step.utterance);
    }
    println!("committed: {text}\nutterances: {utterances:#?}");
    (text, utterances)
}

#[test]
#[ignore = "needs macOS 26 and test audio"]
fn english_speech_comes_out_as_english_text() {
    assert!(kikimimic_asr_apple::is_available());
    let en = Lang::parse("en").unwrap();
    let (text, utterances) = hear("test-en.wav", &Spoken::One(en.clone()));
    assert!(!utterances.is_empty(), "nothing was recognized");
    assert!(utterances.iter().all(|u| u.lang.as_ref() == Some(&en)));
    assert!(utterances.windows(2).all(|w| w[0].end <= w[1].start));
    assert!(text.to_lowercase().contains("search"), "{text}");
}

#[test]
#[ignore = "needs macOS 26 and test audio"]
fn with_two_languages_it_listens_in_the_first_and_does_not_claim_it() {
    let langs = Spoken::Two(Lang::parse("ja").unwrap(), Lang::parse("en").unwrap());
    let (text, utterances) = hear("test-ja.wav", &langs);
    assert!(!utterances.is_empty(), "nothing was recognized");
    assert!(utterances.iter().all(|u| u.lang.is_none()));
    assert!(text.contains("会議"), "{text}");
}
