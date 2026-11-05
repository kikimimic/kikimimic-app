# Contributing to Kikimimic

Thanks for helping. Kikimimic is being rebuilt as v3 (see [plan.md](plan.md));
check the plan before starting larger work so we do not collide.

## Ground rules

- **Fully local.** Nothing in `kikimimic-core` may require the network, a GPU or
  an audio device. Network code lives in the app (model downloads only).
- **Tests first.** Write a failing test, make it pass, then refactor.
- **Structure and behavior in separate commits.** A rename, move or reformat is
  its own commit with no behavior change; say which kind a commit is.
- **Explain why.** Code says how, tests say what, commit messages say why.
- **Licenses.** The app is MIT. Do not depend on or copy from GPL code — in
  particular Zed's `ui` crate (GPUI itself is Apache-2.0 and fine). Models are
  never bundled; each catalog entry declares its license.

## Layout

| Path | What |
|---|---|
| `crates/kikimimic-core` | Headless core: pipeline, state, events, engine traits |
| `crates/kikimimic-asr-*` | Speech recognition engines |
| `crates/kikimimic-mt-*` | Translation engines and prompt formats |
| `crates/kikimimic-capture` | OS audio capture backends |
| `app/` | GPUI desktop app |
| `docs/ADR/` | Architecture decisions |

## Adding an engine or OS backend (v3)

The target shape, from [docs/design/core.md](docs/design/core.md):

1. Create a crate that depends only on `kikimimic-core`.
2. Implement `WindowTranscriber` (window-based ASR) or `AsrEngine` (streaming
   ASR), `MtEngine`, `Diarizer`, or `CaptureBackend`.
3. Run the matching contract suite from `kikimimic_core::contract` against it.
4. Register it in `app/src/engines.rs` and add its models to `models/catalog.toml`.
   The app's test checks every catalog entry against the registry.

You should not need to change `kikimimic-core` to add one. If you do, open an
issue first: the boundary may be wrong.

## Checks

```sh
just ci      # fmt-check, lint, test and licenses, as CI does on macOS
just core    # only the headless core
```

CI runs the core on macOS, Linux and Windows, and the engines on macOS.
