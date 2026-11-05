//! Haas-ja (FastConformer/TDT, speech.cpp) as a v3 speech engine: a
//! [`WindowTranscriber`] over libspeech, reached by `dlopen`, plus Silero VAD,
//! run per lane by the core's `asr::Windowed`.
//!
//! libspeech vendors its own ggml, so linking it next to whisper-rs would
//! put two ggml copies in one image; `dlopen` keeps them apart. The C API is
//! hand-declared below (speech.h, API major 4). Correctness of the FFI is
//! checked by the ignored real-model test; what is unit-tested is finding the
//! library, which differs between a checkout and a bundled `.app`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kikimimic_asr_whisper::SileroVad;
use kikimimic_core::asr::{Hypothesis, WindowTranscriber, Windowed};
use kikimimic_core::domain::Spoken;
use kikimimic_core::glossary::Keyword;

/// Points at a specific libspeech build, for development and bisecting.
const DYLIB_ENV: &str = "KKM_SPEECH_DYLIB";

/// The model recognizes Japanese only, whatever the app's language pair is.
const LANG: &str = "ja";

const SAMPLE_RATE: c_int = 16_000;
const SPEECH_OK: c_int = 0;
/// `SPEECH_OPT_TIMESTAMPS` from speech.h.
const OPT_TIMESTAMPS: c_int = 8;
/// `SPEECH_OPT_KEYWORDS` from speech.h.
const OPT_KEYWORDS: c_int = 60;

type Status = c_int;
type LoadFn = unsafe extern "C" fn(*const c_char, *const c_void, *mut *mut c_void) -> Status;
type FreeFn = unsafe extern "C" fn(*mut c_void);
type RequestNewFn = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status;
type SetAudioFn = unsafe extern "C" fn(*mut c_void, *const f32, usize, c_int) -> Status;
type SetBoolFn = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> Status;
type SetStringFn = unsafe extern "C" fn(*mut c_void, c_int, *const c_char) -> Status;
type TranscribeFn = unsafe extern "C" fn(*mut c_void) -> Status;
type ResultFn = unsafe extern "C" fn(*const c_void) -> *const c_void;
type TextFn = unsafe extern "C" fn(*const c_void) -> *const c_char;
type CountFn = unsafe extern "C" fn(*const c_void) -> usize;
type SegmentFn =
    unsafe extern "C" fn(*const c_void, usize, *mut f64, *mut f64, *mut *const c_char) -> Status;
type LastErrorFn = unsafe extern "C" fn() -> *const c_char;

/// The libspeech file name for API major 4 on this platform.
pub const fn dylib_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libspeech.4.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "speech.dll"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "libspeech.so.4"
    }
}

/// Where libspeech might be, given where the running executable is,
/// most-specific first: beside the executable, in a bundled app's
/// `Contents/Frameworks`, then the sibling `target/<profile>` directories
/// (the shell is rebuilt in debug while the library is fetched once).
pub fn dylib_candidates(exe: &Path) -> Vec<PathBuf> {
    let name = dylib_name();
    let Some(dir) = exe.parent() else {
        return vec![PathBuf::from(name)];
    };
    let mut candidates = vec![dir.join(name)];
    if let Some(contents) = dir.parent() {
        candidates.push(contents.join("Frameworks").join(name));
        for profile in ["release", "debug"] {
            let sibling = contents.join(profile).join(name);
            if !candidates.contains(&sibling) {
                candidates.push(sibling);
            }
        }
    }
    candidates
}

