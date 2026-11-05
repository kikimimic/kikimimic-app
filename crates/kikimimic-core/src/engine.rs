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

    /// Words to listen for, from the user's glossary. Replaces the previous
    /// list; engines that cannot listen for words ignore it.
    fn set_keywords(&mut self, _keywords: &[crate::glossary::Keyword]) {}

    /// Recognize one short clip on its own (re-recognizing a diarized turn).
    /// The default runs it through a scratch lane.
    fn transcribe_clip(
        &mut self,
        audio: &Pcm16k,
        langs: &Spoken,
    ) -> anyhow::Result<Vec<Recognized>> {
        let lane = LaneId(u8::MAX);
        self.open_lane(lane);
        self.push(lane, audio);
        Ok(self
            .finish(lane, langs)?
            .into_iter()
            .filter_map(|s| s.utterance)
            .collect())
    }
}

/// Finds who spoke when in a finished recording.
pub trait Diarizer: Send {
    /// Write the speaker turns of `mix_wav` to `rttm` (NIST RTTM, speakers
    /// named `speaker_<n>`).
    fn diarize(&mut self, mix_wav: &std::path::Path, rttm: &std::path::Path) -> anyhow::Result<()>;
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

/// What a factory gets to load a model: its catalog entry and the paths of
/// its files and its requirements' files.
pub struct LoadRequest<'a> {
    pub entry: &'a crate::catalog::ModelEntry,
    pub files: &'a crate::store::ResolvedFiles,
}

pub type AsrFactory = fn(&LoadRequest<'_>) -> anyhow::Result<Box<dyn AsrEngine>>;
pub type MtFactory = fn(&LoadRequest<'_>) -> anyhow::Result<Box<dyn MtEngine>>;
/// Checks a catalog entry's `options` for an engine.
pub type OptionsCheck = fn(&toml::Table) -> Result<(), String>;

/// Engine crates by the name the catalog uses. The app fills it; the core
/// never names an engine.
#[derive(Default, Clone)]
pub struct EngineRegistry {
    asr: std::collections::HashMap<String, (OptionsCheck, AsrFactory)>,
    mt: std::collections::HashMap<String, (OptionsCheck, MtFactory)>,
}

impl EngineRegistry {
    pub fn asr(&mut self, engine: &str, check: OptionsCheck, load: AsrFactory) -> &mut Self {
        self.asr.insert(engine.into(), (check, load));
        self
    }

    pub fn mt(&mut self, engine: &str, check: OptionsCheck, load: MtFactory) -> &mut Self {
        self.mt.insert(engine.into(), (check, load));
        self
    }

    /// Every recognition and translation model in `catalog` names a
    /// registered engine that accepts its options. Run it as a test in the
    /// app, so a catalog entry without an engine fails the build.
    pub fn check(&self, catalog: &crate::catalog::Catalog) -> Result<(), String> {
        use crate::catalog::ModelKind;
        for m in catalog.models() {
            let check = match m.kind {
                ModelKind::Asr => self.asr.get(&m.engine).map(|(c, _)| c),
                ModelKind::Mt => self.mt.get(&m.engine).map(|(c, _)| c),
                ModelKind::Vad => continue,
            };
            let check = check.ok_or_else(|| format!("{}: no engine named {}", m.id, m.engine))?;
            check(&m.options).map_err(|e| format!("{}: {e}", m.id))?;
        }
        Ok(())
    }

    pub fn load_asr(&self, req: &LoadRequest<'_>) -> anyhow::Result<Box<dyn AsrEngine>> {
        let (_, load) = self
            .asr
            .get(&req.entry.engine)
            .ok_or_else(|| anyhow::anyhow!("no speech engine named {}", req.entry.engine))?;
        load(req)
    }

    pub fn load_mt(&self, req: &LoadRequest<'_>) -> anyhow::Result<Box<dyn MtEngine>> {
        let (_, load) = self
            .mt
            .get(&req.entry.engine)
            .ok_or_else(|| anyhow::anyhow!("no translation engine named {}", req.entry.engine))?;
        load(req)
    }
}
