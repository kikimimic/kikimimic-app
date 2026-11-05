//! The user's dictionary: misrecognitions and their correct spelling.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Glossary {
    pub entries: Vec<GlossaryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryEntry {
    /// What the recognizer writes.
    pub wrong: String,
    /// What it should say.
    pub right: String,
}

impl Glossary {
    pub fn validate(&self) -> Result<(), String> {
        for e in &self.entries {
            if e.wrong.trim().is_empty() || e.right.trim().is_empty() {
                return Err("glossary entries need both a wrong and a right spelling".into());
            }
        }
        Ok(())
    }

    /// Correct a committed utterance before it is shown, saved or translated.
    pub fn apply(&self, text: &str) -> String {
        self.entries
            .iter()
            .fold(text.to_string(), |t, e| t.replace(&e.wrong, &e.right))
    }

    /// Correct spellings, handed to the translator as terms to keep.
    pub fn terms(&self) -> Vec<String> {
        let mut terms: Vec<String> = self.entries.iter().map(|e| e.right.clone()).collect();
        terms.dedup();
        terms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(pairs: &[(&str, &str)]) -> Glossary {
        Glossary {
            entries: pairs
                .iter()
                .map(|(w, r)| GlossaryEntry {
                    wrong: w.to_string(),
                    right: r.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn every_misrecognition_is_replaced() {
        let g = g(&[("きき耳ミック", "Kikimimic"), ("ジェミナイ", "Gemini")]);
        assert_eq!(g.apply("きき耳ミックとジェミナイ"), "KikimimicとGemini");
    }

    #[test]
    fn an_entry_needs_both_sides() {
        assert!(g(&[("", "x")]).validate().is_err());
        assert!(g(&[("x", " ")]).validate().is_err());
        assert!(g(&[("x", "y")]).validate().is_ok());
    }
}