/// The library to load: the override if set, else the first candidate that
/// exists. A missing override is an error rather than a fallback, so a bisect
/// never silently loads a different build than the one named.
pub fn find_dylib() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(DYLIB_ENV) {
        let path = PathBuf::from(path);
        anyhow::ensure!(
            path.exists(),
            "{DYLIB_ENV} points at {}, which does not exist",
            path.display()
        );
        return Ok(path);
    }
    let exe = std::env::current_exe().context("locate the running executable")?;
    let candidates = dylib_candidates(&exe);
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .with_context(|| {
            format!(
                "no {} in {} (scripts/bundle-mac.sh fetches it, or set {DYLIB_ENV})",
                dylib_name(),
                candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// The exported functions, copied out of the loaded library.
struct Api {
    model_free: FreeFn,
    request_new: RequestNewFn,
    request_free: FreeFn,
    set_audio: SetAudioFn,
    set_bool: SetBoolFn,
    set_string: SetStringFn,
    transcribe: TranscribeFn,
    request_result: ResultFn,
    result_text: TextFn,
    segment_count: CountFn,
    segment: SegmentFn,
    last_error: LastErrorFn,
}

impl Api {
    fn error(&self, what: &str, status: Status) -> anyhow::Error {
        let msg = unsafe {
            let p = (self.last_error)();
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        anyhow::anyhow!("{what} failed (status {status}): {msg}")
    }
}

/// A loaded Haas model. The library is declared last so it drops last: `api`
/// points into it and the model must be freed before it unloads.
pub struct SpeechModel {
    model: *mut c_void,
    /// speech.h's `keywords` value, `None` when the glossary is empty.
    keywords: Option<CString>,
    api: Api,
    _lib: libloading::Library,
}

// The model is owned by whichever thread holds the struct and every request
// is created and freed inside one `&mut self` call, so nothing is shared.
unsafe impl Send for SpeechModel {}

impl SpeechModel {
    /// Slow (hundreds of MB off disk): call off the UI thread, once.
    pub fn load(model_path: &Path) -> Result<Self> {
        let dylib = find_dylib()?;
        let path = CString::new(model_path.as_os_str().as_encoded_bytes())
            .context("model path contains a NUL byte")?;
        unsafe {
            let lib = libloading::Library::new(&dylib)
                .with_context(|| format!("dlopen {}", dylib.display()))?;
            let load: LoadFn = *lib.get(b"speech_model_load")?;
            let api = Api {
                model_free: *lib.get(b"speech_model_free")?,
                request_new: *lib.get(b"speech_request_new")?,
                request_free: *lib.get(b"speech_request_free")?,
                set_audio: *lib.get(b"speech_request_set_audio")?,
                set_bool: *lib.get(b"speech_request_set_bool")?,
                set_string: *lib.get(b"speech_request_set_string")?,
                transcribe: *lib.get(b"speech_transcribe")?,
                request_result: *lib.get(b"speech_request_result")?,
                result_text: *lib.get(b"speech_result_text")?,
                segment_count: *lib.get(b"speech_result_segment_count")?,
                segment: *lib.get(b"speech_result_segment")?,
                last_error: *lib.get(b"speech_last_error")?,
            };
            let mut model = std::ptr::null_mut();
            let status = load(path.as_ptr(), std::ptr::null(), &mut model);
            if status != SPEECH_OK || model.is_null() {
                return Err(api.error(&format!("load {}", model_path.display()), status));
            }
            Ok(Self {
                model,
                keywords: None,
                api,
                _lib: lib,
            })
        }
    }

    /// Text and the window-relative `(start_ms, end_ms)` of its segments.
    fn recognize(&mut self, samples: &[f32]) -> Result<(String, Option<(u64, u64)>)> {
        let api = &self.api;
        // A request carries its audio and result, so each window gets its own.
        let mut req = std::ptr::null_mut();
        let status = unsafe { (api.request_new)(self.model, &mut req) };
        if status != SPEECH_OK {
            return Err(api.error("create request", status));
        }
        let out = unsafe { run(api, req, samples, self.keywords.as_deref()) };
        unsafe { (api.request_free)(req) };
        out
    }
}

/// Everything is copied out before the caller frees the request, which owns
/// the result and its strings.
unsafe fn run(
    api: &Api,
    req: *mut c_void,
    samples: &[f32],
    keywords: Option<&CStr>,
) -> Result<(String, Option<(u64, u64)>)> {
    let status = (api.set_audio)(req, samples.as_ptr(), samples.len(), SAMPLE_RATE);
    if status != SPEECH_OK {
        return Err(api.error("set audio", status));
    }
    let status = match keywords {
        // speech.cpp refuses timestamps with keywords (the keywords rewrite
        // the text after decoding), so the window then has no segment times
        // and the caller falls back to the whole window.
        Some(k) => (api.set_string)(req, OPT_KEYWORDS, k.as_ptr()),
        // Segment times are what lets the scheduler slide its window.
        None => (api.set_bool)(req, OPT_TIMESTAMPS, 1),
    };
    if status != SPEECH_OK {
        return Err(api.error("set keywords or timestamps", status));
    }
    let status = (api.transcribe)(req);
    if status != SPEECH_OK {
        return Err(api.error("transcribe", status));
    }
    let result = (api.request_result)(req);
    anyhow::ensure!(!result.is_null(), "transcribe produced no result");
    let text = (api.result_text)(result);
    let text = if text.is_null() {
        String::new()
    } else {
        CStr::from_ptr(text).to_string_lossy().into_owned()
    };
    let ms = |s: f64| (s.max(0.0) * 1000.0) as u64;
    let (mut start, mut end, mut seg_text) = (0.0, 0.0, std::ptr::null());
    let mut span: Option<(u64, u64)> = None;
    for i in 0..(api.segment_count)(result) {
        if (api.segment)(result, i, &mut start, &mut end, &mut seg_text) == SPEECH_OK {
            span = Some((span.map_or(ms(start), |(s, _)| s), ms(end)));
        }
    }
    Ok((text, span))
}

impl Drop for SpeechModel {
    fn drop(&mut self) {
        unsafe { (self.api.model_free)(self.model) };
    }
}

impl WindowTranscriber for SpeechModel {
    fn transcribe(
        &mut self,
        window: &[f32],
        _lang: Option<&str>,
        _allowed: &Spoken,
    ) -> Result<Hypothesis> {
        let (text, span) = self.recognize(window)?;
        // No segments (silence): span the window so the slide stays sane.
        let (start_ms, end_ms) =
            span.unwrap_or((0, window.len() as u64 * 1000 / SAMPLE_RATE as u64));
        Ok(Hypothesis {
            text: text.trim().to_string(),
            start_ms,
            end_ms,
            lang: Some(LANG.to_string()),
        })
    }

    fn set_keywords(&mut self, keywords: &[Keyword]) {
        let text = keywords_text(keywords);
        self.keywords = (!text.is_empty()).then(|| CString::new(text).expect("no NUL in keywords"));
    }
}

/// speech.h's `keywords` format: one keyword a line, `written=reading,reading`.
/// A keyword or reading holding a separator cannot be written in it, so it is
/// left out rather than turned into a different keyword.
fn keywords_text(keywords: &[Keyword]) -> String {
    let clean =
        |s: &str, extra: char| !s.trim().is_empty() && !s.contains(['\n', '\r', '=', '\0', extra]);
    keywords
        .iter()
        .filter(|k| clean(&k.written, '\n'))
        .map(|k| {
            let readings: Vec<&str> = k
                .readings
                .iter()
                .map(String::as_str)
                .filter(|r| clean(r, ','))
                .collect();
            if readings.is_empty() {
                k.written.clone()
            } else {
                format!("{}={}", k.written, readings.join(","))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The v3 engine: one Haas model and Silero VAD serving every lane.
pub fn load(asr_model: &Path, vad_model: &str) -> Result<Windowed<SpeechModel, SileroVad>> {
    Ok(Windowed::new(
        SpeechModel::load(asr_model)?,
        SileroVad::load(vad_model)?,
    ))
}

/// For `EngineRegistry::asr("speech", check_options, factory)`. The model
/// takes nothing from the catalog yet, so any key is a typo.
pub fn check_options(options: &toml::Table) -> Result<(), String> {
    match options.keys().next() {
        None => Ok(()),
        Some(key) => Err(format!("the speech engine takes no option {key:?}")),
    }
}

/// Loads the catalog's Haas model with the Silero VAD it requires.
pub fn factory(
    req: &kikimimic_core::engine::LoadRequest<'_>,
) -> Result<Box<dyn kikimimic_core::engine::AsrEngine>> {
    use kikimimic_core::catalog::ModelKind;
    let path = |kind| {
        req.files
            .path(kind)
            .with_context(|| format!("{} is missing its {kind:?} file", req.entry.id))
    };
    let vad = path(ModelKind::Vad)?
        .to_str()
        .context("VAD path is not UTF-8")?;
    Ok(Box::new(load(path(ModelKind::Asr)?, vad)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundled_app_finds_libspeech_in_its_frameworks() {
        let exe = Path::new("/Applications/Kikimimic.app/Contents/MacOS/Kikimimic");
        assert!(dylib_candidates(exe).contains(&PathBuf::from(format!(
            "/Applications/Kikimimic.app/Contents/Frameworks/{}",
            dylib_name()
        ))));
    }

    #[test]
    fn a_debug_shell_can_use_a_release_fetched_library() {
        let candidates = dylib_candidates(Path::new("/repo/target/debug/kikimimic"));
        assert_eq!(
            candidates[0],
            PathBuf::from(format!("/repo/target/debug/{}", dylib_name()))
        );
        assert!(candidates.contains(&PathBuf::from(format!(
            "/repo/target/release/{}",
            dylib_name()
        ))));
    }

    #[test]
    fn the_same_directory_is_never_searched_twice() {
        let candidates = dylib_candidates(Path::new("/repo/target/release/kikimimic"));
        let mut sorted = candidates.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), candidates.len(), "{candidates:?}");
    }

    #[test]
    fn keywords_are_one_a_line_with_their_readings() {
        let kw = |written: &str, readings: &[&str]| Keyword {
            written: written.into(),
            readings: readings.iter().map(|r| r.to_string()).collect(),
        };
        assert_eq!(
            keywords_text(&[
                kw("Gemini", &["ジェミナイ", "ジェミニ"]),
                kw("AIモデル", &[]),
            ]),
            "Gemini=ジェミナイ,ジェミニ\nAIモデル"
        );
    }

    #[test]
    fn keywords_that_would_break_the_format_are_left_out() {
        let kw = |written: &str, readings: &[&str]| Keyword {
            written: written.into(),
            readings: readings.iter().map(|r| r.to_string()).collect(),
        };
        assert_eq!(
            keywords_text(&[
                kw("a=b", &["x"]),
                kw("two\nlines", &[]),
                kw("Kikimimic", &["きき,耳", "きき耳ミック", "x=y"]),
            ]),
            "Kikimimic=きき耳ミック"
        );
    }

    #[test]
    fn options_are_empty_or_rejected() {
        let opts = |t: &str| t.parse::<toml::Table>().unwrap();
        assert!(check_options(&opts("")).is_ok());
        assert!(check_options(&opts(r#"prompt = "x""#)).is_err());
    }
}
