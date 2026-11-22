# Local meeting inference evaluation (#21)

Preparation only. No models downloaded; inference quality, latency, GPU contention and offline operation remain unmeasured. Adoption is deferred until the experiment below passes. Synthetic time labels do not represent recorded audio or realistic speech density.

Run `python3 docs/evaluation/issue-21/evaluate.py /tmp/meeting-eval`. Outputs contain deterministic 10/60-minute Japanese/English transcripts with utterance IDs, withdrawal, unknown owner/deadline, noise, gold claims and Q&A evidence. Full and five-utterance chunk prompts support comparison. Chunk size is not a tokenizer guarantee. Synthesize chunk claims with original evidence IDs and recheck against original text.

Run with `--metrics latency.json` for nearest-rank p50/p95 of a nonempty JSON list of nonnegative milliseconds. Keep raw samples separate per metric and mode.

## Predeclared experiment

Record model name, file hash, quantization, license, machine/OS/RAM/GPU, engine revision, repository commit, audio/input hashes, sampling, token limits, warm/cold state and repetitions. Reuse Qwen first; PLaMo translation ability is not evidence of meeting understanding. Additional models need separate acquisition consent. Apple models remain an untested macOS-only candidate requiring device/region/prefetch checks; preserve a cross-platform baseline.

Current source: `crates/kikimimic-mt-llama-dylib/src/lib.rs` sets context 2048 and output cap 512, rejects oversized prompts and returns possibly truncated text. Prompt budget is 1536 tokens including wrappers; use the actual tokenizer. This is an application limit, not the model's intrinsic capacity. `crates/kikimimic-mt-llama/src/lib.rs` exposes synchronous mutable generation without cancellation. Separate handles do not prove contention-free execution.

Run five paired repetitions of identical consented audio and GPU load: ASR+translation baseline versus added summary/Q&A. Capture first display, ASR finalization, translation and summary p50/p95, peak process/system memory, queue drops and stop-request-to-idle time. Fix machine memory ceiling before runs. Proposed gates: <=10% additional ASR/translation p95, stop <=1000ms, no additional finalized-utterance drops, gold claim recall >=90%, cited-claim evidence agreement >=95%, zero fabricated decisions/owners/deadlines. These are experiment gates, not achieved guarantees.

Manually annotate recovered gold claims, unsupported generated claims and entailed cited claims. Report counts and denominators separately (recovered/gold, unsupported/generated, entailed/all cited); empty denominator is N/A. Unknown owners and deadlines must remain unknown; withdrawn date must not become confirmed. JSON correctness does not establish factual accuracy.

Inject oversized input, output truncation, malformed JSON, unknown/missing evidence IDs and cancellation during decode. Reject invalid output. Record errors and stop latency; existing ABI has no cancellation, so mark unsupported. After acquisition, block network at OS level, retain isolation command and failed external connection probe, then rerun from local cache with cold process. Online success does not demonstrate offline support.

Parent #20 mapping: #21 supplies evaluation to #22 summaries, #23 Q&A and #25 detection; their runtime implementation remains separate. Share fixture format with #30 real-meeting evaluation. #24 search and #26 external research are independent; #27–#29/#31 and #4–#8 remain separate. Parent completion and real-model acceptance for #21 remain open. Defer adoption if quality, latency, memory or cancellation gates fail; do not schedule meeting inference ahead of finalized translation.
