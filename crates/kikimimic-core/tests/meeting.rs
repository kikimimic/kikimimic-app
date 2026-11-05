//! The whole pipeline, headless: fixture audio in, scripted engines, virtual
//! time. What the UI would see and what lands on disk are both checked.

use std::time::Duration;

use kikimimic_core::domain::{LaneId, LaneRole, Lang, LanguagePolicy, RecordingMs, Spoken};
use kikimimic_core::event::{Event, TranslationOutcome};
use kikimimic_core::sim::{Fixture, Scenario, ScriptedAsr};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn lang(c: &str) -> Lang {
    Lang::parse(c).unwrap()
}

fn ja_en() -> LanguagePolicy {
    LanguagePolicy {
        spoken: Spoken::Two(lang("ja"), lang("en")),
        target: Some(lang("ja")),
        mutual: false,
    }
}

#[test]
fn a_late_lanes_sentence_is_transcribed_translated_and_journaled_on_the_recording_clock() {
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(6000)))
        .lane(
            LaneRole::System,
            Fixture::tone(ms(5600)).starting_at(ms(400)),
        )
        .asr(ScriptedAsr::new().say(LaneId(1), 500..1800, "en", "Hello there."))
        .mt_prefix("[ja] ", ms(400))
        .languages(ja_en())
        .stop_at(ms(6000))
        .run();

    let finished: Vec<_> = out
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Transcript {
                lane,
                finished: Some(u),
                ..
            } => Some((lane.as_str(), u)),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 1, "{:#?}", out.events);
    let (lane, u) = finished[0];
    assert_eq!(lane, "speaker");
    assert_eq!(u.text, "Hello there.");
    assert!(u.translating);
    // The lane started 400 ms into the recording.
    assert_eq!((u.start, u.end), (RecordingMs(900), RecordingMs(2200)));

    let translated: Vec<_> = out
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Translation { id, outcome, .. } => Some((*id, outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(
        translated,
        [(
            u.id,
            &TranslationOutcome::Text {
                text: "[ja] Hello there.".into(),
                lang: lang("ja")
            }
        )]
    );

    assert_eq!(
        out.journal(),
        [
            serde_json::json!({"type":"utterance","id":u.id.0,"lane":"speaker","startMs":900,"endMs":2200,"lang":"en","text":"Hello there."}),
            serde_json::json!({"type":"translation","id":u.id.0,"lang":"ja","text":"[ja] Hello there."}),
        ]
    );
    out.assert_wavs_share_one_length();
}

#[test]
fn a_language_change_mid_session_applies_from_the_next_utterance() {
    let no_translation = LanguagePolicy {
        target: None,
        ..ja_en()
    };
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(6000)))
        .asr(
            ScriptedAsr::new()
                .say(LaneId(0), 200..900, "en", "First.")
                .say(LaneId(0), 2500..3500, "en", "Second."),
        )
        .languages(ja_en())
        .languages_at(ms(2000), no_translation)
        .stop_at(ms(6000))
        .run();

    let translating: Vec<_> = out
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Transcript {
                finished: Some(u), ..
            } => Some((u.text.as_str(), u.translating)),
            _ => None,
        })
        .collect();
    assert_eq!(translating, [("First.", true), ("Second.", false)]);
}
