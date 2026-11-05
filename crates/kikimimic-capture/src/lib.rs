//! OS audio sources for the Kikimimic core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kikimimic_core::capture::CaptureStream;
use kikimimic_core::domain::LaneRole;
use kikimimic_core::{CaptureBackend, CaptureHandle};

mod mic;
#[cfg(target_os = "macos")]
mod speaker;

/// Seconds of audio a lane's ring holds: survives multi-second decode stalls.
const RING: Duration = Duration::from_secs(8);
/// How long a device may take to report its format.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// The capture backend for the platform this was built for.
pub fn platform_backend() -> Box<dyn CaptureBackend> {
    Box::new(Platform)
}

struct Platform;

impl CaptureBackend for Platform {
    fn open(&mut self, role: LaneRole) -> anyhow::Result<(CaptureStream, Box<dyn CaptureHandle>)> {
        match role {
            LaneRole::Mic => mic::open(),
            #[cfg(target_os = "macos")]
            LaneRole::System => speaker::open(),
            #[cfg(not(target_os = "macos"))]
            LaneRole::System => {
                anyhow::bail!("system audio capture is not implemented on this OS yet")
            }
        }
    }
}

/// Stops its source's thread when dropped.
struct Running(Arc<AtomicBool>);

impl CaptureHandle for Running {}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
