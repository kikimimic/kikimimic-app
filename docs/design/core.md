# kikimimic-core 設計

Status: accepted（2026-11-05）。決定の要約は [ADR-120001](../ADR/20261105-120001-core-as-conductor-and-inference-runtime.md)。

`kikimimic-core` は、内部に専用スレッドを持つライブラリです。アプリはハンドル `Kikimimic` を1つ作り、メソッドを呼び、イベントを1本のストリームで受け取ります。

内部には2つの持ち主がいます。

- **conductor**: ドメインの状態をすべて持ちます。セッション、レーン、時計、タイムライン、ID、翻訳キュー、録音、設定、通知です。
- **inference**: モデルをすべて持ちます。GPU に触るのはここだけです。

どちらも `handle(now, input) -> outputs` という形の状態機械として書きます。CI では仮想時計と偽エンジンを使い、パイプライン全体を1スレッドで決定的に動かします。

## 背景

v2 の実質的なコアは `src-tauri/src/pipeline.rs`（2038行）です。セッションの状態遷移、時刻合わせ、ID 採番、翻訳の流れ、録音の方針が、Tauri の emit と混ざっています。このファイルはマイクと GPU がないと動かず、CI で検証できません（現状の詳細は `~/.ai/kikimimic/architect/grounding.md`）。

OSS として公開するには、次の2つが必要です。

- コントリビューターが `pipeline.rs` を読まなくても、エンジンや OS 対応を足せること
- GPU もモデルもマイクもない CI で、パイプライン全体を検証できること

v2 で実測した制約のうち、新しい設計でも守るものは次のとおりです。

1. 異なる ggml を持つ whisper.cpp と llama.cpp を、同じイメージに静的リンクしない（シンボルが衝突して SIGABRT になる）。
2. llama のハンドルはスレッドセーフではなく、バックエンドの初期化はプロセスで1回だけ。
3. 予算は、最初の部分結果まで2秒以内、確定まで1.5秒以内。
4. 認識は翻訳を待たない。翻訳キューは上限付きで最古を捨て、捨てたことを UI に伝える。
5. ASR モデルは1つをレーン間で共有する（約1.1GB）。
6. 録音は VAD より前の生音声を書く。全 wav が同じ長さ・同じ時間軸で、jsonl の ms は録音上の位置。
7. 全レーンのキャプチャ時刻を1つの単調時計に載せる。
8. macOS のスピーカーレーンは、署名済み .app でないと TCC が黙って無音を返す。
9. 言語設定の変更は、モデルを読み直さずに次の発話から効かせる。話される言語は最大2つ。
10. ネイティブにストリーミングするエンジンも受け入れる。LocalAgreement は window 型エンジンのアダプタの中に閉じ込める。
11. `transcript.jsonl` を ID で突き合わせる方式は互換を保つ。

## 使い方（呼び出し側）

UI は GPUI（Zed 公式）で書きます（[ADR-120002](../ADR/20261105-120002-replace-tauri-svelte-with-gpui.md)）。アプリは1つの Rust の crate `app/`（バイナリ `kikimimic`）で、core の型をそのまま使います。下のスケッチの gpui の API 名は、固定する Zed のタグに合わせて読み替えてください。

### アプリの起動（`app/src/main.rs`）

```rust
fn main() -> anyhow::Result<()> {
    let (kkm, events) = Kikimimic::start(Config {
        data_dir: paths::data_dir()?,                      // dirs crate。settings.toml, models/, licenses/
        recordings_dir: paths::audio_dir()?.join("kikimimic"),
        engines: engines::registry(),                      // エンジンの配線はアプリ側。core はエンジン名を知らない
        capture: kikimimic_capture::platform_backend(),
    })?;
    gpui_platform::application().run(move |cx: &mut App| {
        let state = cx.new(|_| AppState::new(kkm.clone(), kkm.snapshot()));
        bridge::forward(events, state.downgrade(), cx);    // Events（ブロッキング）→ フォアグラウンドのタスク
        cx.open_window(main_window_options(), |_, cx| cx.new(|cx| MainWindow::new(state, cx))).unwrap();
    });
    Ok(())
}
```

### イベントの受け口（`app/src/bridge.rs` と `app/src/state.rs`）

```rust
pub fn forward(events: Events, state: WeakEntity<AppState>, cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    std::thread::spawn(move || for ev in events { if tx.unbounded_send(ev).is_err() { break } });
    cx.spawn(async move |cx| {
        while let Some(ev) = rx.next().await {
            if state.update(cx, |s, cx| { s.apply(ev); cx.notify(); }).is_err() { break }
        }
    }).detach();
}

/// UI の状態。core の Event を畳み込むだけの純粋な reducer で、gpui なしでユニットテストできる。
pub struct AppState { kkm: Kikimimic, status: StatusView, lines: Lines /* ID で訳文を突き合わせ、レーンごとに上限 */, prompt: Option<Prompt>, models: Vec<ModelState> }
impl AppState { pub fn apply(&mut self, ev: Event) { match ev { /* 網羅的な match */ } } }
```

