//! [`Translator`] over the llama.cpp backend in the `kikimimic-mt-llama-dylib`
//! cdylib, reached by `dlopen` rather than by linking.
//!
//! Linking it would defeat the isolation: llama-cpp-2 and whisper-rs each
//! vendor their own ggml, and one image cannot hold both (docs/step0-results.md).
//! So this crate depends only on the C ABI, and the app stays a single process
//! (ADR-153800).
//!
//! FFI boundary: correctness is verified by running the app and the
//! `cdylib-check` spike, not unit tests. What *is* unit-tested is finding the
//! library, which differs between a `cargo run` checkout and a bundled `.app`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

mod prompt;
pub use prompt::PromptFormat;

/// Points at a specific build of the backend, for development and bisecting.
const DYLIB_ENV: &str = "KKM_TRANSLATE_DYLIB";

/// Offload everything to the GPU by default; whisper leaves room and Step 0
/// measured the pair fitting comfortably. `KKM_LLM_GPU_LAYERS=0` forces CPU.
const DEFAULT_GPU_LAYERS: u32 = 1_000_000;

type InitFn = unsafe extern "C" fn(*const c_char, u32) -> *mut c_void;
type GenerateFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type FreeFn = unsafe extern "C" fn(*mut c_char);
type ShutdownFn = unsafe extern "C" fn(*mut c_void);

/// The backend's file name on this platform.
pub const fn dylib_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libkikimimic_mt_llama_dylib.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "kikimimic_mt_llama_dylib.dll"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "libkikimimic_mt_llama_dylib.so"
    }
}

/// Where the backend might be, given where the running executable is,
/// most-specific first.
///
/// A bundled app has it in `Contents/Frameworks`; a `cargo run` checkout has
/// it beside the executable in `target/<profile>`. The sibling profile
/// directories are there because the two are routinely built apart: the
/// backend is slow to compile, so it gets built once in release while the
/// shell is rebuilt in debug all day.
pub fn dylib_candidates(exe: &Path) -> Vec<PathBuf> {
    let name = dylib_name();
    let Some(dir) = exe.parent() else {
        return vec![PathBuf::from(name)];
    };
    let mut candidates = vec![dir.join(name)];
    // .app/Contents/MacOS/<exe> → .app/Contents/Frameworks/<name>
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

/// The backend to load: the override if set, else the first candidate that
/// exists. A missing override is an error rather than a fallback — silently
/// loading a different build than the one named would waste a bisect.
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
                "no {} in {} (build it with `cargo build --release -p kikimimic-mt-llama-dylib`)",
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
    generate: GenerateFn,
    free: FreeFn,
    shutdown: ShutdownFn,
}

pub struct GgmlTranslator {
    handle: *mut c_void,
    api: Api,
    /// Declared last so it drops last: `api`'s pointers point into it.
    _lib: libloading::Library,
}

// The handle is owned by whichever thread holds the struct and is never
// shared: the backend is explicitly not thread-safe per handle, which is why
// translation runs on one queue thread. Send (not Sync) says exactly that.
unsafe impl Send for GgmlTranslator {}

