# Kikimimic OSS 版（v3）計画

方針は次の2本の ADR で決めました。

- [ADR-120000](docs/ADR/20261105-120000-publish-as-oss-monorepo-in-kikimimic-org.md): kikimimic org のモノレポにし、完全ローカルに絞る
- [ADR-120001](docs/ADR/20261105-120001-core-as-conductor-and-inference-runtime.md): core を conductor と inference の2つの持ち主を持つライブラリにする
- [ADR-120002](docs/ADR/20261105-120002-replace-tauri-svelte-with-gpui.md): UI を Tauri + Svelte から GPUI（Zed 公式）に置き換える

設計の詳細は [docs/design/core.md](docs/design/core.md) にあります。v2 の計画と実測の記録は、git の履歴（`726884a` 時点の `plan.md`）と `docs/step0-*.md`、`docs/nemotron-spike-results.md` に残っています。

## 要件

- 【MUST】完全ローカルで動くこと。既定の動作ではネットワークを使わない（使うのはモデルのダウンロードだけ）
- 【MUST】単語単位のリアルタイム文字起こし。最初の部分結果まで2秒以内、確定まで1.5秒以内
- 【MUST】確定文単位のリアルタイム翻訳。認識は翻訳を待たない
- 【MUST】マイクとシステム音声のレーン別録音。同じ時間軸に載せる
- 【MUST】OSS として、clone してビルドすれば動くこと。core は GPU もモデルもない CI でテストできること
- 【SHOULD】録音後の話者分離
- 【SHOULD】Homebrew cask で配布する（`kikimimic/homebrew-tap` は作成済み）
- 作らないもの: 視聴ページ、デーモン、CLI 製品、クラウドの Soniox
- UI は GPUI（Zed のリリースタグを固定した git 依存）。Zed の GPL の crate（`ui` など）には依存せず、コードも持ち込まない

## 進め方

TDD（Red → Green → Refactor）で進めます。構造の変更と振る舞いの変更はコミットを分けます（Tidy First）。各ステップは、v2 のアプリが動く状態を保ったまま積み上げます。GPUI のアプリは `app/` に並べて育て、v2 の `src-tauri` と Svelte を削除するのは Step 6 です。

### Step 0: リポジトリの体裁

- [x] `LICENSE`（MIT）を置き、README を OSS 向けに英語で書き直す。日本語版は `README.ja.md`
- [x] crate の名前を `kkm-*` から `kikimimic-*` に変える（構造の変更だけのコミット）
- [x] CI: fmt、clippy、test。対象は core とテスト用ユーティリティで、macOS・Linux・Windows のマトリクスで回す
- [x] bundle identifier を `io.github.kikimimic.app` に変える（`bundle-mac.sh` の `Info.plist` で設定する。TCC の許可は取り直しになる）
- [x] CONTRIBUTING（エンジンや OS バックエンドの足し方、契約テストの使い方）

### Step 0.5: GPUI の土台（リスクの高いものを先に確かめる）

- [x] `app/` に GPUI のアプリを作る。Zed のタグ（`v1.27.0` から始める）を固定した git 依存にし、CI で fetch をキャッシュする
      ※ GPUI は Rust 1.98 が必要なので `rust-toolchain.toml` で 1.98.1 に固定し、flake は rustup を提供する形にした。Tauri と GPUI はロックファイルを共有できないので、Step 6 までは `app/` を独立したワークスペースにしている
- [x] `scripts/bundle-mac.sh`: `Info.plist`（`NSMicrophoneUsageDescription`、`NSAudioCaptureUsageDescription`）、`Contents/Frameworks` への cdylib の配置、ad-hoc 署名。`build-app.sh` を置き換える
- [ ] 組み立てた `.app` で、v2 の `capture` を使ってスピーカーレーンに音が入ることを確かめる（TCC）
- [ ] `widgets/text_input`（gpui の `examples/input.rs` が出発点）で、日本語 IME の変換・確定・取り消しが自然に動くことを確かめる
- [ ] 確定部分は通常の色、未確定部分はグレーで、同じ行に続けて描くこと。偽のイベントを連続で流して追従を確かめる
- [x] CI: GPL だけで提供されている crate が依存に入ったら落とす（`scripts/check-licenses.py`）

