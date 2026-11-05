//! The only tests that cross the FFI boundary: the real backend, real
//! models, real sentences. Gigabytes of RAM, so opt-in:
//!
//! ```sh
//! cargo build --release -p kikimimic-mt-llama-dylib
//! KKM_TRANSLATE_DYLIB="$PWD/target/release/libkikimimic_mt_llama_dylib.dylib" \
//! KKM_QWEN_GGUF=/path/to/Qwen3-4B-Instruct-2507-Q4_K_M.gguf \
//! KKM_PLAMO_GGUF=/path/to/plamo-3-translate-2b-Q4_K_M.gguf \
//!   cargo test --release -p kikimimic-mt-llama --test real_model -- --ignored --nocapture --test-threads=1
//! ```

use std::path::PathBuf;
use std::time::Instant;

use kikimimic_core::domain::Lang;
use kikimimic_core::engine::{MtEngine, MtRequest};
use kikimimic_mt_llama::{LlamaMt, PromptFormat};

const SENTENCES: &[(&str, &str, &str)] = &[
    ("en", "ja", "The implementation of the new search feature was completed on schedule."),
    ("en", "ja", "Next week we plan to start working on performance improvements."),
    ("ja", "en", "先週の進捗を共有します。"),
    ("ja", "en", "新しい検索機能の実装は予定どおり完了しましたが、パフォーマンスにはまだ改善の余地があります。"),
];

fn run(env: &str, format: PromptFormat) {
    let path = PathBuf::from(std::env::var(env).unwrap_or_else(|_| panic!("set {env}")));
    let started = Instant::now();
    let mut mt = LlamaMt::load(&path, format).expect("load");
    println!("{env}: loaded in {:?}", started.elapsed());
    let context = Vec::new();
    let terms = vec!["Kikimimic".to_string()];
    for (source, target, text) in SENTENCES {
        let (source, target) = (Lang::parse(source).unwrap(), Lang::parse(target).unwrap());
        let started = Instant::now();
        let out = mt
            .translate(&MtRequest {
                text,
                source: Some(&source),
                target: &target,
                context: &context,
                terms: &terms,
            })
            .expect("translate");
        println!(
            "{env} {:>5} ms  {text}\n            → {out}",
            started.elapsed().as_millis()
        );
        assert!(!out.trim().is_empty());
        let has_japanese = out.chars().any(|c| {
            ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{4e00}'..='\u{9fff}').contains(&c)
        });
        assert_eq!(has_japanese, target.as_str() == "ja", "{out}");
    }
}

#[test]
#[ignore = "loads a multi-gigabyte model"]
fn qwen_translates_with_chatml() {
    run("KKM_QWEN_GGUF", PromptFormat::ChatMl);
}

#[test]
#[ignore = "loads a multi-gigabyte model"]
fn plamo_translates_with_pfml3() {
    run("KKM_PLAMO_GGUF", PromptFormat::Plamo3);
}
