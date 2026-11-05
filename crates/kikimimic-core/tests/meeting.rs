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

fn finished_texts(events: &[Event]) -> Vec<(String, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Transcript {
                finished: Some(u), ..
            } => Some((u.text.clone(), u.start.0)),
            _ => None,
        })
        .collect()
}

fn outcomes(events: &[Event]) -> Vec<TranslationOutcome> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Translation { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_slow_translator_drops_the_oldest_waiting_sentences_and_says_so() {
    let mut asr = ScriptedAsr::new();
    for i in 0..12u64 {
        asr = asr.say(
            LaneId(0),
            i * 400 + 100..i * 400 + 300,
            "en",
            &format!("s{i}"),
        );
    }
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(8000)))
        .asr(asr)
        .mt_prefix("", ms(3000))
        .languages(ja_en())
        .stop_at(ms(8000))
        .run();
    let o = outcomes(&out.events);
    assert_eq!(o.len(), 12, "every translating line ends exactly once");
    assert!(o.contains(&TranslationOutcome::Dropped));
    assert!(matches!(
        o.last(),
        Some(TranslationOutcome::Text { .. }) | Some(TranslationOutcome::Dropped)
    ));
}

#[test]
fn stop_gives_up_on_translations_that_outlast_the_drain() {
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(3000)))
        .asr(ScriptedAsr::new().say(LaneId(0), 100..800, "en", "Slow."))
        .mt_prefix("", ms(60_000))
        .languages(ja_en())
        .stop_at(ms(3000))
        .run();
    assert_eq!(outcomes(&out.events), [TranslationOutcome::Dropped]);
    assert_eq!(
        out.journal().len(),
        1,
        "only the utterance; no translation line"
    );
}

#[test]
fn a_hole_in_a_lanes_audio_stays_a_hole_on_the_recording() {
    // The system lane delivers nothing between 1 s and 3 s (nothing playing).
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(6000)))
        .lane(
            LaneRole::System,
            Fixture::tone(ms(6000)).with_gap(ms(1000), ms(2000)),
        )
        .asr(ScriptedAsr::new().say(LaneId(1), 1500..2000, "en", "After the gap."))
        .languages(LanguagePolicy {
            target: None,
            ..ja_en()
        })
        .stop_at(ms(6000))
        .run();
    // 1.5 s of the lane's own audio is 3.5 s into the recording, give or take
    // the resampler's holdback (TIMELINE_SLACK_MS); without the hole it
    // would be 1.5 s.
    let lines = finished_texts(&out.events);
    assert_eq!(lines.len(), 1);
    assert!((3480..=3530).contains(&lines[0].1), "{lines:?}");
    out.assert_wavs_share_one_length();
}

#[test]
fn a_recording_that_cannot_start_does_not_stop_the_transcript() {
    let not_a_dir =
        std::env::temp_dir().join(format!("kikimimic-not-a-dir-{}", std::process::id()));
    std::fs::write(&not_a_dir, b"").unwrap();
    let out = Scenario::new()
        .lane(LaneRole::Mic, Fixture::tone(ms(3000)))
        .asr(ScriptedAsr::new().say(LaneId(0), 100..800, "en", "Still here."))
        .languages(LanguagePolicy {
            target: None,
            ..ja_en()
        })
        .record_into(not_a_dir.clone())
        .stop_at(ms(3000))
        .run();
    std::fs::remove_file(&not_a_dir).unwrap();
    assert_eq!(finished_texts(&out.events).len(), 1);
    let noticed = out.events.iter().any(|e| {
        matches!(e, Event::Status(s) if s.notices.iter().any(|n| matches!(n, kikimimic_core::event::Notice::RecordingOff { .. })))
    });
    assert!(noticed, "{:#?}", out.events);
}
