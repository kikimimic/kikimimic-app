//! Meetings from Google Calendar and which of them deserve a card now.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

// ponytail: fixed; become settings when someone asks.
/// The card appears this long before a meeting starts.
pub const LEAD: Duration = Duration::minutes(2);
/// A meeting is still worth a card this long after it started.
pub const GRACE: Duration = Duration::minutes(5);

#[derive(Clone, Debug, PartialEq)]
pub struct Meeting {
    /// `iCalUID@start`: one occurrence of a recurring meeting.
    pub key: String,
    pub title: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub join_url: Option<String>,
}

#[derive(Deserialize)]
struct Events {
    #[serde(default)]
    items: Vec<Event>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    id: String,
    #[serde(rename = "iCalUID")]
    ical_uid: Option<String>,
    status: Option<String>,
    event_type: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    hangout_link: Option<String>,
    start: EventTime,
    end: EventTime,
    #[serde(default)]
    attendees: Vec<Attendee>,
    conference_data: Option<ConferenceData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventTime {
    date_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attendee {
    #[serde(rename = "self", default)]
    is_self: bool,
    response_status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConferenceData {
    #[serde(default)]
    entry_points: Vec<EntryPoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryPoint {
    entry_point_type: Option<String>,
    uri: Option<String>,
}

const NOT_A_MEETING: [&str; 4] = ["outOfOffice", "focusTime", "workingLocation", "birthday"];

/// Meetings worth a card from one page of an events list.
pub fn parse_events(json: &str) -> Result<Vec<Meeting>, serde_json::Error> {
    let events: Events = serde_json::from_str(json)?;
    Ok(events.items.into_iter().filter_map(meeting).collect())
}

fn meeting(e: Event) -> Option<Meeting> {
    let time = |t: &EventTime| DateTime::parse_from_rfc3339(t.date_time.as_deref()?).ok();
    let (start, end) = (time(&e.start)?.to_utc(), time(&e.end)?.to_utc());
    let declined = e
        .attendees
        .iter()
        .any(|a| a.is_self && a.response_status.as_deref() == Some("declined"));
    if e.status.as_deref() == Some("cancelled")
        || declined
        || e.event_type
            .as_deref()
            .is_some_and(|t| NOT_A_MEETING.contains(&t))
    {
        return None;
    }
    let join_url = join_url(&e);
    Some(Meeting {
        key: format!(
            "{}@{}",
            e.ical_uid.as_deref().unwrap_or(&e.id),
            start.to_rfc3339()
        ),
        title: e.summary.filter(|t| !t.is_empty()),
        start,
        end,
        join_url,
    })
}

fn join_url(e: &Event) -> Option<String> {
    if let Some(link) = &e.hangout_link {
        return Some(link.clone());
    }
    let video = e
        .conference_data
        .iter()
        .flat_map(|c| &c.entry_points)
        .find_map(|p| {
            (p.entry_point_type.as_deref() == Some("video"))
                .then(|| p.uri.clone())
                .flatten()
        });
    video.or_else(|| {
        [&e.location, &e.description]
            .into_iter()
            .flatten()
            .find_map(|text| known_link(text))
    })
}

const MEETING_HOSTS: [&str; 5] = [
    "zoom.us",
    "teams.microsoft.com",
    "teams.live.com",
    "meet.google.com",
    "webex.com",
];

/// The first `https://` link in `text` whose host is a known meeting service.
fn known_link(text: &str) -> Option<String> {
    let text = text.replace("&amp;", "&");
    text.match_indices("https://").find_map(|(at, _)| {
        let link: String = text[at..]
            .chars()
            .take_while(|c| !c.is_whitespace() && !"\"'<>".contains(*c))
            .collect();
        let host = link["https://".len()..].split(['/', '?', '#']).next()?;
        MEETING_HOSTS
            .iter()
            .any(|h| host == *h || host.strip_suffix(h).is_some_and(|p| p.ends_with('.')))
            .then_some(link)
    })
}

/// Meetings whose card should be showing: from `LEAD` before the start until
/// `GRACE` after it, or the end if that comes first.
pub fn due(meetings: &[Meeting], now: DateTime<Utc>, dismissed: &HashSet<String>) -> Vec<Meeting> {
    meetings
        .iter()
        .filter(|m| {
            !dismissed.contains(&m.key)
                && now >= m.start - LEAD
                && now < (m.start + GRACE).min(m.end)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(items: &str) -> String {
        format!(r#"{{"items":[{items}]}}"#)
    }

    fn one(extra: &str) -> Vec<Meeting> {
        parse_events(&events(&format!(
            r#"{{"id":"e1","iCalUID":"u1@google.com","summary":"Sync",
               "start":{{"dateTime":"2026-11-06T10:00:00+09:00"}},
               "end":{{"dateTime":"2026-11-06T10:30:00+09:00"}}{extra}}}"#
        )))
        .unwrap()
    }

    #[test]
    fn a_plain_event_becomes_a_meeting() {
        let m = &one("")[0];
        assert_eq!(m.title.as_deref(), Some("Sync"));
        assert_eq!(
            m.start,
            "2026-11-06T01:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            m.end,
            "2026-11-06T01:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(m.key, "u1@google.com@2026-11-06T01:00:00+00:00");
        assert_eq!(m.join_url, None);
    }

    #[test]
    fn key_falls_back_to_the_event_id() {
        let m = parse_events(&events(
            r#"{"id":"e9","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}}"#,
        ))
        .unwrap();
        assert_eq!(m[0].key, "e9@2026-11-06T10:00:00+00:00");
        assert_eq!(m[0].title, None);
    }

    #[test]
    fn fractional_seconds_and_offsets_parse() {
        let m = parse_events(&events(
            r#"{"id":"a","start":{"dateTime":"2026-11-06T10:00:00.250-05:00"},"end":{"dateTime":"2026-11-06T11:00:00.5Z"}}"#,
        ))
        .unwrap();
        assert_eq!(
            m[0].start,
            "2026-11-06T15:00:00.250Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn events_nobody_attends_are_skipped() {
        let all = events(
            r#"{"id":"1","status":"cancelled","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
               {"id":"2","start":{"date":"2026-11-06"},"end":{"date":"2026-11-07"}},
               {"id":"3","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"},
                "attendees":[{"self":false,"responseStatus":"accepted"},{"self":true,"responseStatus":"declined"}]},
               {"id":"4","eventType":"outOfOffice","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
               {"id":"5","eventType":"focusTime","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
               {"id":"6","eventType":"workingLocation","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
               {"id":"7","eventType":"birthday","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
               {"id":"8","eventType":"default","attendees":[{"self":true,"responseStatus":"tentative"}],
                "start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}}"#,
        );
        let kept = parse_events(&all).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].key.starts_with("8@"));
    }

    #[test]
    fn someone_elses_decline_does_not_hide_the_event() {
        let m = one(
            r#","attendees":[{"responseStatus":"declined"},{"self":true,"responseStatus":"accepted"}]"#,
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn hangout_link_wins() {
        let m = one(r#","hangoutLink":"https://meet.google.com/aaa-bbbb-ccc",
               "conferenceData":{"entryPoints":[{"entryPointType":"video","uri":"https://zoom.us/j/1"}]},
               "location":"https://zoom.us/j/2""#);
        assert_eq!(
            m[0].join_url.as_deref(),
            Some("https://meet.google.com/aaa-bbbb-ccc")
        );
    }

    #[test]
    fn video_entry_point_is_next() {
        let m = one(r#","conferenceData":{"entryPoints":[
                 {"entryPointType":"phone","uri":"tel:+1"},
                 {"entryPointType":"video","uri":"https://example.com/room"}]},
               "location":"https://zoom.us/j/2""#);
        assert_eq!(m[0].join_url.as_deref(), Some("https://example.com/room"));
    }

    #[test]
    fn known_hosts_in_free_text_are_last() {
        let m = one(r#","location":"https://example.com/not-a-meeting",
               "description":"Join: https://acme.zoom.us/j/123?pwd=x and https://meet.google.com/zzz-zzzz-zzz""#);
        assert_eq!(
            m[0].join_url.as_deref(),
            Some("https://acme.zoom.us/j/123?pwd=x")
        );
        let t = one(
            r#","description":"<a href=\"https://teams.microsoft.com/l/meetup-join/abc\">x</a>""#,
        );
        assert_eq!(
            t[0].join_url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc")
        );
        let none = one(
            r#","description":"http://zoom.us/j/1 https://evilzoom.us/j/1 https://zoom.us.evil.com/j/1""#,
        );
        assert_eq!(
            none[0].join_url, None,
            "http and look-alike hosts are not joined"
        );
    }

    #[test]
    fn an_empty_title_is_no_title() {
        let m = parse_events(&events(
            r#"{"id":"a","summary":"","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}}"#,
        ))
        .unwrap();
        assert_eq!(m[0].title, None);
    }

    fn at(minutes_from_start: i64) -> (Vec<Meeting>, DateTime<Utc>) {
        let start: DateTime<Utc> = "2026-11-06T01:00:00Z".parse().unwrap();
        let m = Meeting {
            key: "k".into(),
            title: None,
            start,
            end: start + Duration::minutes(30),
            join_url: None,
        };
        (vec![m], start + Duration::minutes(minutes_from_start))
    }

    #[test]
    fn due_opens_at_the_lead_and_closes_after_the_grace() {
        let none = HashSet::new();
        let count = |min| {
            let (m, now) = at(min);
            due(&m, now, &none).len()
        };
        assert_eq!(count(-3), 0, "before the lead");
        assert_eq!(count(-2), 1, "exactly at the lead");
        assert_eq!(count(0), 1, "at the start");
        assert_eq!(count(4), 1, "inside the grace");
        assert_eq!(count(5), 0, "after the grace");
    }

    #[test]
    fn a_short_meeting_is_over_when_it_ends() {
        let (mut m, _) = at(0);
        m[0].end = m[0].start + Duration::minutes(3);
        let now = m[0].start + Duration::minutes(3);
        assert!(due(&m, now, &HashSet::new()).is_empty());
        assert_eq!(
            due(&m, now - Duration::seconds(1), &HashSet::new()).len(),
            1
        );
    }

    #[test]
    fn dismissed_meetings_stay_dismissed() {
        let (m, now) = at(0);
        let dismissed = HashSet::from(["k".to_string()]);
        assert!(due(&m, now, &dismissed).is_empty());
    }
}
