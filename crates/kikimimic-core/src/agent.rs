//! Asking a coding agent (Claude Code, Codex) about a meeting. The agent is
//! an external CLI the user already signed in to; this module builds what it
//! is sent and checks what comes back. Answers must quote the transcript;
//! a quote that is not in the original is treated as no answer at all.

use serde::{Deserialize, Serialize};

use crate::library::Session;
use crate::meeting_qa::{Answer, Citation, Snapshot, Source};
use crate::minutes::Minutes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::ClaudeCode, Agent::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    /// Program and arguments; the prompt goes to stdin so it never shows up
    /// in the process list and has no length limit.
    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Agent::ClaudeCode => ("claude", &["-p", "--output-format", "text"]),
            // Read-only: answering about a meeting needs no tools that change anything.
            Agent::Codex => (
                "codex",
                &[
                    "exec",
                    "--sandbox",
                    "read-only",
                    "--skip-git-repo-check",
                    "-",
                ],
            ),
        }
    }

    /// Who receives what is sent: shown before anything leaves the Mac.
    pub fn recipient(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Anthropic",
            Agent::Codex => "OpenAI",
        }
    }
}

/// The transcript as the validator sees it: what the agent may quote.
pub fn snapshot(session: &Session) -> Snapshot {
    Snapshot {
        meeting_id: session.name.clone(),
        revision: session.lines.len() as u64,
        sources: session
            .lines
            .iter()
            .map(|l| Source {
                utterance_id: l.id.to_string(),
                start_ms: l.start_ms,
                original: l.text.clone(),
            })
            .collect(),
    }
}

fn clock(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms / 1000 % 60)
}

