#!/usr/bin/env bash
set -euo pipefail

# Keep both the version and the upstream release asset digest pinned.
version=0.3.172
archive="$RUNNER_TEMP/release-plz.tar.gz"
directory="$RUNNER_TEMP/release-plz"
mkdir -p "$directory"
curl --fail --location --retry 3 \
  "https://github.com/release-plz/release-plz/releases/download/release-plz-v${version}/release-plz-x86_64-unknown-linux-musl.tar.gz" \
  --output "$archive"
echo "db58a4668ce9ef0b6431e96a665715745e9a5d7f0b0b28a610034d94de3970df  $archive" | sha256sum --check
tar -xzf "$archive" -C "$directory"
binary=$(find "$directory" -type f -name release-plz -print -quit)
test -n "$binary"
chmod +x "$binary"
dirname "$binary" >> "$GITHUB_PATH"
"$binary" --version
