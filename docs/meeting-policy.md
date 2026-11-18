# Issue 27: recording policy foundation

This is a partial implementation of #27, tracked by parent #20. The pure core
policy is ready for adapter integration; automatic meeting capture and menu-bar
controls are not shipped by this change. No other issue's features are duplicated.

Automatic start requires explicit opt-in (default off). A start reserves one
session. Manual starts take ownership and cannot be stopped by detector absence.
Automatic sessions stop after 60 seconds of continuous absence; renewed activity
resets that grace period. Manual stop suppresses restart until 60 seconds of
absence. The shell must serialize policy decisions with calendar/manual commands
through the existing session owner, acknowledge failures with stopped(false),
and render waiting/recording/error using actual runtime events.

## Reproducible evaluation, still required

For Zoom and browser Meet separately, log monotonic timestamps, process identity,
input/output activity, policy decisions, actual session IDs and user actions.
Run start/end, long mute (over 60 seconds), late join, reconnect, input-only,
separate-tab video plus microphone, and Kikimimic's own capture. Record false
starts/stops and start/stop latency against manually timestamped ground truth.
Do not treat process audio as proof of a meeting. Browser tab attribution remains
unresolved. No platform detection accuracy or latency has been measured here.

Verify opt-out and calendar starts, simultaneous start sources, close/reopen main
window with menu-bar stop, then sleep/wake (#7) and crash recovery (#6). Those
integrations and native menu-bar status/actions remain outstanding, independently
of this policy. Do not close #27 from this PR.

Run the dependency-free policy tests without downloading models:

```sh
nix develop -c sh -c 'rustc --edition 2021 --test crates/kikimimic-core/src/meeting.rs -o /tmp/meeting-policy-tests && /tmp/meeting-policy-tests'
```

Research snapshots and validation logs are stored locally at
`/Users/wagomu/.ai/live-poly-trans/issue-27/`.
