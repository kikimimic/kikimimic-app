# UI シェルを Tauri + Svelte から GPUI（Zed 公式）に置き換える

| | |
|---|---|
| **Status** | accepted |
| **Date** | 2026-11-05 |
| **Decision-makers** | staticWagomU |
| **Consulted** | Claude Code |
| **Informed** | - |

Supersedes [ADR-153802](20260810-153802-keep-tauri-svelte-shell.md)。

## Context and Problem Statement

ADR-120001 で、core を「ハンドル1つ + `Event` ストリーム1本」を公開するライブラリにすると決めました。これで UI はイベントを受け取って描画するだけの薄い層になり、UI のフレームワークを `app/` の中だけで選べるようになりました。

一方で、UI はどのみち書き直します。イベントを1本にまとめて通知を型付きにするので、v2 の `+page.svelte`（2039行）は作り直しになるためです。

この機会に、Rust だけで書ける UI に移るかどうかを決めます。

## Decision Drivers

* Rust だけで完結させ、コントリビューターに求める前提知識を減らしたい
* core と UI の間の IPC、serde によるイベントの書き出し、ts-rs による型生成を無くしたい
* 日本語の表示と IME の品質（ADR-153802 で最も重視した点）
* OSS としてのライセンスの整合（アプリは MIT）
* 依存が予測できること（clone してビルドすれば動く）

## Considered Options

1. GPUI（Zed 公式、`zed-industries/zed` の `crates/gpui`）
2. gpui-kit（Longbridge。GPUI を再公開したものに部品群を載せたもの）
3. Tauri 2 + Svelte 5 を続ける

## Decision Outcome

**Chosen option**: 1。部品は自前で書きます。

**依存のしかた**

- crates.io の公式 `gpui` は 0.2.2（2025-10）から更新されていません。そのため、Zed のリリースタグを固定した git 依存にします（例: `git = "https://github.com/zed-industries/zed", tag = "v1.27.0"`）。
- タグを上げるのは意図して行う作業とし、1つの PR で1つのタグだけを上げます。

**ライセンス**

| crate | ライセンス | 扱い |
|---|---|---|
| `gpui`、`gpui_platform` | Apache-2.0 | 使う |
| Zed の `ui` crate など | GPL-3.0-or-later | 依存にもコピー元にもしない |

部品（テキスト入力、ポップオーバー、仮想リスト）は、gpui の `examples/`（Apache-2.0。`input.rs`、`popover.rs`、`uniform_list.rs`、`list_example.rs`、`a11y.rs`）を出発点にして自前で書きます。

**Tauri が担っていた仕事の移し先**

| 仕事 | 移し先 |
|---|---|
| `.app` の組み立て。`Info.plist`（`NSMicrophoneUsageDescription`、`NSAudioCaptureUsageDescription`）を入れ、cdylib を `Contents/Frameworks` に置く | `scripts/bundle-mac.sh` |
| 署名と notarization | 同じスクリプト（配布の段階で） |
| 複数ウィンドウ（出力デバイス切り替えの確認） | gpui の `open_window` |
| アプリデータのパス | `dirs` crate |

### Consequences

**Positive:**
* UI も Rust になり、core の `Event` を型のまま受け取れる。core の `Event` から serde と ts-rs を外せる（serde が残るのは jsonl の行の型だけ）
* UI の状態の更新（ID による訳文の突き合わせ、行の上限）を、Rust のユニットテストと gpui のテストで確かめられる
* WebView の常駐メモリ（macOS で 120〜180MB）が無くなる
* フロントエンドのツール（bun、Vite、svelte-check）がビルドから消える

**Negative:**
* 上流の gpui は Zed 本体と一緒に動いており、1.0 前の API 変更がタグごとに入る。タグを上げるたびに追従が必要
* 部品ライブラリが無いので、テキスト入力、ポップオーバー、仮想リスト、テーマを自前で書く
* git 依存は Zed のモノレポ全体を fetch するので、CI ではキャッシュが要る
* UI に貢献できる人の母数は、Web より小さい
* `.app` の組み立てを自前で持つ。スピーカーレーンは署名済み .app でしか音が入らないので（docs/step0-tap-results.md）、最初から組み立てを用意する必要がある

**Neutral:**
* HTML のモックアップ（`mockups/`）は、見た目の仕様としてそのまま使う

### Confirmation

* 日本語 IME で glossary の編集欄に入力し、変換・確定・取り消しが自然に動くこと
* 確定部分は通常の色、未確定部分はグレーで、同じ行に続けて表示し、部分結果を連続で受けても追従すること
* `scripts/bundle-mac.sh` で作った署名済み `.app` で、スピーカーレーンに音が入ること
* 固定した Zed タグで、macOS の CI ビルドが通ること（Linux と Windows は対応を始める時点で追加する）
* `cargo tree` に GPL の crate が出てこないこと（CI で確かめる）

## Pros and Cons of the Options

### GPUI（公式）

* Good: 上流そのものなので、Zed で実証された描画と IME を使える。ライセンスが明快（Apache-2.0）
* Good: 間に第三者の再公開が入らない
* Bad: crates.io のリリースが止まっており、git のタグを固定して使う必要がある。部品を自前で書く

### gpui-kit

* Good: 75 を超える部品（仮想リスト、テーマ、AccessKit）がすぐに使える
* Bad: 上流の GPUI を Longbridge が `gpui-pre` として再公開したものに依存するので、上流との間に1層挟まる
* Bad: 0.x で、部品の API の変更も別に追う必要がある

### Tauri + Svelte を続ける

* Good: 既存の UI と組み立ての仕組みが使える。Web の人材が多い
* Bad: core と UI の間に IPC とイベントの書き出しが残り、言語が2つになる

## More Information

UI の構成は [docs/design/core.md](../design/core.md) の「使い方」と「crate 構成」を参照。Zed のタグとリリース状況: https://github.com/zed-industries/zed/releases 、crates.io の更新を求める issue: https://github.com/zed-industries/zed/issues/66275 。
