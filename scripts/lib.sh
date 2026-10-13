# shellcheck shell=bash
# Shared by dev.sh and install.sh; source it, don't run it.

# Mirrors ftm_core::paths::socket_path without the FTM_SOCKET override.
default_socket() {
  if [ -n "${XDG_RUNTIME_DIR:-}" ]; then
    echo "$XDG_RUNTIME_DIR/ftm/ftmd.sock"
  elif [ "$(uname)" = Darwin ]; then
    echo "$HOME/Library/Caches/ftm/ftmd.sock"
  else
    echo "${XDG_CACHE_HOME:-$HOME/.cache}/ftm/ftmd.sock"
  fi
}

# npm records what it installed in node_modules/.package-lock.json; reinstall when the manifest or lockfile is newer.
ensure_app_deps() {
  local app=$1 stamp=$1/node_modules/.package-lock.json
  if [ ! -f "$stamp" ] || [ "$app/package.json" -nt "$stamp" ] || [ "$app/package-lock.json" -nt "$stamp" ]; then
    echo "==> installing app dependencies"
    (cd "$app" && npm install --no-audit --no-fund) && touch "$stamp"
  fi
}

# The socket a running process talks to: its FTM_SOCKET, else the default.
process_socket() {
  local env
  if [ -r "/proc/$1/environ" ]; then
    env=$(tr '\0' '\n' <"/proc/$1/environ")
  else
    env=$(ps eww -o command= -p "$1" 2>/dev/null | tr ' ' '\n')
  fi
  env=$(printf '%s\n' "$env" | sed -n 's/^FTM_SOCKET=//p' | head -n1)
  echo "${env:-$(default_socket)}"
}

# The ftm-app processes that are `exe` or talk to `socket`, as "pid exe" lines. Matching by socket keeps an installed
# app (default socket) and a dev one (dev socket) apart, whichever binary each runs.
find_apps() {
  local socket=$1 exe=${2:-} pid comm
  while read -r pid comm; do
    [ "${comm##*/}" = ftm-app ] || continue
    # Linux's comm is only the name.
    [ -L "/proc/$pid/exe" ] && comm=$(readlink "/proc/$pid/exe")
    if [ "$comm" = "$exe" ] || [ "$(process_socket "$pid")" = "$socket" ]; then
      echo "$pid $comm"
    fi
  done < <(ps -axo pid=,comm=)
}

# Quits every ftm-app that find_apps matches, except those whose path matches the glob `keep`.
stop_apps() {
  local socket=$1 exe=${2:-} keep=${3:-} pids="" pid comm
  while read -r pid comm; do
    # shellcheck disable=SC2053
    [ -n "$keep" ] && [[ $comm == $keep ]] && continue
    pids="$pids $pid"
  done < <(find_apps "$socket" "$exe")
  [ -n "$pids" ] || return 0
  echo "==> stopping the running app"
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  # shellcheck disable=SC2086
  wait_gone $pids || { echo "ftm-app ($pids ) did not quit" >&2; exit 1; }
}

wait_gone() {
  local alive pid
  for _ in $(seq 50); do
    alive=0
    for pid in "$@"; do kill -0 "$pid" 2>/dev/null && alive=1; done
    [ $alive -eq 0 ] && return 0
    sleep 0.1
  done
  return 1
}

# SIGTERMs the ftmd on `socket` so the next one adopts its panes, which stay in their holder processes. `ftm` is used
# to spot a daemon from before pane holders, whose panes die with it; then this asks first.
stop_ftmd() {
  local socket=$1 ftm=$2 daemons panes pane orphaned=0 answer
  daemons=$(lsof -t "$socket" 2>/dev/null | sort -u || true)
  [ -n "$daemons" ] || return 0
  # An ftmd from before pane holders kills its panes when it stops: then panes are listed but have no holder socket.
  panes=$(FTM_SOCKET=$socket "$ftm" ls --json 2>/dev/null | python3 -c 'import json,sys
def walk(l): return [l["id"]] if l["type"] == "pane" else walk(l["first"]) + walk(l["second"])
print(" ".join(p for w in json.load(sys.stdin) for t in w["tabs"] for p in walk(t["layout"])))' 2>/dev/null || true)
  for pane in $panes; do [ -S "$socket.d/$pane.sock" ] || orphaned=$((orphaned + 1)); done
  if [ "$orphaned" -gt 0 ]; then
    echo "The running ftmd ($daemons) predates pane holders: restarting it closes $orphaned pane(s) for good."
    read -r -p "Restart it anyway? [y/N] " answer
    [ "$answer" = y ] || [ "$answer" = Y ] || { echo "Left it running; quit those sessions first."; exit 1; }
  fi
  echo "==> stopping ftmd (panes stay in their holders)"
  # shellcheck disable=SC2086
  kill -TERM $daemons 2>/dev/null || true
  # shellcheck disable=SC2086
  wait_gone $daemons || { echo "ftmd ($daemons) did not exit" >&2; exit 1; }
}
