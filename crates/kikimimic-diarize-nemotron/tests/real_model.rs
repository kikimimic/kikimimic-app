//! The real Nemotron on a real recording. Downloads the model on first run
//! (uv), so opt-in:
//!
//! ```sh
//! KKM_TEST_MIX=/path/to/mix.wav cargo test -p kikimimic-diarize-nemotron -- --ignored --nocapture
//! ```

use kikimimic_core::engine::Diarizer;

#[test]
#[ignore = "runs Nemotron through uv; downloads the model on first use"]
fn a_recording_gets_speaker_turns() {
    let mix = std::path::PathBuf::from(std::env::var("KKM_TEST_MIX").expect("set KKM_TEST_MIX"));
    let work = std::env::temp_dir().join(format!("kikimimic-nemotron-{}", std::process::id()));
    let rttm = work.join("out.rttm");
    std::fs::create_dir_all(&work).unwrap();
    let started = std::time::Instant::now();
    kikimimic_diarize_nemotron::Nemotron::new(work.clone()).diarize(&mix, &rttm).unwrap();
    let text = std::fs::read_to_string(&rttm).unwrap();
    let turns = text.lines().filter(|l| l.starts_with("SPEAKER")).count();
    let speakers: std::collections::BTreeSet<_> =
        text.lines().filter_map(|l| l.split_whitespace().nth(7)).collect();
    println!("{turns} turns, speakers {speakers:?}, in {:?}", started.elapsed());
    assert!(turns > 0);
    std::fs::remove_dir_all(&work).unwrap();
}