/// One line per utterance: `[id] mm:ss speaker: original`. Translations are
/// left out so the agent can only quote what was actually said.
fn transcript(session: &Session) -> String {
    session
        .lines
        .iter()
        .map(|l| {
            let who =
                l.speaker
                    .as_deref()
                    .unwrap_or(if l.lane == "mic" { "自分" } else { "相手" });
            format!(
                "[{}] {} {}: {}",
                l.id,
                clock(l.start_ms),
                who,
                l.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn header(session: &Session) -> String {
    format!(
        "会議: {}\n",
        session.title.as_deref().unwrap_or(&session.name)
    )
}

pub fn question_prompt(session: &Session, question: &str) -> String {
    format!(
        "あなたは会議の文字起こしだけを根拠に質問に答えます。推測や外部の知識は使いません。\n\
         各行は [id] 時刻 話者: 発言 です。\n\
         答えは JSON 1 つだけで返してください:\n\
         {{\"answer\": \"答え\", \"citations\": [{{\"id\": \"発言の id\", \"quote\": \"その発言からそのまま抜き出した一部\"}}]}}\n\
         根拠が文字起こしに無ければ {{\"answer\": null, \"citations\": []}} を返してください。\n\n\
         {}\n{}\n\n質問: {}\n",
        header(session),
        transcript(session),
        question.trim()
    )
}

/// The first JSON object in `output`, allowing prose or a code fence around it.
fn json_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    (end > start).then(|| &output[start..=end])
}

#[derive(Deserialize)]
struct Reply {
    answer: Option<String>,
    #[serde(default)]
    citations: Vec<ReplyCitation>,
}

#[derive(Deserialize)]
struct ReplyCitation {
    id: serde_json::Value,
    quote: String,
}

/// Read the agent's reply, then keep it only if every quote is exact.
pub fn parse_answer(snapshot: &Snapshot, output: &str) -> Answer {
    let Some(reply) = json_object(output).and_then(|j| serde_json::from_str::<Reply>(j).ok())
    else {
        return Answer::Unconfirmed;
    };
    let Some(text) = reply.answer else {
        return Answer::Unconfirmed;
    };
    let citations = reply
        .citations
        .into_iter()
        .map(|c| Citation {
            // Agents write ids as numbers or strings; both mean the same line.
            utterance_id: match c.id {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            },
            quote: c.quote,
        })
        .collect();
    snapshot.validate(
        &snapshot.meeting_id,
        snapshot.revision,
        Answer::Supported { text, citations },
    )
}

pub fn minutes_prompt(session: &Session) -> String {
    format!(
        "次の会議の文字起こしから議事録を作ります。文字起こしに無いことは書きません。\n\
         各行は [id] 時刻 話者: 発言 です。\n\
         JSON 1 つだけで返してください:\n\
         {{\"items\": [{{\"kind\": \"Summary\" | \"Decision\" | \"Action\" | \"OpenQuestion\", \"text\": \"内容\", \"evidence\": [根拠の発言の id], \"owner\": null または発言中にそのまま出てくる担当者名, \"deadline\": null または発言中にそのまま出てくる期限, \"supersedes\": null}}]}}\n\
         すべての項目に根拠の id を 1 つ以上付けてください。担当者と期限は、根拠の発言にその言葉がそのまま無ければ null にします。\n\n\
         {}\n{}\n",
        header(session),
        transcript(session)
    )
}

/// Minutes from the agent's reply, checked against the transcript.
pub fn parse_minutes(session: &Session, output: &str) -> anyhow::Result<Minutes> {
    let json =
        json_object(output).ok_or_else(|| anyhow::anyhow!("議事録の JSON が返りませんでした"))?;
    let minutes: Minutes = serde_json::from_str(json)?;
    minutes.validate(&session.lines)?;
    Ok(minutes)
}

/// Run `agent` with `prompt` on stdin and return what it printed.
pub fn run(agent: Agent, prompt: &str) -> anyhow::Result<String> {
    let (program, args) = agent.command();
    run_program(program, args, prompt)
}

fn run_program(program: &str, args: &[&str], input: &str) -> anyhow::Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "{program} を起動できません（インストールとサインインを確認してください）: {e}"
            )
        })?;
    // Write on a thread: a large prompt can fill the pipe while the agent
    // is already writing its answer.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    anyhow::ensure!(
        out.status.success(),
        "{program} が失敗しました: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Line;

    fn session() -> Session {
        let line = |id: u64, start_ms: u64, text: &str, speaker: &str| Line {
            id,
            lane: "mic".into(),
            start_ms,
            end_ms: start_ms + 2000,
            lang: None,
            text: text.into(),
            translation: Some("(translation)".into()),
            translation_lang: None,
            speaker: Some(speaker.into()),
        };
        Session {
            name: "20261117100000".into(),
            dir: String::new(),
            title: Some("週次定例".into()),
            started: Some("20261117100000".into()),
            duration_ms: 60_000,
            lanes: vec!["mic".into()],
            lines: vec![
                line(1, 4_000, "SRT には話者名を含めましょう。", "自分"),
                line(2, 9_000, "木曜までにエクスポーターを直します。", "Alex"),
            ],
        }
    }

    #[test]
    fn the_prompt_carries_ids_times_speakers_and_the_question_but_no_translations() {
        let p = question_prompt(&session(), "SRT はどうなった？");
        assert!(p.contains("[1] 00:04 自分: SRT には話者名を含めましょう。"));
        assert!(p.contains("[2] 00:09 Alex: 木曜までにエクスポーターを直します。"));
        assert!(p.contains("SRT はどうなった？"));
        assert!(
            !p.contains("(translation)"),
            "only the original may be quoted"
        );
    }

    #[test]
    fn an_answer_with_exact_quotes_is_kept() {
        let s = session();
        let out = "Sure.\n```json\n{\"answer\":\"話者名を含めます\",\"citations\":[{\"id\":\"1\",\"quote\":\"話者名を含めましょう\"}]}\n```";
        match parse_answer(&snapshot(&s), out) {
            Answer::Supported { text, citations } => {
                assert_eq!(text, "話者名を含めます");
                assert_eq!(
                    citations,
                    vec![Citation {
                        utterance_id: "1".into(),
                        quote: "話者名を含めましょう".into()
                    }]
                );
            }
            other => panic!("expected a supported answer, got {other:?}"),
        }
    }

    #[test]
    fn invented_quotes_unknown_ids_and_prose_are_unconfirmed() {
        let snap = snapshot(&session());
        for out in [
            "{\"answer\":\"x\",\"citations\":[{\"id\":\"1\",\"quote\":\"話者名を省きます\"}]}",
            "{\"answer\":\"x\",\"citations\":[{\"id\":\"9\",\"quote\":\"SRT\"}]}",
            "{\"answer\":\"x\",\"citations\":[]}",
            "{\"answer\":null,\"citations\":[]}",
            "I could not find it.",
        ] {
            assert_eq!(parse_answer(&snap, out), Answer::Unconfirmed, "{out}");
        }
    }

    #[test]
    fn minutes_come_back_validated() {
        let s = session();
        let ok = "{\"items\":[{\"kind\":\"Action\",\"text\":\"エクスポーターを直す\",\"evidence\":[2],\"owner\":null,\"deadline\":\"木曜\",\"supersedes\":null}]}";
        assert_eq!(
            parse_minutes(&s, ok).unwrap().items[0].deadline.as_deref(),
            Some("木曜")
        );
        let guessed = "{\"items\":[{\"kind\":\"Action\",\"text\":\"直す\",\"evidence\":[2],\"owner\":\"田中\",\"deadline\":null,\"supersedes\":null}]}";
        assert!(
            parse_minutes(&s, guessed).is_err(),
            "an owner nobody said is refused"
        );
        assert!(minutes_prompt(&s).contains("[2] 00:09 Alex"));
    }

    #[test]
    fn agents_read_the_prompt_from_stdin() {
        assert_eq!(
            Agent::ClaudeCode.command(),
            ("claude", &["-p", "--output-format", "text"][..])
        );
        assert_eq!(Agent::Codex.command().0, "codex");
        assert!(Agent::Codex.command().1.ends_with(&["-"]));
    }

    #[cfg(unix)]
    #[test]
    fn running_pipes_the_prompt_through_and_reports_failures() {
        assert_eq!(run_program("cat", &[], "hello").unwrap(), "hello");
        assert!(run_program("sh", &["-c", "echo nope >&2; exit 3"], "")
            .unwrap_err()
            .to_string()
            .contains("nope"));
        assert!(run_program("kkm-no-such-agent", &[], "").is_err());
    }
}
