#!/usr/bin/env bash
# cargo-release's pre-release hook (see RELEASE.md): syncs the version into the frontend and writes CHANGELOG.md.
#
#   scripts/release.sh VERSION
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 VERSION" >&2; exit 2; }
VERSION=$1
REPO=$(cd "$(dirname "$0")/.." && pwd -P)

if [ "${DRY_RUN:-false}" = true ]; then
  echo "would set app/package.json to $VERSION and write CHANGELOG.md for v$VERSION"
  exit 0
fi

(cd "$REPO/app" && npm version "$VERSION" --no-git-tag-version --allow-same-version >/dev/null)
git -C "$REPO" cliff --config cliff.toml --tag "v$VERSION" -o CHANGELOG.md
git -C "$REPO" add app/package.json app/package-lock.json CHANGELOG.md
