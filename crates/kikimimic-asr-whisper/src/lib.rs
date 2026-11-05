//! [`AsrEngine`] implementation over whisper.cpp (via whisper-rs, Metal).
//!
//! FFI boundary: correctness is verified by running the app and the spike
//! measurements (docs/step0-results.md), not unit tests.
//!
//! Must NOT be linked into the same binary as llama-cpp-2 until the ggml
//! symbol collision is resolved (docs/step0-results.md).

use anyhow::{Context, Result};
use kikimimic_core::{AsrEngine, Hypothesis, SpeechDetector};

/// Segments whisper itself considers likely non-speech (coughs, breaths,
/// keyboard noise) hallucinate text; drop them above this probability.
const NO_SPEECH_THRESHOLD: f32 = 0.6;

/// Below this detection probability the guessed language is used for the
/// current decode but withheld from the hypothesis, so the scheduler keeps
/// re-detecting instead of pinning it for the whole window. The first decode
/// of a window sees as little as 1s of audio, where ja/en detection is close
/// to a coin flip — and whisper forced to the wrong language does not
/// misrecognize, it silently *translates* (Japanese speech comes out as
/// fluent English). Override with KKM_LANG_PIN_THRESHOLD.
const LANG_PIN_THRESHOLD: f32 = 0.6;

pub struct WhisperEngine {
    /// Decode state, created once and reused: whisper_init_state allocates
    /// KV caches and buffers, which is wasteful per window. The state keeps
    /// the model context alive via an internal Arc.
    state: whisper_rs::WhisperState,
    /// When non-empty, auto-detection picks only among these languages
    /// (e.g. the app's Main/Sub pair) instead of whisper's full set.
    allowed_lang_ids: Vec<i32>,
    /// Tuning resolved from the environment once at load time.
    lang_pin_threshold: f32,
}

/// Silero VAD via whisper.cpp. Runs on CPU so it never competes with the
/// whisper decode for GPU time.
///
/// Tuning (all optional, whisper.cpp defaults otherwise):
/// KKM_VAD_THRESHOLD (0..1, default 0.5), KKM_VAD_MIN_SPEECH_MS,
/// KKM_VAD_MIN_SILENCE_MS, KKM_VAD_PAD_MS.
pub struct SileroVad {
    ctx: whisper_rs::WhisperVadContext,
    /// Tuning resolved from the environment once at load time.
    params: whisper_rs::WhisperVadParams,
}

