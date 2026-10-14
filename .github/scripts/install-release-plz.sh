#!/usr/bin/env bash
set -euo pipefail

# Keep both the version and the upstream release asset digest pinned.
version=0.3.172
archive="$RUNNER_TEMP/release-plz.tar.gz"
directory="$RUNNER_TEMP/release-plz"
mkdir -p "$directory"
curl --fail --location --retry 3 \
  "https://github.com/release-plz/release-plz/releases/download/release-plz-v${version}/release-plz-x86_64-unknown-linux-gnu.tar.gz" \
  --output "$archive"
echo "84b7013d164e1fc780b31dc9ef190977830599d6cb5ce81c993f6fcbd3cdc1aa  $archive" | sha256sum --check
tar -xzf "$archive" -C "$directory"
binary=$(find "$directory" -type f -name release-plz -print -quit)
test -n "$binary"
chmod +x "$binary"
dirname "$binary" >> "$GITHUB_PATH"
"$binary" --version
