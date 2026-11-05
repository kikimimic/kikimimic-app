# Kikimimic

[日本語](README.ja.md)

Real-time multilingual transcription and translation for your desktop, running
entirely on your machine. Kikimimic listens to two lanes — your microphone and
the audio your meeting app plays — transcribes each with a local Whisper model,
translates confirmed sentences with a local LLM, and records both lanes to disk.
Nothing leaves your computer except the one-time model downloads.

The name blends *kikimimi* (聞き耳, "listening closely") and *mimic*; the shared
`mimi` (耳, "ear") stands for the two lanes.

> **Status: rebuilding as v3.** The project is moving to the `kikimimic` org as
> an OSS monorepo: a headless `kikimimic-core` library and a GPUI desktop app.
> See [plan.md](plan.md) and [docs/design/core.md](docs/design/core.md). The
> rest of this README describes the v2 app that runs today.

## Features

- Word-level live transcription with local Whisper (whisper.cpp, Metal)
- Sentence-level live translation with a local LLM (llama.cpp, GGUF)
- Separate lanes for your microphone and system audio, recorded on one timeline
- Post-recording speaker diarization (optional, Apple Silicon + `uv`)
- macOS (Apple Silicon) today; Windows and Linux are planned

## Models

Models are never bundled. Download them from the app's **Models** screen, or put
them in `models/` (git-ignored):

| File | Use | License |
|---|---|---|
| `ggml-large-v3-turbo-q8_0.bin` | Whisper ASR (q8_0 is faster than q5_0 on Metal) | MIT |
| `ggml-silero-v5.1.2.bin` | Silero VAD (GGML build from whisper.cpp) | MIT |
| `Qwen3-4B-Instruct-2507-Q4_K_M.gguf` | Translation (optional; transcription works without it) | Apache-2.0 |

## Building (macOS)

You need Xcode command line tools and either Nix (`nix develop`) or Rust, CMake,
Bun and `uv` on your `PATH`.

The translation backend is built as a separate cdylib, because llama.cpp's ggml
cannot share one binary with whisper.cpp's
([ADR-153805](docs/ADR/20260819-143000-isolate-llama-cpp-in-a-cdylib.md)):

```sh
cargo build --release -p kikimimic-mt-llama-dylib
```

Then:

- Development: `bun run tauri dev`
- Real use: `./scripts/build-app.sh --open` builds and launches `Kikimimic.app`.
  The system-audio lane only works from a signed `.app`; launched any other way,
  macOS silently returns silence.

## Recordings

Each session gets a folder under `~/Music/kikimimic/<yyyyMMddHHmmss>/`:

- `mic.wav`, `speaker.wav`, `mix.wav` — 48 kHz mono 16-bit, same length, same timeline
- `transcript.jsonl` — confirmed utterances and their translations, one per line

```json
{"type":"utterance","id":1,"lane":"mic","startMs":1200,"endMs":3400,"lang":"ja","text":"こんにちは"}
{"type":"translation","id":1,"lang":"en","text":"Hello."}
```

Translations arrive a few seconds later and out of order across lanes, so join
them to utterances by `id`. `startMs`/`endMs` are positions in the recording.

## Documentation

- Design decisions: [docs/ADR/](docs/ADR/)
- v3 core design: [docs/design/core.md](docs/design/core.md)
- Measurements: [docs/step0-results.md](docs/step0-results.md), [docs/nemotron-spike-results.md](docs/nemotron-spike-results.md)

Most design documents are written in Japanese.

## License

MIT. Model licenses are listed above and shown before download.
