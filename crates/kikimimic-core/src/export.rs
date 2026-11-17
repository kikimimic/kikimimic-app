//! Pure transcript exporters. Timing remains on the recording clock.
use crate::library::Session;

fn clock(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
fn single_line(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join(" ")
}
/// Markdown with recording metadata and translated block quotes.
pub fn markdown(session: &Session) -> String {
    let mut out = format!(
        "# {}\n\n日時: {}\n\n長さ: {} ms\n\n",
        session.title.as_deref().unwrap_or(&session.name),
        session.started.as_deref().unwrap_or("不明"),
        session.duration_ms
    );
    for line in &session.lines {
        out.push_str(&format!(
            "**[{:02}:{:02}] {}:** {}\n\n",
            line.start_ms / 60_000,
            line.start_ms / 1000 % 60,
            line.speaker.as_deref().unwrap_or(if line.lane == "mic" {
                "自分"
            } else {
                "相手"
            }),
            single_line(&line.text)
        ));
        if let Some(text) = &line.translation {
            for part in text.lines() {
                out.push_str(&format!("> {part}\n"));
            }
            out.push('\n');
        }
    }
    out
}
/// SRT, with the translation on the second line when available.
pub fn srt(session: &Session) -> String {
    let mut out = String::new();
    for (i, line) in session.lines.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n",
            i + 1,
            clock(line.start_ms),
            clock(line.end_ms),
            match &line.speaker {
                Some(speaker) => format!("{}: {}", single_line(speaker), single_line(&line.text)),
                None => single_line(&line.text),
            }
        ));
        if let Some(text) = &line.translation {
            out.push_str(&single_line(text));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exports_metadata_original_translation_and_exact_cue() {
        let mut session = Session {
            name: "20261116090000".into(),
            dir: String::new(),
            title: Some("定例".into()),
            started: Some("20261116090000".into()),
            duration_ms: 4000,
            lanes: vec![],
            lines: vec![crate::library::Line {
                id: 1,
                lane: "mic".into(),
                start_ms: 1234,
                end_ms: 3456,
                lang: None,
                text: "こんにちは".into(),
                translation: Some("Hello".into()),
                translation_lang: None,
                speaker: None,
            }],
        };
        assert_eq!(
            srt(&session),
            "1\n00:00:01,234 --> 00:00:03,456\nこんにちは\nHello\n\n"
        );
        let md = markdown(&session);
        assert!(md.starts_with("# 定例\n"));
        assert!(md.contains("日時: 20261116090000"));
        assert!(md.contains("長さ: 4000 ms"));
        assert!(md.contains("**[00:01] 自分:** こんにちは\n\n> Hello"));
        session.lines[0].lane = "speaker".into();
        session.lines[0].translation = None;
        assert!(markdown(&session).contains("相手:**"));
        assert_eq!(
            srt(&session),
            "1\n00:00:01,234 --> 00:00:03,456\nこんにちは\n\n"
        );
        session.lines.clear();
        assert_eq!(srt(&session), "");
    }
    #[test]
    fn timestamps_preserve_milliseconds_and_hours() {
        assert_eq!(clock(3_661_234), "01:01:01,234");
        assert_eq!(clock(0), "00:00:00,000");
    }
    #[test]
    fn multiline_subtitles_cannot_create_extra_cues() {
        assert_eq!(single_line("hello\n\nworld"), "hello  world");
    }
}
