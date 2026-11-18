//! Export destinations: write a recording to a folder the user chose, under a
//! file name and in a format the user chose. A note vault (Obsidian or any
//! other) is just one destination; nothing here knows about a specific app.
//!
//! What was last written to each destination is remembered next to the
//! recording (`exports.json`) as a hash of the rendered text, so an edit made
//! after writing shows up as a stale export without re-reading the target.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::library::Session;
use crate::minutes::Minutes;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// The destination's own template with `{{variable}}` placeholders.
    Template,
    /// SubRip, with speaker names (export::srt).
    Srt,
    /// Every line with its times, speaker and translation.
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    Manual,
    /// As soon as minutes exist for the recording.
    AfterMinutes,
    /// When a recording or an import finishes.
    AfterRecording,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub name: String,
    /// May start with `~/`.
    pub folder: String,
    /// A template like the body's, e.g. `{{date}} {{title}}.md`.
    pub file_name: String,
    pub format: Format,
    pub when: When,
    pub enabled: bool,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportState {
    NotWritten,
    Fresh,
    Stale,
}

pub const VARIABLES: &[&str] = &[
    "title",
    "date",
    "time",
    "duration",
    "attendees",
    "attendees_links",
    "summary",
    "decisions",
    "actions",
    "open",
    "transcript",
    "transcript_tr",
    "id",
];

pub const DEFAULT_TEMPLATE: &str = "---\ndate: {{date}} {{time}}\nattendees: [{{attendees_links}}]\nduration: {{duration}}\ntags: [meeting]\n---\n\n# {{title}}\n\n## 要約\n{{summary}}\n\n## 決定事項\n{{decisions}}\n\n## 宿題\n{{actions}}\n\n## 未決事項\n{{open}}\n\n## 文字起こし\n{{transcript}}\n";

/// The destination a new install starts with: Markdown notes in Documents.
pub fn defaults() -> Vec<Destination> {
    vec![Destination {
        id: "notes".into(),
        name: "Markdown ノート".into(),
        folder: "~/Documents/Kikimimic".into(),
        file_name: "{{date}} {{title}}.md".into(),
        format: Format::Template,
        when: When::Manual,
        enabled: true,
        template: DEFAULT_TEMPLATE.into(),
    }]
}

/// Fill `{{name}}` placeholders; unknown names stay as they are so a typo is
/// visible in the output instead of silently vanishing.
pub fn fill(template: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) if vars.contains_key(&after[..close]) => {
                out.push_str(&vars[&after[..close]]);
                rest = &after[close + 2..];
            }
            _ => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Who spoke, in order of first appearance.
fn attendees(session: &Session) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for name in session.lines.iter().filter_map(|l| l.speaker.as_deref()) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn speaker_or_lane(line: &crate::library::Line) -> &str {
    line.speaker.as_deref().unwrap_or(if line.lane == "mic" {
        "自分"
    } else {
        "相手"
    })
}

fn bullets(minutes: Option<&Minutes>, kind: crate::minutes::Kind) -> String {
    let Some(minutes) = minutes else {
        return "- なし".into();
    };
    let items: Vec<String> = minutes
        .items
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| {
            let who: Vec<&str> = [i.owner.as_deref(), i.deadline.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            if who.is_empty() {
                format!("- {}", i.text)
            } else {
                format!("- {}（{}）", i.text, who.join("・"))
            }
        })
        .collect();
    if items.is_empty() {
        "- なし".into()
    } else {
        items.join("\n")
    }
}

