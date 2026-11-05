//! The user's settings, persisted as `settings.toml` in the data directory.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, ModelKind};
use crate::domain::LanguagePolicy;
use crate::glossary::Glossary;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub languages: LanguagePolicy,
    #[serde(default)]
    pub glossary: Glossary,
    pub asr_model: String,
    /// `None`: no translation model chosen.
    pub mt_model: Option<String>,
}

impl Settings {
    pub fn defaults(catalog: &Catalog) -> Self {
        Self {
            languages: LanguagePolicy::default(),
            glossary: Glossary::default(),
            asr_model: catalog.defaults.asr.clone(),
            mt_model: catalog.defaults.mt.clone(),
        }
    }

    /// The saved settings, or the defaults when there are none or they no
    /// longer make sense (a model removed from the catalog).
    pub fn load(path: &Path, catalog: &Catalog) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<Self>(&text).ok())
            .filter(|s| s.validate(catalog).is_ok())
            .unwrap_or_else(|| Self::defaults(catalog))
    }

    pub fn validate(&self, catalog: &Catalog) -> Result<(), String> {
        self.glossary.validate()?;
        let kind = |id: &str| catalog.get(id).map(|m| m.kind);
        if kind(&self.asr_model) != Some(ModelKind::Asr) {
            return Err(format!(
                "{} is not a speech recognition model",
                self.asr_model
            ));
        }
        if let Some(mt) = &self.mt_model {
            if kind(mt) != Some(ModelKind::Mt) {
                return Err(format!("{mt} is not a translation model"));
            }
        }
        Ok(())
    }

    /// Write atomically: a crash mid-save keeps the previous file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Lang, Spoken};
    use crate::glossary::GlossaryEntry;

    fn path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kikimimic-settings-{label}-{}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn settings_survive_a_restart() {
        let c = Catalog::builtin();
        let mut s = Settings::defaults(c);
        s.languages.spoken = Spoken::One(Lang::parse("en").unwrap());
        s.languages.target = None;
        s.glossary.entries.push(GlossaryEntry {
            wrong: "きき耳".into(),
            right: "Kikimimic".into(),
        });
        s.mt_model = Some("plamo-3-translate-2b-q4_k_m".into());
        let p = path("roundtrip");
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p, c), s);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn missing_or_stale_settings_fall_back_to_the_defaults() {
        let c = Catalog::builtin();
        assert_eq!(Settings::load(&path("absent"), c), Settings::defaults(c));
        let p = path("stale");
        let mut s = Settings::defaults(c);
        s.asr_model = "removed-model".into();
        std::fs::write(&p, toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(Settings::load(&p, c), Settings::defaults(c));
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn a_translation_model_cannot_be_chosen_for_recognition() {
        let c = Catalog::builtin();
        let mut s = Settings::defaults(c);
        s.asr_model = "qwen3-4b-instruct-2507-q4_k_m".into();
        assert!(s.validate(c).is_err());
    }
}
