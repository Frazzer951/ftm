#!/usr/bin/env bash
# Run FTM on the code in this checkout:
#
#   scripts/dev.sh
#
# Builds ftmd, ftm and the app, quits any running app, stops the running ftmd (its panes survive in their holders),
# then runs `tauri dev` in the foreground. The app starts the freshly built ftmd, which reconnects to every pane.
# Ctrl+C stops the app and Vite; ftmd and your sessions keep running. Rerun after changing code to move everything
# onto the new build. Honors FTM_SOCKET.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd -P)
BIN="$REPO/target/debug"

# Mirrors ftm_core::paths::socket_path.
if [ -n "${FTM_SOCKET:-}" ]; then
  SOCKET=$FTM_SOCKET
elif [ -n "${XDG_RUNTIME_DIR:-}" ]; then
  SOCKET=$XDG_RUNTIME_DIR/ftm/ftmd.sock
elif [ "$(uname)" = Darwin ]; then
  SOCKET=$HOME/Library/Caches/ftm/ftmd.sock
else
  SOCKET=${XDG_CACHE_HOME:-$HOME/.cache}/ftm/ftmd.sock
fi
export FTM_SOCKET=$SOCKET

echo "==> building"
cargo build --manifest-path "$REPO/Cargo.toml" -p ftmd -p ftm -p ftm-app
# npm records what it installed in node_modules/.package-lock.json; reinstall when the manifest or lockfile is newer.
stamp="$REPO/app/node_modules/.package-lock.json"
if [ ! -f "$stamp" ] || [ "$REPO/app/package.json" -nt "$stamp" ] || [ "$REPO/app/package-lock.json" -nt "$stamp" ]; then
  echo "==> installing app dependencies"
  (cd "$REPO/app" && npm install --no-audit --no-fund) && touch "$stamp"
fi

echo "==> stopping the running app"
pkill -x ftm-app 2>/dev/null || true
# A Vite left over from an earlier run would hold the dev port.
for pid in $(lsof -ti tcp:1420 -sTCP:LISTEN 2>/dev/null); do
  ps -o command= -p "$pid" | grep -q node && kill "$pid" 2>/dev/null || true
done

daemons=$(lsof -t "$SOCKET" 2>/dev/null | sort -u || true)
if [ -n "$daemons" ]; then
  # An ftmd from before pane holders kills its panes when it stops: then panes are listed but have no holder socket.
  panes=$("$BIN/ftm" ls --json 2>/dev/null | python3 -c 'import json,sys
def walk(l): return [l["id"]] if l["type"] == "pane" else walk(l["first"]) + walk(l["second"])
print(" ".join(p for w in json.load(sys.stdin) for t in w["tabs"] for p in walk(t["layout"])))' 2>/dev/null || true)
  orphaned=0
  for pane in $panes; do [ -S "$SOCKET.d/$pane.sock" ] || orphaned=$((orphaned + 1)); done
  if [ "$orphaned" -gt 0 ]; then
    echo "The running ftmd ($daemons) predates pane holders: restarting it closes $orphaned pane(s) for good."
    read -r -p "Restart it anyway? [y/N] " answer
    [ "$answer" = y ] || [ "$answer" = Y ] || { echo "Left it running; quit those sessions first."; exit 1; }
  fi
  echo "==> restarting ftmd (panes stay in their holders)"
  kill -TERM $daemons 2>/dev/null || true
  for _ in $(seq 50); do
    alive=0
    for pid in $daemons; do kill -0 "$pid" 2>/dev/null && alive=1; done
    [ $alive -eq 0 ] && break
    sleep 0.1
  done
fi

echo "==> tauri dev (Ctrl+C stops the app; ftmd and sessions keep running)"
cd "$REPO/app"
exec npm run tauri dev