### Step 1: core の骨組みと、最初の Red

- [x] `domain.rs`（`Lang`、`Spoken`、`LaneId`、`UtteranceId`、時刻の newtype、`Pcm16k`）
- [x] `engine.rs`（`AsrEngine`、`MtEngine`、`Diarizer`、Factory、`EngineRegistry`）、`event.rs`
- [x] `sim.rs`（`Scenario`、`ScriptedAsr`、`EchoMt`、`Fixture`）
- [x] 1レーン・1文・訳・jsonl のシナリオテストを Red にする
- [x] `cargo metadata` で依存の禁止リストを調べるテスト

### Step 2: 純粋な部品を移す

- [x] `StreamScheduler`、`LocalAgreement`、`LanguagePolicy`、`TranslationQueue`、resample、mix を、テストごと core に移す
      ※ 元から core にあったものは場所を変えず、新しい `domain::LanguagePolicy`（`Spoken` 型）と `conductor::ledger` を足した。旧 API は Step 6 で消す
- [x] `asr::Windowed`（`WindowTranscriber` + `SpeechDetector` → `AsrEngine`）。レーンごとの状態をエンジンが持つ形にする

### Step 3: conductor

- [x] `pipeline.rs` から次のものをテストごと移す: `LaneRuntime`、`SessionClock`、`AnchorTrail`、`LaneTimeline`、`EmitGate`、`Notices`、`SilenceWatch`
- [x] `Ledger`: ID の採番、owed、最古を捨てるキュー、context の履歴。「終端の Translation がちょうど1回届く」ことをユニットテストと溢れのシナリオで確かめる（proptest は入れていない）
- [x] `record.rs`: wav の穴埋めと jsonl の形式。v2 の録音を `library` で読めること（互換テスト）
- [x] シナリオテスト: 遅れて始まるレーン、2秒の穴、最古を捨てる動作、Stop 時に訳を打ち切る動作、会議中の言語変更、録音ディレクトリの失敗（遅れて届く訳と MT の読み込み失敗は Ledger のユニットテストと runtime の実装で扱う）

### Step 4: inference と runtime

- [x] `Inference`: エンジンの置き場、cadence の管理、MT は1件ずつ
      ※ 独立した状態機械にはせず、cadence と「1件ずつ」は conductor（`Lane::stepping`、`Ledger::in_flight`）が持ち、エンジンの実行は runtime の ASR/MT スレッドと sim が担う形にした
- [x] `runtime.rs`: conductor のスレッド、ASR と MT のスレッド、ローダ、実時計
- [x] スレッドを使う smoke テスト（約2秒、順序に依存しない結果だけを確かめる）

### Step 5: エンジン crate とモデル管理

- [ ] `kikimimic-asr-whisper`（`WindowTranscriber` + Silero）
- [ ] `kikimimic-mt-llama`（`isolated` を既定）。`PromptFormat::ChatMl` とゴールデンテスト
- [ ] `kikimimic-capture`（macOS: cpal mic + Process Tap）を `capture::channel` に載せ替える
- [ ] `models/catalog.toml`、`ModelStore`（license、sha256、.partial、atomic な install）、`EngineRegistry::check`
- [ ] 各 crate で契約テストを通す（`contract::*_suite`）

### Step 6: アプリを薄くする

