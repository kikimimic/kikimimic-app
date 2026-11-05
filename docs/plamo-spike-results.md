# PLaMo 3 Translate 2B と Qwen3 4B の比較（spike S3）

2026-11-05、M4 Pro、Metal。`crates/kikimimic-mt-llama/tests/real_model.rs` で同じ4文を訳した結果です。ASR と同時には動かしていません（同時に動かしたときの比較は spike S2 で行います）。

再現:

```sh
cargo build --release -p kikimimic-mt-llama-dylib
KKM_TRANSLATE_DYLIB="$PWD/target/release/libkikimimic_mt_llama_dylib.dylib" \
KKM_QWEN_GGUF=/path/to/Qwen3-4B-Instruct-2507-Q4_K_M.gguf \
KKM_PLAMO_GGUF=/path/to/plamo-3-translate-2b-Q4_K_M.gguf \
  cargo test --release -p kikimimic-mt-llama --test real_model -- --ignored --nocapture --test-threads=1
```

## 結果

| モデル | 形式 | 読み込み | 1文あたり |
|---|---|---|---|
| PLaMo 3 Translate 2B Q4_K_M（1.8GB） | PFML3 | 8.0 s | 209〜554 ms |
| Qwen3 4B Instruct 2507 Q4_K_M（2.5GB） | ChatML | 1.1〜1.3 s | 858〜2003 ms |

| 原文 | PLaMo | Qwen |
|---|---|---|
| Next week we plan to start working on performance improvements. | 来週からパフォーマンス改善に取り組む予定です。 | 来週、パフォーマンスの改善作業を開始する予定です。 |
| 新しい検索機能の実装は予定どおり完了しましたが、パフォーマンスにはまだ改善の余地があります。 | The new search functionality has been implemented as planned, but performance still has room for improvement. | The implementation of the new search function has been completed as planned, but there is still room for performance improvements. |

## わかったこと

- PLaMo は1文あたり Qwen の3〜4倍速い。v2 で問題だった「ASR と同時に動かすと翻訳が3〜4倍遅くなる」の余裕を、そのまま取り戻せる大きさです。
- 訳はどちらも正確で、PLaMo の方が自然な言い回しでした。
- PLaMo の GGUF はトークナイザに `plamo3` を指定しており、crates.io の `llama-cpp-2`（0.1.161 まで）が同梱する llama.cpp では読めません。`kikimimic-mt-llama-dylib` は `utilityai/llama-cpp-rs` の git リビジョン（llama.cpp 8642541）に固定しています。
- PFML3 のプロンプトは PFN の `chat_template.jinja` と一致させています（`prompt.rs` のゴールデンテスト）。

既定のモデルは Qwen のままです。PLaMo は PLaMo Community License（商用は登録制）なので、既定にはしません（ADR-120000）。
