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

# ASR と同時に動かしたとき（spike S2、暫定）

`crates/kikimimic-mt-llama/tests/concurrent.rs`。2レーン分の Whisper のステップを休まず回しながら翻訳し、`crates/kikimimic-asr-whisper/tests/decode_cost.rs` でデコード1回の時間も測りました。

**注意: 計測したマシンは高負荷でした**（ロードアベレージ 6〜12。GPU を使う別のアプリが動いていた）。Whisper のデコード1回が、ウィンドウ1秒でも 2.8〜3.5 秒（言語判定ありで 5.4〜6.3 秒）かかっており、v2 の Step 0 で測った 0.7〜0.8 秒の約4倍です。絶対値は使わず、同じ条件の中での比較だけを読んでください。

| | 単体 | 2レーンの ASR と同時 |
|---|---|---|
| PLaMo の翻訳（1文） | 平均 557 ms | 平均 2666 ms（最大 4302） |
| Qwen の翻訳（1文） | 平均 1579 ms | 平均 5648 ms（最大 10114） |
| ASR の1ステップ（PLaMo の回） | 平均 3059 ms | 平均 3963 ms |
| ASR の1ステップ（Qwen の回） | 平均 3784 ms | 平均 4194 ms |

読み取れること:

- 翻訳は ASR と同時だと4〜5倍遅くなる（v2 の Step 0 と同じ傾向）。それでも PLaMo は Qwen の約半分で済む。
- ASR のステップは、翻訳を同時に動かしても揺れの範囲でしか遅くならない。
- したがって「ASR を優先して翻訳を区切る（直列化）」に切り替える根拠は今のところない。runtime は並行のまま（ADR-120001 の既定）にする。

やり残し: 負荷のないマシンでの測り直し。デコード1回が 0.7 秒前後に戻ることの確認も兼ねる。

追記（同日）: CPU のロードアベレージが 4〜5 まで下がっても、デコード1回は 2.5〜3.2 秒のままでした。このとき Kikimimic の処理を何も動かしていない状態で、GPU の使用率が 72% ありました（`ioreg -r -d 1 -w 0 -c IOAccelerator` の `Device Utilization %`）。遅さの原因は CPU ではなく、他のアプリによる GPU の使用です。測り直すときは、ロードアベレージではなく GPU の使用率が下がっていることを確かめてください。
