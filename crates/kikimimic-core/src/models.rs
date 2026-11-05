//! Finding the model files, and landing a download where the finder looks.
//!
//! Three models are loaded at runtime — the recogniser, the VAD, and the
//! translator — and each was resolved its own way while the pipeline grew
//! (an env var here, a build-time path there). This is the one place that
//! decides. A download writes into the first search directory; it does not
//! add another special case.
//!
//! Downloads are not loaded here. [`install_model_file`] only moves bytes
//! onto disk, and only the finished file is visible to [`ModelManager`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Suffix of an in-progress download. The resolver looks up
/// [`Model::default_file`] exactly, so `{file}.partial` is never ready, and a
/// failed download must not be renamed onto the real name.
pub const DOWNLOAD_PARTIAL_SUFFIX: &str = ".partial";

/// The default file name for a model the app ships with a choice of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// The speech recogniser (whisper GGML).
    Asr,
    /// Silero VAD, in whisper.cpp's GGML packaging.
    Vad,
    /// The translation LLM (GGUF).
    Translator,
}

impl Model {
    /// The environment variable that overrides this model outright, with a
    /// full path. Kept from the pipeline's env-var era: it is how a
    /// measurement run swaps in a different model size.
    pub fn env_var(self) -> &'static str {
        match self {
            Model::Asr => "KKM_WHISPER_MODEL",
            Model::Vad => "KKM_VAD_MODEL",
            Model::Translator => "KKM_LLM_MODEL",
        }
    }

    /// The file looked for in each search directory.
    pub fn default_file(self) -> &'static str {
        match self {
            Model::Asr => "ggml-large-v3-turbo-q8_0.bin",
            Model::Vad => "ggml-silero-v5.1.2.bin",
            Model::Translator => "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        }
    }
}

/// Resolves a model to a path that exists, searching the given directories in
/// order. The first directory is the one a download would land in; the last is
/// the checkout's `models/`, which is what makes `cargo run` work.
pub struct ModelManager {
    dirs: Vec<PathBuf>,
}

impl ModelManager {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self { dirs }
    }

    /// Where `model` is, or an error naming every place that was looked at —
    /// "model not found" without the list is a support ticket.
    ///
    /// An empty file is not ready: a download that died before the rename
    /// must not satisfy this, and neither does a zero-byte placeholder.
    pub fn resolve(&self, model: Model) -> anyhow::Result<PathBuf> {
        self.resolve_with(model, |name| std::env::var(name).ok(), file_is_ready)
    }

    /// The decision itself, with the environment and the filesystem injected
    /// so it can be tested without either.
    fn resolve_with(
        &self,
        model: Model,
        env: impl Fn(&str) -> Option<String>,
        exists: impl Fn(&Path) -> bool,
    ) -> anyhow::Result<PathBuf> {
        // An explicit override is used as given: pointing it at a missing
        // file is a mistake worth reporting, not a reason to quietly load a
        // different model than the one that was asked for.
        if let Some(path) = env(model.env_var()) {
            let path = PathBuf::from(path);
            anyhow::ensure!(
                exists(&path),
                "{} points at {}, which does not exist",
                model.env_var(),
                path.display()
            );
            return Ok(path);
        }
        let file = model.default_file();
        for dir in &self.dirs {
            let candidate = dir.join(file);
            if exists(&candidate) {
                return Ok(candidate);
            }
        }
        anyhow::bail!(
            "no {file} in {} (set {} to use another path)",
            self.dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            model.env_var()
        )
    }
}

/// A non-empty file. Directories and zero-byte placeholders are not models.
fn file_is_ready(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() > 0)
        .unwrap_or(false)
}

/// Stream `reader` into `models_dir` / `file_name`.
///
/// Bytes are written to `{file_name}.partial` in that directory (created
/// here, when the write starts). The partial is renamed onto `file_name`
/// only after the read ends, the body is non-empty, and — when `total` is
/// known — the counts match. A failure deletes the partial and does not
/// touch an existing `file_name`, so the resolver cannot observe a truncated
/// download.
pub fn install_model_file(
    models_dir: &Path,
    file_name: &str,
    mut reader: impl Read,
    total: Option<u64>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        valid_model_file_name(file_name),
        "refusing to install {file_name:?}"
    );
    if total == Some(0) {
        anyhow::bail!("download was empty");
    }
    std::fs::create_dir_all(models_dir)
        .with_context(|| format!("create {}", models_dir.display()))?;
    let dest = models_dir.join(file_name);
    let partial = models_dir.join(format!("{file_name}{DOWNLOAD_PARTIAL_SUFFIX}"));
    let written = (|| -> anyhow::Result<u64> {
        let mut file = std::fs::File::create(&partial)
            .with_context(|| format!("create {}", partial.display()))?;
        let mut buf = [0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let n = reader.read(&mut buf).context("read download")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("write download")?;
            received += n as u64;
            progress(received, total);
        }
        file.flush().context("flush download")?;
        file.sync_all().context("sync download")?;
        drop(file);
        if received == 0 {
            anyhow::bail!("download was empty");
        }
        if let Some(expected) = total {
            anyhow::ensure!(
                received == expected,
                "download stopped at {received} of {expected} bytes"
            );
        }
        Ok(received)
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(err);
    }
    if let Err(err) = replace_file(&partial, &dest) {
        let _ = std::fs::remove_file(&partial);
        return Err(err)
            .with_context(|| format!("move {} onto {}", partial.display(), dest.display()));
    }
    Ok(dest)
}