impl SileroVad {
    pub fn load(model_path: &str) -> Result<Self> {
        let mut ctx_params = whisper_rs::WhisperVadContextParams::new();
        ctx_params.set_n_threads(2);
        ctx_params.set_use_gpu(false);
        let ctx = whisper_rs::WhisperVadContext::new(model_path, ctx_params)
            .with_context(|| format!("load VAD model {model_path}"))?;
        let mut params = whisper_rs::WhisperVadParams::new();
        if let Some(v) = env_parse::<f32>("KKM_VAD_THRESHOLD") {
            params.set_threshold(v);
        }
        if let Some(v) = env_parse::<i32>("KKM_VAD_MIN_SPEECH_MS") {
            params.set_min_speech_duration(v);
        }
        if let Some(v) = env_parse::<i32>("KKM_VAD_MIN_SILENCE_MS") {
            params.set_min_silence_duration(v);
        }
        if let Some(v) = env_parse::<i32>("KKM_VAD_PAD_MS") {
            params.set_speech_pad(v);
        }
        Ok(Self { ctx, params })
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.parse().ok()
}

impl SpeechDetector for SileroVad {
    fn speech_segments(&mut self, samples: &[f32]) -> Result<Vec<(usize, usize)>> {
        let segments = self.ctx.segments_from_samples(self.params, samples)?;
        // timestamps are centiseconds → x160 samples at 16 kHz
        Ok(segments
            .map(|seg| {
                (
                    ((seg.start * 160.0) as usize).min(samples.len()),
                    ((seg.end * 160.0) as usize).min(samples.len()),
                )
            })
            .collect())
    }
}

/// Pick the allowed language with the highest detection probability,
/// with that probability so callers can gate pinning on confidence.
fn best_allowed_lang(probs: &[f32], allowed_ids: &[i32]) -> Option<(i32, f32)> {
    allowed_ids
        .iter()
        .filter_map(|&id| probs.get(id as usize).map(|p| (id, *p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

impl WhisperEngine {
    pub fn load(model_path: &str, allowed_langs: &[String]) -> Result<Self> {
        let mut params = whisper_rs::WhisperContextParameters::default();
        params.use_gpu(true);
        let ctx = whisper_rs::WhisperContext::new_with_params(model_path, params)
            .with_context(|| format!("load whisper model {model_path}"))?;
        let state = ctx.create_state().context("create whisper state")?;
        let allowed_lang_ids = to_lang_ids(allowed_langs)?;
        Ok(Self {
            state,
            allowed_lang_ids,
            lang_pin_threshold: env_parse("KKM_LANG_PIN_THRESHOLD").unwrap_or(LANG_PIN_THRESHOLD),
        })
    }

    /// Change which languages detection may choose between, mid-session.
    /// `transcribe` reads this per decode, so no model reload is involved —
    /// which is what lets the language picker work while recording.
    /// An unknown code leaves the set untouched rather than emptying it.
    pub fn set_allowed_langs(&mut self, langs: &[String]) -> Result<()> {
        self.allowed_lang_ids = to_lang_ids(langs)?;
        Ok(())
    }
}

fn to_lang_ids(langs: &[String]) -> Result<Vec<i32>> {
    langs
        .iter()
        .map(|l| whisper_rs::get_lang_id(l).with_context(|| format!("unknown language: {l}")))
        .collect()
}

impl AsrEngine for WhisperEngine {
    fn transcribe(&mut self, samples: &[f32], lang: Option<&str>) -> Result<Hypothesis> {
        let state = &mut self.state;

        // Restricted detection: pick the most probable of the allowed
        // languages, so e.g. a ja/en meeting can never drift into zh/ko.
        // The scheduler pins a confident result per window; an unconfident
        // guess still steers this decode but is withheld from the
        // hypothesis, so detection re-runs as the window grows.
        let detected: Option<(String, f32)> = match lang {
            Some(_) => None,
            None if !self.allowed_lang_ids.is_empty() => {
                state.pcm_to_mel(samples, 4)?;
                let (_, probs) = state.lang_detect(0, 4)?;
                best_allowed_lang(&probs, &self.allowed_lang_ids)
                    .and_then(|(id, p)| whisper_rs::get_lang_str(id).map(|l| (l.to_string(), p)))
            }
            None => None,
        };

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        // None here = whisper's unrestricted auto-detect.
        params.set_language(detected.as_ref().map(|(l, _)| l.as_str()).or(lang));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_nst(true); // no *cough* / event annotations
        state.full(params, samples)?;

        let mut text = String::new();
        let mut start_ms = None;
        let mut end_ms = 0u64;
        for i in 0..state.full_n_segments() {
            if let Some(seg) = state.get_segment(i) {
                if seg.no_speech_probability() > NO_SPEECH_THRESHOLD {
                    continue;
                }
                // whisper timestamps are centiseconds
                start_ms.get_or_insert((seg.start_timestamp().max(0) * 10) as u64);
                end_ms = (seg.end_timestamp().max(0) * 10) as u64;
                text.push_str(&seg.to_str_lossy()?);
            }
        }
        let start_ms = start_ms.unwrap_or(0);
        let confident = detected.is_none_or(|(_, p)| p >= self.lang_pin_threshold);
        let reported = confident
            .then(|| whisper_rs::get_lang_str(state.full_lang_id_from_state()))
            .flatten();
        Ok(Hypothesis {
            text: text.trim().to_string(),
            start_ms,
            end_ms,
            lang: reported.map(str::to_string),
        })
    }
}

/// The loaded models, shared by every lane and outliving a session.
///
/// One copy: the model is most of a gigabyte, and a second lane wanting its
/// own would double the app's memory to recognise the same languages. They
/// are only ever used from the pipeline thread, one lane at a time.
pub struct WhisperModels {
    pub engine: WhisperEngine,
    pub vad: SileroVad,
    /// What the engine was last told detection may choose between. The
    /// models outlive a session while the policy can change between (and
    /// during) sessions, so the two have to be compared rather than assumed.
    pub spoken: Vec<String>,
}

impl WhisperModels {
    pub fn load(asr_model: &str, vad_model: &str, spoken: &[String]) -> Result<Self> {
        Ok(Self {
            engine: WhisperEngine::load(asr_model, spoken)?,
            vad: SileroVad::load(vad_model)?,
            spoken: spoken.to_vec(),
        })
    }
}

/// The Whisper strategy: a rolling window, re-decoded on a cadence, with
/// LocalAgreement deciding what has settled ([`kikimimic_core::scheduler`]).
///
/// The window state is per lane; the models behind it are not. `Rc` rather
/// than `Arc` because both live and die on the pipeline thread — the sharing
/// here is between two lanes, never between two threads.
pub struct WhisperRecognizer {
    models: std::rc::Rc<std::cell::RefCell<WhisperModels>>,
    scheduler: kikimimic_core::scheduler::StreamScheduler,
}

impl WhisperRecognizer {
    pub fn new(models: std::rc::Rc<std::cell::RefCell<WhisperModels>>) -> Self {
        Self {
            models,
            scheduler: kikimimic_core::scheduler::StreamScheduler::new(),
        }
    }
}

impl kikimimic_core::Recognizer for WhisperRecognizer {
    fn push_audio(&mut self, samples: &[f32]) {
        self.scheduler.push_audio(samples);
    }

    fn step(&mut self, lang: Option<&str>) -> Result<Option<kikimimic_core::scheduler::StepOutput>> {
        // Borrowed for the decode only. Nothing else on this thread runs
        // during it, and nothing holds the models across calls.
        let models = &mut *self.models.borrow_mut();
        self.scheduler
            .step(&mut models.engine, &mut models.vad, lang)
    }

    fn finish(&mut self, lang: Option<&str>) -> Result<Option<kikimimic_core::scheduler::StepOutput>> {
        let models = &mut *self.models.borrow_mut();
        self.scheduler
            .finish(&mut models.engine, &mut models.vad, lang)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_highest_probability_among_allowed() {
        let mut probs = vec![0.0; 10];
        probs[1] = 0.9; // highest overall, but not an allowed language
        probs[3] = 0.2;
        probs[7] = 0.5;
        // the winner's probability comes along so callers can gate pinning
        assert_eq!(best_allowed_lang(&probs, &[3, 7]), Some((7, 0.5)));
    }

    #[test]
    fn empty_allowed_set_returns_none() {
        assert_eq!(best_allowed_lang(&[0.1, 0.9], &[]), None);
    }
}
