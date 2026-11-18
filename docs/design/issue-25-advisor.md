# Issue #25: manual advisor baseline

This independently reviewable unit is a headless, manual-only lexical baseline in
kikimimic-core::advisor. It does not complete the product acceptance criteria.
No model, network, inference worker, recording hook, or automatic notification is
introduced. A new Advisor scopes suppression and stop state to one meeting.

request consumes at most the last 100 finalized utterances, skips texts over
4096 bytes, and returns at most three evidence cards. Exact trimmed evidence is
suppressed for the lifetime of the instance, including dismissed evidence.
stop is terminal. Sources are verbatim and there is no owner/name field.

The Japanese explicit cues and first-person follow-up template are deliberately
narrow. They are not calibrated semantic confidence thresholds. Quoted speech,
negation and later answers can invalidate a cue: consumers must treat every card
as an unverified suggestion. An input source (mic/speaker) is not a person.
Do not infer that “私は” refers to the user.

## Reproduce

Run in the repository's development shell:

    nix develop -c cargo test -p kikimimic-core advisor
    nix develop -c cargo clippy -p kikimimic-core --all-targets -- -D warnings

The eight labeled cases in explicit_cues_and_abstention include three explicit
positives, a decision, another person's follow-up, self-talk, an ordinary question
and empty evidence. They are behavioral fixtures, not independent precision or
recall evidence. meeting_controls_and_budget checks suppression, dismissal, stop,
and the card limit. No accuracy percentage should be inferred from these tests.

## Dependency and evaluation gate

Parent #20 tracks the broader roadmap; #25 depends on meeting-understanding
evaluation #21. This PR supplies only #25's independent baseline and controls;
it does not implement #21 or historical retrieval. UI cards, a manual UI trigger,
automatic enable/disable controls, semantic resolution, and cross-utterance
deduplication remain open.

Before connecting UI or automation, freeze a held-out annotated set with decided
versus unresolved items, first/third-person and quoted follow-ups, real questions
versus self-talk, later answers, paraphrases, ambiguous speakers and ASR errors.
Record exact evidence spans and unresolved status at each prefix of the meeting.
Report per-kind precision/recall, false interruptions per hour, repeated cards,
abstentions, and annotation disagreement. Tune on a separate development set.
Replay identical audio with advisor disabled/manual/automatic and report ASR/MT
p50/p95 latency, dropped audio, CPU and memory on named hardware. Automatic
presentation stays unavailable until that gate establishes a latency budget.
Real meeting accuracy, recording/translation latency, and UI usability are
unmeasured here. No large model download is required or authorized.