fn valid_model_file_name(file_name: &str) -> bool {
    !file_name.is_empty()
        && file_name != "."
        && file_name != ".."
        && !file_name.ends_with(DOWNLOAD_PARTIAL_SUFFIX)
        && !file_name.contains('/')
        && !file_name.contains('\\')
        && !file_name.contains('\0')
}

/// `rename` replaces on Unix. On Windows it does not, so move the previous
/// file aside and put it back if the new name cannot be taken.
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if cfg!(windows) && to.exists() {
        let backup = to.with_extension("replacing");
        std::fs::rename(to, &backup)?;
        if let Err(err) = std::fs::rename(from, to) {
            let _ = std::fs::rename(&backup, to);
            return Err(err);
        }
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    }
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> ModelManager {
        ModelManager::new(vec![PathBuf::from("/data"), PathBuf::from("/repo/models")])
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_first_directory_that_has_the_file_wins() {
        let found = manager()
            .resolve_with(Model::Asr, no_env, |p| p.starts_with("/repo"))
            .unwrap();
        assert_eq!(
            found,
            PathBuf::from("/repo/models/ggml-large-v3-turbo-q8_0.bin")
        );
    }

    #[test]
    fn a_downloaded_model_shadows_the_checkouts_copy() {
        let found = manager()
            .resolve_with(Model::Vad, no_env, |_| true)
            .unwrap();
        assert_eq!(found, PathBuf::from("/data/ggml-silero-v5.1.2.bin"));
    }

    #[test]
    fn an_env_override_wins_over_every_directory() {
        let found = manager()
            .resolve_with(
                Model::Translator,
                |name| (name == "KKM_LLM_MODEL").then(|| "/tmp/other.gguf".to_string()),
                |_| true,
            )
            .unwrap();
        assert_eq!(found, PathBuf::from("/tmp/other.gguf"));
    }

    #[test]
    fn an_override_pointing_nowhere_is_an_error_not_a_fallback() {
        // Loading a different model than the one that was named would make a
        // measurement run silently meaningless.
        let err = manager()
            .resolve_with(
                Model::Asr,
                |_| Some("/tmp/missing.bin".to_string()),
                |p| !p.starts_with("/tmp"),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("KKM_WHISPER_MODEL"), "{err}");
        assert!(err.contains("/tmp/missing.bin"), "{err}");
    }

    #[test]
    fn a_missing_model_names_everywhere_it_was_looked_for() {
        let err = manager()
            .resolve_with(Model::Asr, no_env, |_| false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/data"), "{err}");
        assert!(err.contains("/repo/models"), "{err}");
        assert!(err.contains("ggml-large-v3-turbo-q8_0.bin"), "{err}");
    }

    fn scratch(label: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kkm-models-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_is_not_ready_and_a_present_file_is() {
        let dir = scratch("ready");
        let file = dir.join(Model::Vad.default_file());
        let manager = ModelManager::new(vec![dir]);
        let missing = manager
            .resolve_with(Model::Vad, no_env, file_is_ready)
            .unwrap_err()
            .to_string();
        assert!(missing.contains(Model::Vad.default_file()), "{missing}");

        std::fs::write(&file, b"vad-weights").unwrap();
        let found = manager
            .resolve_with(Model::Vad, no_env, file_is_ready)
            .unwrap();
        assert_eq!(found, file);
    }

    #[test]
    fn an_empty_file_is_not_ready() {
        let dir = scratch("empty");
        let file = dir.join(Model::Asr.default_file());
        std::fs::write(&file, b"").unwrap();
        assert!(!file_is_ready(&file));
        let err = ModelManager::new(vec![dir])
            .resolve_with(Model::Asr, no_env, file_is_ready)
            .unwrap_err()
            .to_string();
        assert!(err.contains(Model::Asr.default_file()), "{err}");
    }

    #[test]
    fn a_partial_download_is_not_treated_as_ready() {
        let dir = scratch("partial-sibling");
        let partial = dir.join(format!(
            "{}{DOWNLOAD_PARTIAL_SUFFIX}",
            Model::Asr.default_file()
        ));
        std::fs::write(&partial, b"truncated-weights").unwrap();
        let err = ModelManager::new(vec![dir.clone()])
            .resolve_with(Model::Asr, no_env, file_is_ready)
            .unwrap_err()
            .to_string();
        assert!(err.contains(Model::Asr.default_file()), "{err}");
        assert!(file_is_ready(&partial));
        assert!(!dir.join(Model::Asr.default_file()).exists());
    }

    /// A reader that fails after `ok_bytes`, the way a dropped connection does.
    struct Cut {
        ok_bytes: usize,
        sent: usize,
    }

    impl std::io::Read for Cut {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.sent >= self.ok_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection dropped",
                ));
            }
            let n = buf.len().min(self.ok_bytes - self.sent).min(8);
            buf[..n].fill(b'x');
            self.sent += n;
            Ok(n)
        }
    }

    #[test]
    fn a_finished_download_is_ready_and_leaves_no_partial() {
        let dir = scratch("install-ok");
        let name = Model::Vad.default_file();
        let mut saw_partial_unresolved = false;
        let dest = install_model_file(
            &dir,
            name,
            &b"vad-weights"[..],
            Some(11),
            |received, total| {
                let partial = dir.join(format!("{name}{DOWNLOAD_PARTIAL_SUFFIX}"));
                let final_path = dir.join(name);
                // While bytes are still arriving, the name the resolver uses
                // must not exist. The sibling partial must not satisfy it.
                if !saw_partial_unresolved {
                    assert!(partial.exists());
                    assert!(!final_path.exists());
                    let err = ModelManager::new(vec![dir.clone()])
                        .resolve_with(Model::Vad, no_env, file_is_ready)
                        .unwrap_err()
                        .to_string();
                    assert!(err.contains(name), "{err}");
                    saw_partial_unresolved = true;
                }
                assert_eq!(total, Some(11));
                assert!(received > 0);
            },
        )
        .unwrap();
        assert!(saw_partial_unresolved);
        assert_eq!(dest, dir.join(name));
        assert_eq!(std::fs::read(&dest).unwrap(), b"vad-weights");
        assert!(!dir
            .join(format!("{name}{DOWNLOAD_PARTIAL_SUFFIX}"))
            .exists());
        assert_eq!(
            ModelManager::new(vec![dir])
                .resolve_with(Model::Vad, no_env, file_is_ready)
                .unwrap(),
            dest
        );
    }

    #[test]
    fn a_dropped_download_leaves_nothing_the_resolver_can_use() {
        let dir = scratch("install-drop");
        let name = Model::Translator.default_file();
        // A previous good file must survive a failed replacement attempt's
        // sibling, and must not be confused with the partial.
        std::fs::write(dir.join(name), b"previous-good").unwrap();
        let err = install_model_file(
            &dir,
            name,
            Cut {
                ok_bytes: 32,
                sent: 0,
            },
            Some(100),
            |_, _| {},
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("connection dropped") || err.contains("read download"),
            "{err}"
        );
        assert_eq!(std::fs::read(dir.join(name)).unwrap(), b"previous-good");
        assert!(!dir
            .join(format!("{name}{DOWNLOAD_PARTIAL_SUFFIX}"))
            .exists());
    }

    #[test]
    fn a_short_body_is_not_renamed_into_place() {
        let dir = scratch("install-short");
        let name = Model::Asr.default_file();
        let err = install_model_file(&dir, name, &b"short"[..], Some(64), |_, _| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("64"), "{err}");
        assert!(!dir.join(name).exists());
        assert!(!dir
            .join(format!("{name}{DOWNLOAD_PARTIAL_SUFFIX}"))
            .exists());
        assert!(ModelManager::new(vec![dir])
            .resolve_with(Model::Asr, no_env, file_is_ready)
            .is_err());
    }

    #[test]
    fn an_empty_download_is_not_installed() {
        let dir = scratch("install-empty");
        let name = Model::Vad.default_file();
        assert!(install_model_file(&dir, name, &b""[..], None, |_, _| {}).is_err());
        assert!(!dir.join(name).exists());
        assert!(!dir
            .join(format!("{name}{DOWNLOAD_PARTIAL_SUFFIX}"))
            .exists());
    }
}
