# Run `nix develop` (or use direnv) first so just, rustup and cmake are on PATH.
# The Xcode command line tools come from macOS itself.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list

# Format every source.
fmt:
    cargo fmt --all

# Check formatting without changing files.
fmt-check:
    cargo fmt --all --check

# Lint the whole workspace with every warning as an error, as CI does.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the tests.
test *args:
    cargo test --workspace {{ args }}

# Lint and test only the headless core, the part CI also runs on Linux and Windows.
core:
    cargo clippy -p kikimimic-core --all-targets -- -D warnings
    cargo test -p kikimimic-core

# Fail when a dependency is copyleft-only (ADR-120002).
licenses:
    python3 -I scripts/check-licenses.py Cargo.toml

# Everything CI runs on macOS.
ci: fmt-check lint test licenses

# The backend is a separate cdylib (ADR-153805), so cargo never builds it as a
# dependency of the app; the app finds it in target/release.

# Build the translation backend.
dylib:
    cargo build --release -p kikimimic-mt-llama-dylib

# Run the app from the checkout. The system-audio lane stays silent here; use `just run`.
dev: dylib
    cargo run -p kikimimic

# Assemble target/bundle/Kikimimic.app.
bundle:
    scripts/bundle-mac.sh

# Build the bundle and launch it.
run:
    scripts/bundle-mac.sh --open

# Zip the bundle for a release (notarized when KKM_NOTARY_PROFILE is set).
package:
    scripts/bundle-mac.sh --zip

# Check a recording session; the newest one when none is given.
check-recording *args:
    python3 -I scripts/check-recording.py {{ args }}
