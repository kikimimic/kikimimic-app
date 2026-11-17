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

#[cfg(test)]
mod tests {
    use super::*;
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
