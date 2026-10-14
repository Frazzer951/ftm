#!/usr/bin/env bash
# Stamp VERSION into the working tree for a one-off build, without committing: the workspace version, the ftm-core
# requirements of the crates that use it and Cargo.lock. The app takes its version from Cargo, so this is all a bundle
# needs. The nightly workflow uses it; releases go through cargo-release instead (see RELEASE.md).
#
#   scripts/set-version.sh VERSION
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 VERSION" >&2; exit 2; }
VERSION=$1
REPO=$(cd "$(dirname "$0")/.." && pwd -P)

perl -pi -e '$in = /^\[workspace\.package\]/ if /^\[/; s/^version = "[^"]*"/version = "'"$VERSION"'"/ if $in' "$REPO/Cargo.toml"
for manifest in crates/ftm/Cargo.toml crates/ftmd/Cargo.toml app/src-tauri/Cargo.toml; do
  perl -pi -e 's/^(ftm-core = \{ version = )"[^"]*"/$1"'"$VERSION"'"/' "$REPO/$manifest"
done
cargo update --manifest-path "$REPO/Cargo.toml" --workspace --offline --quiet

actual=$(cargo metadata --manifest-path "$REPO/Cargo.toml" --locked --format-version 1 --no-deps |
  jq -r '[.packages[].version] | unique | join(" ")')
[ "$actual" = "$VERSION" ] || { echo "workspace is at $actual, wanted $VERSION" >&2; exit 1; }
echo "workspace at $VERSION"
