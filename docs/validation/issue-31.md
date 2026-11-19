# Saved recording playback (#31)

> Superseded: playback now runs inside the app process through rodio (`app/src/playback.rs`), driven by the detail player. The separate Swift player and its window described below no longer exist.

The recording detail opens a separate native macOS player. Timestamp buttons open it at the utterance start. Play/pause, slider seek, elapsed/duration and available mix/mic/speaker selection operate without writing recordings. Starting capture terminates the player; playback is refused while listening. Closing the main window terminates the child.

This implementation requires the macOS Swift command line tools at runtime. It is an independently reviewable first increment, not release-ready packaging. The helper is embedded in the executable and materialized in the temporary directory. Player startup compilation failures currently go to stderr.

Dependencies: parent #20 tracks the broader meeting workflow. #5 export is unchanged; #6 recovery is unchanged (AVAudioPlayer errors stay in the player). Face-to-face saving and summary/Q&A evidence are separate issues; this adds transcript timestamp entry points only and does not implement their features.

Manual acceptance (not yet run): with an existing saved meeting, open Play audio; play/pause; seek to zero, silence, between utterances and end; click transcript timestamps and compare audible speech. Repeat with mic-only and speaker-only fixtures, truncated WAV and missing files. Verify original file hashes unchanged. Start capture while playing and confirm output ceases before capture can re-ingest it. The current observer stops output once listening is reported, so a startup race remains unverified and needs real-device validation before release.

No models were downloaded. Actual audio output, timing accuracy, loopback prevention and bundled application behavior remain unmeasured.

Local validation: `nix develop -c cargo clippy -p kikimimic --offline -- -D warnings`, `cargo fmt --all` in devShell, `swiftc -typecheck app/src/playback.swift`, and `git diff --check` passed. No audio-device acceptance was performed. The explicit Session start handler kills and reaps the player before requesting capture; observer cleanup is an additional fallback.