pub fn variables(session: &Session, minutes: Option<&Minutes>) -> BTreeMap<&'static str, String> {
    use crate::minutes::Kind;
    let started = session.started.as_deref().unwrap_or(&session.name);
    let digits = |r: std::ops::Range<usize>| started.get(r).unwrap_or("").to_string();
    let names = attendees(session);
    let transcript = |with_tr: bool| {
        session
            .lines
            .iter()
            .map(|l| {
                let mut s = format!(
                    "- {} **{}**: {}",
                    clock(l.start_ms),
                    speaker_or_lane(l),
                    l.text.trim()
                );
                if let (true, Some(tr)) = (with_tr, &l.translation) {
                    s.push_str(&format!("\n  - {}", tr.trim()));
                }
                s
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    BTreeMap::from([
        (
            "title",
            session
                .title
                .clone()
                .unwrap_or_else(|| session.name.clone()),
        ),
        (
            "date",
            format!("{}-{}-{}", digits(0..4), digits(4..6), digits(6..8)),
        ),
        ("time", format!("{}:{}", digits(8..10), digits(10..12))),
        ("duration", clock(session.duration_ms)),
        ("attendees", names.join(", ")),
        (
            "attendees_links",
            names
                .iter()
                .map(|n| format!("\"[[{n}]]\""))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "summary",
            if minutes.is_some() {
                bullets(minutes, Kind::Summary)
            } else {
                "（議事録はまだありません）".into()
            },
        ),
        ("decisions", bullets(minutes, Kind::Decision)),
        ("actions", bullets(minutes, Kind::Action)),
        ("open", bullets(minutes, Kind::OpenQuestion)),
        ("transcript", transcript(false)),
        ("transcript_tr", transcript(true)),
        ("id", session.name.clone()),
    ])
}

pub fn render(dest: &Destination, session: &Session, minutes: Option<&Minutes>) -> String {
    match dest.format {
        Format::Template => fill(&dest.template, &variables(session, minutes)),
        Format::Srt => crate::export::srt(session),
        Format::Json => serde_json::to_string_pretty(session).unwrap_or_default(),
    }
}

/// The file name, with path separators replaced so a title cannot escape the
/// folder.
pub fn file_name(dest: &Destination, session: &Session) -> String {
    fill(&dest.file_name, &variables(session, None))
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn path(dest: &Destination, session: &Session, home: &Path) -> PathBuf {
    let folder = match dest.folder.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(&dest.folder),
    };
    folder.join(file_name(dest, session))
}

const RECORD: &str = "exports.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Written {
    path: PathBuf,
    sha256: String,
}

fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn load(recording_dir: &Path) -> BTreeMap<String, Written> {
    std::fs::read_to_string(recording_dir.join(RECORD))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write the rendered text and remember what was written next to the recording.
pub fn write(
    dest: &Destination,
    session: &Session,
    minutes: Option<&Minutes>,
    recording_dir: &Path,
    home: &Path,
) -> anyhow::Result<PathBuf> {
    let text = render(dest, session, minutes);
    let target = path(dest, session, home);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, &text)?;
    let mut record = load(recording_dir);
    record.insert(
        dest.id.clone(),
        Written {
            path: target.clone(),
            sha256: digest(&text),
        },
    );
    let tmp = recording_dir.join(format!("{RECORD}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(&record)?)?;
    std::fs::rename(tmp, recording_dir.join(RECORD))?;
    Ok(target)
}

pub fn state(
    dest: &Destination,
    session: &Session,
    minutes: Option<&Minutes>,
    recording_dir: &Path,
) -> ExportState {
    match load(recording_dir).get(&dest.id) {
        None => ExportState::NotWritten,
        Some(w) if w.sha256 == digest(&render(dest, session, minutes)) => ExportState::Fresh,
        Some(_) => ExportState::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Line;
    use crate::minutes::{Item, Kind};

    fn line(
        id: u64,
        lane: &str,
        start_ms: u64,
        text: &str,
        speaker: Option<&str>,
        tr: Option<&str>,
    ) -> Line {
        Line {
            id,
            lane: lane.into(),
            start_ms,
            end_ms: start_ms + 2000,
            lang: None,
            text: text.into(),
            translation: tr.map(Into::into),
            translation_lang: None,
            speaker: speaker.map(Into::into),
        }
    }

    fn session() -> Session {
        Session {
            name: "20261117100000".into(),
            dir: String::new(),
            title: Some("週次定例".into()),
            started: Some("20261117100000".into()),
            duration_ms: 1_820_000,
            lanes: vec!["mic".into(), "speaker".into()],
            lines: vec![
                line(
                    1,
                    "mic",
                    4_000,
                    "始めます。",
                    Some("自分"),
                    Some("Let's start."),
                ),
                line(2, "speaker", 10_000, "SRT keeps names.", Some("Alex"), None),
                line(3, "mic", 18_000, "賛成です。", Some("自分"), None),
            ],
        }
    }

    fn dest(format: Format, template: &str) -> Destination {
        Destination {
            id: "notes".into(),
            name: "Notes".into(),
            folder: "~/Notes/Meetings".into(),
            file_name: "{{date}} {{title}}.md".into(),
            format,
            when: When::Manual,
            enabled: true,
            template: template.into(),
        }
    }

    #[test]
    fn variables_come_from_the_recording() {
        let v = variables(&session(), None);
        assert_eq!(v["title"], "週次定例");
        assert_eq!(v["date"], "2026-11-17");
        assert_eq!(v["time"], "10:00");
        assert_eq!(v["duration"], "30:20");
        assert_eq!(v["attendees"], "自分, Alex");
        assert_eq!(v["attendees_links"], "\"[[自分]]\", \"[[Alex]]\"");
        assert_eq!(
            v["transcript"],
            "- 00:04 **自分**: 始めます。\n- 00:10 **Alex**: SRT keeps names.\n- 00:18 **自分**: 賛成です。"
        );
        assert!(v["transcript_tr"].contains("- 00:04 **自分**: 始めます。\n  - Let's start."));
    }

    #[test]
    fn without_a_speaker_the_lane_is_named() {
        let mut s = session();
        s.lines[1].speaker = None;
        assert!(variables(&s, None)["transcript"].contains("**相手**: SRT keeps names."));
    }

    #[test]
    fn unknown_placeholders_are_left_visible() {
        let out = render(
            &dest(Format::Template, "# {{title}} {{nope}}"),
            &session(),
            None,
        );
        assert_eq!(out, "# 週次定例 {{nope}}");
    }

    #[test]
    fn minutes_fill_their_sections() {
        let minutes = Minutes {
            items: vec![
                Item {
                    kind: Kind::Decision,
                    text: "SRT に話者名を含める".into(),
                    evidence: vec![3],
                    owner: None,
                    deadline: None,
                    supersedes: None,
                },
                Item {
                    kind: Kind::Action,
                    text: "エクスポーターを更新".into(),
                    evidence: vec![2],
                    owner: Some("Alex".into()),
                    deadline: None,
                    supersedes: None,
                },
            ],
        };
        let v = variables(&session(), Some(&minutes));
        assert_eq!(v["decisions"], "- SRT に話者名を含める");
        assert_eq!(v["actions"], "- エクスポーターを更新（Alex）");
        assert_eq!(v["open"], "- なし");
        assert_eq!(
            variables(&session(), None)["summary"],
            "（議事録はまだありません）"
        );
    }

    #[test]
    fn file_names_cannot_leave_the_folder() {
        let mut s = session();
        s.title = Some("a/b: c".into());
        let d = dest(Format::Template, "");
        assert_eq!(file_name(&d, &s), "2026-11-17 a-b- c.md");
        assert_eq!(
            path(&d, &s, Path::new("/home/me")),
            PathBuf::from("/home/me/Notes/Meetings/2026-11-17 a-b- c.md")
        );
    }

    #[test]
    fn srt_and_json_formats_ignore_the_template() {
        let s = session();
        assert_eq!(
            render(&dest(Format::Srt, "x"), &s, None),
            crate::export::srt(&s)
        );
        let json: serde_json::Value =
            serde_json::from_str(&render(&dest(Format::Json, "x"), &s, None)).unwrap();
        assert_eq!(json["lines"][1]["speaker"], "Alex");
    }

    #[test]
    fn an_edit_after_writing_makes_the_export_stale() {
        let tmp = std::env::temp_dir().join(format!("kkm-dest-{}", std::process::id()));
        let (rec, home) = (tmp.join("rec"), tmp.join("home"));
        std::fs::create_dir_all(&rec).unwrap();
        let d = dest(Format::Template, "{{transcript}}");
        let mut s = session();
        assert_eq!(state(&d, &s, None, &rec), ExportState::NotWritten);
        let written = write(&d, &s, None, &rec, &home).unwrap();
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            render(&d, &s, None)
        );
        assert_eq!(state(&d, &s, None, &rec), ExportState::Fresh);
        s.lines[0].text = "始めましょう。".into();
        assert_eq!(state(&d, &s, None, &rec), ExportState::Stale);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn a_new_install_has_one_markdown_destination() {
        let d = defaults();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].format, Format::Template);
        assert_eq!(d[0].template, DEFAULT_TEMPLATE);
    }
}
