use super::*;
use crate::domain::StreamMs;
use std::sync::atomic::AtomicUsize;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kkm-import-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn base(&self) -> PathBuf {
        self.0.join("recordings")
    }
    fn queue(&self) -> Arc<Mutex<Queue>> {
        Arc::new(Mutex::new(Queue::load(self.0.join("data/imports.json"))))
    }
    fn wav(&self, name: &str, seconds: u32, level: i16, rate: u32) -> PathBuf {
        let path = self.0.join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..seconds * rate {
            w.write_sample(level).unwrap();
        }
        w.finalize().unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn settings() -> Settings {
    Settings::defaults(crate::catalog::Catalog::builtin())
}
fn add(q: &Arc<Mutex<Queue>>, paths: Vec<PathBuf>, mode: ImportMode, settings: Settings) {
    q.lock()
        .unwrap()
        .add(paths, mode, settings, "20261117123000".into())
        .unwrap();
}

#[derive(Default)]
struct Fake {
    lengths: Vec<usize>,
    fail: bool,
    hidden_base: Option<PathBuf>,
}
impl ImportEngines for Fake {
    fn prepare(&mut self, _settings: &Settings) -> Result<()> {
        if let Some(base) = &self.hidden_base {
            assert!(
                crate::library::list(base).is_empty(),
                "staged audio must not be listed"
            );
        }
        anyhow::ensure!(!self.fail, "model unavailable");
        Ok(())
    }
    fn recognize(&mut self, audio: Pcm16k, _settings: &Settings) -> Result<Vec<Recognized>> {
        self.lengths.push(audio.as_slice().len());
        Ok(vec![Recognized {
            text: "hello".into(),
            start: StreamMs(100),
            end: StreamMs(300),
            lang: Some(Lang::parse("en").unwrap()),
        }])
    }
    fn translate(
        &mut self,
        text: &str,
        _source: Option<&Lang>,
        _target: &Lang,
        _settings: &Settings,
    ) -> Result<String> {
        Ok(text.to_uppercase())
    }
}
fn run(q: &Arc<Mutex<Queue>>, base: &Path, fake: &mut Fake) {
    super::run(q, base, &mut WavDecoder, fake, |_| {});
}
fn state(q: &Arc<Mutex<Queue>>, i: usize) -> ImportState {
    q.lock().unwrap().view.jobs[i].state.clone()
}

#[test]
fn queue_save_failure_keeps_the_job_pending_and_resumable() {
    let s = Scratch::new();
    let q = s.queue();
    add(
        &q,
        vec![s.wav("recover.wav", 1, 4_000, 48_000)],
        ImportMode::Separate,
        settings(),
    );
    let path = q.lock().unwrap().path.clone();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    run(&q, &s.base(), &mut Fake::default());
    assert_eq!(state(&q, 0), ImportState::Pending);
    assert!(!q.lock().unwrap().view.running);
    assert!(q.lock().unwrap().view.error.is_some());
    std::fs::remove_dir(&path).unwrap();
    q.lock().unwrap().view.error = None;
    run(&q, &s.base(), &mut Fake::default());
    assert_eq!(state(&q, 0), ImportState::Done);
}

#[test]
fn published_job_recovers_without_its_original_sources_even_for_joined_wavs() {
    for mode in [ImportMode::Separate, ImportMode::JoinWav] {
        let s = Scratch::new();
        let q = s.queue();
        let mut inputs = vec![s.wav("first.wav", 1, 4_000, 48_000)];
        if mode == ImportMode::JoinWav {
            inputs.push(s.wav("second.wav", 1, 5_000, 48_000));
        }
        add(&q, inputs.clone(), mode, settings());
        run(&q, &s.base(), &mut Fake::default());
        let destination = output(&q, 0);
        let original = std::fs::read(destination.join("transcript.jsonl")).unwrap();
        {
            let mut queue = q.lock().unwrap();
            assert!(queue.view.jobs[0].fingerprint.is_some());
            queue.view.jobs[0].state = ImportState::Running;
            queue.save().unwrap();
        }
        for input in inputs {
            std::fs::remove_file(input).unwrap();
        }
        let reopened = s.queue();
        let mut fake = Fake::default();
        run(&reopened, &s.base(), &mut fake);
        assert_eq!(state(&reopened, 0), ImportState::Skipped);
        assert!(fake.lengths.is_empty());
        assert_eq!(output(&reopened, 0), destination);
        assert_eq!(
            std::fs::read(destination.join("transcript.jsonl")).unwrap(),
            original
        );
    }
}

#[test]
fn import_identity_is_durable_before_decode_or_publication() {
    struct CheckingDecoder(PathBuf);
    impl AudioDecoder for CheckingDecoder {
        fn decode(&mut self, source: &Path, temporary: &Path) -> Result<PathBuf> {
            let queue: ImportQueue = serde_json::from_slice(&std::fs::read(&self.0)?)?;
            let job = &queue.jobs[0];
            assert_eq!(job.state, ImportState::Running);
            assert!(job.fingerprint.is_some());
            assert!(!job.output.as_ref().unwrap().exists());
            WavDecoder.decode(source, temporary)
        }
    }
    let s = Scratch::new();
    let q = s.queue();
    add(
        &q,
        vec![s.wav("identity.wav", 1, 4_000, 48_000)],
        ImportMode::Separate,
        settings(),
    );
    let path = q.lock().unwrap().path.clone();
    super::run(
        &q,
        &s.base(),
        &mut CheckingDecoder(path),
        &mut Fake::default(),
        |_| {},
    );
    assert_eq!(state(&q, 0), ImportState::Done);
}
fn output(q: &Arc<Mutex<Queue>>, i: usize) -> PathBuf {
    q.lock().unwrap().view.jobs[i].output.clone().unwrap()
}

#[test]
fn streams_long_audio_with_real_offsets_and_atomic_publication() {
    let s = Scratch::new();
    let input = s.wav("long.wav", 35, 4_000, 48_000);
    let original = std::fs::read(&input).unwrap();
    let q = s.queue();
    add(&q, vec![input.clone()], ImportMode::Separate, settings());
    let mut fake = Fake {
        hidden_base: Some(s.base()),
        ..Fake::default()
    };
    run(&q, &s.base(), &mut fake);
    assert_eq!(state(&q, 0), ImportState::Done);
    assert_eq!(fake.lengths, [30 * 16_000, 5 * 16_000]);
    let out = output(&q, 0);
    let session = crate::library::read(&out).unwrap();
    assert_eq!(session.lines.len(), 2);
    assert_eq!(
        (session.lines[0].start_ms, session.lines[1].start_ms),
        (100, 30_100)
    );
    assert_eq!(session.lines[1].end_ms, 30_300);
    assert_eq!(session.lines[0].translation.as_deref(), Some("HELLO"));
    let list = crate::library::list(&s.base());
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].duration_ms, 35_000);
    assert_eq!(list[0].lanes, ["mic"]);
    assert_eq!(
        std::fs::read(out.join("mic.wav")).unwrap(),
        std::fs::read(out.join("mix.wav")).unwrap()
    );
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn join_preserves_selected_order_and_rejects_mismatched_formats_before_models() {
    let s = Scratch::new();
    let q = s.queue();
    let a = s.wav("a.wav", 1, 4_000, 48_000);
    let b = s.wav("b.wav", 1, -8_000, 48_000);
    add(
        &q,
        vec![b.clone(), a.clone()],
        ImportMode::JoinWav,
        settings(),
    );
    run(&q, &s.base(), &mut Fake::default());
    assert_eq!(state(&q, 0), ImportState::Done);
    let mut reader = hound::WavReader::open(output(&q, 0).join("mix.wav")).unwrap();
    let samples = reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(samples.len(), 96_000);
    assert!(samples[..48_000].iter().all(|s| *s < -7_900));
    assert!(samples[48_000..].iter().all(|s| *s > 3_900));
    let c = s.wav("c.wav", 1, 3_000, 44_100);
    add(&q, vec![a, c], ImportMode::JoinWav, settings());
    let mut fake = Fake::default();
    run(&q, &s.base(), &mut fake);
    assert!(
        matches!(state(&q, 1), ImportState::Failed { message } if message.contains("formats must match"))
    );
    assert!(fake.lengths.is_empty());
    assert_eq!(crate::library::list(&s.base()).len(), 1);
}

