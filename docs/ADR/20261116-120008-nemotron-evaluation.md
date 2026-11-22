# Nemotron: 録音後の任意バックエンドを暫定維持する

状態: 暫定。Issue #8 の評価単位。製品方式の最終決定は S5 待ち。

現行の `kikimimic-diarize-nemotron` は uv/MLX 子プロセスを使う。
この例外を任意機能として維持し、リアルタイム処理や新しい実行環境は追加しない。
現行 runtime は `mix.wav` を渡している。Issue の speaker.wav のみという前提とは異なり、マイクを自己話者として保持する仕様は別途検証が必要。

## 根拠と限界

[日本語比較](https://github.com/staticWagomU/whisperx-diarization/issues/1) は確認済み。
相互DERは正解に対する精度ではない。ASRによる発話有無の判定も話者割当の正しさを証明しない。
既存 plan の6.7分・177区間は過去の記録で、今回の再実測ではない。
今回はモデル取得も実音声推論も実行していない。

[公式モデルカード](https://huggingface.co/nvidia/Nemotron-3-Diarization) は NeMo-Speech.cpp のネイティブ実行を案内している。
ただし Rust FFI、transcribe.cpp の対応、ONNXの同等性、ggmlシンボル隔離は今回未検証。
ネイティブ化は ABI と同時ロードの検証を経て別ADRで決める。
0.32秒は入力バッファ遅延であり、アプリの実測遅延ではない。

## 再現手順

既存モデルと uv 依存がキャッシュ済みの環境だけで、ネットワークを無効化して実行する。
`HF_HUB_OFFLINE=1 UV_OFFLINE=1 KKM_TEST_MIX=/path/to/mix.wav cargo test -p kikimimic-diarize-nemotron -- --ignored --nocapture`
既存テストは結果を削除するので、比較用RTTMは同梱 Python スクリプトを直接実行して保存する。
`HF_HUB_OFFLINE=1 UV_OFFLINE=1 uv run --script crates/kikimimic-diarize-nemotron/diarize_nemotron.py input.wav output.rttm`
キャッシュ不足なら停止し、ダウンロードしない。入力同意と匿名化を確認する。

`python3 scripts/evaluate-diarization.py input.wav output.rttm --elapsed-seconds 12.3 > result.json`
実際の壁時計時間を指定する。JSONには入力とRTTMのハッシュ、区間数、話者数、RTFを記録する。
区間時間の合計は重複発話を含み、発話の和集合ではない。精度指標は算出しない。
別々に注釈した日本語参照RTTMで、重複発話の扱いとcollarを固定してDERを評価し、話者の過分割・統合を聴取する。
最終採用には実録音、メモリ、実行時間、失敗時の挙動、自己話者保持を確認する。
