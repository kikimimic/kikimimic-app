//! Hand-counted fixtures also checked independently with pyannote.metrics 4.1.
use kikimimic_core::diarization_evaluation::{evaluate, Interval, Mode, Options};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    duration: f64,
    reference: Vec<(f64, f64, String)>,
    hypothesis: Vec<(f64, f64, String)>,
    uem: Vec<(f64, f64)>,
    collar: f64,
    skip_overlap: bool,
    expected: [f64; 5],
}

#[test]
fn hand_counted_fixtures_match_components_and_rate() {
    let fixtures: Vec<Fixture> =
        serde_json::from_str(include_str!("fixtures/diarization_evaluation.json")).unwrap();
    let rttm = |items: &[(f64, f64, String)]| {
        items
            .iter()
            .map(|(start, end, speaker)| {
                format!(
                    "SPEAKER audio 1 {start} {} <NA> <NA> {speaker} <NA> <NA>\n",
                    end - start
                )
            })
            .collect::<String>()
    };
    for f in fixtures {
        let report = evaluate(
            &rttm(&f.reference),
            &rttm(&f.hypothesis),
            Options {
                mode: Mode::HumanReference,
                recording_duration: f.duration,
                uem: f
                    .uem
                    .into_iter()
                    .map(|(start, end)| Interval { start, end })
                    .collect(),
                collar: f.collar,
                skip_overlap: f.skip_overlap,
            },
        )
        .unwrap();
        let actual = [
            report.reference_speaker_seconds,
            report.evaluated_seconds,
            report.seconds.miss,
            report.seconds.false_alarm,
            report.seconds.confusion,
        ];
        for (actual, expected) in actual.into_iter().zip(f.expected) {
            assert!(
                (actual - expected).abs() < 1e-9,
                "{}: {actual} != {expected}",
                f.name
            );
        }
        let [total, _, miss, fa, confusion] = f.expected;
        if total == 0. {
            assert_eq!(report.der, None, "{}", f.name);
        } else {
            assert!(
                (report.der.unwrap() - (miss + fa + confusion) / total).abs() < 1e-9,
                "{}",
                f.name
            );
        }
    }
}
