//! The only network code in Kikimimic: streaming model files into the
//! core's ModelStore, resumably.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

use kikimimic_core::store::ModelStore;

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Running { received: u64, total: u64 },
    Failed(String),
}

/// Downloads in flight, by model id. Written by download threads, read by
/// the window when it redraws.
#[derive(Clone, Default)]
pub struct Downloads(Arc<Mutex<HashMap<String, Progress>>>);

impl Downloads {
    pub fn get(&self, id: &str) -> Option<Progress> {
        self.0.lock().unwrap().get(id).cloned()
    }

    pub fn any_running(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .values()
            .any(|p| matches!(p, Progress::Running { .. }))
    }

    /// Start downloading `id` and what it requires on a thread. Returns
    /// false if it is already running.
    pub fn start(&self, store: ModelStore, id: String) -> bool {
        {
            let mut map = self.0.lock().unwrap();
            if matches!(map.get(&id), Some(Progress::Running { .. })) {
                return false;
            }
            map.insert(
                id.clone(),
                Progress::Running {
                    received: 0,
                    total: 0,
                },
            );
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let result = fetch(&store, &id, &me);
            let mut map = me.0.lock().unwrap();
            match result {
                Ok(()) => map.remove(&id),
                Err(e) => map.insert(id, Progress::Failed(e)),
            };
        });
        true
    }

    fn set(&self, id: &str, received: u64, total: u64) {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), Progress::Running { received, total });
    }
}

fn fetch(store: &ModelStore, id: &str, progress: &Downloads) -> Result<(), String> {
    let files = store.missing_files(id)?;
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let mut done = 0u64;
    for file in files {
        let mut w = store.begin_install(&file)?;
        let mut received = w.offset();
        if received < file.bytes {
            let response = ureq::get(&file.url)
                .header("Range", &format!("bytes={received}-"))
                .call()
                .map_err(|e| format!("download {}: {e}", file.name))?;
            // A server that ignores Range sends the whole file again.
            if response.status() == 200 && received > 0 {
                return Err(format!(
                    "{}: the server ignored the resume request",
                    file.name
                ));
            }
            let mut body = response.into_body().into_reader();
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = body
                    .read(&mut buf)
                    .map_err(|e| format!("download {}: {e}", file.name))?;
                if n == 0 {
                    break;
                }
                w.write(&buf[..n])?;
                received += n as u64;
                progress.set(id, done + received, total);
            }
        }
        w.commit()?;
        done += file.bytes;
    }
    Ok(())
}
