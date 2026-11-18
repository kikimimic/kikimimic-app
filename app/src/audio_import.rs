//! Native, local decoding. afconvert ships with macOS and uses AudioToolbox;
//! imports need neither a separately installed ffmpeg nor a model download.
use std::path::{Path, PathBuf};

use anyhow::Result;
use kikimimic_core::imports::{AudioDecoder, WavDecoder};

pub struct NativeDecoder;
impl AudioDecoder for NativeDecoder {
    fn decode(&mut self, source: &Path, temporary: &Path) -> Result<PathBuf> {
        if source
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
        {
            return WavDecoder.decode(source, temporary);
        }
        #[cfg(target_os = "macos")]
        {
            let result = std::process::Command::new("/usr/bin/afconvert")
                .args(["-f", "WAVE", "-d", "LEI16@48000", "-c", "1"])
                .arg(source)
                .arg(temporary)
                .output()?;
            anyhow::ensure!(
                result.status.success(),
                "macOS could not decode {}: {}",
                source.display(),
                String::from_utf8_lossy(&result.stderr).trim()
            );
            Ok(temporary.to_path_buf())
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("M4A/MP3 import requires the macOS audio decoder")
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn native_m4a_mp3_decode_and_corrupt_audio_failure() {
        let dir = std::env::temp_dir().join(format!("kkm-native-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("source.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..48_000 {
            writer
                .write_sample(
                    ((i as f32 * 440. * std::f32::consts::TAU / 48_000.).sin() * 8_000.) as i16,
                )
                .unwrap();
        }
        writer.finalize().unwrap();
        let m4a = dir.join("source.m4a");
        let encoded = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(&wav)
            .arg(&m4a)
            .output()
            .unwrap();
        assert!(
            encoded.status.success(),
            "{}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        let original = std::fs::read(&m4a).unwrap();
        let decoded = NativeDecoder
            .decode(&m4a, &dir.join("decoded.wav"))
            .unwrap();
        let reader = hound::WavReader::open(decoded).unwrap();
        assert_eq!(reader.spec(), spec);
        assert!((47_000..50_000).contains(&reader.duration()));
        assert_eq!(std::fs::read(&m4a).unwrap(), original);
        let mp3 = dir.join("source.mp3");
        let original = include_bytes!("../tests/fixtures/import-tone.mp3");
        std::fs::write(&mp3, original).unwrap();
        let decoded = NativeDecoder.decode(&mp3, &dir.join("mp3.wav")).unwrap();
        let reader = hound::WavReader::open(decoded).unwrap();
        assert_eq!(reader.spec(), spec);
        assert!((10_000..20_000).contains(&reader.duration()));
        assert_eq!(std::fs::read(&mp3).unwrap(), original);
        let bad = dir.join("broken.mp3");
        std::fs::write(&bad, b"not audio").unwrap();
        assert!(NativeDecoder.decode(&bad, &dir.join("bad.wav")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
