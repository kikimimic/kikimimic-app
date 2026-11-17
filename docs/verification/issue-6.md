# WAV checkpoint verification (#6)

The recorder now flushes every lane and mix WAV after each capture poll. Hound flush updates the RIFF/data sizes and flushes its buffered writer. Errors use the existing stop-recording/report-once path.

Reproduce the original risk with a recorder writing at least three seconds of samples: read the WAV with a separate WavReader before finish (or terminate the recording process with SIGKILL). Before checkpoints the header does not describe the buffered audio. The regression test reads both lane and mix while the recorder is still alive, before finalize, and asserts 144000 and 48000 decodable samples respectively. Run:

```
nix develop -c cargo test -p kikimimic-core conductor::record
```

This validates readable headers and samples without relying on Drop. A real application SIGKILL, capture permissions, recording-library UI and external player playback have not been exercised. No model is required or downloaded. Existing damaged recordings are not repaired. A crash can lose audio since the last completed poll; the mix additionally holds its existing two-second alignment buffer. Flush is not fsync and does not guarantee power-loss durability. Per-poll disk overhead has not been benchmarked. No dependency feature from other issues is needed.

Manual acceptance: record audio for >5 seconds, SIGKILL the application, relaunch, check library duration and play mic.wav and mix.wav. Expect each duration to reflect its last completed checkpoint, with the mix lag described above.
