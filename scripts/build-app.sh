#!/usr/bin/env bash
# Build the .app, translation backend included.
#
# Two things make this a script rather than one command:
#
#  1. The translation backend is a separate cdylib on purpose (llama's ggml
#     must not be linked next to whisper's — docs/step0-results.md), so cargo
#     does not build it as a dependency of the app. tauri.conf.json copies it
#     into Contents/Frameworks, and it has to exist by then.
#  2. The speaker lane needs a real bundle: launched any other way, macOS
#     withholds the system-audio permission by returning silence
#     (docs/step0-tap-results.md). Hence `open` at the end, not the binary.
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
script_path="$repo_dir/scripts/build-app.sh"
cd "$repo_dir"

# Keep the documented entry point usable from an ordinary shell. The flake
# owns the Rust and Bun versions, so enter it here instead of requiring every
# caller to remember `nix develop -c`.
if ! command -v cargo >/dev/null 2>&1 || ! command -v bun >/dev/null 2>&1 || ! command -v uv >/dev/null 2>&1; then
  if [[ "${KKM_BUILD_ENV_READY:-}" == "1" ]]; then
    echo "error: cargo, bun or uv are unavailable inside the Nix development environment" >&2
    exit 127
  fi
  if ! command -v nix >/dev/null 2>&1; then
    echo "error: cargo, bun or uv is unavailable, and nix is not installed" >&2
    exit 127
  fi
  echo "==> entering Nix development environment"
  exec nix develop -c env KKM_BUILD_ENV_READY=1 "$script_path" "$@"
fi

echo "==> translation backend (cdylib)"
cargo build --release -p kikimimic-mt-llama-dylib

echo "==> app bundle"
bun run tauri build

# The workspace shares one target dir, so the bundle lands at the root.
app="target/release/bundle/macos/Kikimimic.app"
echo "==> built $app"
if [[ "${1-}" == "--open" ]]; then
  open --env "KKM_UV_BIN=$(command -v uv)" "$app"
fi
