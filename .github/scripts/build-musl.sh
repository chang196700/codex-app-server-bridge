#!/bin/sh
set -eu

: "${BUILD_TARGET:?BUILD_TARGET is required}"
: "${HOST_UID:?HOST_UID is required}"
: "${HOST_GID:?HOST_GID is required}"
case "$HOST_UID" in ''|*[!0-9]*) echo 'Invalid HOST_UID' >&2; exit 1 ;; esac
case "$HOST_GID" in ''|*[!0-9]*) echo 'Invalid HOST_GID' >&2; exit 1 ;; esac
case "$BUILD_TARGET" in
    x86_64-unknown-linux-musl) expected_arch=x86_64 ;;
    aarch64-unknown-linux-musl) expected_arch=aarch64 ;;
    *) echo "Unsupported musl target: $BUILD_TARGET" >&2; exit 1 ;;
esac
if [ "$(uname -m)" != "$expected_arch" ]; then
    echo "Expected a native $expected_arch runner." >&2
    exit 1
fi

repository=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$repository"
export CARGO_TARGET_DIR="$repository/target/musl-$BUILD_TARGET"

# Cargo runs as root inside this disposable container; return all generated
# files to the host runner so checkout cleanup and artifact upload can use them.
restore_ownership() {
    for directory in "$repository/target" "$repository/dist"; do
        if [ -d "$directory" ]; then
            chown -R "$HOST_UID:$HOST_GID" "$directory"
        fi
    done
}
trap restore_ownership EXIT

apk add --no-cache musl-dev gcc git jq binutils
channel=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)"[[:space:]]*\(#.*\)\{0,1\}$/\1/p' rust-toolchain.toml)
if [ -z "$channel" ]; then
    echo 'Cannot read the Rust channel from rust-toolchain.toml.' >&2
    exit 1
fi
export RUSTUP_TOOLCHAIN="$channel-$BUILD_TARGET"
rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal --component rustfmt --component clippy --no-self-update
rustup override set "$RUSTUP_TOOLCHAIN"
rustc_version=$(rustc --version --verbose)
printf '%s\n' "$rustc_version"
rustc_host=$(printf '%s\n' "$rustc_version" | sed -n 's/^host: //p')
if [ "$rustc_host" != "$BUILD_TARGET" ]; then
    echo "Expected a native Rust host of $BUILD_TARGET, found $rustc_host." >&2
    exit 1
fi

cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
metadata=$(cargo metadata --no-deps --format-version 1 --locked)
version=$(printf '%s\n' "$metadata" | jq -er '[.packages[] | select(.name == "codex-app-server-bridge")] | if length == 1 then .[0].version else error("Expected exactly one bridge package") end')
executable="$CARGO_TARGET_DIR/release/codex-app-server-bridge"
reported_version=$("$executable" --version)
if [ "$reported_version" != "codex-app-server-bridge $version" ]; then
    echo "Unexpected executable version: $reported_version" >&2
    exit 1
fi
printf '%s\n' "$reported_version"

# A static musl executable must not request an ELF program interpreter or any
# shared libraries. Check readelf itself succeeds before inspecting its output.
program_headers=$(readelf --program-headers "$executable")
dynamic_section=$(readelf --dynamic "$executable")
if printf '%s\n' "$program_headers" | grep -q 'INTERP'; then
    echo 'musl executable unexpectedly requires a program interpreter.' >&2
    exit 1
fi
if printf '%s\n' "$dynamic_section" | grep -q '(NEEDED)'; then
    echo 'musl executable unexpectedly links to shared libraries.' >&2
    exit 1
fi

commit=$(git -c safe.directory="$repository" rev-parse HEAD)
binary_hash=$(sha256sum "$executable" | cut -d ' ' -f 1)
archive_name="codex-app-server-bridge-v$version-$BUILD_TARGET"
staging="$CARGO_TARGET_DIR/release-package/$archive_name"
if [ -e "$staging" ]; then
    echo "Package staging directory already exists: $staging" >&2
    exit 1
fi
mkdir -p "$staging" "$repository/dist"
cp "$executable" README.md README_ZH.md "$staging/"
chmod 755 "$staging/codex-app-server-bridge"
cat > "$staging/BUILDINFO.txt" <<EOF
Package: codex-app-server-bridge
Version: $version
Target: $BUILD_TARGET
Source commit: $commit
Binary SHA256: $binary_hash
Linkage: static musl (no ELF interpreter or shared-library dependencies)

$rustc_version
EOF
archive="$repository/dist/$archive_name.tar.gz"
tar -czf "$archive" -C "$(dirname "$staging")" "$archive_name"
printf 'Created %s\n' "$archive"