impl GgmlTranslator {
    /// Load the backend and the model. Blocking and slow (gigabytes off
    /// disk): call it off the UI thread, and only once.
    pub fn load(model_path: &Path) -> Result<Self> {
        let dylib = find_dylib()?;
        let gpu_layers = std::env::var("KKM_LLM_GPU_LAYERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_GPU_LAYERS);
        let model = CString::new(model_path.as_os_str().as_encoded_bytes())
            .context("model path contains a NUL byte")?;
        unsafe {
            let lib = libloading::Library::new(&dylib)
                .with_context(|| format!("dlopen {}", dylib.display()))?;
            let init: InitFn = *lib.get(b"kkm_translate_init")?;
            let api = Api {
                generate: *lib.get(b"kkm_generate").context(
                    "the translation backend predates kkm_generate; rebuild it with \
                     `cargo build --release -p kikimimic-mt-llama-dylib`",
                )?,
                free: *lib.get(b"kkm_translate_free")?,
                shutdown: *lib.get(b"kkm_translate_shutdown")?,
            };
            let handle = init(model.as_ptr(), gpu_layers);
            anyhow::ensure!(
                !handle.is_null(),
                "translation backend failed to load {} (see stderr for the reason)",
                model_path.display()
            );
            Ok(Self {
                handle,
                api,
                _lib: lib,
            })
        }
    }
}

impl GgmlTranslator {
    /// Greedy generation from a rendered prompt.
    pub fn generate(&mut self, prompt: &str) -> Result<String> {
        let prompt = CString::new(prompt).context("prompt contains a NUL byte")?;
        unsafe {
            let out = (self.api.generate)(self.handle, prompt.as_ptr());
            anyhow::ensure!(!out.is_null(), "generation failed (see stderr)");
            let text = CStr::from_ptr(out).to_string_lossy().into_owned();
            (self.api.free)(out);
            Ok(text)
        }
    }
}

/// The v3 translator: a GGUF model behind the isolated backend, prompted in
/// its own format.
pub struct LlamaMt {
    backend: GgmlTranslator,
    format: PromptFormat,
}

impl LlamaMt {
    pub fn load(model_path: &Path, format: PromptFormat) -> Result<Self> {
        Ok(Self {
            backend: GgmlTranslator::load(model_path)?,
            format,
        })
    }
}

impl kikimimic_core::engine::MtEngine for LlamaMt {
    fn translate(&mut self, req: &kikimimic_core::engine::MtRequest<'_>) -> Result<String> {
        self.backend.generate(&self.format.render(req))
    }
}

/// The prompt format a catalog entry asks for (`options.prompt`).
fn prompt_format(options: &toml::Table) -> Result<PromptFormat, String> {
    match options.get("prompt") {
        None => Ok(PromptFormat::ChatMl),
        Some(toml::Value::String(name)) => {
            PromptFormat::parse(name).ok_or_else(|| format!("unknown prompt format {name:?}"))
        }
        Some(other) => Err(format!("prompt must be a string, not {other}")),
    }
}

/// For `EngineRegistry::mt("llama", check_options, factory)`.
pub fn check_options(options: &toml::Table) -> Result<(), String> {
    prompt_format(options).map(|_| ())
}

/// Loads a catalog GGUF behind the isolated backend.
pub fn factory(
    req: &kikimimic_core::engine::LoadRequest<'_>,
) -> Result<Box<dyn kikimimic_core::engine::MtEngine>> {
    let format = prompt_format(&req.entry.options).map_err(anyhow::Error::msg)?;
    let path = req
        .files
        .path(kikimimic_core::catalog::ModelKind::Mt)
        .with_context(|| format!("{} has no model file", req.entry.id))?;
    Ok(Box::new(LlamaMt::load(path, format)?))
}

impl Drop for GgmlTranslator {
    fn drop(&mut self) {
        unsafe { (self.api.shutdown)(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_names_the_prompt_format() {
        let opts = |t: &str| t.parse::<toml::Table>().unwrap();
        assert_eq!(
            prompt_format(&opts(r#"prompt = "plamo3""#)),
            Ok(PromptFormat::Plamo3)
        );
        assert_eq!(prompt_format(&opts("")), Ok(PromptFormat::ChatMl));
        assert!(check_options(&opts(r#"prompt = "gemma""#)).is_err());
    }

    #[test]
    fn a_bundled_app_finds_the_backend_in_its_frameworks() {
        let exe = Path::new("/Applications/Kikimimic.app/Contents/MacOS/Kikimimic");
        let candidates = dylib_candidates(exe);
        assert!(
            candidates.contains(&PathBuf::from(format!(
                "/Applications/Kikimimic.app/Contents/Frameworks/{}",
                dylib_name()
            ))),
            "{candidates:?}"
        );
    }

    #[test]
    fn a_checkout_finds_the_backend_beside_the_executable_first() {
        let candidates = dylib_candidates(Path::new("/repo/target/release/kikimimic"));
        assert_eq!(
            candidates[0],
            PathBuf::from(format!("/repo/target/release/{}", dylib_name()))
        );
    }

    #[test]
    fn a_debug_shell_can_use_a_release_built_backend() {
        // The backend takes minutes to compile, so it is built once in
        // release while `tauri dev` rebuilds the shell in debug.
        let candidates = dylib_candidates(Path::new("/repo/target/debug/kikimimic"));
        assert!(
            candidates.contains(&PathBuf::from(format!(
                "/repo/target/release/{}",
                dylib_name()
            ))),
            "{candidates:?}"
        );
    }

    #[test]
    fn the_same_directory_is_never_searched_twice() {
        let candidates = dylib_candidates(Path::new("/repo/target/release/kikimimic"));
        let mut sorted = candidates.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), candidates.len(), "{candidates:?}");
    }
}
