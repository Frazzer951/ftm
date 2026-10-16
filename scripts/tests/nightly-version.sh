#!/usr/bin/env bash
set -euo pipefail

VERSION=$(cd "$(dirname "$0")/.." && pwd -P)/nightly-version.sh
failures=0

expect() {
  local want=$1 got
  shift
  if ! got=$("$VERSION" "$@" 2>&1) || [ "$got" != "$want" ]; then
    echo "FAIL: $* gave '$got', wanted '$want'" >&2
    failures=$((failures + 1))
  fi
}

reject() {
  if "$VERSION" "$@" >/dev/null 2>&1; then
    echo "FAIL: $* was accepted" >&2
    failures=$((failures + 1))
  fi
}

expect 0.1.9-nightly.20261016.42 0.1.8 42 20261016
expect 0.99.10-nightly.20261016.1 0.99.9 1 20261016
expect 0.2.0-nightly.20261016.7 0.2.0-rc.1 7 20261016
expect 0.1.4-nightly.20261016.3 0.1.4-nightly.20261015.2 3 20261016
expect "0.1.9-nightly.$(date -u +%Y%m%d).5" 0.1.8 5
reject "" 1 20261016
reject 0.1 1 20261016
reject 01.2.3 1 20261016
reject 0.1.8 0 20261016
reject 0.1.8 "" 20261016
reject 0.1.8 1 2026-10-16

[ $failures -eq 0 ] || exit 1
echo "nightly-version: ok"
