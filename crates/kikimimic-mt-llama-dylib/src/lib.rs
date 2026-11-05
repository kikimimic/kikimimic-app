//! Translation backend isolated in a cdylib.
//!
//! whisper-rs and llama-cpp-2 each vendor their own incompatible ggml;
//! statically linking both into one image merges the C symbols and
//! crashes at runtime (docs/step0-results.md). A cdylib carries a private
//! copy of ggml — macOS two-level namespaces (and per-DLL resolution on
//! Windows) keep it separate from the host's — so the app remains a
//! single process (ADR-153800).
//!
//! C ABI: `kkm_translate_init` once, `kkm_generate` per rendered prompt (or
//! v2's `kkm_translate` per sentence)
//! (blocking; call from a queue thread), `kkm_translate_free` per result,
//! `kkm_translate_shutdown` at the end. Every export catches panics:
//! unwinding out of `extern "C"` aborts the whole process — host app,
//! whisper and all — which would forfeit the isolation this cdylib
//! exists to provide.

use std::ffi::{c_char, CStr, CString};
use std::mem::ManuallyDrop;
use std::num::NonZeroU32;
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::{Context, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};

/// Room for prompt + output; sentences are short, so this is generous.
const N_CTX: u32 = 2048;
/// Hard stop for generation; a translation that long means the model is
/// rambling. The truncation is logged, never silent.
const MAX_OUTPUT_TOKENS: u32 = 512;

pub struct Translator {
    /// Never read after init: held so the backend outlives model and
    /// context (declaration order puts its Drop last).
    #[allow(dead_code)]
    backend: LlamaBackend,
    /// Leaked so `ctx` can borrow it for 'static (LlamaContext<'a> borrows
    /// its model, which a self-referential struct cannot express); reboxed
    /// and freed in Drop.
    model: &'static LlamaModel,
    /// Created once and reused: per-sentence context creation reallocates
    /// the KV cache every call. Cleared between sentences instead.
    ctx: ManuallyDrop<llama_cpp_2::context::LlamaContext<'static>>,
    /// The model's own chat template; None falls back to ChatML.
    template: Option<LlamaChatTemplate>,
}

impl Drop for Translator {
    fn drop(&mut self) {
        // Order matters: the context borrows the model, the model needs
        // the backend. Fields after this body drop in declaration order,
        // so `backend` goes last.
        unsafe {
            ManuallyDrop::drop(&mut self.ctx);
            drop(Box::from_raw(
                self.model as *const LlamaModel as *mut LlamaModel,
            ));
        }
    }
}

/// # Safety
/// `model_path` must be a valid NUL-terminated UTF-8 path.
#[no_mangle]
pub unsafe extern "C" fn kkm_translate_init(
    model_path: *const c_char,
    n_gpu_layers: u32,
) -> *mut Translator {
    let path = CStr::from_ptr(model_path).to_string_lossy().into_owned();
    catch_unwind(AssertUnwindSafe(|| match init(&path, n_gpu_layers) {
        Ok(t) => Box::into_raw(Box::new(t)),
        Err(e) => {
            eprintln!("kikimimic-mt-llama init: {e:#}");
            std::ptr::null_mut()
        }
    }))
    .unwrap_or_else(|_| {
        eprintln!("kikimimic-mt-llama init: panicked");
        std::ptr::null_mut()
    })
}

fn init(path: &str, n_gpu_layers: u32) -> Result<Translator> {
    let backend = LlamaBackend::init()?;
    let params =
        llama_cpp_2::model::params::LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
    let model: &'static LlamaModel = Box::leak(Box::new(
        LlamaModel::load_from_file(&backend, path, &params)
            .with_context(|| format!("load llm model {path}"))?,
    ));
    let ctx_params = llama_cpp_2::context::params::LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX));
    let ctx = match model.new_context(&backend, ctx_params) {
        Ok(ctx) => ctx,
        Err(e) => {
            // reclaim the leak; nothing borrows the model on this path
            unsafe { drop(Box::from_raw(model as *const _ as *mut LlamaModel)) };
            return Err(e).context("create llm context");
        }
    };
    let template = model.chat_template(None).ok();
    if template.is_none() {
        eprintln!("kikimimic-mt-llama init: model has no chat template, using ChatML");
    }
    Ok(Translator {
        backend,
        model,
        ctx: ManuallyDrop::new(ctx),
        template,
    })
}

/// Translate one sentence. Returns a heap CString (free with
/// [`kkm_translate_free`]) or null on error.
///
/// # Safety
/// `handle` must come from [`kkm_translate_init`]; the strings must be
/// valid NUL-terminated UTF-8. Not thread-safe per handle: serialize calls.
#[no_mangle]
pub unsafe extern "C" fn kkm_translate(
    handle: *mut Translator,
    sentence: *const c_char,
    source_lang: *const c_char,
    target_lang: *const c_char,
) -> *mut c_char {
    let Some(translator) = handle.as_mut() else {
        return std::ptr::null_mut();
    };
    let sentence = CStr::from_ptr(sentence).to_string_lossy();
    let source = CStr::from_ptr(source_lang).to_string_lossy();
    let target = CStr::from_ptr(target_lang).to_string_lossy();
    catch_unwind(AssertUnwindSafe(|| {
        match translate(translator, &sentence, &source, &target) {
            Ok(out) => CString::new(out).map_or(std::ptr::null_mut(), CString::into_raw),
            Err(e) => {
                eprintln!("kikimimic-mt-llama: {e:#}");
                std::ptr::null_mut()
            }
        }
    }))
    .unwrap_or_else(|_| {
        eprintln!("kikimimic-mt-llama: panicked");
        std::ptr::null_mut()
    })
}