### UI の操作。1操作につき core の呼び出しは1回

```rust
// views/session.rs
Button::new("record").on_click(cx.listener(|this, _, _, cx| this.call(cx, |k| k.start_session(SessionMode::Meeting))))
Button::new("stop").on_click(cx.listener(|this, _, _, cx| this.call(cx, |k| k.stop_session())))   // 冪等
// 他の操作も同じ形で core を呼ぶ: update_settings / answer_prompt / diarize / library().list・read・rename
// call() は Err を AppState のエラー表示に回すだけ
```

### ダウンロード（`app/src/downloads.rs`）。ネットワークを扱うのはアプリ側のここだけ

```rust
cx.background_spawn(async move {                          // ureq（ブロッキング）をバックグラウンドの executor で動かす
    if accept_license { store.accept_license(&id)?; }    // PLaMo のように承諾が必要なものだけ
    for file in store.missing_files(&id)? {              // requires（whisper → silero-vad）も含む
        let mut w = store.begin_install(&file)?;         // .partial から再開
        /* Range: bytes={w.offset()}- で GET し、w.write(&chunk) */
        w.commit()?;                                     // sha256 の検証と atomic rename。2回呼んでも Ok
    }
    anyhow::Ok(())
})
```

### エンジンの配線（`app/src/engines.rs`）

```rust
pub fn registry() -> EngineRegistry {
    let mut r = EngineRegistry::default();
    r.asr("whisper", kikimimic_asr_whisper::FACTORY);
    r.mt("llama", kikimimic_mt_llama::FACTORY);
    #[cfg(target_os = "macos")] r.diarizer("nemotron-mlx", kikimimic_diarize_nemotron::FACTORY);
    r
}
#[test] fn catalog_and_registry_agree() { registry().check(Catalog::builtin()).unwrap(); }
```

### headless テスト（`crates/kikimimic-core/tests/meeting.rs`）

```rust
let out = Scenario::meeting()
    .lane(LaneRole::Mic, Fixture::silence(secs(6)))
    .lane(LaneRole::System, Fixture::wav("fixtures/en-two-sentences.wav").starting_at(ms(400)))
    .asr(ScriptedAsr::default().say(LaneRole::System, ms(500)..ms(1800), "en", "Hello there."))
    .mt(EchoMt::prefix("[ja] ").cost(ms(400)))
    .languages(["ja", "en"], Some("ja"))
    .stop_at(secs(6))
    .run();                                   // 1スレッド・仮想時計・決定的
insta::assert_yaml_snapshot!(out.events());
insta::assert_snapshot!(out.journal());       // transcript.jsonl のバイト列
out.assert_wavs_share_one_length();           // 制約6
out.assert_every_pending_translation_terminates_once();
```

エンジンを追加するコントリビューターがやることは次の3つです。

- window 型の ASR なら `WindowTranscriber::transcribe` の1メソッドを実装する。ストリーミング型なら `AsrEngine` を直接実装する。
- `engines.rs` に1行足す。
- `catalog.toml` に1件足す。

自分のエンジンが契約を守っているかは、`kikimimic_core::contract::asr_suite(&mut MyEngine)` で確認できます。

## 形

### crate 構成

