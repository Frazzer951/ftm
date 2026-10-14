#!/usr/bin/env bash
# Build ftm and ftmd for a target and stage them where Tauri's externalBin expects them:
#
#   scripts/sidecars.sh [TRIPLE]
#
# Writes app/src-tauri/binaries/{ftm,ftmd}-TRIPLE (TRIPLE defaults to the host's). The release bundle merges
# app/src-tauri/tauri.bundle.conf.json, which lists them, so they land next to ftm-app: in FTM.app/Contents/MacOS on
# macOS and /usr/bin in the .deb and .rpm.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd -P)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
TRIPLE=${1:-$(rustc -vV | sed -n 's/^host: //p')}
OUT=$REPO/app/src-tauri/binaries

cargo build --release --locked --manifest-path "$REPO/Cargo.toml" --target "$TRIPLE" -p ftm -p ftmd
mkdir -p "$OUT"
for bin in ftm ftmd; do
  cp "$TARGET/$TRIPLE/release/$bin" "$OUT/$bin-$TRIPLE"
done
echo "staged sidecars for $TRIPLE in $OUT"
