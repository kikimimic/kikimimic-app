//! Where downloaded models live and whether they are usable. The core decides
//! file states, license gating and verification; the app only moves bytes
//! (the core has no network code).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, FileSpec, ModelEntry, ModelKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelState {
    /// Some files are missing; `partial` bytes of them are already on disk.
    Missing {
        partial: u64,
        total: u64,
    },
    /// The license must be accepted before downloading.
    NeedsLicense,
    Ready,
}

/// Paths of a model and everything it requires.
#[derive(Clone, Debug, Default)]
pub struct ResolvedFiles(Vec<(ModelKind, String, PathBuf)>);

impl ResolvedFiles {
    /// The first file of the model of `kind` in this set.
    pub fn path(&self, kind: ModelKind) -> Option<&Path> {
        self.0
            .iter()
            .find(|(k, ..)| *k == kind)
            .map(|(.., p)| p.as_path())
    }
}

#[derive(Clone)]
pub struct ModelStore {
    catalog: &'static Catalog,
    /// Downloads go here.
    dir: PathBuf,
    /// Also searched, after `dir` (a checkout's `models/` during development).
    extra: Vec<PathBuf>,
}

impl ModelStore {
    pub fn new(catalog: &'static Catalog, dir: PathBuf, extra: Vec<PathBuf>) -> Self {
        Self {
            catalog,
            dir,
            extra,
        }
    }

    pub fn catalog(&self) -> &'static Catalog {
        self.catalog
    }

    pub fn state(&self, id: &str) -> Result<ModelState, String> {
        let models = self.catalog.with_requirements(id)?;
        if models.iter().any(|m| self.needs_license(m)) {
            return Ok(ModelState::NeedsLicense);
        }
        let missing = self.missing_files(id)?;
        if missing.is_empty() {
            return Ok(ModelState::Ready);
        }
        let partial = missing
            .iter()
            .map(|f| fs::metadata(self.partial_path(f)).map_or(0, |m| m.len()))
            .sum();
        let total = missing.iter().map(|f| f.bytes).sum();
        Ok(ModelState::Missing { partial, total })
    }

    /// Files of `id` and its requirements that are not installed yet.
    pub fn missing_files(&self, id: &str) -> Result<Vec<FileSpec>, String> {
        Ok(self
            .catalog
            .with_requirements(id)?
            .into_iter()
            .flat_map(|m| m.files.iter())
            .filter(|f| self.find(f).is_none())
            .cloned()
            .collect())
    }

    /// Paths for loading `id`, or why it cannot be loaded.
    pub fn resolve(&self, id: &str) -> Result<ResolvedFiles, String> {
        let mut out = Vec::new();
        for m in self.catalog.with_requirements(id)? {
            for f in &m.files {
                let path = self
                    .find(f)
                    .ok_or_else(|| format!("{} is not downloaded ({})", m.title, f.name))?;
                out.push((m.kind, m.id.clone(), path));
            }
        }
        Ok(ResolvedFiles(out))
    }

    /// Record that the user accepted `id`'s license. Idempotent.
    pub fn accept_license(&self, id: &str) -> Result<(), String> {
        let entry = self
            .catalog
            .get(id)
            .ok_or_else(|| format!("unknown model: {id}"))?;
        let path = self.license_path(entry);
        fs::create_dir_all(path.parent().expect("has a parent")).map_err(|e| e.to_string())?;
        fs::write(&path, &entry.license.name).map_err(|e| e.to_string())
    }

    /// Start or resume installing `file`. Refused while a license that covers
    /// it is unaccepted.
    pub fn begin_install(&self, file: &FileSpec) -> Result<InstallWriter, String> {
        let owner = self
            .catalog
            .models()
            .iter()
            .find(|m| m.files.contains(file))
            .ok_or("that file is not in the catalog")?;
        if self.needs_license(owner) {
            return Err(format!("accept the {} license first", owner.license.name));
        }
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let partial = self.partial_path(file);
        let out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial)
            .map_err(|e| format!("open {}: {e}", partial.display()))?;
        let offset = out.metadata().map_err(|e| e.to_string())?.len();
        Ok(InstallWriter {
            out,
            offset,
            partial,
            dest: self.dir.join(&file.name),
            spec: file.clone(),
        })
    }

    fn needs_license(&self, m: &ModelEntry) -> bool {
        m.license.requires_acceptance && !self.license_path(m).exists()
    }

    fn license_path(&self, m: &ModelEntry) -> PathBuf {
        self.dir.join("licenses").join(&m.id)
    }

    fn partial_path(&self, f: &FileSpec) -> PathBuf {
        self.dir.join(format!("{}.partial", f.name))
    }

    /// An installed copy of `f`: right size, in `dir` or an extra directory.
    /// (Hashes are checked once, at install; re-hashing gigabytes on every
    /// lookup would stall startup.)
    fn find(&self, f: &FileSpec) -> Option<PathBuf> {
        std::iter::once(&self.dir)
            .chain(&self.extra)
            .map(|d| d.join(&f.name))
            .find(|p| fs::metadata(p).is_ok_and(|m| m.len() == f.bytes))
    }
}

/// Appends a download to its `.partial` file; `commit` verifies and moves it
/// into place.
pub struct InstallWriter {
    out: File,
    offset: u64,
    partial: PathBuf,
    dest: PathBuf,
    spec: FileSpec,
}

