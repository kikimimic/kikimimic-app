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

echo "==> app"
# KKM_GOOGLE_CLIENT_ID and KKM_GOOGLE_CLIENT_SECRET are read from the environment at build time.
cargo build --release -p kikimimic

app="target/bundle/Kikimimic.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
cp app/assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
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
