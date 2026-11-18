//! The recording list as a timeline: recordings grouped by the day they
//! started, and how far each one is through its post-processing. Pure: the
//! app supplies today's date because the core has no time zone.

use std::path::Path;

use crate::destinations::{Destination, ExportState, When};
use crate::library::{Recording, Session};
use crate::minutes::Minutes;

#[derive(Debug, PartialEq)]
pub struct Day<'a> {
    /// `yyyy-mm-dd`, or `None` for recordings whose start is unknown.
    pub date: Option<String>,
    pub recordings: Vec<&'a Recording>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayLabel {
    Today,
    Yesterday,
    /// Month, day and weekday (0 = Sunday).
    Date {
        month: u32,
        day: u32,
        weekday: u32,
    },
    Unknown,
}

/// The step a recording is waiting for, in the order they have to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Transcribe,
    Speakers,
    Minutes,
    Export,
}

/// Group `recordings` (already newest first) into days, keeping their order.
pub fn group_by_day(recordings: &[Recording]) -> Vec<Day<'_>> {
    let date = |r: &Recording| {
        r.started
            .as_deref()
            .filter(|s| s.len() >= 8 && s[..8].bytes().all(|b| b.is_ascii_digit()))
            .map(|s| format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8]))
    };
    let mut days: Vec<Day> = Vec::new();
    let mut unknown = Vec::new();
    for r in recordings {
        match date(r) {
            None => unknown.push(r),
            Some(d) => match days.last_mut() {
                Some(last) if last.date.as_deref() == Some(d.as_str()) => last.recordings.push(r),
                _ => days.push(Day {
                    date: Some(d),
                    recordings: vec![r],
                }),
            },
        }
    }
    if !unknown.is_empty() {
        days.push(Day {
            date: None,
            recordings: unknown,
        });
    }
    days
}

fn ymd(date: &str) -> Option<(i64, u32, u32)> {
    let mut it = date.split('-');
    let y = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    Some((y, m, d))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `date` and `today` are `yyyy-mm-dd`.
pub fn day_label(date: Option<&str>, today: &str) -> DayLabel {
    let (Some((y, m, d)), Some((ty, tm, td))) = (date.and_then(ymd), ymd(today)) else {
        return DayLabel::Unknown;
    };
    let n = days_from_civil(y, m, d);
    match days_from_civil(ty, tm, td) - n {
        0 => DayLabel::Today,
        1 => DayLabel::Yesterday,
        // 1970-01-01 was a Thursday.
        _ => DayLabel::Date {
            month: m,
            day: d,
            weekday: (n + 4).rem_euclid(7) as u32,
        },
    }
}

/// What still has to happen to this recording, or `None` when it is done.
/// Only destinations that write by themselves count: a manual destination
/// that was never used is the user's choice, not unfinished work.
pub fn next_stage(
    session: &Session,
    recording_dir: &Path,
    minutes: Option<&Minutes>,
    destinations: &[Destination],
) -> Option<Stage> {
    if session.lines.is_empty() {
        return Some(Stage::Transcribe);
    }
    if !recording_dir.join(crate::postprocess::RTTM).exists() {
        return Some(Stage::Speakers);
    }
    let Some(minutes) = minutes else {
        return Some(Stage::Minutes);
    };
    destinations
        .iter()
        .filter(|d| d.enabled && d.when != When::Manual)
        .any(|d| {
            crate::destinations::state(d, session, Some(minutes), recording_dir)
                != ExportState::Fresh
        })
        .then_some(Stage::Export)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::Format;
    use crate::library::Line;

    fn rec(name: &str, started: Option<&str>) -> Recording {
        Recording {
            name: name.into(),
            dir: String::new(),
            title: None,
            started: started.map(Into::into),
            duration_ms: 0,
            lanes: vec![],
            snippet: String::new(),
            utterances: 0,
        }
    }

    #[test]
    fn recordings_are_grouped_by_the_day_they_started() {
        let recs = vec![
            rec("a", Some("20261117100000")),
            rec("b", Some("20261117090000")),
            rec("c", Some("20261116170000")),
            rec("odd", None),
        ];
        let days = group_by_day(&recs);
        let shape: Vec<(Option<&str>, Vec<&str>)> = days
            .iter()
            .map(|d| {
                (
                    d.date.as_deref(),
                    d.recordings.iter().map(|r| r.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (Some("2026-11-17"), vec!["a", "b"]),
                (Some("2026-11-16"), vec!["c"]),
                (None, vec!["odd"]),
            ]
        );
    }

    #[test]
    fn days_are_named_relative_to_today() {
        assert_eq!(day_label(Some("2026-11-17"), "2026-11-17"), DayLabel::Today);
        assert_eq!(
            day_label(Some("2026-11-16"), "2026-11-17"),
            DayLabel::Yesterday
        );
        // Across a month boundary.
        assert_eq!(
            day_label(Some("2026-11-30"), "2026-12-01"),
            DayLabel::Yesterday
        );
        assert_eq!(
            day_label(Some("2026-11-14"), "2026-11-17"),
            DayLabel::Date {
                month: 11,
                day: 14,
                weekday: 6
            }
        );
        assert_eq!(day_label(None, "2026-11-17"), DayLabel::Unknown);
    }

    fn session(lines: usize) -> Session {
        Session {
            name: "20261117100000".into(),
            dir: String::new(),
            title: Some("定例".into()),
            started: Some("20261117100000".into()),
            duration_ms: 10_000,
            lanes: vec!["mic".into()],
            lines: (0..lines as u64)
                .map(|id| Line {
                    id,
                    lane: "mic".into(),
                    start_ms: id * 1000,
                    end_ms: id * 1000 + 900,
                    lang: None,
                    text: "はい".into(),
                    translation: None,
                    translation_lang: None,
                    speaker: Some("自分".into()),
                })
                .collect(),
        }
    }

    fn auto_dest() -> Destination {
        Destination {
            id: "notes".into(),
            name: "Notes".into(),
            folder: "/tmp".into(),
            file_name: "x.md".into(),
            format: Format::Template,
            when: When::AfterMinutes,
            enabled: true,
            template: "{{transcript}}".into(),
            template_file: None,
        }
    }

    #[test]
    fn the_next_stage_follows_the_processing_order() {
        let dir = std::env::temp_dir().join(format!("kkm-timeline-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("postprocess")).unwrap();
        let minutes = Minutes { items: vec![] };
        let mut dest = auto_dest();
        dest.folder = dir.join("notes").display().to_string();
        let dests = [dest];
        assert_eq!(
            next_stage(&session(0), &dir, None, &dests),
            Some(Stage::Transcribe)
        );
        assert_eq!(
            next_stage(&session(2), &dir, None, &dests),
            Some(Stage::Speakers)
        );
        std::fs::write(dir.join(crate::postprocess::RTTM), "").unwrap();
        assert_eq!(
            next_stage(&session(2), &dir, None, &dests),
            Some(Stage::Minutes)
        );
        assert_eq!(
            next_stage(&session(2), &dir, Some(&minutes), &dests),
            Some(Stage::Export)
        );
        let home = dir.join("home");
        crate::destinations::write(&dests[0], &session(2), Some(&minutes), &dir, &home).unwrap();
        assert_eq!(next_stage(&session(2), &dir, Some(&minutes), &dests), None);
        // A manual or disabled destination is never pending.
        let mut manual = dests[0].clone();
        manual.id = "manual".into();
        manual.when = When::Manual;
        assert_eq!(
            next_stage(
                &session(2),
                &dir,
                Some(&minutes),
                &[dests[0].clone(), manual]
            ),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