```
models/catalog.toml              モデル一覧はここだけ。core が include_str! で読み込む
crates/
  kikimimic-core/                UI・デバイス・GPU・ネットワークを持たない。使う I/O は std::fs だけ
    lib.rs                       Kikimimic, Config, Events, Error, SessionMode
    domain.rs                    Lang, Spoken, LanguagePolicy, LaneId/LaneRole/LaneLabel, UtteranceId, 時刻の newtype, Pcm16k
    event.rs                     Event とその中身。UI との契約（Rust の型のまま渡す。serde は付けない）
    settings.rs                  Settings, SettingsPatch, Glossary。settings.toml を atomic に読み書き
    catalog.rs / store.rs        Catalog, ModelEntry, License / ModelStore, InstallWriter
    engine.rs                    AsrEngine, MtEngine, Diarizer, 各 Factory, EngineRegistry
    asr/windowed.rs              WindowTranscriber + SpeechDetector → AsrEngine（StreamScheduler, LocalAgreement）
    capture.rs                   CaptureBackend, capture::channel(), CaptureWriter/CaptureStream
    library.rs                   一覧・ID での突き合わせ・タイトル変更
    sim.rs, contract.rs          feature "sim"。Scenario、偽エンジン、偽キャプチャ、エンジン向けの契約テスト
    conductor/   (private)       状態機械, lane.rs, ledger.rs, notices.rs, record.rs, postprocess.rs
    inference.rs (private)       エンジンの置き場と、実行する作業の選択
    runtime.rs   (private)       スレッドと実時計（std::time::Instant を使うのはここだけ）
  kikimimic-asr-whisper/         whisper-rs + Silero VAD → asr::Windowed
  kikimimic-mt-llama/            PromptFormat {ChatMl, Plamo3}。非公開の Backend {Dylib, Linked} を feature で切り替え
  kikimimic-mt-llama-dylib/      cdylib（今の kkm-translate-ggml）。`isolated` のときだけビルド
  kikimimic-capture/             platform_backend(): macOS（cpal mic + Process Tap）。後で Windows（WASAPI loopback）, Linux（PipeWire）
  kikimimic-diarize-nemotron/    macOS 専用・任意。今の uv/MLX スクリプトを Diarizer trait の向こうに置く
app/                             GPUI のアプリ（bin `kikimimic`）。main, bridge, state（reducer）, engines, downloads, paths, views/,
                                 widgets/（text_input, popover, list。gpui の examples/ を出発点に自前で書く）
scripts/bundle-mac.sh            .app の組み立て（Info.plist, Contents/Frameworks の cdylib, 署名）
```

依存の向きは「エンジンと capture → core」「app → core とエンジン」です。core はワークスペース内の他の crate に依存しません。CI では `cargo metadata` を調べ、core が cpal・whisper-rs・llama-cpp-2・gpui・HTTP クライアントに依存したら落とします。app の依存ツリーに GPL の crate（Zed の `ui` など）が入っても落とします。

### データ構造

```rust
// domain.rs
pub struct Lang(Box<str>);                           // parse で検証する。core の中で生の String は使わない
pub enum Spoken { One(Lang), Two(Lang, Lang) }       // 最大2言語を型で保証（制約9）
pub struct LanguagePolicy { pub spoken: Spoken, pub target: Option<Lang>, pub mutual: bool }
pub struct LaneId(u8);                               // レーン数を2に固定しない
pub enum LaneRole { Mic, System }
pub struct LaneLabel(Box<str>);                      // "mic", "speaker", "mic-2"…。1つの関数で導出し、jsonl の互換を保つ
pub struct UtteranceId(u64);                         // アプリ起動中は通し番号。採番するのは Ledger だけ
pub struct HostNanos(u64);                           // キャプチャ時刻（プラットフォームの単調時計）
pub struct StreamMs(u64);                            // 1レーンの認識器に入れた音声の位置
pub struct RecordingMs(u64);                         // wav 上の位置。jsonl と UI に出るのはこれだけ
// WindowMs は asr::windowed の中だけで使い、外に出さない
pub struct Pcm16k(Vec<f32>);                         // リサンプラだけが作れる。ASR に生のデバイス音声は渡らない
```

### エンジンの境界（`engine.rs`）

```rust
/// 1つのモデルで全レーンを受け持つ。レーンごとの状態はエンジンが内部に持つ（制約5、Rc<RefCell> は不要）。
pub trait AsrEngine: Send {
    fn cadence(&self) -> Duration;                                   // whisper は 1000ms、ストリーミング型は 100ms 程度
    fn open_lane(&mut self, lane: LaneId);
    fn push(&mut self, lane: LaneId, audio: &Pcm16k);
    fn step(&mut self, lane: LaneId, langs: &Spoken) -> Result<Option<AsrStep>>;   // 言語の変更はここで効く。読み直さない
    fn finish(&mut self, lane: LaneId, langs: &Spoken) -> Result<Vec<AsrStep>>;
    fn transcribe_clip(&mut self, audio: &Pcm16k, langs: &Spoken) -> Result<Vec<Recognized>> { /* scratch lane を使う既定実装 */ }
    fn set_keywords(&mut self, keywords: &[Keyword]) { /* 既定は何もしない。Haas は glossary を keywords として聞き取りに使う */ }
}
pub struct AsrStep { pub committed_delta: String, pub volatile: String, pub utterance: Option<Recognized> }
pub struct Recognized { pub text: String, pub start: StreamMs, pub end: StreamMs, pub lang: Option<Lang> }

/// &mut self なので、同時に走る生成は常に1つ（制約2）。処理はブロッキングで行う。
pub trait MtEngine: Send {
    fn translate(&mut self, req: &MtRequest<'_>) -> Result<String>;
}
pub struct MtRequest<'a> {
    pub text: &'a str,
    pub source: Option<&'a Lang>,   // None は判定に自信がないとき。言語を名指ししない
    pub target: &'a Lang,
    pub context: &'a [ContextLine], // 直前の発話。PFML3 はそのまま使い、ChatML は無視してよい
    pub terms: &'a [Term],          // 翻訳先言語で絞り込み済み
}

pub trait Diarizer: Send { fn diarize(&mut self, mix_wav: &Path) -> Result<Vec<SpeakerTurn>>; }

pub struct LoadRequest<'a> { pub entry: &'a ModelEntry, pub files: &'a ResolvedFiles, pub options: &'a EngineOptions }
pub struct AsrFactory { pub validate: fn(&EngineOptions) -> Result<()>, pub load: fn(&LoadRequest) -> Result<Box<dyn AsrEngine>> }
pub struct MtFactory  { /* 同じ形 */ }
pub struct EngineRegistry { /* 種類ごとに HashMap<&'static str, Factory> */ }
impl EngineRegistry { pub fn check(&self, catalog: &Catalog) -> Result<()> { todo!() } }  // カタログの全エントリにエンジンがあり、options が検証を通る
```

