//! Window-based recognizers (Whisper) as an [`engine::AsrEngine`]: the
//! rolling window, VAD and LocalAgreement live here once, per lane, over one
//! shared model. Natively streaming engines skip this module.

use std::collections::HashMap;
use std::time::Duration;

use crate::domain::{LaneId, Lang, Pcm16k, Spoken, StreamMs};
use crate::engine::{self, AsrStep, Recognized};
use crate::scheduler::{StepOutput, StreamScheduler};

/// A transcription hypothesis for a window of audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Hypothesis {
    pub text: String,
    /// Milliseconds from the start of the *transcribed window* (the samples
    /// passed to [`WindowTranscriber::transcribe`]) — not from capture start. The
    /// scheduler's slide carry-over arithmetic depends on this; a backend
    /// returning stream-absolute times would silently break it.
    pub start_ms: u64,
    /// End of the last decoded segment, window-relative like `start_ms`.
    pub end_ms: u64,
    /// Language the engine detected (or was told) for this window.
    pub lang: Option<String>,
}

/// Voice activity detection over one window of 16 kHz mono audio.
///
/// Implementations run on the ASR thread.
pub trait SpeechDetector: Send {
    /// Speech regions as (start, end) sample offsets within `samples`,
    /// in ascending order.
    fn speech_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<(usize, usize)>>;
}

/// Transcribes one window of 16 kHz mono audio. Times in the returned
/// [`Hypothesis`] are relative to the window.
pub trait WindowTranscriber: Send {
    /// `lang` pins the language for this window when known; otherwise the
    /// engine detects it, choosing only among `allowed`.
    fn transcribe(
        &mut self,
        window: &[f32],
        lang: Option<&str>,
        allowed: &Spoken,
    ) -> anyhow::Result<Hypothesis>;
}

/// Re-decodes a rolling window every second, committing what two decodes
/// agree on.
pub struct Windowed<T, V> {
    transcriber: T,
    vad: V,
    lanes: HashMap<LaneId, StreamScheduler>,
}

impl<T: WindowTranscriber, V: SpeechDetector> Windowed<T, V> {
    pub fn new(transcriber: T, vad: V) -> Self {
        Self {
            transcriber,
            vad,
            lanes: HashMap::new(),
        }
    }
}

/// The scheduler predates [`WindowTranscriber`]; this lets it call one with
/// the allowed languages bound in.
struct Bound<'a, T> {
    transcriber: &'a mut T,
    allowed: &'a Spoken,
}

impl<T: WindowTranscriber> crate::scheduler::Decode for Bound<'_, T> {
    fn transcribe(&mut self, samples: &[f32], lang: Option<&str>) -> anyhow::Result<Hypothesis> {
        self.transcriber.transcribe(samples, lang, self.allowed)
    }
}

fn convert(out: StepOutput) -> AsrStep {
    AsrStep {
        committed_delta: out.committed_delta,
        volatile: out.volatile,
        utterance: out.utterance_final.map(|u| Recognized {
            text: u.text,
            start: StreamMs(u.start_ms),
            end: StreamMs(u.end_ms),
            lang: u.lang.and_then(|l| Lang::parse(&l).ok()),
        }),
    }
}

impl<T: WindowTranscriber, V: SpeechDetector> engine::AsrEngine for Windowed<T, V> {
    fn cadence(&self) -> Duration {
        Duration::from_millis(1000)
    }

    fn open_lane(&mut self, lane: LaneId) {
        self.lanes.insert(lane, StreamScheduler::new());
    }

    fn push(&mut self, lane: LaneId, audio: &Pcm16k) {
        self.lanes
            .entry(lane)
            .or_default()
            .push_audio(audio.as_slice());
    }

    fn step(&mut self, lane: LaneId, langs: &Spoken) -> anyhow::Result<Option<AsrStep>> {
        let Some(scheduler) = self.lanes.get_mut(&lane) else {
            return Ok(None);
        };
        let mut bound = Bound {
            transcriber: &mut self.transcriber,
            allowed: langs,
        };
        let pinned = langs.pinned().map(Lang::as_str);
        Ok(scheduler
            .step(&mut bound, &mut self.vad, pinned)?
            .map(convert))
    }

    fn finish(&mut self, lane: LaneId, langs: &Spoken) -> anyhow::Result<Vec<AsrStep>> {
        let Some(mut scheduler) = self.lanes.remove(&lane) else {
            return Ok(Vec::new());
        };
        let mut bound = Bound {
            transcriber: &mut self.transcriber,
            allowed: langs,
        };
        let pinned = langs.pinned().map(Lang::as_str);
        Ok(scheduler
            .finish(&mut bound, &mut self.vad, pinned)?
            .map(convert)
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AsrEngine as _;

    /// Hears "hello" in any window of speech; records what it was told.
    #[derive(Default)]
    struct Hello {
        calls: Vec<(Option<String>, Vec<Lang>)>,
    }

    impl WindowTranscriber for Hello {
        fn transcribe(
            &mut self,
            window: &[f32],
            lang: Option<&str>,
            allowed: &Spoken,
        ) -> anyhow::Result<Hypothesis> {
            self.calls
                .push((lang.map(str::to_string), allowed.iter().cloned().collect()));
            Ok(Hypothesis {
                text: "hello".into(),
                start_ms: 0,
                end_ms: window.len() as u64 * 1000 / 16_000,
                lang: Some("en".into()),
            })
        }
    }

    struct AllSpeech;
    impl SpeechDetector for AllSpeech {
        fn speech_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<(usize, usize)>> {
            Ok(vec![(0, samples.len())])
        }
    }

    fn second() -> Pcm16k {
        Pcm16k::assume_16k(vec![0.1; 16_000])
    }

    fn two() -> Spoken {
        Spoken::Two(Lang::parse("ja").unwrap(), Lang::parse("en").unwrap())
    }

    #[test]
    fn lanes_share_the_model_but_not_their_windows() {
        let mut w = Windowed::new(Hello::default(), AllSpeech);
        w.open_lane(LaneId(0));
        w.open_lane(LaneId(1));
        w.push(LaneId(0), &second());
        assert!(w.step(LaneId(0), &two()).unwrap().is_some());
        assert!(
            w.step(LaneId(1), &two()).unwrap().is_none(),
            "lane 1 heard nothing"
        );
    }

    #[test]
    fn detection_is_restricted_to_the_spoken_languages() {
        let mut w = Windowed::new(Hello::default(), AllSpeech);
        w.open_lane(LaneId(0));
        w.push(LaneId(0), &second());
        w.step(LaneId(0), &two()).unwrap();
        let (pinned, allowed) = &w.transcriber.calls[0];
        assert_eq!(pinned, &None);
        assert_eq!(
            allowed,
            &[Lang::parse("ja").unwrap(), Lang::parse("en").unwrap()]
        );
    }

    #[test]
    fn finishing_a_lane_commits_what_it_heard_and_forgets_it() {
        let mut w = Windowed::new(Hello::default(), AllSpeech);
        w.open_lane(LaneId(0));
        w.push(LaneId(0), &second());
        let steps = w.finish(LaneId(0), &two()).unwrap();
        let u = steps
            .last()
            .and_then(|s| s.utterance.clone())
            .expect("an utterance");
        assert_eq!(u.text, "hello");
        assert_eq!(u.lang, Some(Lang::parse("en").unwrap()));
        assert!(w.finish(LaneId(0), &two()).unwrap().is_empty());
    }
}
