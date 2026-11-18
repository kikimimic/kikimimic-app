//! A minimal MCP server (JSON-RPC 2.0, one message per line on stdio) so a
//! coding agent in a terminal can read meetings: list, search, transcripts
//! and minutes. Read-only, local files only; the agent's own sign-in decides
//! where what it reads goes.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Answer one request line; `None` for notifications, which get no reply.
pub fn handle(base: &Path, folders: &[PathBuf], line: &str) -> Option<String> {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return Some(error(Value::Null, -32700, "parse error"));
    };
    // Notifications carry no id and get no reply.
    let id = request.get("id")?.clone();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "kikimimic", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            match call(base, folders, name, &params["arguments"]) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(e) => {
                    json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })
                }
            }
        }
        _ => return Some(error(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

fn error(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn tools() -> Value {
    let id = json!({ "type": "string", "description": "A meeting id from list_meetings, or \"latest\" for the newest one (the meeting being recorded, if any)" });
    json!([
        { "name": "list_meetings", "description": "Recorded meetings, newest first: id, start, title, length.",
          "inputSchema": { "type": "object", "properties": { "limit": { "type": "integer", "description": "How many (default 30)" } } } },
        { "name": "search_meetings", "description": "Find what was said: literal, case-insensitive match over transcripts, translations, titles and notes folders.",
          "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] } },
        { "name": "get_transcript", "description": "One meeting's transcript: [line id] mm:ss speaker: original (translation).",
          "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] } },
        { "name": "get_minutes", "description": "One meeting's saved minutes, each item with the line ids it is based on.",
          "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] } },
    ])
}

fn clock(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, ms / 1000 % 60)
}

/// The meeting directory for `id`; ids are directory names, never paths.
fn meeting_dir(base: &Path, id: &str) -> anyhow::Result<PathBuf> {
    if id == "latest" {
        let newest = crate::library::list(base).into_iter().next();
        return newest
            .map(|r| PathBuf::from(r.dir))
            .ok_or_else(|| anyhow::anyhow!("no meetings yet"));
    }
    anyhow::ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "unknown meeting id {id:?}"
    );
    let dir = base.join(id);
    anyhow::ensure!(dir.is_dir(), "unknown meeting id {id:?}");
    Ok(dir)
}

fn call(base: &Path, folders: &[PathBuf], name: &str, args: &Value) -> anyhow::Result<String> {
    let id = || args["id"].as_str().unwrap_or("").to_string();
    match name {
        "list_meetings" => {
            let limit = args["limit"].as_u64().unwrap_or(30) as usize;
            Ok(crate::library::list(base)
                .into_iter()
                .take(limit)
                .map(|r| {
                    format!(
                        "{} | {} | {} | {}",
                        r.name,
                        r.started.unwrap_or_default(),
                        r.title.unwrap_or_default(),
                        clock(r.duration_ms)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "search_meetings" => {
            let query = args["query"].as_str().unwrap_or("");
            let hits = crate::search::search(base, folders, query)?;
            Ok(if hits.is_empty() {
                "no matches".into()
            } else {
                hits.iter()
                    .take(50)
                    .map(|h| {
                        let id = h
                            .source
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        format!(
                            "{id} | {} | {}",
                            h.start_ms.map(clock).unwrap_or_default(),
                            h.text
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        "get_transcript" => {
            let session = crate::library::read(&meeting_dir(base, &id())?)?;
            let mut out = format!("# {}\n", session.title.as_deref().unwrap_or(&session.name));
            for l in &session.lines {
                let who = l.speaker.as_deref().unwrap_or(if l.lane == "mic" {
                    "自分"
                } else {
                    "相手"
                });
                out.push_str(&format!(
                    "[{}] {} {}: {}",
                    l.id,
                    clock(l.start_ms),
                    who,
                    l.text.trim()
                ));
                if let Some(tr) = &l.translation {
                    out.push_str(&format!(" ({})", tr.trim()));
                }
                out.push('\n');
            }
            Ok(out)
        }
        "get_minutes" => {
            let dir = meeting_dir(base, &id())?;
            Ok(match crate::minutes::load(&dir)? {
                Some(m) => serde_json::to_string_pretty(&m)?,
                None => "議事録はまだありません (no minutes yet)".into(),
            })
        }
        other => anyhow::bail!("unknown tool {other:?}"),
    }
}

/// Serve requests from `input` until it closes.
pub fn serve(
    base: &Path,
    folders: &[PathBuf],
    input: impl BufRead,
    mut output: impl Write,
) -> anyhow::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(base, folders, &line) {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!("kkm-mcp-{label}-{}", std::process::id()));
            for (name, text) in [
                ("20261116090000", "昨日の会議です"),
                ("20261117100000", "SRT に話者名を含めましょう"),
            ] {
                let dir = base.join(name);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("mic.wav"), []).unwrap();
                std::fs::write(
                    dir.join("transcript.jsonl"),
                    format!("{{\"type\":\"utterance\",\"id\":1,\"lane\":\"mic\",\"startMs\":4000,\"endMs\":6000,\"text\":\"{text}\"}}\n"),
                )
                .unwrap();
            }
            Self(base)
        }
        fn call(&self, request: Value) -> Value {
            serde_json::from_str(&handle(&self.0, &[], &request.to_string()).expect("a reply"))
                .unwrap()
        }
        fn tool(&self, name: &str, arguments: Value) -> Value {
            self.call(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn text(reply: &Value) -> &str {
        reply["result"]["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn initialize_announces_tools_and_echoes_the_protocol_version() {
        let f = Fixture::new("init");
        let r = f.call(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}));
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "kikimimic");
        assert!(handle(
            &f.0,
            &[],
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
    }

    #[test]
    fn the_tools_are_listed_with_schemas() {
        let f = Fixture::new("list");
        let r = f.call(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_meetings",
                "search_meetings",
                "get_transcript",
                "get_minutes"
            ]
        );
        assert!(r["result"]["tools"][2]["inputSchema"]["properties"]["id"].is_object());
    }

    #[test]
    fn transcripts_are_read_by_id_or_as_the_latest_meeting() {
        let f = Fixture::new("read");
        assert!(text(&f.tool("list_meetings", json!({}))).starts_with("20261117100000"));
        let latest = f.tool("get_transcript", json!({"id":"latest"}));
        assert!(text(&latest).contains("[1] 00:04 自分: SRT に話者名を含めましょう"));
        assert!(
            text(&f.tool("get_transcript", json!({"id":"20261116090000"})))
                .contains("昨日の会議です")
        );
        assert!(
            text(&f.tool("search_meetings", json!({"query":"話者名"}))).contains("20261117100000")
        );
        assert!(
            text(&f.tool("get_minutes", json!({"id":"latest"}))).contains("議事録はまだありません")
        );
    }

    #[test]
    fn bad_calls_are_errors_not_crashes() {
        let f = Fixture::new("bad");
        assert_eq!(
            f.tool("get_transcript", json!({"id":"../etc"}))["result"]["isError"],
            true
        );
        assert_eq!(f.tool("no_such_tool", json!({}))["result"]["isError"], true);
        assert_eq!(
            f.call(json!({"jsonrpc":"2.0","id":3,"method":"nope"}))["error"]["code"],
            -32601
        );
        let parse = handle(&f.0, &[], "{not json").unwrap();
        assert!(parse.contains("-32700"));
    }
}
