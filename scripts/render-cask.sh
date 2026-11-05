#!/usr/bin/env bash
# Print the Homebrew cask for a released version: scripts/render-cask.sh 0.1.0 <sha256>
set -euo pipefail
version="$1"
sha="$2"
cat <<CASK
cask "kikimimic" do
  version "${version}"
  sha256 "${sha}"

  url "https://github.com/kikimimic/kikimimic-app/releases/download/v#{version}/Kikimimic-#{version}-macos-arm64.zip"
  name "Kikimimic"
  desc "Local real-time transcription and translation for meetings"
  homepage "https://github.com/kikimimic/kikimimic-app"

  depends_on arch: :arm64
  depends_on macos: ">= :sonoma"

  app "Kikimimic.app"

  zap trash: [
    "~/Library/Application Support/io.github.kikimimic.app",
    "~/Library/Caches/io.github.kikimimic.app",
  ]
end
CASK
