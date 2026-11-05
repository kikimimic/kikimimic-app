//! The speech recognizer built into macOS (SpeechAnalyzer, macOS 26 and
//! later) as an [`AsrEngine`]. It streams natively, so it skips
//! [`kikimimic_core::asr::Windowed`]; the OS owns the model and its download.

mod steps;

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::NonNull;
use std::time::Duration;

use anyhow::{anyhow, Result};
use kikimimic_core::domain::{LaneId, Lang, Pcm16k, Spoken};
use kikimimic_core::engine::{AsrEngine, AsrStep, LoadRequest};

use steps::{Heard, Pending};

type HeardFn = extern "C" fn(*mut c_void, bool, *const c_char, i64, i64);

extern "C" {
    fn kkm_speech_available() -> bool;
    fn kkm_speech_string_free(s: *mut c_char);
    fn kkm_speech_open(locale: *const c_char, error: *mut *mut c_char) -> *mut c_void;
    fn kkm_speech_push(h: *mut c_void, samples: *const f32, count: usize);
    fn kkm_speech_drain(h: *mut c_void, ctx: *mut c_void, cb: HeardFn) -> *mut c_char;
    fn kkm_speech_finish(h: *mut c_void) -> *mut c_char;
    fn kkm_speech_close(h: *mut c_void, finished: bool);
}

/// Whether this Mac has the recognizer (macOS 26 or later).
pub fn is_available() -> bool {
    unsafe { kkm_speech_available() }
}

/// Takes an error string the bridge allocated.
fn take_error(s: *mut c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    unsafe { kkm_speech_string_free(s) };
    Some(text)
}

/// One SpeechAnalyzer listening in one language.
struct Analyzer {
    handle: NonNull<c_void>,
    lang: Lang,
    /// Where its timeline starts on the lane's.
    offset_ms: u64,
    finished: bool,
}

// The handle is only used from the thread that owns the engine.
unsafe impl Send for Analyzer {}

impl Analyzer {
    fn open(lang: &Lang, offset_ms: u64) -> Result<Self> {
        let locale = CString::new(lang.as_str())?;
        let mut error = std::ptr::null_mut();
        let handle = unsafe { kkm_speech_open(locale.as_ptr(), &mut error) };
        let handle =
            NonNull::new(handle).ok_or_else(|| anyhow!(take_error(error).unwrap_or_default()))?;
        Ok(Self {
            handle,
            lang: lang.clone(),
            offset_ms,
            finished: false,
        })
    }

    fn push(&mut self, samples: &[f32]) {
        unsafe { kkm_speech_push(self.handle.as_ptr(), samples.as_ptr(), samples.len()) }
    }

    fn drain(&mut self) -> Result<Vec<Heard>> {
        extern "C" fn collect(
            ctx: *mut c_void,
            fin: bool,
            text: *const c_char,
            start: i64,
            end: i64,
        ) {
            let (out, offset) = unsafe { &mut *(ctx as *mut (Vec<Heard>, u64)) };
            let text = unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned();
            let ms = |t: i64| t.max(0) as u64 + *offset;
            out.push(if fin {
                Heard::Final {
                    text,
                    start_ms: ms(start),
                    end_ms: ms(end),
                }
            } else {
                Heard::Volatile(text)
            });
        }
        let mut ctx = (Vec::new(), self.offset_ms);
        let error = unsafe {
            kkm_speech_drain(
                self.handle.as_ptr(),
                &mut ctx as *mut _ as *mut c_void,
                collect,
            )
        };
        match take_error(error) {
            Some(e) => Err(anyhow!("macOS speech recognition: {e}")),
            None => Ok(ctx.0),
        }
    }

    /// Finalizes everything pushed and returns what it heard.
    fn finish(mut self) -> Result<Vec<Heard>> {
        if let Some(e) = take_error(unsafe { kkm_speech_finish(self.handle.as_ptr()) }) {
            return Err(anyhow!("macOS speech recognition: {e}"));
        }
        self.finished = true;
        self.drain()
    }
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        unsafe { kkm_speech_close(self.handle.as_ptr(), self.finished) }
    }
}

#[derive(Default)]
struct Lane {
    analyzer: Option<Analyzer>,
    /// Audio pushed before the first step, which says the language.
    waiting: Vec<f32>,
    samples: u64,
    pending: Pending,
}

impl Lane {
    /// The recognizer listens in one language: the first spoken one. With two,
    /// what it hears is not detected, so it is reported as unconfident.
    fn listen(&mut self, langs: &Spoken) -> Result<()> {
        let lang = langs.iter().next().expect("at least one language");
        let reported = langs.pinned().cloned();
        if let Some(old) = self.analyzer.take_if(|a| &a.lang != lang) {
            for heard in old.finish()? {
                self.pending.hear(heard, reported.as_ref());
            }
        }
        if self.analyzer.is_none() {
            let start_ms = (self.samples - self.waiting.len() as u64) * 1000 / 16_000;
            let mut analyzer = Analyzer::open(lang, start_ms)?;
            analyzer.push(&std::mem::take(&mut self.waiting));
            self.analyzer = Some(analyzer);
        }
        Ok(())
    }

    fn collect(&mut self, langs: &Spoken, heard: Vec<Heard>) {
        let reported = langs.pinned();
        for h in heard {
            self.pending.hear(h, reported);
        }
    }
}

/// Every lane gets its own analyzer; the OS shares the model between them.
#[derive(Default)]
pub struct AppleSpeech {
    lanes: HashMap<LaneId, Lane>,
}

impl AsrEngine for AppleSpeech {
    fn cadence(&self) -> Duration {
        Duration::from_millis(200)
    }

    fn open_lane(&mut self, lane: LaneId) {
        self.lanes.insert(lane, Lane::default());
    }

    fn push(&mut self, lane: LaneId, audio: &Pcm16k) {
        let lane = self.lanes.entry(lane).or_default();
        let samples = audio.as_slice();
        lane.samples += samples.len() as u64;
        match &mut lane.analyzer {
            Some(a) => a.push(samples),
            None => lane.waiting.extend_from_slice(samples),
        }
    }

    fn step(&mut self, lane: LaneId, langs: &Spoken) -> Result<Option<AsrStep>> {
        let Some(lane) = self.lanes.get_mut(&lane) else {
            return Ok(None);
        };
        lane.listen(langs)?;
        let heard = lane.analyzer.as_mut().expect("listening").drain()?;
        lane.collect(langs, heard);
        Ok(lane.pending.next_step())
    }

    fn finish(&mut self, lane: LaneId, langs: &Spoken) -> Result<Vec<AsrStep>> {
        let Some(mut lane) = self.lanes.remove(&lane) else {
            return Ok(Vec::new());
        };
        if lane.analyzer.is_some() || !lane.waiting.is_empty() {
            lane.listen(langs)?;
            let heard = lane.analyzer.take().expect("listening").finish()?;
            lane.collect(langs, heard);
        }
        Ok(lane.pending.drain())
    }
}

/// For `EngineRegistry::asr("apple-speech", check_options, factory)`.
pub fn check_options(_options: &toml::Table) -> Result<(), String> {
    Ok(())
}

pub fn factory(_req: &LoadRequest<'_>) -> Result<Box<dyn AsrEngine>> {
    if !is_available() {
        return Err(anyhow!("macOS speech recognition needs macOS 26 or later"));
    }
    Ok(Box::new(AppleSpeech::default()))
}
