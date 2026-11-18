//! Model-independent, evidence-checked meeting artifacts. No inference or IO.
use crate::library::Line;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum Kind {
    Summary,
    Decision,
    OpenQuestion,
    Action,
}

/// AI organization, never a confirmed human decision without review.
#[derive(Debug, Serialize, Deserialize)]
pub struct Item {
    pub kind: Kind,
    pub text: String,
    pub evidence: Vec<u64>,
    /// Exact source wording only; None means unknown, not inferred.
    pub owner: Option<String>,
    pub deadline: Option<String>,
    /// Earlier item replaced or withdrawn by this item (zero-based index).
    pub supersedes: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Minutes {
    pub items: Vec<Item>,
}

impl Minutes {
    /// Reject dangling evidence, guessed metadata and forward/cyclic revisions.
    /// This checks provenance structure, not semantic truth: human review remains necessary.
    pub fn validate(&self, lines: &[Line]) -> anyhow::Result<()> {
        for (index, item) in self.items.iter().enumerate() {
            anyhow::ensure!(!item.text.trim().is_empty(), "empty item");
            anyhow::ensure!(!item.evidence.is_empty(), "missing evidence");
            let mut sources = Vec::new();
            for id in &item.evidence {
                let matches: Vec<_> = lines.iter().filter(|line| line.id == *id).collect();
                anyhow::ensure!(matches.len() == 1, "missing or ambiguous evidence {id}");
                sources.push(matches[0]);
            }
            for value in [&item.owner, &item.deadline].into_iter().flatten() {
                anyhow::ensure!(
                    !value.trim().is_empty()
                        && sources.iter().any(|line| line.text.contains(value)),
                    "metadata absent from original evidence"
                );
            }
            if let Some(previous) = item.supersedes {
                anyhow::ensure!(previous < index, "revision must reference an earlier item");
            }
        }
        Ok(())
    }
}

/// Complete ordered source for final minutes; callers may select a recent slice
/// for live summaries. JSON escaping prevents source text breaking framing.
pub fn source_json(lines: &[Line]) -> anyhow::Result<String> {
    Ok(serde_json::to_string(lines)?)
}

/// Where a recording's minutes live, next to its transcript.
pub const FILE: &str = "minutes.json";

/// The saved minutes, or `None` when none were made yet.
pub fn load(dir: &std::path::Path) -> anyhow::Result<Option<Minutes>> {
    match std::fs::read_to_string(dir.join(FILE)) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Validate against the transcript, then replace the file atomically so a
/// refused or interrupted save leaves the previous minutes intact.
pub fn save(dir: &std::path::Path, minutes: &Minutes, lines: &[Line]) -> anyhow::Result<()> {
    minutes.validate(lines)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(minutes)?)?;
    std::fs::rename(tmp, dir.join(FILE))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(id: u64, text: &str) -> Line {
        Line {
            id,
            lane: "mic".into(),
            start_ms: id * 1000,
            end_ms: id * 1000 + 900,
            lang: None,
            text: text.into(),
            translation: None,
            translation_lang: None,
            speaker: None,
        }
    }

    #[test]
    fn saved_minutes_come_back_and_unchecked_ones_are_refused() {
        let dir = std::env::temp_dir().join(format!("kkm-minutes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [said(1, "木曜までに直します")];
        assert!(load(&dir).unwrap().is_none());
        let ok = Minutes {
            items: vec![Item {
                kind: Kind::Action,
                text: "直す".into(),
                evidence: vec![1],
                owner: None,
                deadline: Some("木曜".into()),
                supersedes: None,
            }],
        };
        save(&dir, &ok, &lines).unwrap();
        assert_eq!(
            load(&dir).unwrap().unwrap().items[0].deadline.as_deref(),
            Some("木曜")
        );
        let dangling = Minutes {
            items: vec![Item {
                kind: Kind::Summary,
                text: "x".into(),
                evidence: vec![9],
                owner: None,
                deadline: None,
                supersedes: None,
            }],
        };
        assert!(save(&dir, &dangling, &lines).is_err());
        assert_eq!(
            load(&dir).unwrap().unwrap().items.len(),
            1,
            "a refused save keeps the old file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn refuses_unsupported_output_and_forward_revisions() {
        let mut minutes = Minutes {
            items: vec![Item {
                kind: Kind::Action,
                text: "Review".into(),
                evidence: vec![42],
                owner: None,
                deadline: None,
                supersedes: None,
            }],
        };
        assert!(minutes.validate(&[]).is_err());
        let lines = vec![Line {
            id: 42,
            lane: "mic".into(),
            start_ms: 0,
            end_ms: 100,
            lang: None,
            text: "林が明日確認する / review tomorrow".into(),
            translation: None,
            translation_lang: None,
            speaker: None,
        }];
        assert!(minutes.validate(&lines).is_ok());
        minutes.items[0].owner = Some("田中".into());
        assert!(minutes.validate(&lines).is_err());
        minutes.items[0].owner = Some("林".into());
        minutes.items[0].deadline = Some("明日".into());
        assert!(minutes.validate(&lines).is_ok());
        minutes.items[0].supersedes = Some(0);
        assert!(minutes.validate(&lines).is_err());
        assert!(source_json(&lines).unwrap().contains("startMs"));
    }
}
