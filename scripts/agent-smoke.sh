#!/usr/bin/env bash
# Live smoke test of agent workspaces with real Claude Code (Haiku). Opt-in: it costs a little usage.
#
#   scripts/agent-smoke.sh
#
# Starts its own ftmd on a private socket, spawns a leader agent whose brief makes it report its FTM_* env, list
# workspaces and spawn a child agent, plus an unnamed agent that must name itself from its task, then checks the
# leader's results file, `ftm ls --json` and `ftm capture`. Always closes everything and stops its ftmd.
#
# Agents run in FTM_SMOKE_DIR (default: this repo's root), which Claude must already trust: if it shows the trust
# prompt, the script fails and says so. Accept it once by running `claude` in that dir yourself, or point
# FTM_SMOKE_DIR at a trusted checkout. Nothing is written there; the run's files live in a temp dir.
set -euo pipefail

TIMEOUT=${FTM_SMOKE_TIMEOUT:-180}
REPO=$(cd "$(dirname "$0")/.." && pwd -P)
DIR=$(cd "${FTM_SMOKE_DIR:-$REPO}" && pwd -P)
RUN=$(mktemp -d "${TMPDIR:-/tmp}/ftm-smoke.XXXXXX")
RUN=$(cd "$RUN" && pwd -P)
SOCKET=/tmp/ftm-smoke-$$.sock

cargo build --quiet --manifest-path "$REPO/Cargo.toml" -p ftmd -p ftm
BIN="$REPO/target/debug"
export PATH="$BIN:$PATH" FTM_SOCKET="$SOCKET"
unset FTM_WORKSPACE_ID FTM_PANE_ID FTM_NAME FTM_BRIEF
export FTM_AGENT_CMD="claude --model haiku --add-dir '$RUN' --allowedTools 'Bash(ftm:*)' 'Bash(env)' 'Bash(printenv:*)' 'Write'"

FTMD_PID=
failures=()
fail() { failures+=("$1"); echo "FAIL: $1"; }

cleanup() {
  local code=$?
  if [ -n "$FTMD_PID" ] && kill -0 "$FTMD_PID" 2>/dev/null; then
    for id in $(ftm ls --json 2>/dev/null | python3 -c 'import json,sys; [print(w["id"]) for w in json.load(sys.stdin)]' 2>/dev/null); do
      ftm close --id "$id" >/dev/null 2>&1 || true
    done
    kill -TERM "$FTMD_PID" 2>/dev/null || true
    wait "$FTMD_PID" 2>/dev/null || true
  fi
  rm -f "$SOCKET"
  if [ "$code" -eq 0 ]; then rm -rf "$RUN"; else echo "kept run files in $RUN"; fi
}
trap cleanup EXIT

"$BIN/ftmd" >"$RUN/ftmd.log" 2>&1 &
FTMD_PID=$!
for _ in $(seq 100); do ftm ping >/dev/null 2>&1 && break; sleep 0.1; done
ftm ping >/dev/null

ws_json() { ftm ls --json; }
# field <workspace-id> <python expr over w>
field() { ws_json | python3 -c "import json,sys; w=[w for w in json.load(sys.stdin) if w['id']=='$1']; print(($2) if w else '')"; }

# Preflight: the panes' login shell must resolve this build's ftm, and send/capture must round-trip.
term=$(ftm spawn terminal --path "$DIR" --name smoke-preflight)
sleep 1
ftm send "$term" 'echo "FTM=$(command -v ftm)"' --enter
for _ in $(seq 50); do ftm capture "$term" | grep -q "^FTM=/" && break; sleep 0.2; done
resolved=$(ftm capture "$term" | grep "^FTM=/" | tail -1 | cut -d= -f2-)
[ "$resolved" = "$BIN/ftm" ] || fail "panes resolve ftm to '${resolved:-nothing}', not $BIN/ftm (check PATH in your shell profile)"
ftm close --id "$term"

cat >"$RUN/child-brief.md" <<EOF
You are a child agent in an automated FTM smoke test. Reply with just the word: done
Do not use any tools, ask questions or do anything else.
EOF
cat >"$RUN/leader-brief.md" <<EOF
You are the leader agent in an automated FTM smoke test. Do these steps in order with the Bash and Write tools, then stop. Do not ask questions or do anything else.
1. Run: env
   Note the values of FTM_NAME, FTM_WORKSPACE_ID, FTM_PANE_ID and FTM_SOCKET.
2. Run: ftm ls --json
3. Run: ftm spawn agent --name smoke-child --path $DIR --brief $RUN/child-brief.md
   It prints the new workspace's id.
4. Use the Write tool to create $RUN/results.json containing only this JSON object, with the real values filled in:
   {"FTM_NAME": "...", "FTM_WORKSPACE_ID": "...", "FTM_PANE_ID": "...", "FTM_SOCKET": "...", "ls_ok": true or false (whether step 2 printed a JSON array), "child_id": "the id printed in step 3"}
EOF

leader=$(ftm spawn agent --path "$DIR" --name smoke-leader --brief "$RUN/leader-brief.md")
printf 'In one sentence, explain what the Rust ? operator does. Do not use any tools.\n' >"$RUN/unnamed-brief.md"
unnamed=$(ftm spawn agent --path "$DIR" --brief "$RUN/unnamed-brief.md" --detached)
echo "leader $leader; waiting up to ${TIMEOUT}s for it and its child"