window 型エンジン向けの部品（`asr/windowed.rs`、公開）:

```rust
pub trait WindowTranscriber: Send { fn transcribe(&mut self, window: &Pcm16k, langs: &Spoken) -> Result<Hypothesis>; } // 時刻は WindowMs
pub trait SpeechDetector: Send { fn speech_segments(&mut self, window: &Pcm16k) -> Result<Vec<(usize, usize)>>; }
pub struct WindowConfig { /* min 1s, max 15s, carry 5s, onset_guard 0.3s, trailing_silence 1s */ }
pub struct Windowed<T, V> { transcriber: T, vad: V, cfg: WindowConfig, lanes: HashMap<LaneId, StreamScheduler> }
impl<T: WindowTranscriber, V: SpeechDetector> AsrEngine for Windowed<T, V> { /* todo!() */ }
```

プロンプト形式は `kikimimic-mt-llama` の中に置き、カタログの `options.prompt` で選びます。

```rust
pub enum PromptFormat { ChatMl, Plamo3 }
impl PromptFormat { pub fn render(&self, req: &MtRequest<'_>) -> String { todo!() } } // 純関数。ゴールデンテストで確かめる
#[cfg(feature = "isolated")] struct Dylib { /* libloading + C ABI */ }
#[cfg(feature = "linked")]   struct Linked { /* ggml を1つにまとめる -sys を使った llama-cpp-2 */ }
```

### キャプチャの境界（`capture.rs`）

チャネルは core が用意します。OS バックエンドがやるのは、サンプルとタイムスタンプを書き込むことだけです。次の処理はすべて core 側にあり、全プラットフォーム共通に1回だけテストします。

- アンカーの追跡と時刻の飛びの検出
- ダウンミックス
- overrun の集計
- 無音の監視

```rust
pub trait CaptureBackend: Send + Sync {
    fn clock(&self) -> ClockDomain;                                  // MachHostTime | Qpc | ClockMonotonic | Virtual
    fn available(&self) -> Vec<LaneRole>;
    fn open(&self, role: LaneRole) -> Result<OpenedSource>;          // バックエンドは capture::channel() を呼び、writer を OS のコールバックへ渡す
}
pub fn channel(format: Format, ring: Duration) -> (CaptureWriter, CaptureStream) { todo!() }
impl CaptureWriter { pub fn push(&mut self, interleaved: &[f32], captured_at: HostNanos) { todo!() } }  // RT 安全。確保もロックもしない
pub trait CaptureHandle: Send { fn poll_event(&mut self) -> Option<SourceEvent>; fn decide(&mut self, d: SourceDecision); }
// SourceEvent: OutputSwitchProposed / OutputSwitched / OutputSwitchFailed（既定の出力デバイスの追従）
```

セッション開始時に、全レーンの `ClockDomain` が一致しているかを確かめ、違えば起動しません（制約7）。

### UI への契約（`event.rs`）

