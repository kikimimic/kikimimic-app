# Issue #7: recording across system sleep

Status: reproducible evaluation prepared; actual sleep/resume recording NOT measured.
Issue #7 must remain open until the hardware run below is recorded. No runtime
policy change is justified by the available evidence.

## Code evidence

- `crates/kikimimic-core/src/capture.rs`: Anchor pairs stream frame and host nanoseconds.
- `crates/kikimimic-core/src/conductor/lane.rs`: SessionClock.position subtracts
  the first host timestamp and converts the difference to recording samples.
  It does not consult wall time or receive a sleep notification.
- The same file's SilenceWatch ignores empty buffers and reports sustained zero
  samples; it is not a system sleep detector.
- `crates/kikimimic-capture/src/speaker.rs` converts mHostTime to nanoseconds.

These facts do not establish actual post-resume callback ordering, device
reconnection behavior, or cross-lane drift. A wall-time deficit alone does not
prove lane desynchronization. Wall-clock adjustment can also produce a deficit.
Python monotonic timestamps are supporting observations, not capture timestamps.

## Manual hardware protocol

Use a disposable recording with existing installed models. Do not download models.
Record the commit, macOS version, audio devices, output route, recording directory,
and whether automatic clock synchronization was enabled. Use actual speakers so
both lanes capture the same sound; headphones invalidate cross-correlation.

1. Start `python scripts/sleep-clock-probe.py > clock.jsonl` in another terminal.
2. Start a recording. Play or clap a recognizable repeated signal for 15 seconds.
3. Note wall time, manually sleep the Mac for about 30 seconds, then wake it.
4. Repeat the signal for 15 seconds, stop recording, and Ctrl-C the probe.
5. Run `python scripts/check-recording.py SESSION_DIR --sync > sync.txt`.
   Retain the session WAVs and transcript.jsonl. Do not commit private recordings.
6. Inspect matching signal positions separately before and after sleep: a whole
   file correlation can hide a post-resume offset. Report per-lane positions,
   post-minus-pre offset change, WAV durations, transcript spans, wall duration,
   and the probe's wall_minus_monotonic_ns around resume. Listen to transcript
   boundaries. Record missing callbacks, errors, silence notifications and recovery.
7. Repeat with delayed device reconnect and output-device change, plus an awake
   baseline. Report each run independently, including failed or silent recordings.

Store all raw evidence and an outcome report under
`/Users/wagomu/.ai/live-poly-trans/issue-7/`. No actual sleep run exists yet.

## Decision gate

Choose and document the intended timeline semantics before changing behavior:
active capture time may legitimately exclude sleep, while wall time includes it.
If both lanes retain alignment and active-time semantics are intended, no change
may be needed. If recovery corrupts alignment, evaluate automatic stop first;
wall-time silence insertion requires explicit clock discontinuity and per-device
recovery handling. Do not infer either policy from this probe alone.

The probe uses only Python's standard library and is independent of pending
features. Runtime sleep handling and hardware acceptance remain follow-up work.
