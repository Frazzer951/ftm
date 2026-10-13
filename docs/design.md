# FTM design

Frazzer's Terminal Manager: a standalone terminal workspace manager for running many Claude Code agents (and plain shells) across repos and git worktrees, with a sidebar that shows which agent spawned which. Targets macOS and Linux.

## Concepts

- **Workspace**: one sidebar row. A folder (or a git worktree of a repo) plus its tabs and panes. Kind is `terminal` or `agent`.
- **Parent**: a workspace may have a parent. Agents spawned by a leader agent appear nested under it. Nesting depth is unbounded.
- **Tab / pane**: each workspace holds tabs; each tab is a split tree of panes; each pane is one PTY.
- **Status**: `starting | working | needs_input | idle | done`, driven by Claude Code hooks for agents.

## Architecture

```
ftmd (daemon)     owns PTYs, worktrees, workspace tree, Unix socket; keeps scrollback + terminal state
   ▲  Unix socket, newline-delimited JSON (ftm-core::protocol)
   ├── app        sidebar, tabs, panes, terminal rendering (UI choice below)
   └── ftm (CLI)  used by Claude skills, hooks, and scripts
```

Sessions survive both the app quitting and ftmd restarting or being upgraded: each pane's PTY lives in a small holder process (see PTY persistence), and workspace IDs stay stable across restarts.

Every PTY the daemon starts gets:

| Env var | Meaning |
|---|---|
| `FTM_SOCKET` | daemon socket path |
| `FTM_WORKSPACE_ID` | the workspace this pane belongs to |
| `FTM_PANE_ID` | the pane itself |
| `FTM_NAME` | the workspace name (`claude -n`) |

Inherited `CLAUDECODE`, `CLAUDE_CODE_*` and `SUPACODE_*` are stripped, so an agent started from inside another agent is not treated as nested Claude Code. Panes run `$SHELL -l`; an agent's first pane runs `$SHELL -l -i -c '<setup; >claude -n "$FTM_NAME" ${FTM_BRIEF:+"$FTM_BRIEF"}; exec "$SHELL" -l'`, with the brief passed in the environment rather than interpolated. `FTM_AGENT_CMD` (read by ftmd, default `claude`) replaces `claude` in that script, e.g. `claude --model haiku` or the fake agent the tests use; it is shell syntax, so it may carry quoted flags. Each pane keeps the last 1 MiB of raw output; `Subscribe` replays it, then streams live.

`ftm spawn` run inside a workspace defaults `--parent` to `$FTM_WORKSPACE_ID`. That is the whole leader/worker mechanism: no registration step, no config.

Socket location: `$XDG_RUNTIME_DIR/ftm/ftmd.sock` on Linux, `~/Library/Caches/ftm/ftmd.sock` on macOS. Unix socket paths are limited to ~104 bytes on macOS, so keep overrides short.

## CLI

```
ftm ping
ftm ls [--json]
ftm spawn terminal|agent [--path P] [--name N] [--worktree BRANCH [--base REF] [--copy-changes]]
                         [--brief FILE] [--parent ID | --detached]
ftm status working|needs-input|idle|done      # id from $FTM_WORKSPACE_ID
ftm close [--remove-worktree]
ftm capture <pane|workspace> [--raw]          # screen + scrollback as text; --raw: last 1 MiB of output bytes
ftm send <pane|workspace> <text> [--enter]    # type into a pane; a workspace id means its first pane
ftm attach <pane|workspace>                   # this terminal shows the pane; Ctrl+] then d detaches
```

Planned: `ftm gc` (remove worktrees whose PR merged), `ftm tab`/`ftm split`.

## Worktree creation

Per-repo config at `~/.config/ftm/repos/<repo>.toml` (`$XDG_CONFIG_HOME/ftm/...` when set), plain files the app never holds in memory:

```toml
base = "origin/main"
setup = "direnv allow; rm -rf .venv; uv sync --all-extras"
copy_ignored = [".env", ".envrc.local", "profiles.yml"]
```

Steps:

1. `git worktree add <wt> -b <branch> <base>`
2. If `--copy-changes`: `git diff HEAD --binary | git -C <wt> apply` (staged + unstaged edits to tracked files)
3. If `--copy-changes`: copy `git ls-files --others --exclude-standard` (untracked, not ignored)
4. Always: copy gitignored paths matching `copy_ignored`. Copying all ignored files would drag in `.venv`, `node_modules`, `target/`.
5. Run `setup` in the first pane, then start `claude` (or the shell). Setup runs inline before the agent starts, so nothing has to wait on a sentinel file.

Worktrees live under `~/.local/share/ftm/worktrees/<repo>/<branch>` (`$XDG_DATA_HOME/ftm/worktrees` when set). `--remove-worktree` never forces: it fails on a dirty worktree, deletes the branch only if merged (`git branch -d`), and prunes empty `prefix/` directories.

## Testing

- `cargo test` runs `crates/ftmd/tests/agents.rs`: ftmd on a private socket with temp `HOME`/`XDG_*` and `FTM_AGENT_CMD` set to a fake agent that drives the real `ftm` CLI (nesting, `--detached`, status, worktree copy + setup, close/reparent/kill, worktree removal).
- `scripts/agent-smoke.sh` is an opt-in live check with real Claude (Haiku): an isolated ftmd, a leader agent that spawns a child, and assertions on its results file, `ftm ls --json` and `ftm capture`. It costs a little usage, so it is not part of `cargo test`. Hook-driven status checks get added with milestone 5.

## Claude Code integration

