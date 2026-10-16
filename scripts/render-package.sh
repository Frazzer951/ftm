#!/usr/bin/env bash
# Render a Homebrew cask or AUR PKGBUILD template from packaging/ with a release's version and checksums:
#
#   scripts/render-package.sh cask TEMPLATE BASE_URL ASSET_PREFIX VERSION
#   scripts/render-package.sh pkgbuild TEMPLATE BASE_URL ASSET_PREFIX VERSION LICENSE
#
# Downloads <BASE_URL>/<ASSET_PREFIX>_<VERSION>_<arch>.{dmg,deb} to hash them and prints the rendered file. Exits
# non-zero if a download fails or any placeholder is left, so a missing asset never ships an empty-input checksum.
# release.yml and nightly.yml use it; scripts/tests/render-package.sh covers it.
set -euo pipefail

usage() { echo "usage: $0 cask TEMPLATE BASE_URL ASSET_PREFIX VERSION | pkgbuild ... VERSION LICENSE" >&2; exit 2; }
[ $# -ge 5 ] || usage
KIND=$1 TEMPLATE=$2 BASE=$3 PREFIX=$4 VERSION=$5

EMPTY_SHA=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

check_sha() {
  local what=$1 sum=$2
  if ! [[ $sum =~ ^[0-9a-f]{64}$ ]] || [ "$sum" = "$EMPTY_SHA" ]; then
    echo "bad sha256 '$sum' for $what" >&2
    exit 1
  fi
  echo "$sum"
}

sha_url() {
  local sum
  sum=$(curl -fsSL "$1" | sha256sum | cut -d' ' -f1)
  check_sha "$1" "$sum"
}

sha_file() {
  local sum
  sum=$(sha256sum "$1" | cut -d' ' -f1)
  check_sha "$1" "$sum"
}

asset() { echo "$BASE/${PREFIX}_${VERSION}_$1"; }

case $KIND in
  cask)
    [ $# -eq 5 ] || usage
    arm=$(sha_url "$(asset aarch64.dmg)")
    intel=$(sha_url "$(asset x64.dmg)")
    rendered=$(sed -e "s/@VERSION@/$VERSION/" -e "s/@SHA256_ARM@/$arm/" -e "s/@SHA256_INTEL@/$intel/" "$TEMPLATE")
    ;;
  pkgbuild)
    [ $# -eq 6 ] || usage
    x86_64=$(sha_url "$(asset amd64.deb)")
    aarch64=$(sha_url "$(asset arm64.deb)")
    license=$(sha_file "$6")
    rendered=$(sed -e "s/@VERSION@/$VERSION/" -e "s/@SHA256_X86_64@/$x86_64/" -e "s/@SHA256_AARCH64@/$aarch64/" \
      -e "s/@SHA256_LICENSE@/$license/" "$TEMPLATE")
    ;;
  *) usage ;;
esac

if grep -qE '@[A-Z0-9_]+@' <<<"$rendered"; then
  echo "unfilled placeholders in $TEMPLATE:" >&2
  grep -E '@[A-Z0-9_]+@' <<<"$rendered" >&2
  exit 1
fi
printf '%s\n' "$rendered"
