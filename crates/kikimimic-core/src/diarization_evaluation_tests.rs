use super::*;

fn rttm(items: &[(f64, f64, &str)]) -> String {
    items
        .iter()
        .map(|(start, length, speaker)| {
            format!("SPEAKER audio 1 {start} {length} <NA> <NA> {speaker} <NA> <NA>\n")
        })
        .collect()
}
fn options() -> Options {
    Options {
        mode: Mode::HumanReference,
        recording_duration: 10.0,
        uem: vec![],
        collar: 0.0,
        skip_overlap: false,
    }
}
fn score(r: &[(f64, f64, &str)], h: &[(f64, f64, &str)]) -> Report {
    evaluate(&rttm(r), &rttm(h), options()).unwrap()
}
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn names_are_permutations_and_silence_counts_as_evaluated_time() {
    let result = score(
        &[(0., 2., "A"), (2., 3., "B")],
        &[(0., 2., "z"), (2., 3., "x")],
    );
    assert_eq!(result.der, Some(0.0));
    assert_eq!(result.mapping["z"], "A");
    assert_eq!(result.mapping["x"], "B");
    near(result.evaluated_seconds, 10.);
    near(result.reference_speaker_seconds, 5.);
}

#[test]
fn hand_calculated_miss_false_alarm_and_confusion_seconds() {
    let missed = score(&[(0., 5., "A")], &[(0., 3., "x")]);
    assert_eq!(
        missed.seconds,
        Components {
            miss: 2.,
            false_alarm: 0.,
            confusion: 0.
        }
    );
    near(missed.der.unwrap(), 0.4);
    let false_alarm = score(&[(0., 3., "A")], &[(0., 5., "x")]);
    assert_eq!(
        false_alarm.seconds,
        Components {
            miss: 0.,
            false_alarm: 2.,
            confusion: 0.
        }
    );
    near(false_alarm.der.unwrap(), 2. / 3.);
    // The same hypothesis speaker covers two distinct reference speakers.
    let confusion = score(&[(0., 6., "A"), (6., 4., "B")], &[(0., 10., "x")]);
    assert_eq!(
        confusion.seconds,
        Components {
            miss: 0.,
            false_alarm: 0.,
            confusion: 4.
        }
    );
    near(confusion.der.unwrap(), 0.4);
    assert_eq!(
        confusion.disagreements[0].interval,
        Interval {
            start: 6.,
            end: 10.
        }
    );
}

#[test]
fn assignment_is_global_and_exact_not_greedy() {
    // Cooccurrence matrix [[6,5],[5,0]]: greedy gets 6, exact gets 10.
    assert_eq!(
        assignment(&[vec![6., 5.], vec![5., 0.]], 2),
        vec![Some(1), Some(0)]
    );
    let r = rttm(&[(0., 6., "A"), (6., 5., "A"), (11., 5., "B")]);
    let h = rttm(&[(0., 6., "x"), (6., 5., "y"), (11., 5., "x")]);
    let result = evaluate(
        &r,
        &h,
        Options {
            recording_duration: 16.,
            ..options()
        },
    )
    .unwrap();
    near(result.seconds.confusion, 6.);
    near(result.der.unwrap(), 6. / 16.);
    assert_eq!(result.mapping["x"], "B");
    assert_eq!(result.mapping["y"], "A");
    // Reassigning each interval would incorrectly yield zero.
    let global = score(&[(0., 5., "A"), (5., 5., "B")], &[(0., 10., "x")]);
    near(global.seconds.confusion, 5.);
}

#[test]
fn overlap_is_distinct_speaker_time_and_can_be_excluded() {
    let r = rttm(&[(0., 4., "A"), (2., 4., "B")]);
    let h = rttm(&[(0., 6., "x")]);
    let result = evaluate(&r, &h, options()).unwrap();
    near(result.reference_speaker_seconds, 8.);
    near(result.seconds.miss, 2.);
    near(result.seconds.confusion, 2.);
    near(result.der.unwrap(), 0.5);
    let skipped = evaluate(
        &r,
        &h,
        Options {
            skip_overlap: true,
            ..options()
        },
    )
    .unwrap();
    near(skipped.reference_speaker_seconds, 4.);
    near(skipped.evaluated_seconds, 8.);
    near(skipped.seconds.miss, 0.);
    near(skipped.seconds.confusion, 2.);
}