# Claude Code clears the screen and scrollback as it redraws, so its startup banner may be gone; the prompt box
# border carries the session name from `-n`.
claude_ui() { ftm capture "$1" | grep -qE "Claude Code|─ $2 ─"; }

blocked() {
  # Prompts a human must answer: fail with the screen instead of answering them.
  local id=$1 screen
  screen=$(ftm capture "$id" 2>/dev/null || true)
  if grep -qiE "trust the files|Do you trust|Yes, proceed|Do you want to (proceed|make this edit|create)" <<<"$screen"; then
    echo "$screen" | tail -40 >"$RUN/blocked-$id.txt"
    fail "agent $id is waiting on a prompt in $DIR (trust or permission); see $RUN/blocked-$id.txt"
    return 0
  fi
  return 1
}

# Neither brief mentions `ftm status`: every status change below comes from the hooks ftmd gives each agent.
child=
leader_seen=
deadline=$((SECONDS + TIMEOUT))
while [ $SECONDS -lt $deadline ]; do
  [ -z "$child" ] && child=$(ws_json | python3 -c "import json,sys; c=[w['id'] for w in json.load(sys.stdin) if w['name']=='smoke-child']; print(c[0] if c else '')")
  leader_status=$(field "$leader" "w[0]['status']")
  case " $leader_seen " in *" $leader_status "*) ;; *) leader_seen="$leader_seen $leader_status" ;; esac
  if [ -s "$RUN/results.json" ] && [ -n "$child" ] && [ "$leader_status" = idle ] \
    && [ "$(field "$child" "w[0]['status']")" = idle ] \
    && [ -n "$(field "$unnamed" "w[0].get('auto_title') or ''")" ]; then break; fi
  blocked "$leader" && break
  blocked "$unnamed" && break
  [ -n "$child" ] && blocked "$child" && break
  sleep 1
done
echo "leader statuses seen:$leader_seen"

if [ ${#failures[@]} -eq 0 ]; then
  [ -s "$RUN/results.json" ] || fail "leader never wrote results.json"
  [ -n "$child" ] || fail "no smoke-child workspace appeared"
fi

if [ -s "$RUN/results.json" ]; then
  leader_pane=$(field "$leader" "w[0]['tabs'][0]['layout']['id'] if w[0]['tabs'] else ''")
  python3 - "$RUN/results.json" "$leader" "$leader_pane" "$SOCKET" "${child:-none}" <<'EOF' || fail "results.json mismatch (see above)"
import json, re, sys
path, leader, pane, socket, child = sys.argv[1:]
text = open(path).read()
r = json.loads(re.search(r"\{.*\}", text, re.S).group(0))
want = {"FTM_NAME": "smoke-leader", "FTM_WORKSPACE_ID": leader, "FTM_PANE_ID": pane, "FTM_SOCKET": socket,
        "ls_ok": True, "child_id": child}
bad = {k: (r.get(k), v) for k, v in want.items() if r.get(k) != v}
for k, (got, exp) in bad.items():
    print(f"  results.{k}: got {got!r}, want {exp!r}")
sys.exit(1 if bad else 0)
EOF
fi

if [ -n "$child" ]; then
  [ "$(field "$child" "w[0]['parent']")" = "$leader" ] || fail "smoke-child is not nested under the leader"
  [ "$(field "$child" "w[0]['kind']")" = agent ] || fail "smoke-child is not an agent"
  [ "$(field "$child" "w[0]['status']")" = idle ] || fail "smoke-child status is $(field "$child" "w[0]['status']"), want idle (Stop hook)"
  claude_ui "$child" smoke-child || fail "no Claude prompt box named smoke-child in the child's pane"
fi
case " $leader_seen " in *" working "*) ;; *) fail "hooks never marked the leader working (saw:$leader_seen)" ;; esac
[ "$(field "$leader" "w[0]['status']")" = idle ] || fail "leader status is $(field "$leader" "w[0]['status']"), want idle (Stop hook)"
# The unnamed agent ran without -n, so Claude titled its terminal with its task and that named the workspace.
unnamed_name=$(field "$unnamed" "w[0]['name']")
unnamed_title=$(field "$unnamed" "w[0].get('auto_title') or ''")
[ -n "$unnamed_title" ] || fail "the unnamed agent never got a task title (auto_title)"
[ "$unnamed_title" != "$unnamed_name" ] && [ "$unnamed_title" != "$(basename "$DIR")" ] \
  || fail "the unnamed agent's auto_title is just its name: $unnamed_title"
[ "$(field "$leader" "w[0].get('auto_title') or ''")" = "" ] || fail "the named leader got an auto_title"
claude_ui "$leader" smoke-leader || fail "no Claude prompt box named smoke-leader in the leader's pane"

ftm capture "$leader" >"$RUN/leader-screen.txt" || true
[ -n "$child" ] && { ftm capture "$child" >"$RUN/child-screen.txt" || true; }

if [ ${#failures[@]} -eq 0 ]; then
  echo "PASS: leader and child Claude agents ran and nested, hooks drove status (working -> idle), unnamed agent named itself \"$unnamed_title\", env reported ($((SECONDS))s)"
else
  echo "FAIL: ${#failures[@]} check(s) failed"
  exit 1
fi
