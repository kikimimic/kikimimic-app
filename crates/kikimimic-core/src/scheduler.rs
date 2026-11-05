//! Drives an [`AsrEngine`] over a rolling audio window and turns raw
//! hypotheses into committed/volatile text via [`LocalAgreement`].

use crate::local_agreement::LocalAgreement;
use crate::resample::TARGET_RATE;
use crate::{AsrEngine, SpeechDetector};

/// Below this much audio a decode is wasted (whisper needs ~1s).
const MIN_WINDOW_SAMPLES: usize = TARGET_RATE as usize;
/// Beyond this the window slides: re-decode cost grows with window length
/// (measured in docs/step0-results.md) and old audio no longer changes.
const MAX_WINDOW_SAMPLES: usize = 15 * TARGET_RATE as usize;
/// This much non-speech (per VAD) at the window tail marks an utterance
/// boundary: the hypothesis is stable there, so the volatile tail can be
/// flushed to committed and the window can slide without losing text.
const TRAILING_NON_SPEECH_SAMPLES: usize = TARGET_RATE as usize;
/// Upper bound on audio carried across a max-window slide. The decode
/// normally reaches within a second or two of the window end; a hypothesis
/// claiming otherwise has garbage timestamps, and carrying it all would
/// keep the window pinned at max size forever.
const MAX_CARRY_SAMPLES: usize = 5 * TARGET_RATE as usize;
/// Tail kept whenever non-speech audio is discarded. A VAD with a minimum
/// speech duration (Silero: 250ms by default) cannot see an utterance that
/// only just began, so dropping the whole window would clip its onset.
const ONSET_GUARD_SAMPLES: usize = 3 * TARGET_RATE as usize / 10;

fn samples_to_ms(samples: usize) -> u64 {
    samples as u64 * 1000 / TARGET_RATE as u64
}

/// An utterance that has ended, with where it sits in the audio.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Utterance {
    pub text: String,
    /// Milliseconds from the start of this scheduler's stream — all the audio
    /// ever pushed into it, not the current window. Callers that record the
    /// same audio can line the two up (`pipeline::LaneTimeline`).
    pub start_ms: u64,
    pub end_ms: u64,
    /// What it was recognised as — pinned by the caller, or detected for the
    /// window it was spoken in. `None` when detection was unconfident, which
    /// leaves the translator to infer it.
    pub lang: Option<String>,
}

/// One decode step's outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepOutput {
    pub committed_delta: String,
    pub volatile: String,
    /// The utterance that just ended; present only on the step that crossed
    /// an utterance boundary (trailing silence or max window). This is the
    /// unit the translation lane consumes.
    pub utterance_final: Option<Utterance>,
}

#[derive(Debug, Default)]
pub struct StreamScheduler {
    /// The current window: audio since the last slide.
    buffer: Vec<f32>,
    /// Where `buffer[0]` sits in the stream, in samples since the first
    /// `push_audio`. Windows come and go; this is what makes a timestamp
    /// mean something outside the window it was measured in.
    window_start: usize,
    /// Stream position where the current utterance's speech began, fixed by
    /// the first hypothesis that produced text for it.
    utterance_start_ms: Option<u64>,
    agreement: LocalAgreement,
    /// Language detected for the current window (auto mode only). Pinning it
    /// keeps hypotheses stable within a window while letting each new window
    /// (usually a new speaker turn) re-detect.
    window_lang: Option<String>,
    /// Text committed so far for the utterance in the current window; emitted
    /// whole as `utterance_final` when the utterance ends.
    utterance_acc: String,
}

