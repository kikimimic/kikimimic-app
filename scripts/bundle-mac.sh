#!/usr/bin/env bash
# Build Kikimimic.app from the GPUI app (ADR-120002), translation backend included.
#
# A script rather than a cargo command for two reasons:
#  1. The translation backend is a separate cdylib on purpose (llama's ggml must
#     not be linked next to whisper's, ADR-153805), so cargo never builds it as a
#     dependency of the app. It goes into Contents/Frameworks, where
#     kikimimic-mt-llama looks for it.
#     libspeech (speech.cpp, for the Haas ASR models) is isolated the same way; it
#     is a pinned prebuilt release, downloaded once into target/ and checked
#     against its sha256.
#  2. The system-audio lane needs a real, signed bundle with
#     NSAudioCaptureUsageDescription; launched any other way macOS withholds the
#     permission by returning silence (docs/step0-tap-results.md).
#
# Usage: scripts/bundle-mac.sh [--open | --zip]
# Signs ad hoc unless KKM_SIGN_IDENTITY names a signing identity (a
# "Developer ID Application" one for distribution, or a local self-signed one
# so macOS keeps the microphone and audio permissions across rebuilds). With
# --zip and KKM_NOTARY_PROFILE (a `notarytool store-credentials` profile),
# the zip is notarized and the app stapled.
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

echo "==> libspeech (speech.cpp)"
speech_version="0.10.1"
speech_sha256="ff24f6f58c9d2dedb01caa88ba043ae845c76eff228e4cbfae5de749174404c1"
speech_zip="target/speech-${speech_version}-macos-arm64-metal.zip"
speech_dir="target/speech-${speech_version}"
speech_dylib="libspeech.4.dylib"
mkdir -p target
if ! echo "$speech_sha256  $speech_zip" | shasum -a 256 -c - >/dev/null 2>&1; then
  curl -fL -o "$speech_zip" \
    "https://github.com/nyosegawa/speech.cpp/releases/download/v${speech_version}/speech-${speech_version}-macos-arm64-metal.zip"
  echo "$speech_sha256  $speech_zip" | shasum -a 256 -c -
fi
rm -rf "$speech_dir"
mkdir -p "$speech_dir"
unzip -q -o "$speech_zip" -d "$speech_dir"
speech_lib="$(find "$speech_dir" -name "$speech_dylib" -type f | head -1)"
[[ -n "$speech_lib" ]] || { echo "$speech_dylib is not in $speech_zip" >&2; exit 1; }
# Also beside the release build, where a `cargo run` checkout looks for it.
mkdir -p target/release && cp "$speech_lib" "target/release/$speech_dylib"

echo "==> app"
cargo build --release -p kikimimic

app="target/bundle/Kikimimic.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
cp app/assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cp "target/release/$exe" "$app/Contents/MacOS/$exe"
cp "target/release/$dylib" "$app/Contents/Frameworks/$dylib"
cp "$speech_lib" "$app/Contents/Frameworks/$speech_dylib"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kikimimic</string>
  <key>CFBundleDisplayName</key><string>Kikimimic</string>
  <key>CFBundleIdentifier</key><string>${bundle_id}</string>
  <key>CFBundleExecutable</key><string>${exe}</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
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
identity="${KKM_SIGN_IDENTITY:--}"
# Only a Developer ID build is notarized, which needs the hardened runtime, a
# secure timestamp, and the entitlements the runtime would otherwise deny.
# Ad hoc and self-signed local builds skip all three: Apple's timestamp server
# rejects non-Apple certificates, and library validation would refuse the
# translation dylib from a certificate with no team ID.
runtime=()
if [[ "$identity" == "Developer ID Application"* ]]; then
  runtime=(--options runtime --timestamp --entitlements app/Kikimimic.entitlements)
fi
codesign --force --sign "$identity" "$app/Contents/Frameworks/$dylib"
codesign --force --sign "$identity" "$app/Contents/Frameworks/$speech_dylib"
codesign --force "${runtime[@]}" --sign "$identity" "$app"
codesign --verify --strict "$app"

echo "==> built $app"
case "${1-}" in
  --open)
    open "$app"
    ;;
  --zip)
    zip="target/bundle/Kikimimic-${version}-macos-arm64.zip"
    rm -f "$zip"
    ditto -c -k --keepParent "$app" "$zip"
    if [[ -n "${KKM_NOTARY_PROFILE:-}" ]]; then
      echo "==> notarize"
      xcrun notarytool submit "$zip" --keychain-profile "$KKM_NOTARY_PROFILE" --wait
      xcrun stapler staple "$app"
      rm -f "$zip"
      ditto -c -k --keepParent "$app" "$zip"
    fi
    echo "==> $zip"
    ;;
esac
