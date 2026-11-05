//! Kikimimic core: engine-agnostic domain types and traits.
//!
//! UI shells and inference backends depend on this crate; it depends on
//! nothing platform-specific so it stays testable headless (ADR-153800).

pub mod language;
pub mod local_agreement;
pub mod mix;
pub mod models;
pub mod resample;
pub mod scheduler;
pub mod translate;

/// Which audio lane an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Mic,
    Speaker,
}

/// A transcription hypothesis for a window of audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Hypothesis {
    pub text: String,
    /// Milliseconds from the start of the *transcribed window* (the samples
    /// passed to [`AsrEngine::transcribe`]) — not from capture start. The
    /// scheduler's slide carry-over arithmetic depends on this; a backend
    /// returning stream-absolute times would silently break it.
    pub start_ms: u64,
    /// End of the last decoded segment, window-relative like `start_ms`.
    pub end_ms: u64,
    /// Language the engine detected (or was told) for this window.
    pub lang: Option<String>,
}

/// Streaming-agnostic ASR engine: transcribe one window of 16 kHz mono f32.
///
/// The LocalAgreement scheduler drives this repeatedly over a rolling
/// window; implementations must be safe to call from a dedicated thread.
pub trait AsrEngine: Send {
    fn transcribe(&mut self, samples: &[f32], lang: Option<&str>) -> anyhow::Result<Hypothesis>;
}

/// One lane's recognition, end to end: audio in, committed and volatile text
/// out. This is the seam the pipeline chooses an ASR at.
///
/// [`AsrEngine`] sits *below* this one. It is how an engine that can only
/// transcribe a window at a time — Whisper — is driven, and the window, the
/// slide and the agreement that turn that into a stream are the price of
/// that limitation. An engine that streams natively pays none of it, so
/// none of it appears here: an implementation may hold a window, or may
/// simply forward audio to a model that keeps its own state.
///
/// Implementations are per lane, but the model behind them need not be:
/// two lanes sharing one loaded Whisper is the difference between 1GB and
/// two of them.
pub trait Recognizer {
    /// Captured audio in order, 16 kHz mono f32.
    fn push_audio(&mut self, samples: &[f32]);

    /// Advance as far as the audio pushed so far allows. `None` means there
    /// was nothing to do yet. `lang` pins the spoken language for this step;
    /// `None` leaves the choice to the implementation.
    fn step(&mut self, lang: Option<&str>) -> anyhow::Result<Option<scheduler::StepOutput>>;

    /// Capture stopped: commit whatever is still pending, and reset so the
    /// same recogniser can serve the next session.
    fn finish(&mut self, lang: Option<&str>) -> anyhow::Result<Option<scheduler::StepOutput>>;
}

/// Voice activity detection over one window of 16 kHz mono audio.
///
/// Like [`AsrEngine`], implementations run on the decode thread.
pub trait SpeechDetector: Send {
    /// Speech regions as (start, end) sample offsets within `samples`,
    /// in ascending order.
    fn speech_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<(usize, usize)>>;
}

/// Sentence-level translator (queued, sequential; never blocks recognition).
pub trait Translator: Send {
    fn translate(
        &mut self,
        sentence: &str,
        source_lang: &str,
        target_lang: &str,
    ) -> anyhow::Result<String>;
}