```rust
pub enum Event {
    /// utterance の ID は、その発話の最初の文字が出た時点で振る（部分訳とリアルタイム話者ラベルの入口）
    Transcript { lane: LaneLabel, utterance: UtteranceId, committed_delta: String, volatile: String, finished: Option<UtteranceView> },
    /// translating: true だった発話には、必ずちょうど1回届く
    Translation { id: UtteranceId, lane: LaneLabel, outcome: TranslationOutcome },
    Status(StatusView),                 // phase + 型付きの notices。文言は UI 側で組み立てる
    Prompt { prompt: Option<Prompt> },  // 出力デバイス切り替えの確認
    ModelInstall { id: ModelId, state: ModelState },
    Job { recording: RecordingId, progress: JobProgress },
}
pub enum TranslationOutcome { Text { text: String, lang: Lang }, Dropped, Failed { message: String } }
pub struct UtteranceView { pub id: UtteranceId, pub text: String, pub start_ms: RecordingMs, pub end_ms: RecordingMs, pub lang: Option<Lang>, pub translating: bool }
pub enum Notice { SourceUnavailable{..}, SystemAudioSilent, RecordingOff{..}, Overrun{..}, DecodeFailed{..}, TranslationUnavailable{..}, AsrLagging{..} }
```

UI 向けのイベントは互換を切ります。イベントを1本にまとめ、通知を型付きにするためです。UI は GPUI で書き直し、core の `Event` を Rust の型のまま受け取ります（serde も型生成も要りません）。ディスク上の形式は今と同じです。対象は次のとおりです。

- `<lane>.wav`、`mix.wav`、`metadata.json`
- `transcript.jsonl` の `utterance` 行と `translation` 行
- `postprocess/transcript.jsonl`

`Dropped` と `Failed` は、jsonl には書きません。jsonl の形式を持つのは `conductor/record.rs` だけで、`library.rs` も同じ非公開の型を通して読みます。

### 2つの持ち主（private）

```rust
// conductor/mod.rs
pub(crate) enum Input { Command(Command), FromInference(InferenceOut), Tick }
pub(crate) enum Output { Emit(Event), ToInference(InferenceIn) }
pub(crate) struct Conductor { settings: Settings, ledger: Ledger, phase: SessionPhase, notices: Notices, snapshot: Arc<Mutex<Snapshot>> }
enum SessionPhase { Idle, Loading(Live), Listening(Live), Draining { live: Live, deadline: Instant }, PostProcessing(RecordingId) }
impl Conductor {
    pub(crate) fn handle(&mut self, now: Instant, input: Input) -> Vec<Output> { todo!() }
    pub(crate) fn next_wake(&self) -> Option<Instant> { todo!() }
}

// conductor/ledger.rs: 発話 ID と翻訳の管理を持つのはここだけ
pub(crate) struct Ledger {
    next_id: UtteranceId,
    owed: BTreeMap<UtteranceId, LaneLabel>,   // ここから外したときに、終端の Translation を出す
    queue: VecDeque<Job>,                     // 上限8、溢れたら最古を捨てる（制約4）
    in_flight: Option<UtteranceId>,           // inference に渡す仕事は常に1件まで
    history: VecDeque<ContextLine>,           // MtRequest.context に渡す
}

// inference.rs
pub(crate) enum InferenceIn { LoadAsr(ModelId), LoadMt(Option<ModelId>), OpenLane(LaneId), Audio(LaneId, Pcm16k), FinishLanes(Spoken), Translate(Job), Diarize(PostJob) }
pub(crate) enum InferenceOut { AsrReady(..), MtReady(..), Step(LaneId, AsrStep), StepFailed(..), Lagging(..), Finished(..), Translated(UtteranceId, Result<String, String>), PostDone(..) }
pub(crate) struct Inference { asr: Slot<Box<dyn AsrEngine>>, mt: Slot<Box<dyn MtEngine>>, lanes: HashMap<LaneId, LaneDue>, langs: Spoken }
impl Inference {
    /// 次に実行する作業を選ぶ。ASR のステップは cadence に従い、MT は1件ずつ。
    pub(crate) fn handle(&mut self, now: Instant, input: Option<InferenceIn>) -> Vec<InferenceOut> { todo!() }
}
```

### データの流れ

1. キャプチャスレッド（OS のコールバック）が `CaptureWriter::push(frames, HostNanos)` を呼びます。ここではそれ以外のことはしません。
2. conductor は100msごとに各レーンを処理します。
   - リングを読み出し、`AnchorTrail` で時刻の飛びの位置で区切ります。
   - ダウンミックスし、`SessionClock::position` で録音上の位置を決めて、VAD より前の音声を recorder に書きます。
   - リサンプルして `Pcm16k` にし、`LaneTimeline` を更新し、`Audio` を inference に送ります。
3. inference は、cadence が来たレーンについて `asr.step` を実行し、`Step` を返します。
4. conductor は `EmitGate` を通し、`Ledger::number` を呼びます。発話の確定時には次を行います。
   - glossary の置換を適用します。
   - `target_for` で翻訳先を決め、キューに積みます。追い出された仕事は `Translation{Dropped}` にします。
   - `StreamMs` を `RecordingMs` に変換し、jsonl に書いて、`Transcript` を emit します。
   - inference が空いていれば `Translate` を送ります。
