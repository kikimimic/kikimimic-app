# Kikimimic を kikimimic org のモノレポとして OSS 公開し、完全ローカルに絞る

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-11-05 |
| **Decision-makers** | staticWagomU |
| **Consulted** | Claude Code |
| **Informed** | - |

## Context and Problem Statement

Kikimimic は、これまで個人リポジトリ（`staticWagomU/live-poly-trans`）で開発してきました。これを OSS として公開し、外部からの貢献を受けられるようにします。

方向性は「talky-jerky（Recall.ai + Soniox + Cloudflare の会議 bot）を完全にローカルで動かす版」です。ただし、視聴ページ（参加者のブラウザに字幕を配る機能）は作りません。

公開にあたって、次の3点を決めておく必要があります。

- どのリポジトリに分けるか
- 何をスコープに入れるか
- モデルのライセンスをどう扱うか

## Decision Drivers

* 会議の内容を外に出さない。既定の動作でネットワークを使わない
* OSS として、`git clone` してビルドすれば動き、貢献の範囲が分かりやすいこと
* ライセンスを汚さない。アプリは MIT で、モデルは同梱しない
* macOS（Apple Silicon）を先に作り、Windows と Linux へは作り直しなしで進められること
* Homebrew cask で配布する

## Considered Options

1. org の下に、モノレポ1つと付属のリポジトリだけを置く
2. 責務ごとにリポジトリを分ける（core / engines / app / protocol / viewer）
3. 個人リポジトリのまま公開する

## Decision Outcome

**Chosen option**: 1。

**リポジトリ**

| リポジトリ | 役割 |
|---|---|
| `kikimimic/kikimimic-app` | 本体のモノレポ。旧 `staticWagomU/live-poly-trans` を transfer したもので、履歴と ADR を引き継ぐ |
| `kikimimic/homebrew-tap` | cask の置き場。`brew tap` の規約で、名前が `homebrew-` で始まる必要がある |
| `kikimimic/.github` | org 共通のプロフィールと CONTRIBUTING。必要になったら作る |

**モノレポの中身**: Rust の workspace です。

- `crates/kikimimic-core`: UI、デバイス、GPU、ネットワークを持たない
- `crates/kikimimic-asr-*`、`crates/kikimimic-mt-*`、`crates/kikimimic-capture`、`crates/kikimimic-diarize-*`
- `app/`: GPUI のアプリで、core をライブラリとして直接使う（[ADR-120002](20261105-120002-replace-tauri-svelte-with-gpui.md)）
- `models/catalog.toml`、`docs/ADR/`

作らないもの: デーモン、CLI 製品、プロトコル用の crate、ブラウザの視聴ページ。

**スコープ**: 完全ローカルにします。v2 にあったクラウドの Soniox による対面モードは OSS 版には入れず、対面モードはローカルの Whisper で実現します（マイクのみ・録音なし）。クラウドのエンジンは、将来も core に手を入れずに別の crate として足せる形にとどめます。

**モデル**: アプリには同梱せず、ユーザーがアプリの中で選んでダウンロードします。既定のモデルは、ライセンスが寛容なものにします。

| 用途 | 既定のモデル | ライセンス |
|---|---|---|
| ASR | Whisper large-v3-turbo q8_0 | MIT |
| VAD | Silero | MIT |
| 翻訳 | Qwen3-4B-Instruct-2507 | Apache-2.0 |

PLaMo 3 Translate（PLaMo Community License。商用は登録制）は、選べるモデルの1つにします。ダウンロード時にライセンスを表示して承諾を取り、承諾したことを記録します。

### Consequences

**Positive:**
* プロトコル、core、UI が1つの PR で同時に変わるので、バージョン合わせが要らない
* モデルを同梱しないので、アプリの配布物は MIT のまま、ライセンス表示の範囲がはっきりする
* ネットワークを使うコードがアプリ側のモデルダウンロードだけになる

**Negative:**
* 外部の人がエンジンだけを独立に配布したくなったときは、別リポジトリを検討し直す必要がある
* Soniox の対面モードを使っていた場合は、ローカルの Whisper へ移ることになる。認識の質と遅延は変わる

**Neutral:**
* ドキュメントサイトと Hugging Face org は、必要になるまで作らない

### Confirmation

* `kikimimic-app` を clone して `cargo build` すれば、app 以外の全 crate がビルドできる
* core の CI に、ネットワークに依存するテストがない。`cargo metadata` の禁止リストのテストで確認する
* catalog の全エントリに `license` があり、承諾が必要なものはダウンロード前に承諾を求める

## Pros and Cons of the Options

### モノレポ + 付属リポジトリ

* Good: 頻繁に変わるイベント契約を1つの PR で直せる
* Good: 貢献の範囲は crate の境界で区切れる
* Bad: リポジトリ単位で権限を細かく分けられない（今は必要ない）

### 責務ごとにリポジトリを分ける

* Good: 部品ごとにリリースできる
* Bad: 1つの変更が複数の PR に分かれ、バージョン合わせも要る。分ける理由（リリースの時期が違う、使う人が違う、権限を分けたい）が今はどれもない

### 個人リポジトリのまま

* Good: 移行の手間がない
* Bad: 共同メンテナを迎えにくく、homebrew-tap と名前空間が揃わない

## More Information

コアの形は [ADR-120001](20261105-120001-core-as-conductor-and-inference-runtime.md) と [docs/design/core.md](../design/core.md) を参照。進め方は [plan.md](../../plan.md)。
