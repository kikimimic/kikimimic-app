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

/// A word to listen for: how to write it, and what it may sound like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyword {
    pub written: String,
    pub readings: Vec<String>,
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

    /// Each right spelling with what the recognizer wrote instead, for a
    /// recognizer that can listen for words (Haas's custom vocabulary).
    pub fn keywords(&self) -> Vec<Keyword> {
        let mut out: Vec<Keyword> = Vec::new();
        for e in &self.entries {
            let at = match out.iter().position(|k| k.written == e.right) {
                Some(i) => i,
                None => {
                    out.push(Keyword {
                        written: e.right.clone(),
                        readings: Vec::new(),
                    });
                    out.len() - 1
                }
            };
            let k = &mut out[at];
            if e.wrong != k.written && !k.readings.contains(&e.wrong) {
                k.readings.push(e.wrong.clone());
            }
        }
        out
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
    fn keywords_list_each_right_spelling_with_what_was_heard_instead() {
        let g = g(&[
            ("ジェミナイ", "Gemini"),
            ("きき耳ミック", "Kikimimic"),
            ("ジェミニ", "Gemini"),
            ("Kikimimic", "Kikimimic"),
        ]);
        let kw = |written: &str, readings: &[&str]| Keyword {
            written: written.into(),
            readings: readings.iter().map(|r| r.to_string()).collect(),
        };
        assert_eq!(
            g.keywords(),
            [
                kw("Gemini", &["ジェミナイ", "ジェミニ"]),
                kw("Kikimimic", &["きき耳ミック"]),
            ]
        );
    }

    #[test]
    fn an_entry_needs_both_sides() {
        assert!(g(&[("", "x")]).validate().is_err());
        assert!(g(&[("x", " ")]).validate().is_err());
        assert!(g(&[("x", "y")]).validate().is_ok());
    }
}