5. `Translated` が返ってきたら、`Ledger::settle` で `Translation` を emit し、jsonl に書いて、次の仕事を送ります。
6. Stop では次の順に進みます。
   - キャプチャを止め、リング、リサンプラ、`FinishLanes` の順に残りを回収します。
   - `Draining` 状態で、owed が空になるか10秒経つまで待ちます。
   - 残りは `abandon_all` で `Dropped` にし、recorder を閉じて `Idle` に戻ります。
   - そのあと届いた `Translated` は owed に ID がないので無視されます。

utterance 1件の流れは `conductor/mod.rs`（受け取り）→ `ledger.rs`（ID と翻訳）→ `record.rs`（バイト列）の3ファイルで追えます。

### 並行性と GPU

```
OS コールバック ─push─▶ [rtrb 8s] ─drain─▶ CONDUCTOR ─Audio/Translate─▶ INFERENCE
                                          │ ドメイン状態の全部         │ モデルの全部
UI の操作 ───Command(mpsc)────────────▶│ wav と jsonl の書き込み    │ ASR スレッド + MT スレッド（最初の実装）
           ◀─ Snapshot（書き手は1つ）────│                            │
イベント転送 ◀──────────── Events ───────│◀────── InferenceOut ───────┘
```

- 共有している可変な構造は、SPSC のリングと Snapshot（書き手は conductor だけ）の2つだけです。今の `PolicyStore`、`StatusStore`、`OutputDevicePromptStore` の Mutex と `Rc<RefCell<WhisperModels>>` はなくなります。
- `inference` が実行に使うスレッドは2本から始めます。ASR 用1本と MT 用1本で、どちらもアプリが終了するまで生きます。今と同じで、実測済みの「並行に投げて Metal に任せる」構成です。MT 用のスレッドがプロセスで1本なので、llama の初期化は1回で済みます。
- 「ASR と MT を1スレッドで直列にし、MT をトークン単位で区切って ASR を優先する」案もあります。これは spike S2 で測ってから決めます。採用する場合も変更は `inference.rs` と `MtEngine` に閉じ、app・conductor・UI 契約には影響しません。
- 過負荷時の扱いは次の4つです。
  - リングの overrun は `Notice::Overrun` にします。
  - ASR が3秒以上遅れたら `Notice::AsrLagging` にします。15秒で強制スライドする既存の仕組みはそのまま使います。
  - 翻訳キューは最古を捨てて `Dropped` にします。
  - `Audio` メッセージは実時間で上限が決まります（1レーンあたり 64KB/s）。

### テスト

| 層 | 内容 |
|---|---|
| 純粋なユニット | `LocalAgreement`、`StreamScheduler`、`LanguagePolicy`、`Mixer`、リサンプラ、`AnchorTrail`、`LaneTimeline`、`EmitGate`、`Notices`、`Ledger`。v2 のテストごと移す |
| シミュレーション | `sim::Scenario` が `Conductor` と `Inference` を1スレッドで動かす。偽エンジンは1回の呼び出しにかかる仮想コストを持ち、GPU の混雑や10秒の drain も決定的に再現する。イベント列と jsonl を insta で比べる |
| 不変条件 | proptest でコストを揺らし、次の4つを確かめる。翻訳中の発話に終端の Translation がちょうど1回届く、jsonl の ID が重複しない、全 wav が同じ長さ、レーン内で RecordingMs が減らない |
| スレッドを使う smoke | 実時計で約2秒動かし、順序に依存しない結果（ID で突き合わせた最終 jsonl）だけを確かめる |
| エンジン crate | `contract::{asr_suite, mt_suite, capture_suite}` を各 crate が自分の型で呼ぶ。加えて `PromptFormat::render` のゴールデンテストと、`EngineRegistry::check` |
| UI | `AppState::apply` のユニットテスト（ID での突き合わせ、行の上限、Dropped の表示）。gpui のテスト用コンテキストで、操作が core の正しいメソッドを呼ぶことを確かめる |
| 実エンジン | macOS の nightly で ggml-tiny を使い、同じ Scenario を通す。GPU とモデルが必要なのはこれだけ |
| CI マトリクス | core と sim を macOS・Linux・Windows でビルドしてテストする |

## 統合の判断（arena）

設計案は4つのモデル（opus / fable / sonnet / haiku）にそれぞれ独立して書かせ、fable に採点させました。