ftmd writes hook settings to `claude-settings.json` next to its socket and starts every agent with `claude --settings <that file>`, so the user's own `~/.claude/settings.json` is untouched (hook entries from both apply). Each hook runs `<ftm next to ftmd> status <s> >/dev/null 2>&1 || true` with a 5s timeout: silent (a `UserPromptSubmit` hook's stdout would be fed to Claude), never failing, and a no-op outside FTM.

| Hook (matcher) | Status |
|---|---|
| `UserPromptSubmit`, `PostToolUse` | `working` |
| `PermissionRequest` (fires as soon as a tool needs approval) | `needs_input` |
| `PreToolUse` (`AskUserQuestion`) | `needs_input` |
| `Notification` (`permission_prompt\|elicitation_dialog\|elicitation_url_dialog`) | `needs_input` |
| `Stop`, `StopFailure` | `idle` |
| `SessionEnd` | `done` (best effort: a killed session may not send it) |

The 60s `idle_prompt` notification is deliberately not mapped, so a finished agent never turns red. `PostToolUse` rather than `PreToolUse` marks work so it cannot race the `AskUserQuestion` hook, and it clears `needs_input` once a prompt is answered. In the app, agent rows show a dot (starting blue, working green, needs input red, idle grey, done hollow grey; CSS variables `--status-*`); terminals show none.

`needs_input` also raises a desktop notification (agent name, "needs input") and badges every ancestor row with the count of agents below it that need input. Agents are started with `claude -n <name> "<brief>"`, so `SendMessage` between leader and workers keeps working as today. `sonarx:spawn-repo-agent` and `sonarx:agent-shutdown` switch from `supacode ...` to `ftm spawn` / `ftm close`.

## UI

Sidebar with a `+` button: pick terminal or agent, pick a folder; if it is a git repo, optionally tick "worktree" and enter a branch (plus "copy my changes"). Rows nest by parent and show status, branch, and PR link. Each workspace has tabs; each tab splits into panes.

Must run on macOS and Linux as a standalone desktop app (never inside a browser).

**Decision: Tauri v2 + xterm.js.** Tauri renders in the OS webview (WKWebView on macOS, WebKitGTK on Linux) inside its own native window. gpui stays the fallback if xterm.js rendering or webview quirks become a problem.

| Option | Terminal | Notes |
|---|---|---|
| **Tauri v2 + xterm.js** (chosen) | xterm.js WebGL | Rust backend, TS frontend. Proven for TUIs (VS Code). Fastest path to the full sidebar/tabs/splits UI. |
| gpui + alacritty_terminal | alacritty_terminal grid, GPU rendered | All Rust, native feel. Zed's terminal panel is the reference. gpui's API is young and thinly documented. |
| libghostty | Ghostty | Ghostty's own frontends are Swift (macOS) and GTK (Linux): two UIs to build. Not worth it for a cross-platform app. |

The daemon/CLI are independent of this choice; the app is just another socket client plus a PTY stream consumer.

## PTY persistence

Each pane runs under a holder: `ftmd pty-holder`, the same binary in a hidden mode, started in its own session so nothing aimed at ftmd (a signal to its process group, its terminal closing) reaches it. The holder owns the PTY master and the pane's process tree, keeps the last 1 MiB of output, and serves one ftmd at a time on `<ftmd socket>.d/<pane>.sock` with length-prefixed frames (`crates/ftmd/src/frame.rs`): `Hello{version, rows, cols}` then the whole buffer then live output to ftmd; `Input`, `Resize` and `Kill` back. A new connection replaces the old one, which is how a restarted ftmd takes over. On `Kill` the holder hangs up the shell's process group and the terminal's foreground group (an agent run by an interactive shell has its own group and ignores SIGHUP), SIGKILLs what is left after 2s, and exits once the terminal is empty, removing its socket; ftmd waits for that before `--remove-worktree` deletes anything.

ftmd keeps its own copy of each pane's output and feeds it through a headless `vt100` emulator, so `ftm capture` reads the screen and scrollback as text, `ftm attach` starts with a redraw of the current screen, and `Subscribe` replays raw output to the app. It saves the workspace tree (workspaces, tabs, layouts, statuses) to `$XDG_STATE_HOME/ftm/<socket path>.json` (`~/.local/state/ftm/...`) on every change, keyed by socket so a dev or test daemon never picks up another daemon's panes, and removes the file when the tree is empty. On start it reloads the tree, reconnects to each pane's holder and rebuilds its screen from the holder's buffer. A pane whose holder is gone (its processes exited while ftmd was down, or the machine rebooted) is dropped from its layout; the workspace stays, possibly without tabs. Nothing is restarted after a reboot. Stopping ftmd (SIGTERM) leaves every pane running; closing a pane or workspace is what kills it.

The app is just a client: quitting it changes nothing, and on launch it starts ftmd if needed, then restores its window geometry (`tauri-plugin-window-state`) and the selected workspace and tabs (`localStorage`).

## Milestones

1. **Daemon + CLI**: workspace tree, parent from env, status, tree saved to disk.
2. **Worktrees**: repo config, worktree creation with the copy steps, setup script, `ftm gc`.
3. **PTYs**: per-pane holders that survive ftmd restarts, headless terminal state, `ftm attach` as a minimal terminal client.
4. **App**: sidebar, `+` flow, tabs, splits, status dots, notifications.
5. **Claude integration**: hooks, port the sonarx spawn/shutdown skills, retire Supacode.

## Open questions

- Should closing a leader close its children, or reparent them to the top level?
