#!/usr/bin/env bash
# Run FTM on the code in this checkout:
#
#   scripts/dev.sh
#
# Builds ftmd, ftm and the app, quits the running dev app, stops the dev ftmd (its panes survive in their holders),
# then runs `tauri dev` in the foreground. The app starts the freshly built ftmd, which reconnects to every pane.
# Ctrl+C stops the app and Vite; ftmd and your sessions keep running. Rerun after changing code to move everything
# onto the new build.
#
# Dev runs on its own socket, ftmd-dev.sock next to the default one, and its own app identifier, so an installed FTM
# (scripts/install.sh) keeps its daemon, sessions and window state. FTM_DEV_SOCKET picks another socket; so does
# FTM_SOCKET when set outside FTM (inside an FTM pane it names the pane's own daemon, so it is ignored).
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd -P)
BIN="$REPO/target/debug"
# shellcheck source=scripts/lib.sh
. "$REPO/scripts/lib.sh"

if [ -n "${FTM_DEV_SOCKET:-}" ]; then
  SOCKET=$FTM_DEV_SOCKET
elif [ -n "${FTM_SOCKET:-}" ] && [ -z "${FTM_PANE_ID:-}" ]; then
  SOCKET=$FTM_SOCKET
else
  SOCKET=$(dirname "$(default_socket)")/ftmd-dev.sock
fi
if [ "$SOCKET" = "$(default_socket)" ]; then
  echo "Refusing to run dev on the installed app's socket ($SOCKET); pick another with FTM_DEV_SOCKET." >&2
  exit 1
fi
export FTM_SOCKET=$SOCKET
echo "==> dev socket $SOCKET"

echo "==> building"
cargo build --manifest-path "$REPO/Cargo.toml" -p ftmd -p ftm -p ftm-app
ensure_app_deps "$REPO/app"

# Never an installed app, even one started on this socket by hand.
stop_apps "$SOCKET" "" "*.app/Contents/MacOS/*"
# A Vite left over from an earlier run would hold the dev port.
for pid in $(lsof -ti tcp:1420 -sTCP:LISTEN 2>/dev/null); do
  # shellcheck disable=SC2015 # neither a non-node listener nor a failed kill should stop the script
  ps -o command= -p "$pid" | grep -q node && kill "$pid" 2>/dev/null || true
done

stop_ftmd "$SOCKET" "$BIN/ftm"

echo "==> tauri dev (Ctrl+C stops the app; ftmd and sessions keep running)"
cd "$REPO/app"
exec npm run tauri dev -- --config '{"identifier":"dev.frazzer.ftm.dev"}'