| 候補 | 合計（30点満点） | 形 |
|---|---|---|
| A（opus） | 28 | conductor と inference の2つの持ち主。どちらも sans-IO の状態機械。キャプチャのチャネルは core が持つ |
| B（fable） | 26 | conductor が ASR をその場で実行し、MT は Dispatcher スレッド。GPU の優先は Turnstile で制御 |
| C（sonnet） | 24 | ASR、MT、recorder の3つの host スレッド。ComputeGate。契約テスト |
| D（haiku） | 19 | セッション actor と、指示を実行するだけの worker。DTO はアプリ側 |

**土台は A です。** 採点役と私の評価が一致しました。理由は次の3つです。

- パイプライン全体を1スレッド・仮想時計で決定的に動かせるのは A だけでした。
- 状態の持ち主が2つに分かれ、Mutex で守る共有状態と `first_id` フィルタが構造上いらなくなります。
- キャプチャのチャネルを core が用意するので、OS バックエンドが最も薄くなります。

**A から変えた点**: A は「ASR と MT を1スレッドで直列にし、MT をトークン単位で区切る」方式を最初から既定にしていました。これは測定していない賭けです。そこで最初の実装は、今と同じ「ASR と MT を別スレッドで並行」にしました。あわせて `MtEngine` を `begin/advance/cancel` ではなく、ブロッキングの `translate` 1メソッドにしました（B の形）。区切る方式は spike S2 の結果を見て `inference.rs` の中で入れ替えます。

**他の案から取り込んだもの**

- B: 発話 ID を最初の部分結果の時点で振ること（部分訳の入口）。`ClockDomain` が一致しているかの確認（制約7を起動時に強制する）。
- C: エンジン向けの契約テスト（`contract::*_suite`）。`cargo metadata` による依存の禁止リストのテスト。`Translation` の結果を `Text/Dropped/Failed` の型で表すこと。
- D: `Pcm16k` の newtype（ASR に生のデバイス音声を渡せないようにする）。

**取り込まなかったもの**

| 案 | 取り込まなかったもの | 理由 |
|---|---|---|
| B | Turnstile | GPU の優先を、エンジンが協力することに頼る形になる。core とエンジンに所有が分かれる |
| B | Soniox crate を残すこと | 完全ローカルの方針に合わず、今は必要ない |
| C | session-local な ID | 実測済みの app-lifetime 方針から外れる。A の Ledger なら終端が1回だけ届くので、session ごとに振り直す必要がない |
| C | recorder 専用スレッド | 書き込みが ASR を止めた実績がない |
| C | `kikimimic-fetch` crate | HTTP はアプリ側に置けば足りる |
| C | `rev` / `SpeakerLabel` を今から入れること | まだ使わない機能を先に作ることになる |
| D | アプリ側の wire DTO | 型を手で同期するリストになる |
| D | `Backends` を1つだけ持つ形 | カタログでエンジンを選べない |

## 受け入れたトレードオフ

- UI 向けのイベントの互換を切ります。代わりに、種類をまたいだ順序の保証と、文言を UI 側で組み立てられる通知が手に入ります。UI は GPUI で書き直すので、互換を保つ相手もいません。
- 10秒の drain の後に届いた訳は捨てます。代わりに「終端のイベントがちょうど1回届く」という不変条件が手に入ります。
- 音声のチャンクごとに conductor から inference へメッセージを1つ送ります。代わりに、録音の書き込みがデコード待ちで止まらなくなります。
- `EngineOptions` は core から見ると型のない toml の表です。代わりに、core はプロンプト形式も GPU 設定も知らずに済みます。不整合は `EngineRegistry::check` で検出します。
- sim の仮想コストはあくまでモデルです。レイテンシー予算が守れているかの確認は、nightly の実エンジンのテストに任せます。
- core は std::fs の I/O（設定、モデル、セッションのディレクトリ）を持ちます。「I/O を持たない」は、プラットフォーム API・デバイス・GPU・ネットワークを持たない、という意味で使っています。

## 検討した他の形

- **sans-IO の core とし、ランタイムをアプリ側に置く形**: core が最も小さくなります。しかし、スケジューリング、GPU の扱い、drain の時間管理（今テストされていない部分そのもの）を、呼び出し側がすべて正しく書く必要があります。interface depth で負けました。
- **ステージごとの actor（capture → ASR → MT → recorder）**: 公開する面が増え、隠せるものは減ります。1つの時計・1つの ID 採番・jsonl の書き手が1人という不変条件がスレッドをまたいで散らばります。今の `Rc<RefCell>` と `first_id` フィルタはまさにこの散らばりから生まれました。
- **`pipeline.rs` をそのまま core へ移す形**: 差分は最小です。ただし、エンジンを足すたびに core を触ることになり、`Instant` に直接依存しているので仮想時間でも動かせません。中のアルゴリズムは移しますが、形は引き継ぎません。

## 決定済みの論点（2026-11-05）

