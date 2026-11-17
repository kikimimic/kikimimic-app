# Issue 22: evidence contract (partial delivery)

The core `minutes` module provides serializable AI-authored summary, decision,
open-question and action items. Each item references original utterance IDs;
callers resolve these IDs to original text and start/end timestamps. Unknown
owner/deadline remain null. Validation rejects missing/ambiguous IDs, metadata
absent from the cited original text, and forward/self revision references.
A later item can supersede an earlier one; consumers must hide superseded items
when presenting the current view while retaining their evidence for review.
Source JSON accepts the complete transcript for final minutes or a recent slice
for live summaries. It does not truncate early meeting decisions.

This is an independent prerequisite for parent #20, not completion of #22.
#21 must establish a suitable local generation backend before inference is wired.
No model was downloaded. UI, live update scheduling, atomic sidecar persistence,
regeneration/cancellation, semantic revision accuracy and history display are
not implemented here. Existing recordings and transcript generation are untouched.
#5 owns Markdown export; this module deliberately creates no exporter.

Validation checks structural provenance only. A quoted name can still be the
wrong assignee, and an evidenced sentence can still misrepresent a decision.
Human confirmation and model evaluation remain required.

Reproduce structural checks with:
`nix develop -c cargo test -p kikimimic-core minutes`.
The test covers empty input, absent evidence, unknown metadata, Japanese/English
source and self revisions. Before enabling generation, evaluate sparse speech,
ambiguous same-name speakers, explicit and unknown deadlines, long meetings with
late withdrawal, model failure and cancellation. Measure latency and semantic
accuracy with the selected backend; these measurements have not been performed.
