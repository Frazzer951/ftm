#!/usr/bin/env bash
# Print the version of a nightly built from a commit at CURRENT, the committed workspace version:
#
#   scripts/nightly-version.sh CURRENT RUN_NUMBER [YYYYMMDD]
#
# The patch after CURRENT, so a nightly sorts above the last release and below the next one. A committed pre-release
# (0.2.0-rc.1) is already ahead of the last release: keep its core version. The run number keeps several nightlies a
# day unique and in order; unlike a time of day it never has the leading zero that semver, and so Cargo, rejects. The
# date defaults to today in UTC. nightly.yml uses it; scripts/tests/nightly-version.sh covers it.
set -euo pipefail

[ $# -eq 2 ] || [ $# -eq 3 ] || { echo "usage: $0 CURRENT RUN_NUMBER [YYYYMMDD]" >&2; exit 2; }
CURRENT=$1 RUN=$2 DATE=${3:-$(date -u +%Y%m%d)}

[[ $CURRENT =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]] ||
  { echo "not a semver version: '$CURRENT'" >&2; exit 1; }
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]} pre=${BASH_REMATCH[4]}
[[ $RUN =~ ^[1-9][0-9]*$ ]] || { echo "not a run number: '$RUN'" >&2; exit 1; }
[[ $DATE =~ ^[0-9]{8}$ ]] || { echo "not a YYYYMMDD date: '$DATE'" >&2; exit 1; }

[ -n "$pre" ] || patch=$((patch + 1))
echo "$major.$minor.$patch-nightly.$DATE.$RUN"