- [ ] `app/` に画面を揃える: セッション（1本の文字起こしと話者ピル、言語ピルとポップオーバー）、録音一覧、モデル、出力デバイス切り替えの確認ウィンドウ。見た目は `mockups/` に従う
- [ ] `AppState::apply`（Event の reducer）のテスト。通知の文言は UI 側で組み立てる
- [ ] v2 の `src-tauri`、Svelte、bun と Vite の設定、`scripts/build-app.sh` を削除する
- [ ] 設定の永続化（`settings.toml`）。glossary は全モードに適用する。v2 の辞書（localStorage）は引き継がず、README に「登録し直してください」と書く
- [ ] 実機での確認: 2レーンの会議、言語変更、Stop、録音一覧、過去の録音の読み戻し（`.app` で起動）
- [ ] 既定の出力デバイスの切り替えへの追従（v2 で実機確認が残っていた項目）

### Step 7: 話者分離

- [ ] `kikimimic-diarize-nemotron`（`Diarizer`）と、conductor の `PostProcessing`
- [ ] 当面は任意の追加機能にする。`uv` が見つからなければ、話者分離のボタンに導入方法を表示する
- [ ] spike S5 の結果を見て、1.0 までにプロセス内で動く実装に置き換えるかを決める

### Step 8: PLaMo 3 Translate を選べるようにする

- [x] `PromptFormat::Plamo3`（target_lang / source_lang / context / glossary）と、PFN のリファレンス（`chat_template.jinja`）に合わせたゴールデンテスト
- [ ] カタログへの追加と、ライセンス承諾の UI
- [x] Qwen3-4B との比較（[docs/plamo-spike-results.md](docs/plamo-spike-results.md)）。単体では PLaMo が3〜4倍速い。ASR と同時に動かしたときの比較は S2 で行う。既定は Qwen のまま

### Step 9: 配布

- [ ] `bundle-mac.sh` に Developer ID での署名と notarization を足す（Apple Developer Program が必要）
- [ ] リリースのワークフローと、`homebrew-tap` の cask を自動で更新する仕組み

### 後で

- Windows（WASAPI loopback + cpal mic、spike S4）、Linux（PipeWire monitor）
- ネイティブにストリーミングする ASR（v2 Step 6 の Nemotron Streaming / transcribe.cpp）。`AsrEngine` を直接実装する crate として足す
- 確定前の部分結果の再翻訳、リアルタイムの話者ラベル（発話 ID を最初の部分結果で振る形にしてあるので、入口はある）

## Spike（設計を決めるための計測）

| ID | 確かめること | 失敗したら |
|---|---|---|
| S1 | ggml・whisper.cpp・llama.cpp を1つの -sys でビルドし、1つのバイナリで同時に動かせるか（`~/.ai/kikimimic/architect/ggml-findings.md`） | `isolated`（cdylib + dlopen）のまま進める |
| S2 | 2レーンが続けて話す条件で、ASR と MT を並行に動かす方式と、直列にしてトークン単位で区切る方式の、確定の遅延と1文あたりの翻訳時間（Qwen3-4B と PLaMo 2B） → **暫定**: 並行のままで ASR はほぼ遅くならない。高負荷のマシンで測ったので、空いたマシンで測り直す（[docs/plamo-spike-results.md](docs/plamo-spike-results.md)） | 並行のまま進める（v2 と同じ） |
| S3 | PFML3 の出力が PFN のリファレンスと一致するか。PLaMo 2B の訳の質 → **済み**: 一致、訳は自然、Qwen の3〜4倍速い | PLaMo は選択肢に加えない |
| S4 | Windows で、WASAPI loopback と cpal のマイクの時刻を同じ時計に載せられるか | Windows ではマイクの取得も自前で書く |
| S5 | uv を使わない話者分離（sherpa-onnx など）が実用になるか | 話者分離を任意の追加機能として文書にする |
| S6 | Zed のタグを上げたときに、GPUI の追従にどれだけ手間がかかるか（Step 0.5 の後で1回、タグを上げてみる） | 上げる頻度を下げる（Zed のリリース数回に1回にする） |

## 未決事項

docs/design/core.md の「未決事項とリスク」を参照。主なものは次のとおりです。

- `MtRequest.context` に相手レーンの発話も入れるか
- llama のバックエンドを初期化したまま、GGUF だけを入れ替えられるか
- 話者分離の最終形（S5 の後で決める）
