//! The seams engine crates implement. The core never names an engine.

use std::time::Duration;

use crate::domain::{LaneId, Lang, Pcm16k, Spoken, StreamMs};

/// One loaded speech model serving every lane. Per-lane state lives inside
/// the engine, so lanes share one model (~1.1 GB for Whisper) without the
/// core holding it behind `Rc<RefCell>`.
///
/// Window-based engines (Whisper) get this from [`crate::asr::Windowed`];
/// natively streaming engines implement it directly.
pub trait AsrEngine: Send {
    /// How often `step` is worth calling: ~1 s for a re-decoding window
    /// engine, ~100 ms for a streaming one.
    fn cadence(&self) -> Duration;
    fn open_lane(&mut self, lane: LaneId);
    /// Audio in capture order.
    fn push(&mut self, lane: LaneId, audio: &Pcm16k);
    /// Advance as far as the pushed audio allows. `Ok(None)`: nothing new.
    /// A language change takes effect here, without reloading the model.
    fn step(&mut self, lane: LaneId, langs: &Spoken) -> anyhow::Result<Option<AsrStep>>;
    /// Commit everything pending and forget the lane.
    fn finish(&mut self, lane: LaneId, langs: &Spoken) -> anyhow::Result<Vec<AsrStep>>;
}

/// One step's outcome for one lane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AsrStep {
    /// Text that became final since the last step.
    pub committed_delta: String,
    /// The current guess after the committed text; replaces the previous one.
    pub volatile: String,
    /// Set on the step that ends an utterance.
    pub utterance: Option<Recognized>,
}

/// An utterance that has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recognized {
    pub text: String,
    pub start: StreamMs,
    pub end: StreamMs,
    /// `None` when detection was unconfident.
    pub lang: Option<Lang>,
}

/// Sentence translator. `&mut self`: one generation at a time, because the
/// llama handle is not thread-safe.
pub trait MtEngine: Send {
    fn translate(&mut self, req: &MtRequest<'_>) -> anyhow::Result<String>;
}

#[derive(Clone, Debug)]
pub struct MtRequest<'a> {
    pub text: &'a str,
    /// `None` when detection was unconfident: do not name a language then, or
    /// the model "corrects" the sentence into it instead of translating.
    pub source: Option<&'a Lang>,
    pub target: &'a Lang,
    /// Recent utterances, oldest first. Formats that cannot use them ignore them.
    pub context: &'a [String],
    /// Correct spellings to keep (proper nouns, product names).
    pub terms: &'a [String],
}