#[test]
fn failures_continue_retry_and_survive_restart_without_touching_completed_results() {
    let s = Scratch::new();
    let q = s.queue();
    let bad = s.0.join("bad.wav");
    std::fs::write(&bad, b"broken").unwrap();
    let good = s.wav("good.wav", 1, 4_000, 48_000);
    add(
        &q,
        vec![bad.clone(), good],
        ImportMode::Separate,
        settings(),
    );
    run(&q, &s.base(), &mut Fake::default());
    assert!(matches!(state(&q, 0), ImportState::Failed { .. }));
    assert_eq!(state(&q, 1), ImportState::Done);
    let completed = std::fs::read(output(&q, 1).join("transcript.jsonl")).unwrap();
    s.wav("bad.wav", 2, 2_000, 48_000);
    let restarted = s.queue();
    assert!(matches!(state(&restarted, 0), ImportState::Failed { .. }));
    restarted.lock().unwrap().retry_failed().unwrap();
    run(&restarted, &s.base(), &mut Fake::default());
    assert_eq!(state(&restarted, 0), ImportState::Done);
    assert_eq!(
        std::fs::read(output(&restarted, 1).join("transcript.jsonl")).unwrap(),
        completed
    );
}

#[test]
fn content_and_settings_determine_skipping_and_published_crash_recovery() {
    let s = Scratch::new();
    let q = s.queue();
    let input = s.wav("input.wav", 1, 4_000, 48_000);
    add(&q, vec![input.clone()], ImportMode::Separate, settings());
    run(&q, &s.base(), &mut Fake::default());
    // Simulate crash after publication, before marking queue complete.
    {
        let mut q = q.lock().unwrap();
        q.view.jobs[0].state = ImportState::Running;
        q.save().unwrap();
    }
    let restarted = s.queue();
    assert_eq!(state(&restarted, 0), ImportState::Pending);
    let mut fake = Fake::default();
    run(&restarted, &s.base(), &mut fake);
    assert_eq!(state(&restarted, 0), ImportState::Skipped);
    assert!(fake.lengths.is_empty());
    let renamed = s.0.join("renamed.wav");
    std::fs::copy(&input, &renamed).unwrap();
    add(&restarted, vec![renamed], ImportMode::Separate, settings());
    run(&restarted, &s.base(), &mut fake);
    assert_eq!(state(&restarted, 1), ImportState::Skipped);
    let mut different = settings();
    different.languages.target = None;
    add(
        &restarted,
        vec![input.clone()],
        ImportMode::Separate,
        different,
    );
    run(&restarted, &s.base(), &mut fake);
    assert_eq!(state(&restarted, 2), ImportState::Done);
    assert_eq!(
        crate::library::read(&output(&restarted, 2)).unwrap().lines[0].translation,
        None
    );
    s.wav("input.wav", 1, 8_000, 48_000);
    add(&restarted, vec![input], ImportMode::Separate, settings());
    run(&restarted, &s.base(), &mut fake);
    assert_eq!(state(&restarted, 3), ImportState::Done);
    assert_eq!(crate::library::list(&s.base()).len(), 3);
}

