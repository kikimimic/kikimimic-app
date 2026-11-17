//! Local, interval-based diarization evaluation. See docs/diarization-evaluation.md.
//! No file I/O, inference, platform, or network dependencies.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEGMENTS: usize = 20_000;
const MAX_SPEAKERS: usize = 128;
const MAX_PAIR_OPERATIONS: usize = 50_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    HumanReference,
    ModelDisagreement,
}

impl Mode {
    pub fn description(self) -> &'static str {
        match self {
            Self::HumanReference => "人手で確認した参照に対するDER",
            Self::ModelDisagreement => "モデル間の不一致（優劣・精度の証明ではありません）",
        }
    }
}

/// Half-open seconds on the original recording clock.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub mode: Mode,
    pub recording_duration: f64,
    /// Empty means the entire recording; otherwise the union of these regions.
    pub uem: Vec<Interval>,
    /// Total centered width, i.e. +/- collar/2 around reference boundaries.
    pub collar: f64,
    pub skip_overlap: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Components {
    pub miss: f64,
    pub false_alarm: f64,
    pub confusion: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disagreement {
    pub interval: Interval,
    pub reference_speakers: Vec<String>,
    pub hypothesis_speakers: Vec<String>,
    pub seconds: Components,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub algorithm: String,
    pub interpretation: String,
    pub options: Options,
    pub reference_sha256: String,
    pub hypothesis_sha256: String,
    pub reference_recording_id: Option<String>,
    pub hypothesis_recording_id: Option<String>,
    /// DER only when mode is HumanReference. None when denominator is zero.
    pub der: Option<f64>,
    /// Directional DER-formula disagreement, never an accuracy claim.
    pub model_disagreement_rate: Option<f64>,
    pub seconds: Components,
    pub reference_speaker_seconds: f64,
    /// Wall-clock duration of UEM after exclusions, including silence.
    pub evaluated_seconds: f64,
    /// Hypothesis -> reference; unmatched/zero-overlap pairs are omitted.
    pub mapping: BTreeMap<String, String>,
    pub disagreements: Vec<Disagreement>,
}

#[derive(Clone)]
struct Segment {
    interval: Interval,
    speaker: usize,
}

struct Annotation {
    id: Option<String>,
    speakers: Vec<String>,
    segments: Vec<Segment>,
}

fn valid_interval(interval: Interval, duration: f64) -> bool {
    interval.start.is_finite()
        && interval.end.is_finite()
        && interval.start >= 0.0
        && interval.start < interval.end
        && interval.end <= duration
}

fn union(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    let mut merged: Vec<Interval> = Vec::new();
    for interval in intervals {
        if let Some(last) = merged.last_mut() {
            if interval.start <= last.end {
                last.end = last.end.max(interval.end);
                continue;
            }
        }
        merged.push(interval);
    }
    merged
}

fn parse(text: &str, duration: f64) -> Result<Annotation> {
    ensure!(text.len() <= MAX_INPUT_BYTES, "RTTM exceeds 8 MiB");
    let mut id = None;
    let mut by_speaker: BTreeMap<String, Vec<Interval>> = BTreeMap::new();
    let mut count = 0;
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().take(11).collect();
        let error = || format!("Invalid RTTM line {}", line_index + 1);
        ensure!(
            fields.len() == 10 && fields[0] == "SPEAKER",
            "{}: expected 10-field SPEAKER",
            error()
        );
        ensure!(fields[2] == "1", "{}: only channel 1 is supported", error());
        ensure!(
            !["<NA>", "<na>"].contains(&fields[1]),
            "{}: recording id missing",
            error()
        );
        ensure!(
            !["<NA>", "<na>"].contains(&fields[7]),
            "{}: speaker missing",
            error()
        );
        ensure!(
            fields[1].len() <= 128 && fields[7].len() <= 128,
            "{}: recording id and speaker must be at most 128 bytes",
            error()
        );
        if let Some(previous) = &id {
            ensure!(previous == fields[1], "{}: multiple recording ids", error());
        } else {
            id = Some(fields[1].to_string());
        }
        let start: f64 = fields[3].parse().map_err(|_| anyhow::anyhow!(error()))?;
        let length: f64 = fields[4].parse().map_err(|_| anyhow::anyhow!(error()))?;
        let mut interval = Interval {
            start,
            end: start + length,
        };
        // Decimal RTTM arithmetic can overshoot an exact WAV/UEM endpoint by
        // a few floating-point ULPs (e.g. 0.1 + 0.2). Only trim roundoff.
        let roundoff = 4.0 * f64::EPSILON * duration;
        if interval.end > duration && interval.end - duration <= roundoff {
            interval.end = duration;
        }
        ensure!(
            length.is_finite() && length > 0.0 && valid_interval(interval, duration),
            "{}: non-finite, negative, empty, or outside recording",
            error()
        );
        by_speaker
            .entry(fields[7].into())
            .or_default()
            .push(interval);
        count += 1;
        ensure!(count <= MAX_SEGMENTS, "RTTM exceeds 20000 segments");
        ensure!(
            by_speaker.len() <= MAX_SPEAKERS,
            "RTTM exceeds 128 speakers"
        );
    }
    let speakers: Vec<_> = by_speaker.keys().cloned().collect();
    let segments = by_speaker
        .into_values()
        .enumerate()
        .flat_map(|(speaker, intervals)| {
            union(intervals)
                .into_iter()
                .map(move |interval| Segment { interval, speaker })
        })
        .collect();
    Ok(Annotation {
        id,
        speakers,
        segments,
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Reference(usize),
    Hypothesis(usize),
    Collar,
    Uem,
}
#[derive(Clone, Copy)]
struct Event {
    time: f64,
    kind: Kind,
    delta: i32,
}

struct Slice {
    interval: Interval,
    reference: Vec<usize>,
    hypothesis: Vec<usize>,
}

fn slices(
    reference: &Annotation,
    hypothesis: &Annotation,
    options: &Options,
) -> Result<Vec<Slice>> {
    let mut events = Vec::new();
    let mut add = |interval: Interval, kind| {
        events.push(Event {
            time: interval.start,
            kind,
            delta: 1,
        });
        events.push(Event {
            time: interval.end,
            kind,
            delta: -1,
        });
    };
    for s in &reference.segments {
        add(s.interval, Kind::Reference(s.speaker));
        if options.collar > 0.0 {
            for time in [s.interval.start, s.interval.end] {
                add(
                    Interval {
                        start: time - options.collar / 2.0,
                        end: time + options.collar / 2.0,
                    },
                    Kind::Collar,
                );
            }
        }
    }
    for s in &hypothesis.segments {
        add(s.interval, Kind::Hypothesis(s.speaker));
    }
    let uem = if options.uem.is_empty() {
        vec![Interval {
            start: 0.0,
            end: options.recording_duration,
        }]
    } else {
        union(options.uem.clone())
    };
    for interval in uem {
        add(interval, Kind::Uem);
    }
    events.sort_by(|a, b| a.time.total_cmp(&b.time));
    let mut r = BTreeSet::new();
    let mut h = BTreeSet::new();
    let (mut collar, mut uem) = (0, 0);
    let mut out = Vec::new();
    let mut active_labels = 0;
    let mut pair_operations = 0;
    let mut i = 0;
    while i < events.len() {
        let time = events[i].time;
        while i < events.len() && events[i].time == time {
            let e = events[i];
            match e.kind {
                Kind::Reference(s) => {
                    if e.delta > 0 {
                        r.insert(s);
                    } else {
                        r.remove(&s);
                    }
                }
                Kind::Hypothesis(s) => {
                    if e.delta > 0 {
                        h.insert(s);
                    } else {
                        h.remove(&s);
                    }
                }
                Kind::Collar => collar += e.delta,
                Kind::Uem => uem += e.delta,
            }
            i += 1;
        }
        if i < events.len() && uem > 0 && collar == 0 && !(options.skip_overlap && r.len() > 1) {
            active_labels += r.len() + h.len();
            pair_operations += r.len() * h.len();
            ensure!(
                active_labels <= 1_000_000 && pair_operations <= MAX_PAIR_OPERATIONS,
                "Evaluation resource limit exceeded; select a smaller range"
            );
            out.push(Slice {
                interval: Interval {
                    start: time,
                    end: events[i].time,
                },
                reference: r.iter().copied().collect(),
                hypothesis: h.iter().copied().collect(),
            });
        }
    }
    Ok(out)
}

/// Exact maximum-weight assignment by a padded Hungarian algorithm, O(n^3).
/// Result indexes reference rows by hypothesis columns; dummy pairs are None.
fn assignment(weights: &[Vec<f64>], columns: usize) -> Vec<Option<usize>> {
    let rows = weights.len();
    let n = rows.max(columns);
    let (mut u, mut v) = (vec![0.0; n + 1], vec![0.0; n + 1]);
    let (mut p, mut way) = (vec![0; n + 1], vec![0; n + 1]);
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![f64::INFINITY; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let (mut delta, mut j1) = (f64::INFINITY, 0);
            for j in 1..=n {
                if !used[j] {
                    let weight = weights
                        .get(i0 - 1)
                        .and_then(|row| row.get(j - 1))
                        .copied()
                        .unwrap_or(0.0);
                    let cur = -weight - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    (1..=columns)
        .map(|j| {
            let i = p[j];
            (i > 0 && i <= rows && weights[i - 1][j - 1] > 0.0).then_some(i.wrapping_sub(1))
        })
        .collect()
}

/// Evaluate immutable UTF-8 RTTMs against an explicit recording clock.
/// Empty reference is valid but its rate is undefined. Missing files are an
/// adapter error, never implicitly treated as empty reference.
pub fn evaluate(reference_rttm: &str, hypothesis_rttm: &str, options: Options) -> Result<Report> {
    ensure!(
        options.recording_duration.is_finite()
            && options.recording_duration > 0.0
            && options.recording_duration <= 1e9,
        "Invalid recording duration"
    );
    ensure!(
        options.collar.is_finite() && options.collar >= 0.0 && options.collar <= 1e9,
        "Invalid collar width"
    );
    ensure!(
        options.uem.len() <= MAX_SEGMENTS
            && options
                .uem
                .iter()
                .all(|i| valid_interval(*i, options.recording_duration)),
        "Invalid UEM range"
    );
    let reference = parse(reference_rttm, options.recording_duration)?;
    let hypothesis = parse(hypothesis_rttm, options.recording_duration)?;
    let slices = slices(&reference, &hypothesis, &options)?;
    let mut weights = vec![vec![0.0; hypothesis.speakers.len()]; reference.speakers.len()];
    for slice in &slices {
        for &r in &slice.reference {
            for &h in &slice.hypothesis {
                weights[r][h] += slice.interval.end - slice.interval.start;
            }
        }
    }
    let assigned = assignment(&weights, hypothesis.speakers.len());
    let mapping = assigned
        .iter()
        .enumerate()
        .filter_map(|(h, r)| {
            r.map(|r| {
                (
                    hypothesis.speakers[h].clone(),
                    reference.speakers[r].clone(),
                )
            })
        })
        .collect();
    let mut seconds = Components::default();
    let mut reference_speaker_seconds = 0.0;
    let mut evaluated_seconds = 0.0;
    let mut disagreements: Vec<Disagreement> = Vec::new();
    let mut output_label_bytes = 0;
    for slice in slices {
        let dt = slice.interval.end - slice.interval.start;
        let nr = slice.reference.len();
        let nh = slice.hypothesis.len();
        let correct = slice
            .hypothesis
            .iter()
            .filter(|&&h| assigned[h].is_some_and(|r| slice.reference.contains(&r)))
            .count();
        let miss = nr.saturating_sub(nh) as f64 * dt;
        let false_alarm = nh.saturating_sub(nr) as f64 * dt;
        let confusion = (nr.min(nh) - correct) as f64 * dt;
        seconds.miss += miss;
        seconds.false_alarm += false_alarm;
        seconds.confusion += confusion;
        reference_speaker_seconds += nr as f64 * dt;
        evaluated_seconds += dt;
        if miss + false_alarm + confusion > 0.0 {
            output_label_bytes += slice
                .reference
                .iter()
                .map(|&i| reference.speakers[i].len())
                .sum::<usize>()
                + slice
                    .hypothesis
                    .iter()
                    .map(|&i| hypothesis.speakers[i].len())
                    .sum::<usize>();
            ensure!(
                output_label_bytes <= 32 * 1024 * 1024,
                "Disagreement report exceeds 32 MiB of labels; select a smaller range"
            );
            let r: Vec<_> = slice
                .reference
                .iter()
                .map(|&i| reference.speakers[i].clone())
                .collect();
            let h: Vec<_> = slice
                .hypothesis
                .iter()
                .map(|&i| hypothesis.speakers[i].clone())
                .collect();
            if let Some(last) = disagreements.last_mut() {
                if last.interval.end == slice.interval.start
                    && last.reference_speakers == r
                    && last.hypothesis_speakers == h
                {
                    last.interval.end = slice.interval.end;
                    last.seconds.miss += miss;
                    last.seconds.false_alarm += false_alarm;
                    last.seconds.confusion += confusion;
                    continue;
                }
            }
            disagreements.push(Disagreement {
                interval: slice.interval,
                reference_speakers: r,
                hypothesis_speakers: h,
                seconds: Components {
                    miss,
                    false_alarm,
                    confusion,
                },
            });
        }
    }
    let rate = (reference_speaker_seconds > 0.0).then(|| {
        (seconds.miss + seconds.false_alarm + seconds.confusion) / reference_speaker_seconds
    });
    ensure!(
        rate.is_none_or(f64::is_finite),
        "Rate exceeds floating-point range"
    );
    Ok(Report {
        algorithm: "kikimimic-der-v1-hungarian".into(),
        interpretation: options.mode.description().into(),
        der: if options.mode == Mode::HumanReference {
            rate
        } else {
            None
        },
        model_disagreement_rate: if options.mode == Mode::ModelDisagreement {
            rate
        } else {
            None
        },
        reference_sha256: sha256(reference_rttm),
        hypothesis_sha256: sha256(hypothesis_rttm),
        reference_recording_id: reference.id,
        hypothesis_recording_id: hypothesis.id,
        options,
        seconds,
        reference_speaker_seconds,
        evaluated_seconds,
        mapping,
        disagreements,
    })
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "diarization_evaluation_tests.rs"]
mod tests;
