# Kikimimic

[English](README.md)

完全ローカルで動く、リアルタイム多言語文字起こし＋翻訳のデスクトップアプリ（MIT）。

> **v3 へ作り直し中です。** OSS 化に合わせて、コアを headless なライブラリにし、UI を GPUI に置き換えています（[plan.md](plan.md)、[docs/design/core.md](docs/design/core.md)）。以下は現在動いている v2 の説明です。

名前は「聞き耳（kikimimi）」と「mimic（真似る＝話を写し取る）」の重ね合わせで、
共有している `mimi` が二つのレーン——自分の声とスピーカーから届く相手の声——にあたる。

- 単語単位のリアルタイム文字起こし（通常録音はローカルWhisper、対面モードはSoniox）
- 確定文単位のリアルタイム翻訳（ローカルLLM）
- マイク／スピーカー別レーンの録音
- mac / Windows 対応予定

技術選定の経緯は [docs/ADR/](docs/ADR/)、進行計画は [plan.md](plan.md) を参照。

v1の実装は `v1` ブランチに保存されている。

## 対面モード

対面モードはマイクのみを使い、押している間の音声をSonioxへ送って文字起こしする。録音ファイルは作らない。利用には `SONIOX_API_KEY`（または `API_KEY`）を環境変数かチェックアウトの `.env` に設定する。辞書には「誤認識表記 → 正しい表記」を登録できる。正しい表記をSonioxの認識ヒントに渡し、確定文に残った誤認識は翻訳へ渡す前に置換する。途中表示は確定まで元の認識結果のまま。辞書はこの端末のアプリ内に保存し、通常録音には適用しない。

## 実行準備

モデルファイルを `models/` に置く（gitignore済み）。アプリの「モデル」画面からも取得できる。その場合はアプリデータの `models` に保存され、こちらのコピーより優先される。起動時にどれか足りなければその画面が先に開く。ダウンロードはボタンを押したときだけ始まる。

- `ggml-large-v3-turbo-q8_0.bin` — Whisper本体（q8_0が既定。q5_0はMetalで逆に遅い、docs/step0-results.md参照）
- `ggml-silero-v5.1.2.bin` — Silero VAD（whisper.cpp配布のGGML版）
- `Qwen3-4B-Instruct-2507-Q4_K_M.gguf` — 翻訳LLM（無くても文字起こしは動く。翻訳だけが止まる）

翻訳バックエンドはcdylibとして別にビルドする。llama.cppのggmlをwhisperのggmlと同じバイナリに入れられないため（[ADR-153805](docs/ADR/20260819-143000-isolate-llama-cpp-in-a-cdylib.md)）:

```sh
cargo build --release -p kikimimic-mt-llama-dylib
```

起動:

- 開発: `npm run tauri dev`（上のcdylibを先に一度ビルドしておく。dev/releaseどちらのプロファイルでも探す）
- 実機確認: `./scripts/build-app.sh --open` — cdylibのビルドから`.app`の作成・起動まで。スピーカーレーンには`.app`起動が必須

## 録音

Recordを押すとセッションごとに `~/Music/kikimimic/<yyyyMMddHHmmss>/` が作られ、以下が書き出される（48kHz / モノラル / 16bit PCM）:

- `mic.wav` — マイクレーン
- `speaker.wav` — スピーカーレーン（取得できなかった場合は無音）
- `mix.wav` — 両レーンのミックス
- `transcript.jsonl` — 確定した発話と、その訳文（1行1件）

VADや無音ゲートより手前の音をそのまま書くので、文字起こしが拾わなかった部分もファイルには残る。3つのwavは同じ時間軸・同じ長さで、レーンが音を出していない区間は無音で埋められる。`transcript.jsonl` の `startMs`/`endMs` はこの録音上の位置なので、そのまま音声にシークできる:

```json
{"type":"utterance","id":1,"lane":"mic","startMs":1200,"endMs":3400,"lang":"ja","text":"こんにちは"}
{"type":"translation","id":1,"lang":"en","text":"Hello."}
```

訳文は発話の数秒後に、レーンをまたいで前後して届く。だから位置ではなく `id` で発話に結びつける。

スピーカーレーンはmacOSのシステム音声権限が要るため、`.app`として起動しないと無音になる（[docs/step0-tap-results.md](docs/step0-tap-results.md)）。

### 録音後の話者分離