impl InstallWriter {
    /// Bytes already on disk: resume the download from here (HTTP Range).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.out.write_all(bytes).map_err(|e| e.to_string())?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Verify size and sha256, then rename into place. A file that fails
    /// verification is deleted, so the next attempt starts clean.
    pub fn commit(self) -> Result<PathBuf, String> {
        self.out.sync_all().map_err(|e| e.to_string())?;
        drop(self.out);
        let verified = verify(&self.partial, &self.spec);
        if let Err(e) = verified {
            let _ = fs::remove_file(&self.partial);
            return Err(e);
        }
        fs::rename(&self.partial, &self.dest).map_err(|e| e.to_string())?;
        Ok(self.dest)
    }
}

fn verify(path: &Path, spec: &FileSpec) -> Result<(), String> {
    let len = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len != spec.bytes {
        return Err(format!(
            "{}: expected {} bytes, got {len}",
            spec.name, spec.bytes
        ));
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != spec.sha256.to_ascii_lowercase() {
        return Err(format!("{}: checksum mismatch", spec.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn catalog() -> &'static Catalog {
        let text = format!(
            r#"
            [defaults]
            asr = "asr"
            [[model]]
            id = "vad"
            kind = "vad"
            engine = "x"
            title = "VAD"
            license = {{ name = "MIT" }}
            files = [{{ name = "vad.bin", url = "u", sha256 = "{HELLO_SHA}", bytes = 5 }}]
            [[model]]
            id = "asr"
            kind = "asr"
            engine = "x"
            title = "ASR"
            license = {{ name = "MIT" }}
            requires = ["vad"]
            files = [{{ name = "asr.bin", url = "u", sha256 = "{HELLO_SHA}", bytes = 5 }}]
            [[model]]
            id = "gated"
            kind = "mt"
            engine = "x"
            title = "Gated"
            license = {{ name = "Custom", requires_acceptance = true }}
            files = [{{ name = "gated.bin", url = "u", sha256 = "{HELLO_SHA}", bytes = 5 }}]
            [[model]]
            id = "os"
            kind = "asr"
            engine = "x"
            title = "Built into the OS"
            license = {{ name = "OS" }}
            files = []
            "#
        );
        Box::leak(Box::new(Catalog::parse(&text).unwrap()))
    }

    fn store() -> (ModelStore, PathBuf) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kikimimic-store-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        (ModelStore::new(catalog(), dir.clone(), Vec::new()), dir)
    }

    fn install(store: &ModelStore, f: &FileSpec, bytes: &[u8]) -> Result<PathBuf, String> {
        let mut w = store.begin_install(f)?;
        w.write(bytes)?;
        w.commit()
    }

    #[test]
    fn a_model_the_os_provides_is_ready_without_a_download() {
        let (s, _) = store();
        assert_eq!(s.state("os").unwrap(), ModelState::Ready);
        assert!(s.resolve("os").is_ok());
    }

    #[test]
    fn a_model_is_ready_once_it_and_its_requirements_are_installed() {
        let (s, dir) = store();
        assert_eq!(
            s.state("asr").unwrap(),
            ModelState::Missing {
                partial: 0,
                total: 10
            }
        );
        for f in s.missing_files("asr").unwrap() {
            install(&s, &f, b"hello").unwrap();
        }
        assert_eq!(s.state("asr").unwrap(), ModelState::Ready);
        assert!(s.resolve("asr").unwrap().path(ModelKind::Vad).is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_download_is_rejected_and_discarded() {
        let (s, dir) = store();
        let f = s.missing_files("vad").unwrap().remove(0);
        assert!(install(&s, &f, b"jello").unwrap_err().contains("checksum"));
        assert_eq!(
            s.state("vad").unwrap(),
            ModelState::Missing {
                partial: 0,
                total: 5
            }
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_interrupted_download_resumes_where_it_stopped() {
        let (s, dir) = store();
        let f = s.missing_files("vad").unwrap().remove(0);
        let mut w = s.begin_install(&f).unwrap();
        w.write(b"he").unwrap();
        drop(w);
        assert_eq!(
            s.state("vad").unwrap(),
            ModelState::Missing {
                partial: 2,
                total: 5
            }
        );
        let mut w = s.begin_install(&f).unwrap();
        assert_eq!(w.offset(), 2);
        w.write(b"llo").unwrap();
        w.commit().unwrap();
        assert_eq!(s.state("vad").unwrap(), ModelState::Ready);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gated_model_needs_its_license_accepted_first() {
        let (s, dir) = store();
        assert_eq!(s.state("gated").unwrap(), ModelState::NeedsLicense);
        let f = s.missing_files("gated").unwrap().remove(0);
        assert!(s.begin_install(&f).is_err());
        s.accept_license("gated").unwrap();
        s.accept_license("gated").unwrap();
        install(&s, &f, b"hello").unwrap();
        assert_eq!(s.state("gated").unwrap(), ModelState::Ready);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_copy_in_an_extra_directory_counts_as_installed() {
        let (_, dir) = store();
        let extra = dir.join("checkout-models");
        fs::create_dir_all(&extra).unwrap();
        fs::write(extra.join("vad.bin"), b"hello").unwrap();
        let s = ModelStore::new(catalog(), dir.join("downloads"), vec![extra]);
        assert_eq!(s.state("vad").unwrap(), ModelState::Ready);
        fs::remove_dir_all(dir).unwrap();
    }
}