#[test]
fn stop_is_between_jobs_and_unavailable_models_leave_no_partial_recordings() {
    let s = Scratch::new();
    let q = s.queue();
    let a = s.wav("a.wav", 1, 4_000, 48_000);
    let b = s.wav("b.wav", 1, 3_000, 48_000);
    add(&q, vec![a, b], ImportMode::Separate, settings());
    let stop = Arc::clone(&q.lock().unwrap().stop);
    super::run(
        &q,
        &s.base(),
        &mut WavDecoder,
        &mut Fake::default(),
        |view| {
            if view.jobs[0].state == ImportState::Done {
                stop.store(true, Ordering::SeqCst);
            }
        },
    );
    assert_eq!(state(&q, 0), ImportState::Done);
    assert_eq!(state(&q, 1), ImportState::Pending);
    q.lock().unwrap().stop.store(false, Ordering::SeqCst);
    run(
        &q,
        &s.base(),
        &mut Fake {
            fail: true,
            ..Fake::default()
        },
    );
    assert!(matches!(state(&q, 1), ImportState::Failed { .. }));
    assert_eq!(crate::library::list(&s.base()).len(), 1);
    assert_eq!(
        std::fs::read_dir(s.base().join(".imports"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn resampling_stereo_float_preserves_duration_and_silence_is_not_hallucinated() {
    let s = Scratch::new();
    let q = s.queue();
    let input = s.0.join("stereo.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&input, spec).unwrap();
    for _ in 0..44_100 {
        w.write_sample(0.25f32).unwrap();
        w.write_sample(-0.25f32).unwrap();
    }
    w.finalize().unwrap();
    add(&q, vec![input], ImportMode::Separate, settings());
    let mut fake = Fake::default();
    run(&q, &s.base(), &mut fake);
    assert_eq!(state(&q, 0), ImportState::Done);
    assert!(fake.lengths.is_empty());
    assert_eq!(crate::library::list(&s.base())[0].duration_ms, 1_000);
    assert!(crate::library::read(&output(&q, 0))
        .unwrap()
        .lines
        .is_empty());
}

#[test]
fn modified_completed_results_are_preserved_and_corrupt_queue_is_not_overwritten() {
    let s = Scratch::new();
    let q = s.queue();
    let input = s.wav("input.wav", 1, 4_000, 48_000);
    add(&q, vec![input.clone()], ImportMode::Separate, settings());
    run(&q, &s.base(), &mut Fake::default());
    let transcript = output(&q, 0).join("transcript.jsonl");
    std::fs::write(&transcript, b"user changed this").unwrap();
    add(&q, vec![input], ImportMode::Separate, settings());
    run(&q, &s.base(), &mut Fake::default());
    assert!(
        matches!(state(&q, 1), ImportState::Failed { message } if message.contains("modified"))
    );
    assert_eq!(std::fs::read(transcript).unwrap(), b"user changed this");
    let path = s.0.join("data/imports.json");
    std::fs::write(&path, b"broken queue").unwrap();
    let broken = s.queue();
    assert!(broken.lock().unwrap().view.error.is_some());
    assert!(broken.lock().unwrap().retry_failed().is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"broken queue");
}

#[test]
fn changed_source_and_invalid_engine_times_never_publish() {
    struct Changing {
        input: PathBuf,
    }
    impl ImportEngines for Changing {
        fn prepare(&mut self, _: &Settings) -> Result<()> {
            std::fs::write(&self.input, b"source replaced")?;
            Ok(())
        }
        fn recognize(&mut self, _: Pcm16k, _: &Settings) -> Result<Vec<Recognized>> {
            Ok(Vec::new())
        }
        fn translate(
            &mut self,
            _: &str,
            _: Option<&Lang>,
            _: &Lang,
            _: &Settings,
        ) -> Result<String> {
            unreachable!()
        }
    }
    let s = Scratch::new();
    let q = s.queue();
    let input = s.wav("changed.wav", 1, 4_000, 48_000);
    add(&q, vec![input.clone()], ImportMode::Separate, settings());
    super::run(
        &q,
        &s.base(),
        &mut WavDecoder,
        &mut Changing { input },
        |_| {},
    );
    assert!(
        matches!(state(&q, 0), ImportState::Failed { message } if message.contains("input changed"))
    );
    assert!(crate::library::list(&s.base()).is_empty());
    struct BadTimes;
    impl ImportEngines for BadTimes {
        fn prepare(&mut self, _: &Settings) -> Result<()> {
            Ok(())
        }
        fn recognize(&mut self, _: Pcm16k, _: &Settings) -> Result<Vec<Recognized>> {
            Ok(vec![Recognized {
                text: "bad".into(),
                start: StreamMs(0),
                end: StreamMs(1_001),
                lang: None,
            }])
        }
        fn translate(
            &mut self,
            _: &str,
            _: Option<&Lang>,
            _: &Lang,
            _: &Settings,
        ) -> Result<String> {
            unreachable!()
        }
    }
    let input = s.wav("times.wav", 1, 4_000, 48_000);
    add(&q, vec![input], ImportMode::Separate, settings());
    super::run(&q, &s.base(), &mut WavDecoder, &mut BadTimes, |_| {});
    assert!(
        matches!(state(&q, 1), ImportState::Failed { message } if message.contains("timestamps"))
    );
    assert!(crate::library::list(&s.base()).is_empty());
}

#[test]
fn restart_detects_missing_completed_artifacts() {
    let s = Scratch::new();
    let q = s.queue();
    let input = s.wav("input.wav", 1, 4_000, 48_000);
    add(&q, vec![input], ImportMode::Separate, settings());
    run(&q, &s.base(), &mut Fake::default());
    std::fs::remove_dir_all(output(&q, 0)).unwrap();
    let restarted = s.queue();
    assert!(
        matches!(state(&restarted, 0), ImportState::Failed { message } if message.contains("missing"))
    );
    restarted.lock().unwrap().retry_failed().unwrap();
    run(&restarted, &s.base(), &mut Fake::default());
    assert_eq!(state(&restarted, 0), ImportState::Done);
}

#[test]
fn resampler_compensates_delay_on_the_original_audio_clock() {
    let s = Scratch::new();
    let input = s.0.join("impulse.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&input, spec).unwrap();
    for i in 0..44_100 {
        writer
            .write_sample(if i == 4_410 { 20_000i16 } else { 0 })
            .unwrap();
    }
    writer.finalize().unwrap();
    let normalized = s.0.join("normalized.wav");
    audio::normalize(&[input], &normalized).unwrap();
    audio::clips(&normalized, |clip, offset| {
        assert_eq!(offset, 0);
        assert_eq!(clip.len(), 16_000);
        let peak = clip
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
            .unwrap()
            .0;
        assert!(
            peak.abs_diff(1_600) <= 2,
            "100ms impulse shifted to sample {peak}"
        );
        Ok(())
    })
    .unwrap();
}
