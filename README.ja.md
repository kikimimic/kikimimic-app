# Kikimimic

[English](README.md)

完全ローカルで動く、リアルタイム多言語文字起こし＋翻訳のデスクトップアプリです（MIT）。自分のマイクと、会議アプリが再生する音声の2つのレーンを聞き、それぞれをローカルの Whisper で文字起こしし、確定した文をローカルの LLM で翻訳して、両方のレーンを録音します。外に出るのは、最初のモデルのダウンロードだけです。例外は、設定で Google にサインインしたときだけ行うカレンダーの読み取りで、会議の前に録音を勧めるカードを出すために使います。音声と文字起こしは外に出ません。

名前は「聞き耳（kikimimi）」と「mimic」の重ね合わせで、共有している `mimi`（耳）が2つのレーンにあたります。

> **v3（初期段階）です。** macOS（Apple Silicon）で動きます。Windows と Linux は予定しており、core はすでに3つの OS でビルドとテストが通っています。進め方は [plan.md](plan.md) を参照してください。

## できること

- ローカルの Whisper による単語単位のリアルタイム文字起こし（whisper.cpp、Metal）
- ローカルの LLM による確定文単位のリアルタイム翻訳（llama.cpp、GGUF）
- マイクとシステム音声を別々のレーンで、同じ時間軸に録音
- 辞書（glossary）: 誤認識を正しい表記に直し、翻訳でも固有名詞を保つ。Haas では認識時にもその語を聞き取りやすくする
- 対面モード: マイクだけを使い、録音はしない

## モデル

モデルは同梱しません。アプリの **Models** タブからダウンロードしてください。ダウンロードの前にライセンスを表示します。

| モデル | 用途 | ライセンス |
|---|---|---|
| Whisper large-v3-turbo（q8_0）＋ Silero VAD | 文字起こし（既定） | MIT |
| Haas ja v1.0（Q8_0、Q4_K）＋ Silero VAD | 文字起こし（任意、日本語のみ。speech.cpp で動き、`scripts/bundle-mac.sh` が `libspeech` として同梱） | Apache-2.0 |
| macOS 標準の音声認識（SpeechAnalyzer） | 文字起こし（macOS 26 以降。ダウンロード不要。話される言語の1つ目だけを聞く） | macOS の一部 |
| Qwen3 4B Instruct 2507（Q4_K_M） | 翻訳（既定） | Apache-2.0 |
| PLaMo 3 Translate 2B（Q4_K_M） | 翻訳（任意） | PLaMo Community License。個人・非商用は無料、商用は PFN への登録が必要 |

一覧は [models/catalog.toml](models/catalog.toml) にあります。

## ビルド（macOS）

Rust のバージョンは `rust-toolchain.toml` で固定しています（rustup が読みます）。それ以外は `nix develop` でそろいます（`just` も含む）。Nix を使わない場合は、Xcode のコマンドラインツール（SpeechAnalyzer のため macOS 26 以降の SDK）、CMake、just が必要です。

翻訳バックエンドは別の cdylib としてビルドします。llama.cpp の ggml と whisper.cpp の ggml を同じバイナリに入れられないためです（[ADR-153805](docs/ADR/20260819-143000-isolate-llama-cpp-in-a-cdylib.md)）。

```sh
just dev     # 開発（翻訳バックエンドをビルドしてから cargo run）
just run     # Kikimimic.app
just         # recipe の一覧
```

システム音声のレーンは、署名済みの `.app` から起動したときだけ動きます。それ以外の方法で起動すると、macOS は黙って無音を返します。

開発用の環境変数:

| 変数 | 効果 |
|---|---|
| `KKM_MODELS_DIR` | モデルファイルをここでも探す（既存の `models/` など）。`:` 区切りで複数指定できる |
| `KKM_RECORD_DIR` | 録音の保存先（既定は `~/Music/kikimimic`） |
| `KKM_TRANSLATE_DYLIB` | このビルドの翻訳バックエンドを使う |
| `KKM_LLM_GPU_LAYERS` | 翻訳モデルを GPU に載せる層数（`0` で CPU） |
| `KKM_LANG_PIN_THRESHOLD`、`KKM_VAD_*` | Whisper の言語判定と VAD の調整 |
| `KKM_UI_LANG` | `en` か `ja`。指定しなければ OS の言語に合わせる |
| `KKM_CALENDAR_DEMO` | `1` で、1分後に始まる架空の会議を足す。サインインしなくても会議のカードを確かめられる |
| `KKM_GOOGLE_CLIENT_ID`、`KKM_GOOGLE_CLIENT_SECRET` | **ビルド時**に読む、カレンダーのサインイン用の Google OAuth クライアント（種類は Desktop app）。無くてもビルドは通り、設定に「このビルドでは使えません」と出る |

## 録音

セッションごとに `~/Music/kikimimic/<yyyyMMddHHmmss>/` が作られます。

- `mic.wav`、`speaker.wav`、`mix.wav` — 48kHz / モノラル / 16bit。3つとも同じ長さ・同じ時間軸
- `transcript.jsonl` — 確定した発話と、その訳文（1行1件）

```json
{"type":"utterance","id":1,"lane":"mic","startMs":1200,"endMs":3400,"lang":"ja","text":"こんにちは"}
{"type":"translation","id":1,"lang":"en","text":"Hello."}
```

訳文は発話の数秒後に、レーンをまたいで前後して届きます。位置ではなく `id` で発話に結びつけてください。`startMs`/`endMs` は録音上の位置です。

v2 の対面モードで登録した辞書は引き継がれません。Settings タブで登録し直してください。

## ドキュメント

- core の設計: [docs/design/core.md](docs/design/core.md)
- 技術判断: [docs/ADR/](docs/ADR/)
- 貢献のしかた: [CONTRIBUTING.md](CONTRIBUTING.md)

## ライセンス

MIT（[LICENSE](LICENSE)、[NOTICE](NOTICE)）。モデルのライセンスは上の表のとおりで、ダウンロード前にも表示します。
