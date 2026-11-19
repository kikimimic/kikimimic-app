//! Playing a saved recording's lanes inside the app process.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use rodio::Source;

use crate::window::words::Playback;

const LANES: [&str; 3] = ["mix", "mic", "speaker"];

/// One lane of one recording, playing until dropped.
pub(crate) struct Player {
    player: rodio::Player,
    lane: &'static str,
    start: Duration,
    /// `None` only in tests, which pull samples from the player themselves.
    _device: Option<rodio::MixerDeviceSink>,
}

impl Player {
    pub(crate) fn open(dir: &Path, lane: &str, position_ms: u64) -> anyhow::Result<Self> {
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()?;
        device.log_on_drop(false);
        let player = rodio::Player::connect_new(device.mixer());
        Ok(Self {
            _device: Some(device),
            ..Self::load(player, dir, lane, position_ms)?
        })
    }

    /// The requested lane, or the first one the recording has.
    fn load(
        player: rodio::Player,
        dir: &Path,
        lane: &str,
        position_ms: u64,
    ) -> anyhow::Result<Self> {
        let path = |lane: &str| dir.join(format!("{lane}.wav"));
        let lane = LANES
            .into_iter()
            .find(|l| *l == lane && path(l).exists())
            .or_else(|| LANES.into_iter().find(|l| path(l).exists()))
            .with_context(|| format!("no audio in {}", dir.display()))?;
        let mut source = rodio::Decoder::try_from(std::fs::File::open(path(lane))?)?;
        // Seeking the decoder rather than the player: `rodio::Player::try_seek`
        // blocks until the output thread pulls a sample.
        let start = Duration::from_millis(position_ms);
        source.try_seek(start)?;
        player.append(source);
        Ok(Self {
            _device: None,
            player,
            lane,
            start,
        })
    }

    pub(crate) fn pause(&self) {
        self.player.pause();
    }

    pub(crate) fn resume(&self) {
        self.player.play();
    }

    /// `None` once the lane has played to its end.
    pub(crate) fn state(&self) -> Option<Playback> {
        (!self.player.empty()).then(|| Playback {
            position_ms: (self.start + self.player.get_pos()).as_millis() as u64,
            playing: !self.player.is_paused(),
            lane: self.lane.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_lane(dir: &std::path::Path, lane: &str, seconds: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(dir.join(format!("{lane}.wav")), spec).unwrap();
        for i in 0..16_000 * seconds {
            wav.write_sample((i % 100) as i16).unwrap();
        }
        wav.finalize().unwrap();
    }

    fn recording(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kkm-playback-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reports_the_requested_lane_from_the_requested_position_as_it_plays() {
        let dir = recording("position");
        write_lane(&dir, "mix", 1);
        write_lane(&dir, "mic", 1);
        let (player, mut output) = rodio::Player::new();
        let playing = Player::load(player, &dir, "mic", 500).unwrap();

        output.by_ref().take(4_000).for_each(drop);

        let state = playing.state().expect("still playing");
        assert_eq!(state.lane, "mic");
        assert!(state.playing);
        assert!(
            (740..=750).contains(&state.position_ms),
            "a quarter second after 500 ms, got {}",
            state.position_ms
        );
    }

    #[test]
    fn falls_back_to_a_lane_the_recording_has() {
        let dir = recording("fallback");
        write_lane(&dir, "speaker", 1);
        let (player, _output) = rodio::Player::new();
        let playing = Player::load(player, &dir, "mix", 0).unwrap();
        assert_eq!(playing.state().unwrap().lane, "speaker");
    }

    #[test]
    fn a_recording_without_audio_is_an_error() {
        let dir = recording("empty");
        let (player, _output) = rodio::Player::new();
        assert!(Player::load(player, &dir, "mix", 0).is_err());
    }

    #[test]
    fn pausing_keeps_the_position_and_stops_reporting_playing() {
        let dir = recording("pause");
        write_lane(&dir, "mix", 1);
        let (player, mut output) = rodio::Player::new();
        let playing = Player::load(player, &dir, "mix", 200).unwrap();
        playing.pause();
        output.by_ref().take(4_000).for_each(drop);
        let state = playing.state().unwrap();
        assert!(!state.playing);
        assert_eq!(state.position_ms, 200);

        playing.resume();
        output.by_ref().take(4_000).for_each(drop);
        let state = playing.state().unwrap();
        assert!(state.playing);
        assert!(
            state.position_ms > 200,
            "resumed from 200 ms, got {}",
            state.position_ms
        );
    }

    #[test]
    fn has_no_state_once_the_lane_has_played_to_its_end() {
        let dir = recording("end");
        write_lane(&dir, "mix", 1);
        let (player, mut output) = rodio::Player::new();
        let playing = Player::load(player, &dir, "mix", 900).unwrap();
        output.by_ref().take(4_000).for_each(drop);
        assert!(playing.state().is_none());
    }
}