- glossary は全モードに適用する。`replacements`（誤認識 → 正しい表記）は確定した文字起こしに、`terms` は翻訳へのヒントに使う。語を聞き取れる認識器（Haas）には、正しい表記ごとに誤認識をまとめた `keywords` も渡す。v2 の localStorage にある辞書は引き継がない（GPUI からは読めず、登録し直す方が安い）
- bundle identifier は `io.github.kikimimic.app` に変える（TCC の許可は取り直しになる）
- README は英語を主にし、日本語版を `README.ja.md` に置く

## 実装との突き合わせ

実装（2026-11-05）で設計から変えた点と、それでも設計の約束が守られている理由です。

| 設計 | 実装 | 理由・約束が守られている根拠 |
|---|---|---|
| `inference` を独立した sans-IO の状態機械にする | 作らなかった。cadence と「MT は1件ずつ」は conductor（`Lane::stepping`、`Ledger::in_flight`）が持ち、エンジンの実行は runtime の ASR/MT スレッドと `sim` が担う | 状態の持ち主は conductor だけのまま。sim は conductor の出力をその場で実行するので、テストの決定性も保たれる |
| `Event::Prompt`（出力デバイス切り替え） | `Event::Source(SourceEvent)` と `Kikimimic::decide_source` | 問い合わせの中身はキャプチャのバックエンドが持つので、core はそれを素通しする形にした。古い判断は uid で弾く |
| `Event::ModelInstall` | 進捗はアプリの `downloads.rs` が持つ。core の `ModelStore::state` で状態を読む | ネットワークを扱うのはアプリだけ、という設計を守るため、進捗も同じ側に置いた |
| `Event::Job` | `Event::PostProcess { recording, state }` | 後処理は話者分離だけなので、名前を中身に合わせた |
| `Kikimimic::start` が `Result` を返す | 返さない。失敗は `Event::Status(Phase::Error)` で届く | エンジンの読み込みを遅延させたため、起動時に失敗する処理がない |
| `Config` の `engines` だけでエンジンを選ぶ | `catalog` も `Config` で渡す | テストで小さなカタログを使うため。アプリは `Catalog::builtin()` を渡す |
| 録音の開始時刻（`started_at_ms`） | `started: Option<String>`（ディレクトリ名の `yyyyMMddHHmmss`） | core にタイムゾーン（chrono）を入れないため。アプリが現地時刻に直す |
| GPU は ASR と MT を並行（S2 で判断） | 並行のまま | S2 の暫定結果（docs/plamo-spike-results.md）で、直列化に切り替える根拠がなかった |
| `kikimimic-mt-llama-dylib` は crates.io の `llama-cpp-2` | git リビジョンに固定 | crates.io 版の llama.cpp が PLaMo 3 のトークナイザを持たないため（docs/plamo-spike-results.md） |

受け入れの判断は、この作業の依頼者（staticWagomU）の「全部進めていこう」に基づいて実装者が行いました。見直しが必要なら、この表から始めてください。

## 未決事項とリスク

- S1: whisper.cpp・llama.cpp・ggml を1つの -sys でビルドできるか。失敗したら `isolated` のまま進めます。
- S2: ASR と MT を並行にした場合と、直列で区切った場合のどちらが速いか。2レーンが続けて話す条件で、確定の遅延と1文あたりの翻訳時間を測ります（Qwen3-4B と PLaMo 2B の両方）。
- PFML3 の `context` と `glossary` の出力は、PFN のリファレンスと一致するか。ゴールデンテストの元になる正しいサンプルが必要です。
- `MtRequest.context` に相手レーンの発話も入れるか。対話の訳には効きそうですが、プロンプトが長くなります。
- 話者分離は `uv` に依存しますが、Homebrew cask では `uv` があるとは限りません。当面は任意の追加機能とし（`uv` が無ければ導入方法を表示する）、1.0 までにプロセス内で動く実装（sherpa-onnx, Apache-2.0）への置き換えを目指します。最終的には spike S5 の精度で決めます。Windows/Linux では MLX が使えないので、いずれ必要になります。
- Windows の WASAPI loopback と cpal のマイクの時刻を同じ時計に載せられるか（S4）。
- 1つのプロセスの中で、llama のバックエンドを初期化したまま別の GGUF に入れ替えられるか（Qwen から PLaMo への切り替え）。できなければ「再起動後に反映」とします。

## 次の実装ステップ

`kikimimic-core` を作り、`domain.rs`、`engine.rs`、`event.rs`、`sim.rs` の骨組みを置きます。そのうえで、偽エンジンだけを使った最初のシナリオテスト（1レーン、1文、訳、jsonl）を Red にします。
