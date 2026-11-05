//! The engines this app ships, by the names `models/catalog.toml` uses.

use kikimimic_core::catalog::ModelEntry;
use kikimimic_core::engine::EngineRegistry;

pub fn registry() -> EngineRegistry {
    let mut r = EngineRegistry::default();
    r.asr(
        "whisper",
        kikimimic_asr_whisper::check_options,
        kikimimic_asr_whisper::factory,
    );
    r.asr(
        "apple-speech",
        kikimimic_asr_apple::check_options,
        kikimimic_asr_apple::factory,
    );
    r.mt(
        "llama",
        kikimimic_mt_llama::check_options,
        kikimimic_mt_llama::factory,
    );
    r
}

/// Whether `m` can run on this Mac; the others are not offered.
pub fn runs_here(m: &ModelEntry) -> bool {
    m.engine != "apple-speech" || kikimimic_asr_apple::is_available()
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_catalog_model_has_an_engine_that_accepts_its_options() {
        super::registry()
            .check(kikimimic_core::catalog::Catalog::builtin())
            .unwrap();
    }
}
