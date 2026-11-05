# UIシェルはTauri 2＋Svelte 5を継続する

| | |
|---|---|
| **Status** | superseded by [ADR-120002](20261105-120002-replace-tauri-svelte-with-gpui.md) |
| **Date** | 2026-08-10 |
| **Decision-makers** | staticWagomU |
| **Consulted** | Claude Code（egui / Slint / Iced との比較検討） |
| **Informed** | - |

## Context and Problem Statement

v2はコア構造を作り直すため、UIシェルもゼロベースで再選定した。低メモリ（MUST）とmac/Windows対応が制約で、画面は日本語・英語のテキストが単語単位で高頻度更新されるチャットログが中心となる。

## Decision Drivers

* 【MUST】低メモリ — ただしv2の常駐メモリの支配項はASRモデル（1GB超）であり、シェル層の差（〜100MB）は支配的でない
* 日本語の表示品質とIME入力の安定性
* 開発速度: Svelteの既存知見とv1のUI資産（イベント設計・部品）を再利用できる
* チャットログの仮想スクロール・波形描画などリッチな表示

## Considered Options

1. Tauri 2＋Svelte 5（継続）
2. egui（pure Rust・即時モード）
3. Slint（宣言的DSL）
4. Iced（pure Rust・Elmアーキテクチャ）

## Decision Outcome

**Chosen option**: 「Tauri 2＋Svelte 5（継続）」。メモリの支配項がモデル側にある以上、シェルは日本語表示品質と開発速度で選ぶべきと判断した。v1の反省はUI層ではなくプロセス構成にあり、シェルを変える理由がない。

### Consequences

**Positive:**
* ブラウザ品質の日本語レンダリングとIME、CSSによる自由なスタイリング
* v1のUIコンポーネント・イベント設計を選択的に移植できる
* WindowsはWebView2で追加実装なしに動く

**Negative:**
* WebView分の常駐メモリ（macで120〜180MB、WindowsはWebView2プロセスでさらに増える）
* Rustコア⇔UI間はTauriイベントのIPC境界が残る（単語単位のイベント頻度なら実測上問題にならない想定）

**Neutral:**
* 配布物は単一バイナリではなくインストーラ形式（WindowsはWebView2ランタイム依存）

### Confirmation

* 単語単位のtranscriptイベント連投時にUIが60fps相当で追従すること
* アプリ全体の常駐メモリ計測でシェル層が支配項になっていないこと

## Pros and Cons of the Options

### Tauri 2＋Svelte 5（継続）

* Good: 開発者のSvelte知見とv1資産が直に活きる
* Good: テキスト表示・IME・アクセシビリティがブラウザ品質
* Bad: WebView分のメモリオーバーヘッド

### egui

* Good: 最小メモリ（40〜80MB）・単一バイナリ配布
* Bad: 日本語フォント同梱・仮想スクロール自作が必要で、IMEも粗い
* Bad: 見た目の作り込みに労力がかかる

### Slint

* Good: 宣言的UIと低メモリのバランスが良い
* Bad: 独自DSLの学習コストと小さめのエコシステム

### Iced

* Good: pure RustでElmアーキテクチャ、COSMICでの採用実績
* Bad: ドキュメントが薄くAPI変化が速い。カスタムウィジェット自作が多い

## More Information

前提: [単一プロセスアーキテクチャ](20260810-153800-rebuild-v2-as-single-process-rust-core.md)。v1のUI実装は`v1`ブランチの`src/`を参照。
