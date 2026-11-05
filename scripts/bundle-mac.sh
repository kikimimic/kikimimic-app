#!/usr/bin/env bash
# Build Kikimimic.app from the GPUI app (ADR-120002), translation backend included.
#
# A script rather than a cargo command for two reasons:
#  1. The translation backend is a separate cdylib on purpose (llama's ggml must
#     not be linked next to whisper's, ADR-153805), so cargo never builds it as a
#     dependency of the app. It goes into Contents/Frameworks, where
#     kikimimic-mt-llama looks for it.
#  2. The system-audio lane needs a real, signed bundle with
#     NSAudioCaptureUsageDescription; launched any other way macOS withholds the
#     permission by returning silence (docs/step0-tap-results.md).
#
# Usage: scripts/bundle-mac.sh [--open]
# Signs ad hoc unless KKM_SIGN_IDENTITY names a Developer ID identity.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

bundle_id="io.github.kikimimic.app"
min_macos="14.2"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)"
exe="kikimimic"
dylib="libkikimimic_mt_llama_dylib.dylib"

echo "==> translation backend (cdylib)"
cargo build --release -p kikimimic-mt-llama-dylib

echo "==> app"
cargo build --release -p kikimimic

app="target/bundle/Kikimimic.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks"
cp "target/release/$exe" "$app/Contents/MacOS/$exe"
cp "target/release/$dylib" "$app/Contents/Frameworks/$dylib"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kikimimic</string>
  <key>CFBundleDisplayName</key><string>Kikimimic</string>
  <key>CFBundleIdentifier</key><string>${bundle_id}</string>
  <key>CFBundleExecutable</key><string>${exe}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>${min_macos}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key>
  <string>Kikimimic transcribes your speech in real time.</string>
  <!-- Without this key macOS withholds the Process Tap permission silently:
       the tap runs and its buffers are zero-filled. -->
  <key>NSAudioCaptureUsageDescription</key>
  <string>Kikimimic transcribes what the other participants say.</string>
</dict>
</plist>
PLIST

echo "==> sign"
codesign --force --sign "${KKM_SIGN_IDENTITY:--}" "$app/Contents/Frameworks/$dylib"
codesign --force --sign "${KKM_SIGN_IDENTITY:--}" "$app"
codesign --verify --strict "$app"

echo "==> built $app"
if [[ "${1-}" == "--open" ]]; then
  open "$app"
fi
