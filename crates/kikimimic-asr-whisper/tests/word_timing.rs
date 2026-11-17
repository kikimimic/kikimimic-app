//! Opt-in acceptance on already installed assets, never fetches a model/audio.
use kikimimic_core::asr::WindowTranscriber;

#[test]
#[ignore = "needs KKM_WORD_MODEL and KKM_WORD_AUDIO (local non-confidential mono PCM16 16kHz speech)"]
fn local_model_returns_decoded_text_and_real_token_estimates() {
    let model =
        std::env::var("KKM_WORD_MODEL").expect("set KKM_WORD_MODEL to installed Whisper file");
    let audio =
        std::env::var("KKM_WORD_AUDIO").expect("set KKM_WORD_AUDIO to local non-confidential WAV");
    let mut reader = hound::WavReader::open(audio).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().bits_per_sample, 16);
    let mut samples = reader
        .samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect::<Vec<_>>();
    assert!(!samples.is_empty());
    let duration_ms = samples.len() as u64 * 1000 / 16_000;
    assert!(duration_ms <= 25_000, "provide a short clip");
    samples.resize(samples.len().max(17_600), 0.0);
    let mut engine = kikimimic_asr_whisper::WhisperEngine::load(&model, &[]).unwrap();
    let lang = std::env::var("KKM_WORD_LANG").ok();
    let words = engine.word_timing(&samples, lang.as_deref()).unwrap();
    assert!(!words.is_empty());
    assert!(words.iter().all(|w| !w.text.contains('\u{fffd}')));
    assert!(words
        .iter()
        .filter_map(|w| w.timing)
        .any(|t| t.start_ms < t.end_ms && t.start_ms < duration_ms));
    // Do not assert accuracy against invented reference words or intervals.
    println!(
        "{} decoded units, {} with backend intervals",
        words.len(),
        words.iter().filter(|w| w.timing.is_some()).count()
    );
}