録音一覧からセッションを開き、「•••」→「話者分離を実行」を選ぶ。Nemotron-3-Diarization の MLX 版が `mix.wav` を解析し、結果を録音フォルダの `postprocess/nemotron.rttm` に保存する。続けて、2人以上の話者が0.5秒以上ずつ話している発話を話者の切り替わりで分け、そのレーンのWAVを切り出して文字起こしし直す。元の発話に訳文があれば、分けた発話も同じ言語へ翻訳し直す。結果は `postprocess/transcript.jsonl` に保存し、録音画面とコピーした文字起こしはこちらを使う。1人しか話していない発話は元の文字起こしと訳文のまま、その話者を「話者1」などで表示する。`transcript.jsonl` と WAV は書き換えない。重なりがない発話には話者を付けない。

分け直しには録音と同じWhisperと翻訳モデルを使うため、録音中は実行できない。相づちなど0.5秒未満の発話と、他の話者に重ねて話された発話では分けない。分けた部分は元の発話の言語に固定して認識する。

Apple Silicon と `uv` が必要。`./scripts/build-app.sh --open` は開発シェルの `uv` のパスをアプリへ渡す。Finder から起動する場合は `uv` が PATH にある必要がある。そうでなければ `open --env KKM_UV_BIN=/path/to/uv /path/to/Kikimimic.app` で起動する。初回は Python 依存とモデルの取得が必要で、処理中は録音画面に状態を表示する。話者IDは人物の同定ではなくモデルが付けた番号で、複数話者や日本語での正確さは録音を聴いて確認する。

## 翻訳と言語設定

ヘッダの言語ピル（`日本語 ⇄ English` のような表示）から、**話される言語**（最大2）と**翻訳先**を別々に設定する。表示の記号がそのまま状態を表す:

| 表示 | 意味 |
|---|---|
| `日本語` | 単一言語・訳さない |
| `English → 日本語` | 英語の発言を日本語で読む |
| `日本語 ⇄ English` | 相互翻訳（画面を相手にも見せる） |
| `日本語 · English` | 2言語だが訳さない |

話される言語が1つなら言語判定そのものを行わないので、速く、取り違えも起きない。変更は録音中でも次の発話から反映される（モデルの再ロードは起きない）。

確定した文だけが翻訳に回り、認識をブロックしない。翻訳が追いつかないときは**古い文から捨てる**（画面に出ている最新の文を優先する）。捨てられた行は「訳しています…」のまま残らず、待つのをやめる。

## 環境変数

すべて任意。未設定時は括弧内の既定値で動く。

| 変数 | 既定 | 意味 |
|---|---|---|
| `KKM_WHISPER_MODEL` | `models/ggml-large-v3-turbo-q8_0.bin` | Whisperモデルのパス |
| `KKM_VAD_MODEL` | `models/ggml-silero-v5.1.2.bin` | Silero VADモデルのパス |
| `KKM_LLM_MODEL` | `models/Qwen3-4B-Instruct-2507-Q4_K_M.gguf` | 翻訳LLMのパス |
| `KKM_TRANSLATE_DYLIB` | `.app`内 → `target/{release,debug}` の順に探索 | 翻訳バックエンドcdylibのパス |
| `KKM_LLM_GPU_LAYERS` | 全層 | 翻訳LLMをGPUに載せる層数。`0`でCPU |
| `KKM_LANGS` | `ja,en` | 起動時の「話される言語」候補（最大2）。アプリ内の言語ピルで変更でき、そちらが優先 |
| `KKM_LANG` | 未設定 | 設定すると起動時の話される言語を1つに固定（言語判定を行わない） |
| `KKM_TARGET` | `ja` | 起動時の翻訳先。`none` で翻訳オフ |
| `KKM_VAD_THRESHOLD` | whisper.cpp既定(0.5) | 発話判定しきい値 0..1 |
| `KKM_VAD_MIN_SPEECH_MS` | whisper.cpp既定 | これより短い発話は無視 |
| `KKM_VAD_MIN_SILENCE_MS` | whisper.cpp既定 | 発話区切りとみなす最短無音 |
| `KKM_VAD_PAD_MS` | whisper.cpp既定 | 検出区間の前後パディング |
| `KKM_RECORD_DIR` | `~/Music/kikimimic` | 録音セッションフォルダを作る親ディレクトリ |
| `KKM_UV_BIN` | `uv`（PATHから検索） | 録音後のNemotron話者分離に使うuvの実行ファイル |