impl StreamScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_audio(&mut self, samples: &[f32]) {
        self.buffer.extend_from_slice(samples);
    }

    /// Fix where the current utterance began, from the first hypothesis that
    /// found speech in it. Whisper reports the offset of its first segment,
    /// so an utterance that starts late in a window is stamped where the
    /// speech is, not where the window opened.
    fn note_utterance_start(&mut self, hypothesis: &crate::Hypothesis) {
        if hypothesis.text.is_empty() {
            return;
        }
        let start = self.window_start_ms() + hypothesis.start_ms;
        self.utterance_start_ms.get_or_insert(start);
    }

    /// Slide the window: discard `buffer[..from]`, keep the rest, and start
    /// a fresh agreement context.
    fn slide_keeping(&mut self, from: usize) {
        self.buffer.drain(..from);
        self.window_start += from;
        self.agreement.reset();
        self.window_lang = None;
        self.utterance_acc.clear();
        self.utterance_start_ms = None;
    }

    /// Where the current window begins, in stream milliseconds.
    fn window_start_ms(&self) -> u64 {
        samples_to_ms(self.window_start)
    }

    /// The language to stamp on an utterance ending now: what the caller
    /// pinned, else what this window's detection settled on.
    fn utterance_lang(&self, lang: Option<&str>) -> Option<String> {
        lang.map(str::to_string)
            .or_else(|| self.window_lang.clone())
    }

    /// Commit the agreement's pending tail and take the finished utterance,
    /// which ended at `end_ms` (stream time). Returns (newly committed tail,
    /// whole utterance if any).
    fn finalize_utterance(
        &mut self,
        end_ms: u64,
        lang: Option<&str>,
    ) -> (String, Option<Utterance>) {
        let tail = self.agreement.flush();
        self.utterance_acc.push_str(&tail);
        let full = std::mem::take(&mut self.utterance_acc);
        let full = full.trim();
        // Fallback for text no hypothesis ever pinned a start to: the window
        // it was decoded in is as much as is known.
        let window_start_ms = self.window_start_ms();
        let start_ms = self.utterance_start_ms.take().unwrap_or(window_start_ms);
        let utterance = (!full.is_empty()).then(|| Utterance {
            text: full.to_string(),
            start_ms,
            end_ms,
            lang: lang.map(str::to_string),
        });
        (tail, utterance)
    }

    /// End the stream (capture stopped): decode whatever audio the steps
    /// never reached, commit all pending text, and reset for a fresh
    /// session. Returns None if nothing was pending.
    pub fn finish(
        &mut self,
        engine: &mut dyn AsrEngine,
        vad: &mut dyn SpeechDetector,
        lang: Option<&str>,
    ) -> anyhow::Result<Option<StepOutput>> {
        let mut committed_delta = String::new();
        // The audio ends where it ends: measured before any padding, which
        // would otherwise stretch the last utterance to a decodable length.
        let mut end_ms = self.window_start_ms() + samples_to_ms(self.buffer.len());
        // Audio pushed after the last step (or below MIN_WINDOW entirely)
        // was never transcribed; without this decode, Stop would silently
        // drop the tail of the last utterance.
        if !self.buffer.is_empty() && !vad.speech_segments(&self.buffer)?.is_empty() {
            if self.buffer.len() < MIN_WINDOW_SAMPLES {
                // pad with silence up to a window whisper can decode
                self.buffer.resize(MIN_WINDOW_SAMPLES, 0.0);
            }
            let effective_lang = lang.or(self.window_lang.as_deref());
            let hypothesis = engine.transcribe(&self.buffer, effective_lang)?;
            // Pin from this decode as well: a session stopped inside its
            // first window had no step to pin one, and its last sentence
            // would reach the translator with no language at all.
            if lang.is_none() && self.window_lang.is_none() && !hypothesis.text.is_empty() {
                self.window_lang = hypothesis.lang.clone();
            }
            self.note_utterance_start(&hypothesis);
            // Trust an earlier end (the speech stopped before the audio did)
            // but never a later one: whisper's loose last-segment timestamp
            // routinely claims the padding.
            end_ms = end_ms.min(self.window_start_ms() + hypothesis.end_ms);
            let agreement = self.agreement.feed(&hypothesis.text);
            self.utterance_acc.push_str(&agreement.committed_delta);
            committed_delta.push_str(&agreement.committed_delta);
        }
        let spoken_in = self.utterance_lang(lang);
        let (tail, utterance_final) = self.finalize_utterance(end_ms, spoken_in.as_deref());
        committed_delta.push_str(&tail);
        let all = self.buffer.len();
        self.slide_keeping(all);
        if utterance_final.is_none() {
            return Ok(None);
        }
        Ok(Some(StepOutput {
            committed_delta,
            volatile: String::new(),
            utterance_final,
        }))
    }

    pub fn step(
        &mut self,
        engine: &mut dyn AsrEngine,
        vad: &mut dyn SpeechDetector,
        lang: Option<&str>,
    ) -> anyhow::Result<Option<StepOutput>> {
        if self.buffer.len() < MIN_WINDOW_SAMPLES {
            return Ok(None);
        }
        let window = self.buffer.as_slice();
        let window_len = window.len();
        let speech = vad.speech_segments(window)?;
        let Some(&(_, last_speech_end)) = speech.last() else {
            // No speech at all: drop the audio so silence and non-speech
            // noise never accumulate (or reach whisper). The onset-guard
            // tail survives in case an utterance just began there.
            self.slide_keeping(window_len.saturating_sub(ONSET_GUARD_SAMPLES));
            return Ok(None);
        };

        let effective_lang = lang.or(self.window_lang.as_deref());
        let hypothesis = engine.transcribe(window, effective_lang)?;
        self.note_utterance_start(&hypothesis);
        // Pin only from a hypothesis that produced text: a decode whose
        // segments were all no-speech still reports a (meaningless) language.
        // The engine withholds `lang` when its detection was unconfident,
        // which likewise leaves detection open for the next decode.
        if lang.is_none() && self.window_lang.is_none() && !hypothesis.text.is_empty() {
            self.window_lang = hypothesis.lang.clone();
        }
        let agreement = self.agreement.feed(&hypothesis.text);
        let mut committed_delta = agreement.committed_delta;
        let mut volatile = agreement.volatile;
        self.utterance_acc.push_str(&committed_delta);

        let at_utterance_boundary =
            window_len.saturating_sub(last_speech_end) >= TRAILING_NON_SPEECH_SAMPLES;
        let mut utterance_final = None;
        if at_utterance_boundary || window_len >= MAX_WINDOW_SAMPLES {
            // The hypothesis is as stable as it will get: commit its tail
            // and hand the whole utterance downstream.
            let spoken_in = self.utterance_lang(lang);
            let (tail, full) = self.finalize_utterance(
                self.window_start_ms() + hypothesis.end_ms,
                spoken_in.as_deref(),
            );
            committed_delta.push_str(&tail);
            volatile.clear();
            utterance_final = full;
            if at_utterance_boundary {
                // Everything after the last speech is silence — drop it,
                // except the onset guard (the VAD is blind to an utterance
                // that began in the window's final instants).
                self.slide_keeping(window_len.saturating_sub(ONSET_GUARD_SAMPLES));
            } else {
                // Forced slide mid-speech: the decode may not have reached
                // the window end, and audio past `end_ms` has no text yet.
                // This trusts whisper's notoriously loose end timestamp:
                // too early re-decodes committed audio (duplicated text —
                // the fresh agreement context cannot absorb it), too late
                // discards untranscribed audio. MAX_CARRY_SAMPLES bounds
                // either failure to 5 seconds.
                let end_sample = (hypothesis.end_ms as usize) * (TARGET_RATE as usize) / 1000;
                let keep_from = end_sample
                    .max(window_len.saturating_sub(MAX_CARRY_SAMPLES))
                    .min(window_len);
                self.slide_keeping(keep_from);
            }
        }
        Ok(Some(StepOutput {
            committed_delta,
            volatile,
            utterance_final,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Hypothesis;

    /// One queued hypothesis. `end_ms` of None means "decoded the whole
    /// window".
    struct Line {
        text: String,
        lang: Option<String>,
        start_ms: u64,
        end_ms: Option<u64>,
    }

    /// Scripted engine: returns its queued hypotheses in order and records
    /// call sizes and the lang argument it was given.
    struct FakeEngine {
        script: Vec<Line>,
        received_samples: Vec<usize>,
        received_langs: Vec<Option<String>>,
    }

    impl FakeEngine {
        fn scripted(texts: &[&str]) -> Self {
            Self::scripted_with_langs(&texts.iter().map(|t| (*t, None)).collect::<Vec<_>>())
        }

        fn scripted_with_langs(entries: &[(&str, Option<&str>)]) -> Self {
            Self::new(entries.iter().map(|(t, l)| (*t, *l, None)).collect())
        }

        fn scripted_with_end_ms(entries: &[(&str, Option<u64>)]) -> Self {
            Self::new(entries.iter().map(|(t, e)| (*t, None, *e)).collect())
        }

        /// Hypotheses that claim a stretch of the window rather than all of it.
        fn scripted_with_span(entries: &[(&str, u64, u64)]) -> Self {
            let mut engine = Self::new(
                entries
                    .iter()
                    .map(|(t, _, end)| (*t, None, Some(*end)))
                    .collect(),
            );
            for (line, (_, start, _)) in engine.script.iter_mut().zip(entries.iter().rev()) {
                line.start_ms = *start;
            }
            engine
        }

        fn new(entries: Vec<(&str, Option<&str>, Option<u64>)>) -> Self {
            Self {
                script: entries
                    .iter()
                    .rev()
                    .map(|(t, l, e)| Line {
                        text: t.to_string(),
                        lang: l.map(str::to_string),
                        start_ms: 0,
                        end_ms: *e,
                    })
                    .collect(),
                received_samples: Vec::new(),
                received_langs: Vec::new(),
            }
        }
    }

    impl AsrEngine for FakeEngine {
        fn transcribe(
            &mut self,
            samples: &[f32],
            lang: Option<&str>,
        ) -> anyhow::Result<Hypothesis> {
            self.received_samples.push(samples.len());
            self.received_langs.push(lang.map(str::to_string));
            let line = self.script.pop().expect("script exhausted");
            Ok(Hypothesis {
                text: line.text,
                start_ms: line.start_ms,
                end_ms: line
                    .end_ms
                    .unwrap_or((samples.len() * 1000 / TARGET_RATE as usize) as u64),
                lang: line.lang,
            })
        }
    }

    /// The text of a finished utterance, for the tests that only care about
    /// what was said and not when.
    fn said(utterance: &Option<Utterance>) -> Option<&str> {
        utterance.as_ref().map(|u| u.text.as_str())
    }

    /// Audible fake audio: constant amplitude well above AmplitudeVad's bar.
    fn seconds(n: usize) -> Vec<f32> {
        vec![0.1; TARGET_RATE as usize * n]
    }

    /// Test stand-in for a real VAD: |sample| >= 0.05 counts as speech.
    struct AmplitudeVad;

    impl crate::SpeechDetector for AmplitudeVad {
        fn speech_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<(usize, usize)>> {
            let mut segments = Vec::new();
            let mut start = None;
            for (i, s) in samples.iter().enumerate() {
                if s.abs() >= 0.05 {
                    start.get_or_insert(i);
                } else if let Some(begin) = start.take() {
                    segments.push((begin, i));
                }
            }
            if let Some(begin) = start {
                segments.push((begin, samples.len()));
            }
            Ok(segments)
        }
    }

    #[test]
    fn no_speech_window_is_dropped_except_the_onset_guard_tail() {
        let mut engine = FakeEngine::scripted(&["次"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.0005; 5 * TARGET_RATE as usize]); // noise only
        assert_eq!(sched.step(&mut engine, &mut vad, None).unwrap(), None);
        // the noise was dropped (bar the onset-guard tail): it never
        // accumulates and the next decode is mostly the new speech
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(
            engine.received_samples,
            vec![ONSET_GUARD_SAMPLES + 2 * 16_000]
        );
    }

    /// Amplitude VAD that, like Silero, cannot see speech shorter than a
    /// minimum duration (here 0.5s).
    struct MinDurationVad;

    impl crate::SpeechDetector for MinDurationVad {
        fn speech_segments(&mut self, samples: &[f32]) -> anyhow::Result<Vec<(usize, usize)>> {
            let mut all = AmplitudeVad.speech_segments(samples)?;
            all.retain(|(s, e)| e - s >= TARGET_RATE as usize / 2);
            Ok(all)
        }
    }

    #[test]
    fn dropping_a_no_speech_window_keeps_a_just_started_utterance() {
        let mut engine = FakeEngine::scripted(&["もし"]);
        let mut vad = MinDurationVad;
        let mut sched = StreamScheduler::new();
        // 2s of near-silence, then an utterance begins in the last 200ms —
        // still below the VAD's minimum, so the window reads as "no speech".
        let mut audio = vec![0.0005; 2 * TARGET_RATE as usize];
        audio.extend(vec![0.1; TARGET_RATE as usize / 5]);
        sched.push_audio(&audio);
        assert_eq!(sched.step(&mut engine, &mut vad, None).unwrap(), None);
        // once the utterance continues, its first samples are still there
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(engine.received_samples, vec![ONSET_GUARD_SAMPLES + 16_000]);
    }

    #[test]
    fn auto_mode_pins_detected_language_for_the_window() {
        let mut engine = FakeEngine::scripted_with_langs(&[
            ("Hello", Some("en")),
            ("Hello world", Some("en")),
            ("Hello world", Some("en")),
            ("こんにちは", Some("ja")),
        ]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap(); // first decode: detect
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap(); // pinned to detected lang
        sched.push_audio(&vec![0.0; TARGET_RATE as usize]); // pause → slide
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&seconds(2)); // next speaker, new window
        sched.step(&mut engine, &mut vad, None).unwrap(); // detection restarts
        assert_eq!(
            engine.received_langs,
            vec![None, Some("en".into()), Some("en".into()), None]
        );
    }

    #[test]
    fn an_utterance_reports_the_language_it_was_recognised_in() {
        // The translation lane decides direction from this: a sentence
        // already in the reader's language must not be translated back into
        // it (`language::LanguagePolicy::target_for`).
        let mut engine =
            FakeEngine::scripted_with_langs(&[("Hello", Some("en")), ("Hello world", Some("en"))]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&vec![0.0; TARGET_RATE as usize]); // pause → boundary
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(
            out.utterance_final.unwrap().lang.as_deref(),
            Some("en"),
            "the window's pinned language belongs to the utterance"
        );
    }

    #[test]
    fn a_pinned_language_is_what_the_utterance_reports() {
        // Single-language mode: detection never ran, but the caller knows.
        let mut engine = FakeEngine::scripted(&["こんにちは"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        let out = sched
            .finish(&mut engine, &mut vad, Some("ja"))
            .unwrap()
            .unwrap();
        assert_eq!(out.utterance_final.unwrap().lang.as_deref(), Some("ja"));
    }

    #[test]
    fn the_tail_utterance_reports_the_language_its_own_decode_detected() {
        // Stop during the very first window: no step ever pinned a language,
        // so the finish decode's own detection is all there is. Without it
        // the last sentence of every short session goes out unlabelled.
        let mut engine = FakeEngine::scripted_with_langs(&[("Hello", Some("en"))]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        let out = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(out.utterance_final.unwrap().lang.as_deref(), Some("en"));
    }

    #[test]
    fn auto_mode_does_not_pin_language_from_an_empty_hypothesis() {
        // A decode whose segments were all filtered as no-speech still
        // reports a detected language; pinning it would force e.g. "en"
        // (detected from a cough) onto the whole window.
        let mut engine = FakeEngine::scripted_with_langs(&[
            ("", Some("en")),
            ("こんにちは", Some("ja")),
            ("こんにちは", Some("ja")),
        ]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap();
        // detection stays open until a hypothesis with text pins it
        assert_eq!(engine.received_langs, vec![None, None, Some("ja".into())]);
    }

    #[test]
    fn auto_mode_keeps_detection_open_while_the_engine_withholds_a_language() {
        // The engine reports no language when its detection was not
        // confident (e.g. a coin-flip over the first second of a window).
        // Detection must stay open — re-run each decode — until a
        // confident hypothesis pins it.
        let mut engine = FakeEngine::scripted_with_langs(&[
            ("こんにちは", None),
            ("こんにちは 皆さん", Some("ja")),
            ("こんにちは 皆さん", Some("ja")),
        ]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap();
        sched.push_audio(&seconds(1));
        sched.step(&mut engine, &mut vad, None).unwrap();
        assert_eq!(engine.received_langs, vec![None, None, Some("ja".into())]);
    }

    #[test]
    fn trailing_silence_flushes_volatile_and_slides() {
        let mut engine = FakeEngine::scripted(&["こんにちは"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        let mut audio = vec![0.1; 2 * TARGET_RATE as usize]; // speech
        audio.extend(vec![0.0; TARGET_RATE as usize]); // 1s pause
        sched.push_audio(&audio);
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(out.committed_delta, "こんにちは"); // flushed, not stuck volatile
        assert_eq!(out.volatile, "");
        // window slid past the utterance: the silent remainder is gated
        sched.push_audio(&vec![0.0; TARGET_RATE as usize]);
        assert_eq!(sched.step(&mut engine, &mut vad, None).unwrap(), None);
        assert_eq!(engine.received_samples.len(), 1);
    }

    #[test]
    fn utterance_boundary_reports_the_full_utterance_for_translation() {
        let mut engine =
            FakeEngine::scripted(&["こんにちは、せ", "こんにちは、世界", "こんにちは、世界"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        let first = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(first.utterance_final, None);
        sched.push_audio(&seconds(1));
        let second = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(second.utterance_final, None);
        sched.push_audio(&vec![0.0; TARGET_RATE as usize]); // 1s pause → boundary
        let third = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(third.committed_delta, "世界");
        assert_eq!(third.volatile, "");
        // the whole utterance, in one piece: the translation lane's input
        assert_eq!(said(&third.utterance_final), Some("こんにちは、世界"));
    }

    #[test]
    fn an_utterance_carries_its_position_in_the_stream() {
        let mut engine = FakeEngine::scripted_with_end_ms(&[("こんにちは", Some(2_000))]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        let mut audio = seconds(2);
        audio.extend(vec![0.0; TARGET_RATE as usize]); // 1s pause → boundary
        sched.push_audio(&audio);
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        let utterance = out.utterance_final.unwrap();
        assert_eq!(utterance.text, "こんにちは");
        assert_eq!((utterance.start_ms, utterance.end_ms), (0, 2_000));
    }

    #[test]
    fn positions_keep_counting_across_a_window_slide() {
        // The second utterance's clock must be the stream's, not the fresh
        // window's — otherwise every utterance would claim to start at zero.
        let mut engine = FakeEngine::scripted_with_end_ms(&[
            ("ひとつめ", Some(2_000)),
            ("ふたつめ", Some(2_000)),
        ]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        let mut audio = seconds(2);
        audio.extend(vec![0.0; TARGET_RATE as usize]);
        sched.push_audio(&audio);
        sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        // The slide kept only the onset guard, so the new window starts
        // 3s − 0.3s into the stream.
        sched.push_audio(&audio);
        let second = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        let utterance = second.utterance_final.unwrap();
        let window_start = 3_000 - 300;
        assert_eq!(
            (utterance.start_ms, utterance.end_ms),
            (window_start, window_start + 2_000)
        );
    }

    #[test]
    fn an_utterance_starts_where_the_speech_did_not_where_the_window_did() {
        // Whisper reports the first segment's offset: a window that opens
        // with 2s of noise must not stamp the utterance 2s early.
        let mut engine = FakeEngine::scripted_with_span(&[("はい", 2_000, 3_000)]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        let mut audio = seconds(3);
        audio.extend(vec![0.0; TARGET_RATE as usize]);
        sched.push_audio(&audio);
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        let utterance = out.utterance_final.unwrap();
        assert_eq!((utterance.start_ms, utterance.end_ms), (2_000, 3_000));
    }

    #[test]
    fn finish_stamps_the_tail_utterance() {
        let mut engine = FakeEngine::scripted_with_end_ms(&[("はい", Some(1_500))]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.1; TARGET_RATE as usize * 3 / 2]);
        let fin = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        let utterance = fin.utterance_final.unwrap();
        assert_eq!((utterance.start_ms, utterance.end_ms), (0, 1_500));
    }

    #[test]
    fn finish_does_not_stretch_the_end_into_the_padding() {
        // Half a second of real audio is padded up to a decodable window;
        // whisper's loose end timestamp claims the padding too. The
        // utterance still ends where the captured audio does.
        let mut engine = FakeEngine::scripted_with_end_ms(&[("はい", Some(900))]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.1; TARGET_RATE as usize / 2]);
        let fin = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(fin.utterance_final.unwrap().end_ms, 500);
    }

    #[test]
    fn silent_window_is_not_decoded() {
        let mut engine = FakeEngine::scripted(&[]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.0005; 2 * TARGET_RATE as usize]); // ambient noise level
        let out = sched.step(&mut engine, &mut vad, None).unwrap();
        assert_eq!(out, None);
        assert!(engine.received_samples.is_empty());
    }

    #[test]
    fn consecutive_steps_commit_agreed_prefix() {
        let mut engine = FakeEngine::scripted(&["こんにちは、せ", "こんにちは、世界"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        let first = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(first.committed_delta, "");
        assert_eq!(first.volatile, "こんにちは、せ");
        sched.push_audio(&seconds(1));
        let second = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(second.committed_delta, "こんにちは、");
        assert_eq!(second.volatile, "世界");
        // each step decodes the whole current window
        assert_eq!(engine.received_samples, vec![2 * 16_000, 3 * 16_000]);
    }

    #[test]
    fn window_slides_and_agreement_resets_after_max_window() {
        let mut engine = FakeEngine::scripted(&["長い発話です", "次"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(16));
        // exceeds 15s max → volatile is flushed to committed, then slides
        let flushed = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(flushed.committed_delta, "長い発話です");
        assert_eq!(flushed.volatile, "");
        sched.push_audio(&seconds(2));
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(out.committed_delta, ""); // fresh agreement context
        assert_eq!(out.volatile, "次");
        // the post-slide decode saw only audio pushed after the slide
        assert_eq!(engine.received_samples, vec![16 * 16_000, 2 * 16_000]);
    }

    #[test]
    fn max_window_slide_carries_audio_the_decode_did_not_reach() {
        // 16s of speech, but the hypothesis says the decode only reached
        // 14s: the last 2s were never transcribed and must survive the slide.
        let mut engine =
            FakeEngine::scripted_with_end_ms(&[("長い発話", Some(14_000)), ("続き", None)]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(16));
        let flushed = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(flushed.committed_delta, "長い発話");
        // the carried 2s alone is a decodable window: no audio was lost
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(out.volatile, "続き");
        assert_eq!(engine.received_samples, vec![16 * 16_000, 2 * 16_000]);
    }

    #[test]
    fn finish_flushes_pending_text_as_a_final_utterance() {
        let mut engine = FakeEngine::scripted(&["こんにちは", "こんにちは"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        let out = sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(out.volatile, "こんにちは");
        // stopping capture must not lose the volatile tail
        let fin = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(fin.committed_delta, "こんにちは");
        assert_eq!(fin.volatile, "");
        assert_eq!(said(&fin.utterance_final), Some("こんにちは"));
        // and the scheduler is reset for the next session
        assert_eq!(sched.finish(&mut engine, &mut vad, None).unwrap(), None);
    }

    #[test]
    fn finish_decodes_audio_the_steps_never_reached() {
        let mut engine = FakeEngine::scripted(&["こんにち", "こんにちは"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(2));
        sched.step(&mut engine, &mut vad, None).unwrap().unwrap();
        // one more second arrives, then the user hits Stop before the
        // next step: that audio was never transcribed and must not vanish
        sched.push_audio(&seconds(1));
        let fin = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(said(&fin.utterance_final), Some("こんにちは"));
        assert_eq!(engine.received_samples, vec![2 * 16_000, 3 * 16_000]);
    }

    #[test]
    fn finish_pads_a_short_undecoded_tail_to_a_decodable_window() {
        // Half a second of speech, stopped before it ever reached
        // MIN_WINDOW: finish pads with silence so whisper can decode it.
        let mut engine = FakeEngine::scripted(&["はい"]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.1; TARGET_RATE as usize / 2]);
        assert_eq!(sched.step(&mut engine, &mut vad, None).unwrap(), None);
        let fin = sched.finish(&mut engine, &mut vad, None).unwrap().unwrap();
        assert_eq!(said(&fin.utterance_final), Some("はい"));
        assert_eq!(engine.received_samples, vec![MIN_WINDOW_SAMPLES]);
    }

    #[test]
    fn finish_does_not_decode_a_silent_remainder() {
        // After a boundary slide only the onset-guard silence remains;
        // finish must not wake whisper for it.
        let mut engine = FakeEngine::scripted(&[]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&vec![0.0; 2 * TARGET_RATE as usize]);
        assert_eq!(sched.finish(&mut engine, &mut vad, None).unwrap(), None);
        assert!(engine.received_samples.is_empty());
    }

    #[test]
    fn does_not_decode_below_one_second_of_audio() {
        let mut engine = FakeEngine::scripted(&[]);
        let mut vad = AmplitudeVad;
        let mut sched = StreamScheduler::new();
        sched.push_audio(&seconds(1)[..100]);
        let out = sched.step(&mut engine, &mut vad, None).unwrap();
        assert_eq!(out, None);
        assert!(engine.received_samples.is_empty());
    }
}
