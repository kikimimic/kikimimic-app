# Meeting Q&A provenance boundary (#23)

Implemented: model-independent immutable snapshot and answer validation API in
`kikimimic_core::meeting_qa`. Explicit meeting ID and revision prevent accidental
cross-meeting or stale answer acceptance. Citations resolve to original text and
millisecond timestamps. Unknown IDs, ambiguous IDs, empty quotes, translated or
fabricated excerpts fail closed to 会議内では確認できない.

This is a partial delivery of #23, not a completed Q&A product. Exact substring
validation proves citation provenance, not that the answer follows from it.
No inference backend, persistence adapter, question UI or clickable navigation
is connected. No model was downloaded. Network isolation and concurrent recording
latency were not measured; this pure API performs no IO or engine calls.

#20 dependency mapping: #21 owns inference selection/load evaluation; #22 owns
meeting notes. This module does not implement either. #24 retrieval is intentionally
not connected: callers must explicitly select one meeting. #31 may later consume
citation timestamps. Their current implementation status is not assumed.

Reproduce: `nix develop -c cargo test -p kikimimic-core meeting_qa`.
The unit fixture verifies selected-meeting isolation, revision changes, source
navigation, missing citations and translation mismatch without models.

Before enabling UI: evaluate a Japanese and mixed-language meeting with assignments,
deadlines, reasons and unresolved items; include a reversed decision and an
unanswerable question. Annotate expected answer and original utterance IDs manually.
Score entailment and citation correctness separately. Include transcript instructions
to access another meeting or Web; they must remain data. Run with network blocked
and compare recording/translation latency and stop time with Q&A disabled/enabled.
Record device, model revision, fixture and raw timings. These acceptance checks
remain unmeasured and block claiming full completion of #23.