/// Generate from a prompt the host already rendered (kikimimic-mt-llama's
/// `PromptFormat`). Returns a heap CString (free with
/// [`kkm_translate_free`]) or null on error.
///
/// # Safety
/// As [`kkm_translate`].
#[no_mangle]
pub unsafe extern "C" fn kkm_generate(
    handle: *mut Translator,
    prompt: *const c_char,
) -> *mut c_char {
    let Some(translator) = handle.as_mut() else {
        return std::ptr::null_mut();
    };
    let prompt = CStr::from_ptr(prompt).to_string_lossy();
    catch_unwind(AssertUnwindSafe(|| {
        match generate(translator, &prompt, "rendered prompt") {
            Ok(out) => CString::new(out).map_or(std::ptr::null_mut(), CString::into_raw),
            Err(e) => {
                eprintln!("kikimimic-mt-llama: {e:#}");
                std::ptr::null_mut()
            }
        }
    }))
    .unwrap_or_else(|_| {
        eprintln!("kikimimic-mt-llama: panicked");
        std::ptr::null_mut()
    })
}

/// # Safety
/// `s` must be a pointer returned by [`kkm_translate`] or [`kkm_generate`] (or null).
#[no_mangle]
pub unsafe extern "C" fn kkm_translate_free(s: *mut c_char) {
    if !s.is_null() {
        let _ = catch_unwind(|| drop(CString::from_raw(s)));
    }
}

/// # Safety
/// `handle` must come from [`kkm_translate_init`] (or be null) and must
/// not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn kkm_translate_shutdown(handle: *mut Translator) {
    if !handle.is_null() {
        let _ = catch_unwind(|| drop(Box::from_raw(handle)));
    }
}

/// Render the interpreter prompt with the model's own chat template so a
/// swapped-in GGUF (Step 2's model picker) keeps working; ChatML only as
/// a last resort.
fn build_prompt(t: &Translator, sentence: &str, source: &str, target: &str) -> Result<String> {
    // An empty source means detection was unconfident about this window
    // (kikimimic_core::language). Naming a language we are not sure of is worse
    // than naming none: told the wrong one, the model "corrects" the
    // sentence into it instead of translating what was actually said.
    let from = if source.is_empty() {
        String::new()
    } else {
        format!("from {source} ")
    };
    let system = format!(
        "You are a professional simultaneous interpreter. \
         Translate the user's sentence {from}to {target}. \
         If it is already in {target}, repeat it unchanged. \
         Output only the translation, nothing else."
    );
    if let Some(template) = &t.template {
        let messages = vec![
            LlamaChatMessage::new("system".into(), system.clone())?,
            LlamaChatMessage::new("user".into(), sentence.into())?,
        ];
        match t.model.apply_chat_template(template, &messages, true) {
            Ok(prompt) => return Ok(prompt),
            Err(e) => {
                eprintln!("kikimimic-mt-llama: chat template failed ({e}), falling back to ChatML")
            }
        }
    }
    Ok(format!(
        "<|im_start|>system\n{system}<|im_end|>\n\
         <|im_start|>user\n{sentence}<|im_end|>\n<|im_start|>assistant\n"
    ))
}

fn translate(t: &mut Translator, sentence: &str, source: &str, target: &str) -> Result<String> {
    let prompt = build_prompt(t, sentence, source, target)?;
    generate(t, &prompt, sentence)
}

/// Greedy generation from a fully rendered prompt. `label` only names the
/// request in the truncation log.
fn generate(t: &mut Translator, prompt: &str, label: &str) -> Result<String> {
    let tokens = t.model.str_to_token(prompt, AddBos::Never)?;
    anyhow::ensure!(!tokens.is_empty(), "prompt tokenized to nothing");
    anyhow::ensure!(
        tokens.len() + MAX_OUTPUT_TOKENS as usize <= N_CTX as usize,
        "sentence too long: {} prompt tokens leave no room in a {}-token context",
        tokens.len(),
        N_CTX
    );

    let ctx = &mut *t.ctx;
    ctx.clear_kv_cache();
    let mut batch = llama_cpp_2::llama_batch::LlamaBatch::new(N_CTX as usize, 1);
    let last = tokens.len() - 1;
    for (i, token) in tokens.iter().enumerate() {
        batch.add(*token, i as i32, &[0], i == last)?;
    }
    ctx.decode(&mut batch)?;

    let mut sampler = llama_cpp_2::sampling::LlamaSampler::greedy();
    // Accumulate raw bytes: a UTF-8 sequence can be split across tokens,
    // so per-token string conversion corrupts multi-byte characters.
    let mut out_bytes: Vec<u8> = Vec::new();
    let mut pos = tokens.len() as i32;
    let mut generated = 0u32;
    loop {
        let token = sampler.sample(ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if t.model.is_eog_token(token) {
            break;
        }
        if generated >= MAX_OUTPUT_TOKENS {
            eprintln!(
                "kikimimic-mt-llama: hit the {MAX_OUTPUT_TOKENS}-token output cap, \
                 translation may be truncated: {label}"
            );
            break;
        }
        out_bytes.extend(t.model.token_to_piece_bytes(token, 64, false, None)?);
        generated += 1;
        batch.clear();
        batch.add(token, pos, &[0], true)?;
        ctx.decode(&mut batch)?;
        pos += 1;
    }
    Ok(String::from_utf8_lossy(&out_bytes).trim().to_string())
}
