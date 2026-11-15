# Conversation metrics (#4)

Recording details display deterministic statistics from session transcript lines.
Only positive-duration mic/speaker utterances count. Speech share uses summed
utterance durations, not wall-clock meeting time. Longest runs join same-lane
utterances separated by strictly less than 2000 ms (including the gap).
Speed counts Unicode scalar values excluding whitespace, including punctuation,
per minute of summed speech. A question utterance counts once for either ? or ？.
Overlap counts cross-lane pairs with positive intersection; touching endpoints do
not count. Interpret these as interruptions only when using headphones.

Reproduce: `nix develop -c cargo test -p kikimimic-core --offline`.
Open a recording from the library to see both lanes and the overlap warning.
For acceptance, compare its transcript timestamps and text against these rules;
record headset use and manually inspect every overlap before interpreting it.

No model downloads or other issue implementations are required. Real recording
and visual acceptance have not been performed in this worktree. Diarization
splits may change utterance counts; values describe the displayed transcript.