#[test]
fn collar_is_total_centered_width_and_applied_before_mapping() {
    let r = rttm(&[(2., 4., "A")]);
    let h = rttm(&[(3., 2., "x")]);
    let result = evaluate(
        &r,
        &h,
        Options {
            collar: 2.,
            ..options()
        },
    )
    .unwrap();
    near(result.reference_speaker_seconds, 2.);
    near(result.evaluated_seconds, 6.);
    assert_eq!(result.der, Some(0.));
    // Huge collar consumes the entire UEM, yielding undefined, not zero.
    let removed = evaluate(
        &r,
        &h,
        Options {
            collar: 20.,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(removed.der, None);
    assert_eq!(removed.evaluated_seconds, 0.);
    assert!(removed.mapping.is_empty());
    // Collars erase A's short turns, changing the meeting-wide optimum.
    let r = rttm(&[
        (0., 1., "A"),
        (2., 1., "A"),
        (4., 1., "A"),
        (6., 1., "A"),
        (7., 3., "B"),
    ]);
    let h = rttm(&[(0., 10., "x")]);
    assert_eq!(evaluate(&r, &h, options()).unwrap().mapping["x"], "A");
    let result = evaluate(
        &r,
        &h,
        Options {
            collar: 2.,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(result.mapping["x"], "B");
}

#[test]
fn uem_union_clips_edges_without_inventing_reference_boundaries() {
    let r = rttm(&[(0., 10., "A")]);
    let h = rttm(&[(0., 4., "x")]);
    let result = evaluate(
        &r,
        &h,
        Options {
            uem: vec![
                Interval { start: 2., end: 5. },
                Interval { start: 4., end: 7. },
            ],
            collar: 2.,
            ..options()
        },
    )
    .unwrap();
    near(result.evaluated_seconds, 5.);
    near(result.reference_speaker_seconds, 5.);
    near(result.seconds.miss, 3.);
    near(result.der.unwrap(), 0.6);
    let islands = evaluate(
        &r,
        &h,
        Options {
            uem: vec![
                Interval { start: 1., end: 2. },
                Interval { start: 8., end: 9. },
            ],
            ..options()
        },
    )
    .unwrap();
    near(islands.evaluated_seconds, 2.);
    near(islands.seconds.miss, 1.);
}

#[test]
fn duplicate_and_overlapping_same_speaker_segments_are_union() {
    let r = rttm(&[(0., 5., "A"), (0., 5., "A"), (2., 5., "A")]);
    let h = rttm(&[(0., 7., "x"), (1., 2., "x")]);
    let result = evaluate(
        &r,
        &h,
        Options {
            skip_overlap: true,
            collar: 1.,
            ..options()
        },
    )
    .unwrap();
    near(result.reference_speaker_seconds, 6.);
    assert_eq!(result.der, Some(0.));
}

#[test]
fn empty_reference_is_undefined_and_more_hypothesis_speakers_add_false_alarm() {
    let result = score(&[], &[(0., 2., "x")]);
    assert_eq!(result.der, None);
    near(result.seconds.false_alarm, 2.);
    assert_eq!(score(&[], &[]).der, None);
    let result = score(&[(0., 3., "A")], &[(0., 3., "x"), (0., 3., "y")]);
    near(result.seconds.false_alarm, 3.);
    near(result.der.unwrap(), 1.);
}

#[test]
fn invalid_rttm_and_conditions_are_rejected() {
    let good = rttm(&[(0., 1., "A")]);
    for invalid in [
        "UNKNOWN a 1 0 1 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 NaN 1 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 -1 1 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 0 inf <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 0 0 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 9 2 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 2 0 1 <NA> <NA> x <NA> <NA>",
        "SPEAKER a 1 0 1 <NA> <NA> <NA> <NA> <NA>",
        "SPEAKER a",
        "SPEAKER a 1 0 1 <NA> <NA> x <NA> <NA>\nSPEAKER b 1 0 1 <NA> <NA> y <NA> <NA>",
    ] {
        assert!(evaluate(invalid, &good, options()).is_err(), "{invalid}");
    }
    assert!(evaluate(
        &good,
        &good,
        Options {
            collar: f64::NAN,
            ..options()
        }
    )
    .is_err());
    assert!(evaluate(
        &good,
        &good,
        Options {
            uem: vec![Interval { start: 4., end: 3. }],
            ..options()
        }
    )
    .is_err());
    let too_many = (0..129)
        .map(|i| format!("SPEAKER a 1 0 1 <NA> <NA> s{i} <NA> <NA>\n"))
        .collect::<String>();
    assert!(evaluate(&too_many, &good, options()).is_err());
}

#[test]
fn modes_hashes_conditions_and_json_roundtrip_preserve_provenance() {
    let r = rttm(&[(0., 5., "A")]);
    let before = r.clone();
    let human = evaluate(&r, &r, options()).unwrap();
    let model = evaluate(
        &r,
        &r,
        Options {
            mode: Mode::ModelDisagreement,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(r, before);
    assert_eq!(human.reference_sha256, human.hypothesis_sha256);
    assert_eq!(human.reference_sha256.len(), 64);
    assert_ne!(
        human.reference_sha256,
        evaluate(&(r.clone() + "\n"), &r, options())
            .unwrap()
            .reference_sha256
    );
    assert_eq!(model.der, None);
    assert_eq!(model.model_disagreement_rate, Some(0.));
    assert!(model.interpretation.contains("証明ではありません"));
    assert_eq!(
        serde_json::from_str::<Report>(&serde_json::to_string(&model).unwrap()).unwrap(),
        model
    );
}

#[test]
fn hungarian_matches_independent_exhaustive_assignment() {
    fn brute(w: &[Vec<f64>], row: usize, used: usize) -> f64 {
        if row == w.len() {
            return 0.;
        }
        (0..w.len())
            .filter(|j| used & (1 << j) == 0)
            .map(|j| w[row][j] + brute(w, row + 1, used | (1 << j)))
            .fold(0., f64::max)
    }
    let mut seed: u64 = 9;
    for n in 1..=6 {
        for _ in 0..30 {
            let w: Vec<Vec<f64>> = (0..n)
                .map(|_| {
                    (0..n)
                        .map(|_| {
                            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                            ((seed >> 32) % 20) as f64
                        })
                        .collect()
                })
                .collect();
            let chosen = assignment(&w, n);
            let actual: f64 = chosen
                .iter()
                .enumerate()
                .map(|(h, r)| r.map_or(0., |r| w[r][h]))
                .sum();
            near(actual, brute(&w, 0, 0));
        }
    }
}

#[test]
fn billion_second_clock_does_not_enumerate_milliseconds() {
    let r = rttm(&[(0., 1e9, "A")]);
    let result = evaluate(
        &r,
        &r,
        Options {
            recording_duration: 1e9,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(result.der, Some(0.));
    near(result.evaluated_seconds, 1e9);
}

#[test]
fn decimal_endpoint_roundoff_is_safe_but_real_out_of_range_is_rejected() {
    let text = rttm(&[(0.1, 0.2, "A")]);
    let opts = Options {
        recording_duration: 0.3,
        ..options()
    };
    assert_eq!(evaluate(&text, &text, opts.clone()).unwrap().der, Some(0.));
    assert!(evaluate(&rttm(&[(0.1, 0.201, "A")]), &text, opts).is_err());
    let tiny = rttm(&[(0., f64::from_bits(1), "A")]);
    assert!(evaluate(&tiny, &rttm(&[(0., 10., "x")]), options()).is_err());
}

#[test]
fn interval_and_byte_resource_limits_are_errors() {
    let excessive = rttm(&vec![(0., 1., "A"); 20_001]);
    assert!(evaluate(&excessive, "", options()).is_err());
    assert!(evaluate(&" ".repeat(MAX_INPUT_BYTES + 1), "", options()).is_err());
    let long_name = "x".repeat(129);
    assert!(evaluate(&rttm(&[(0., 1., &long_name)]), "", options()).is_err());
}
