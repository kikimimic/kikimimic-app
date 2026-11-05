# Kikimimic

[日本語](README.ja.md)

Real-time multilingual transcription and translation for your desktop, running
entirely on your machine. Kikimimic listens to two lanes — your microphone and
the audio your meeting app plays — transcribes each with a local Whisper model,
translates confirmed sentences with a local LLM, and records both lanes to disk.
Nothing leaves your computer except the one-time model downloads.

The name blends *kikimimi* (聞き耳, "listening closely") and *mimic*; the shared
`mimi` (耳, "ear") stands for the two lanes.

> **Status: v3, early.** macOS on Apple Silicon. Windows and Linux are planned;
> the core already builds and tests on all three. See [plan.md](plan.md).

## Features

- Word-level live transcription with local Whisper (whisper.cpp, Metal)
- Sentence-level live translation with a local LLM (llama.cpp, GGUF)
- Separate lanes for your microphone and system audio, recorded on one timeline
- A glossary that fixes misrecognitions and keeps names intact in translations; with Haas it also helps recognize those words
- Face-to-face mode: microphone only, nothing recorded

## Models

Models are never bundled. Download them from the app's **Models** tab; each one
shows its license first.

| Model | Use | License |
|---|---|---|
| Whisper large-v3-turbo (q8_0) + Silero VAD | Recognition (default) | MIT |
| Haas ja v1.0 (Q8_0, Q4_K) + Silero VAD | Recognition (optional, Japanese only; runs in speech.cpp, which `scripts/bundle-mac.sh` bundles as `libspeech`) | Apache-2.0 |
| macOS Speech Recognition (SpeechAnalyzer) | Recognition (macOS 26+, nothing to download; listens in the first spoken language) | Part of macOS |
| Qwen3 4B Instruct 2507 (Q4_K_M) | Translation (default) | Apache-2.0 |
| PLaMo 3 Translate 2B (Q4_K_M) | Translation (optional) | PLaMo Community License — free for personal and non-commercial use; commercial use needs registration with PFN |

The list lives in [models/catalog.toml](models/catalog.toml).

## Building (macOS)

Rust is pinned by `rust-toolchain.toml` (rustup picks it up). `nix develop`
provides everything else; without Nix you need the Xcode command line tools
(with the macOS 26 SDK or later, for SpeechAnalyzer) and CMake.

The translation backend is a separate cdylib, because llama.cpp's ggml cannot
share one binary with whisper.cpp's
([ADR-153805](docs/ADR/20260819-143000-isolate-llama-cpp-in-a-cdylib.md)):

```sh
cargo build --release -p kikimimic-mt-llama-dylib
cargo run -p kikimimic                # development
./scripts/bundle-mac.sh --open        # Kikimimic.app
```

The system-audio lane only works from the signed `.app`; launched any other way,
macOS silently returns silence.

Development settings:

| Variable | Effect |
|---|---|
| `KKM_MODELS_DIR` | Also look for model files here (e.g. an existing `models/`); several separated by `:` |
| `KKM_RECORD_DIR` | Where recordings go (default `~/Music/kikimimic`) |
| `KKM_TRANSLATE_DYLIB` | Use this build of the translation backend |
| `KKM_LLM_GPU_LAYERS` | Layers of the translation model on the GPU (`0` for CPU) |
| `KKM_LANG_PIN_THRESHOLD`, `KKM_VAD_*` | Whisper language detection and VAD tuning |
| `KKM_UI_LANG` | `en` or `ja`; the UI otherwise follows the OS language |

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

- Core design: [docs/design/core.md](docs/design/core.md)
- Decisions: [docs/ADR/](docs/ADR/)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)

Most design documents are written in Japanese.

## License

MIT (see [LICENSE](LICENSE) and [NOTICE](NOTICE)). Model licenses are listed
above and shown before download.
