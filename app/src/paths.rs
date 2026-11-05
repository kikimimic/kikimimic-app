//! Where the app keeps things. Overridable for development.

use std::path::PathBuf;

/// Settings and downloaded models.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("io.github.kikimimic.app")
}

/// Scratch space (the diarization script).
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("io.github.kikimimic.app")
}

/// Recordings: `~/Music/kikimimic`, as v2 wrote them, or `KKM_RECORD_DIR`.
pub fn recordings_dir() -> PathBuf {
    std::env::var_os("KKM_RECORD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::audio_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("kikimimic")
        })
}

/// Extra places to find model files, e.g. a checkout's `models/`
/// (`KKM_MODELS_DIR`).
pub fn extra_model_dirs() -> Vec<PathBuf> {
    std::env::var_os("KKM_MODELS_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect()
}

/// A recording directory name for now, in the user's local time.
pub fn session_name() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}
