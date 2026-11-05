# kikimimic-core を、状態の持ち主（conductor）とモデルの持ち主（inference）を内部に持つライブラリとして作り直す

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-11-05 |
| **Decision-makers** | staticWagomU |
| **Consulted** | Claude Code（4モデルの arena と cross-judge） |
| **Informed** | - |

## Context and Problem Statement

ADR-153800 では「コアを headless で、trait で差し替えられるようにする」と決めていました。しかし実際には、セッションの状態遷移、時刻合わせ、ID の採番、翻訳の流れ、録音の方針が `src-tauri/src/pipeline.rs`（2038行）に入っています。`kkm-core` にあるのは純粋なアルゴリズムの部品だけです。

この状態では、次のことができません。

- CI でパイプラインを検証すること
- OSS のコントリビューターがエンジンや OS 対応を足すときに、コアを読まずに済ませること

## Decision Drivers

* マイク、GPU、モデルがなくても、パイプライン全体を決定的にテストできること
* アプリから見える API を小さくし、windowing、VAD、ID、キュー、録音を隠すこと
* 状態ごとに書き手を1つにすること
* エンジン、プロンプト形式、OS バックエンドを、core に手を入れずに追加できること
* v2 で実測した制約を守ること（docs/design/core.md「背景」の11項目）
* ggml を1つにまとめる spike の結果が、core と app に波及しないこと

## Considered Options

1. 内部に2つの持ち主を持つライブラリ。conductor がドメイン状態、inference がモデルと GPU を持ち、どちらも sans-IO の状態機械
2. conductor が ASR をその場で実行し、MT は別スレッドの Dispatcher。GPU の優先は Turnstile で制御
3. ASR・MT・recorder の3つの host スレッド。セッションは ASR host 上のクロージャ
4. sans-IO の core とし、スレッドとエフェクトの実行はアプリが担う
5. `pipeline.rs` をそのまま core へ移す

## Decision Outcome

**Chosen option**: 1。4モデルで設計案を比べた結果、採点役と統合役の評価が一致しました。

**公開するもの**: 次のものだけを公開し、それ以外は `pub(crate)` にします。

- ハンドル `Kikimimic`（`start`、`snapshot`、`start_session`、`stop_session`、`update_settings`、`answer_prompt`、`diarize`、`models`、`library`）
- イベントのストリーム `Events`
- `ModelStore`、`Library`
- エンジンの trait（`AsrEngine`、`MtEngine`、`Diarizer`）と、window 型エンジン向けの部品（`WindowTranscriber`、`SpeechDetector`）
- キャプチャの境界（`CaptureBackend`、`capture::channel`）

**状態の持ち主**

| 持ち主 | 持つもの |
|---|---|
| conductor | セッションの状態、レーン、時計、タイムライン、発話 ID、翻訳キュー、録音、設定、通知、Snapshot |
| inference | モデルのすべて。GPU に触るのはここだけ |

どちらも `handle(now, input) -> outputs` として書きます。CI はこれを1スレッドと仮想時計で動かします。

**UI との契約**: イベントは型付きの `Event` 1本にまとめます。UI は GPUI（ADR-120002）なので、Rust の型のまま受け取ります。UI 向けのイベントは互換を切りますが、ディスク上の形式（wav と `transcript.jsonl`）は v2 と同じに保ちます。

**ggml**: `kikimimic-mt-llama` の feature（`isolated` か `linked` か）を切り替えるだけにします。最初は `isolated`（今の cdylib + dlopen）を既定にします。

**GPU**: 最初の実装は、今と同じ「ASR と MT を別スレッドで並行に実行する」構成にします。ASR を優先して直列にする方式は spike S2 で測ってから決め、変更する場合も `inference.rs` の中だけで済ませます。

詳しい型、データの流れ、テストは [docs/design/core.md](../design/core.md) に書きました。

### Consequences

**Positive:**
* `Rc<RefCell<WhisperModels>>`、`PolicyStore` / `StatusStore` / `OutputDevicePromptStore` の Mutex、`first_id` フィルタが構造上いらなくなる
* 翻訳中の発話には、終端の `Translation` がちょうど1回届く。これは proptest で確かめる
* OS バックエンドは、サンプルとタイムスタンプを書き込むだけになる。時刻の解釈は core で1回だけテストすればよい
* プロンプト形式（ChatML / PFML3）は mt-llama の純関数として閉じる。PLaMo の追加はカタログに1件足すだけで済む

**Negative:**
* 音声のチャンクごとに、conductor から inference へメッセージを1つ送ることになる
* UI のイベント処理を書き直す必要がある（同じモノレポの中で、同じ PR で移行する）
* エンジンの `options` は core から見ると型のない toml になる。不整合は `EngineRegistry::check` のテストで検出する

**Neutral:**
* ADR-153800 の「単一の Rust プロセス」と「trait で抽象化する」はそのまま引き継ぐ。core の中の形と責務の置き場は、この ADR で置き換える
* ADR-153805（cdylib による隔離）は、`isolated` feature としてそのまま残る。spike S1 が成功したら `linked` を既定にし、ADR-153805 を superseded にする

### Confirmation

* `crates/kikimimic-core/tests/` のシナリオテストが、偽エンジンだけを使い、macOS・Linux・Windows の CI で通ること
* `app/` にドメインのロジックがないこと。`app/` にあるのは、イベントを畳み込む reducer、描画、エンジンの配線、ダウンロードだけ
* 新しい ASR エンジンを追加する PR で、`kikimimic-core` に差分が出ないこと

## Pros and Cons of the Options

### 1. conductor + inference（採用）

* Good: パイプライン全体を1スレッド・仮想時計で決定的に再現できる（GPU の混雑や10秒の drain も含む）
* Good: 状態の持ち主が2つで、どちらも書き手が1つ
* Bad: 部品が多く、v2 から最も大きく形が変わる

### 2. conductor で ASR をその場で実行 + Dispatcher + Turnstile

* Good: v2 の形に最も近く、移行のリスクが小さい
* Bad: GPU の優先がエンジンの協力（Turnstile の呼び出し）に依存し、所有が core とエンジンに分かれる
* Bad: キャプチャのストリームが rtrb と `Arc<Mutex>` を公開してしまう

### 3. 3つの host スレッド

* Good: エンジン向けの契約テストと、依存の禁止リストのテストが優れている（採用案に取り込んだ）
* Bad: 本物のスレッドを `settle()` で同期するので、テストが完全には決定的にならない。部品も多い（recorder スレッド、fetch crate、3種類のハンドル）

### 4. sans-IO の core、ランタイムはアプリ側

* Good: core が最も小さく、同期的に書ける
* Bad: スケジューリング、GPU の扱い、drain という今テストされていない部分を、呼び出し側がすべて書き直すことになる

### 5. `pipeline.rs` を移すだけ

* Good: 差分が最小
* Bad: エンジンを足すたびに core を触ることになり、仮想時間でも動かせない

## More Information

arena の候補、採点、grounding は `~/.ai/kikimimic/architect/` にあります（リポジトリの外）。統合の判断は docs/design/core.md の「統合の判断」を参照。
