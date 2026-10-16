#!/usr/bin/env bash
# Renders every packaging template against local fixture assets, and checks that a missing or empty asset fails.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd -P)
RENDER=$REPO/scripts/render-package.sh
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
BASE=file://$TMP/assets
VERSION=1.2.3
failures=0

fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
sha() { sha256sum "$1" | cut -d' ' -f1; }

stage() {
  local prefix=$1 suffix
  rm -rf "$TMP/assets"
  mkdir -p "$TMP/assets"
  for suffix in aarch64.dmg x64.dmg amd64.deb arm64.deb; do
    echo "$prefix $suffix" >"$TMP/assets/${prefix}_${VERSION}_$suffix"
  done
}

expect_line() {
  local out=$1 line=$2 what=$3
  grep -qF -- "$line" <<<"$out" || fail "$what: no line '$line'"
}

expect_failure() {
  local what=$1 out
  shift
  if out=$("$@" 2>"$TMP/err"); then
    fail "$what: exited 0"
  elif [ -n "$out" ]; then
    fail "$what: printed a rendered file"
  fi
}

for cask in ftm:FTM ftm@nightly:FTM.Nightly; do
  template=$REPO/packaging/homebrew/${cask%%:*}.rb prefix=${cask#*:}
  stage "$prefix"
  out=$("$RENDER" cask "$template" "$BASE" "$prefix" "$VERSION") || { fail "$template: exited non-zero"; continue; }
  expect_line "$out" "version \"$VERSION\"" "$template"
  expect_line "$out" "arm:   \"$(sha "$TMP/assets/${prefix}_${VERSION}_aarch64.dmg")\"" "$template"
  expect_line "$out" "intel: \"$(sha "$TMP/assets/${prefix}_${VERSION}_x64.dmg")\"" "$template"

  rm "$TMP/assets/${prefix}_${VERSION}_x64.dmg"
  expect_failure "$template with a missing asset" "$RENDER" cask "$template" "$BASE" "$prefix" "$VERSION"
  : >"$TMP/assets/${prefix}_${VERSION}_x64.dmg"
  expect_failure "$template with an empty asset" "$RENDER" cask "$template" "$BASE" "$prefix" "$VERSION"
done

for pkg in ftm-bin:FTM ftm-nightly-bin:FTM.Nightly; do
  template=$REPO/packaging/aur/${pkg%%:*}/PKGBUILD prefix=${pkg#*:}
  stage "$prefix"
  out=$("$RENDER" pkgbuild "$template" "$BASE" "$prefix" "$VERSION" "$REPO/LICENSE") ||
    { fail "$template: exited non-zero"; continue; }
  expect_line "$out" "_pkgver=$VERSION" "$template"
  expect_line "$out" "sha256sums=('$(sha "$REPO/LICENSE")')" "$template"
  expect_line "$out" "sha256sums_x86_64=('$(sha "$TMP/assets/${prefix}_${VERSION}_amd64.deb")')" "$template"
  expect_line "$out" "sha256sums_aarch64=('$(sha "$TMP/assets/${prefix}_${VERSION}_arm64.deb")')" "$template"

  rm "$TMP/assets/${prefix}_${VERSION}_arm64.deb"
  expect_failure "$template with a missing asset" \
    "$RENDER" pkgbuild "$template" "$BASE" "$prefix" "$VERSION" "$REPO/LICENSE"
done

stage FTM
echo 'extra "@SHA256_OTHER@"' | cat "$REPO/packaging/homebrew/ftm.rb" - >"$TMP/extra.rb"
expect_failure "a template with an unknown placeholder" "$RENDER" cask "$TMP/extra.rb" "$BASE" FTM "$VERSION"

[ $failures -eq 0 ] || exit 1
echo "render-package: ok"
