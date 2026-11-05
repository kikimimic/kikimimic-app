//! `models/catalog.toml`: every downloadable model, embedded at build time.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Asr,
    Vad,
    Mt,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct License {
    pub name: String,
    pub url: Option<String>,
    /// The user must accept it before the model is downloaded.
    #[serde(default)]
    pub requires_acceptance: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FileSpec {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub kind: ModelKind,
    /// The engine that loads it, as registered in the app's EngineRegistry.
    pub engine: String,
    pub title: String,
    pub license: License,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Empty for a model the OS provides: nothing to download, always ready.
    pub files: Vec<FileSpec>,
    /// Opaque to the core; the engine parses it (e.g. `prompt = "plamo3"`).
    #[serde(default)]
    pub options: toml::Table,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Defaults {
    pub asr: String,
    pub mt: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub defaults: Defaults,
    #[serde(rename = "model")]
    models: Vec<ModelEntry>,
}

impl Catalog {
    /// The catalog shipped with this build.
    pub fn builtin() -> &'static Catalog {
        static CATALOG: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
        CATALOG.get_or_init(|| {
            Self::parse(include_str!("../../../models/catalog.toml"))
                .expect("models/catalog.toml is valid (checked by its test)")
        })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let catalog: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn models(&self) -> &[ModelEntry] {
        &self.models
    }

    /// The entry and everything it requires, requirements first.
    pub fn with_requirements(&self, id: &str) -> Result<Vec<&ModelEntry>, String> {
        let entry = self.get(id).ok_or_else(|| format!("unknown model: {id}"))?;
        let mut out = Vec::new();
        for req in &entry.requires {
            out.extend(self.with_requirements(req)?);
        }
        out.push(entry);
        Ok(out)
    }

    fn validate(&self) -> Result<(), String> {
        let mut seen = BTreeMap::new();
        for m in &self.models {
            if seen.insert(m.id.as_str(), ()).is_some() {
                return Err(format!("duplicate model id: {}", m.id));
            }
            for f in &m.files {
                let hex = f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit());
                if !hex {
                    return Err(format!("{}: {} needs a sha256", m.id, f.name));
                }
                if f.name.contains('/') || f.name.contains("..") {
                    return Err(format!(
                        "{}: file name {} must be a plain name",
                        m.id, f.name
                    ));
                }
            }
        }
        for m in &self.models {
            for r in &m.requires {
                if !seen.contains_key(r.as_str()) {
                    return Err(format!("{} requires unknown model {r}", m.id));
                }
            }
        }
        let kind_of = |id: &str| self.get(id).map(|m| m.kind);
        if kind_of(&self.defaults.asr) != Some(ModelKind::Asr) {
            return Err(format!(
                "default asr {} is not an asr model",
                self.defaults.asr
            ));
        }
        if let Some(mt) = &self.defaults.mt {
            if kind_of(mt) != Some(ModelKind::Mt) {
                return Err(format!("default mt {mt} is not an mt model"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalog_is_valid() {
        let c = Catalog::builtin();
        assert!(c.get(&c.defaults.asr).is_some());
    }

    #[test]
    fn the_default_models_are_permissively_licensed() {
        let c = Catalog::builtin();
        for id in [Some(&c.defaults.asr), c.defaults.mt.as_ref()]
            .into_iter()
            .flatten()
        {
            for m in c.with_requirements(id).unwrap() {
                assert!(!m.license.requires_acceptance, "{} is a default", m.id);
            }
        }
    }

    #[test]
    fn plamo_asks_for_its_license_first() {
        let plamo = Catalog::builtin()
            .get("plamo-3-translate-2b-q4_k_m")
            .unwrap();
        assert!(plamo.license.requires_acceptance);
    }

    #[test]
    fn requirements_come_before_the_model_that_needs_them() {
        let ids: Vec<_> = Catalog::builtin()
            .with_requirements("whisper-large-v3-turbo-q8_0")
            .unwrap()
            .into_iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["silero-vad-v5.1.2", "whisper-large-v3-turbo-q8_0"]);
    }

    #[test]
    fn a_file_name_cannot_escape_the_models_directory() {
        let text = r#"
            [defaults]
            asr = "a"
            [[model]]
            id = "a"
            kind = "asr"
            engine = "x"
            title = "a"
            license = { name = "MIT" }
            files = [{ name = "../evil", url = "u", sha256 = "0000000000000000000000000000000000000000000000000000000000000000", bytes = 1 }]
        "#;
        assert!(Catalog::parse(text).unwrap_err().contains("plain name"));
    }
}
