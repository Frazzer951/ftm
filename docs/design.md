# FTM design

Frazzer's Terminal Manager: a standalone terminal workspace manager for running many Claude Code agents (and plain shells) across repos and git worktrees, with a sidebar that shows which agent spawned which. Targets macOS and Linux.

## Concepts

- **Workspace**: one sidebar row. A folder (or a git worktree of a repo) plus its tabs and panes. Kind is `terminal` or `agent`.
- **Parent**: a workspace may have a parent. Agents spawned by a leader agent appear nested under it. Nesting depth is unbounded.
- **Tab / pane**: each workspace holds tabs; each tab is a split tree of panes; each pane is one PTY.
- **Status**: `starting | working | needs_input | idle | done`, driven by Claude Code hooks for agents.
- **Unseen**: a flag on the workspace, set when an agent finishes a turn (working or needs_input to idle or done) and cleared when it works again or the app shows it (`MarkSeen`). It lives in ftmd and is saved with the tree, so it survives app restarts and every client agrees.

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
| `FTM_NAME` | the workspace's spawn name (`claude -n`); a later rename does not change it |
| `FTM_BIN` | absolute path of the `ftm` next to ftmd, for agents and skills when `ftm` is not on `PATH` |

Inherited `CLAUDECODE`, `CLAUDE_CODE_*` and `SUPACODE_*` are stripped, so an agent started from inside another agent is not treated as nested Claude Code. Panes run `$SHELL -l`; an agent's first pane runs `$SHELL -l -i -c '[/bin/sh -c "$FTM_SETUP"; ]claude [--settings <hooks>] [-n "$FTM_NAME"] ${FTM_BRIEF:+"$FTM_BRIEF"}; exec "$SHELL" -l'`, with the setup and brief passed in the environment rather than interpolated. If that first pane exits while the workspace is still `starting` (the shell could not run the script, so neither the agent nor the fallback shell started), ftmd keeps the dead pane in its tab with its last output and a note, logs its screen, and sets the workspace `failed`; typing into it does nothing and closing it works as usual. A terminal spawned with `SpawnRequest.command` (the app's update pane; there is no CLI flag) runs `[/bin/sh -c "$FTM_SETUP"; ]/bin/sh -c "$FTM_COMMAND"; exec "$SHELL" -l` the same way, so the command is POSIX sh whatever the login shell and the pane stays open on a shell afterwards. ftmd refuses a command for an agent and does not add its cwd to the remembered folders. The command lives only in that first pane's environment, not in the saved tree, so a restarted or self-upgraded ftmd reattaches to the pane rather than running it again. `FTM_AGENT_CMD` (read by ftmd, default `claude`) replaces `claude` in that script, e.g. `claude --model haiku` or the fake agent the tests use; it is shell syntax, so it may carry quoted flags. Each pane keeps the last 1 MiB of raw output; `Subscribe` replays it, then streams live.

**Names.** A workspace shows its `title` (set by `ftm rename` or the sidebar) if it has one. An agent spawned with a name (`--name`, which the spawn skills always pass) runs as `claude -n <name>`, so `name` is its stable `SendMessage` address and is what it shows otherwise. An agent spawned without one (the `+` dialog's default) runs without `-n`; Claude Code then titles its terminal (OSC 0/2) "<status glyph> <task summary>", and ftmd takes the title of the pane the agent started in, drops the glyph (a spinner frame while working), ignores the placeholder "Claude Code" and a title equal to the name, and stores the rest as `auto_title`, shown until a rename. So unnamed agents name themselves without a tool call; their `SendMessage` address is then Claude's own default session name, not `FTM_NAME`. `name` never changes and is always `FTM_NAME` (for an unnamed workspace, the folder or branch). `ftm ls` prints `shown (name)` when they differ.

`ftm spawn` run inside a workspace defaults `--parent` to `$FTM_WORKSPACE_ID`. That is the whole leader/worker mechanism: no registration step, no config.

Socket location: `$XDG_RUNTIME_DIR/ftm/ftmd.sock` on Linux, `~/Library/Caches/ftm/ftmd.sock` on macOS. Unix socket paths are limited to ~104 bytes on macOS, so keep overrides short.

Everything a daemon keeps is keyed by its socket, so daemons on different sockets never share state: holder sockets in `<socket>.d/`, the saved tree in `$XDG_STATE_HOME/ftm/<socket path>.json`, hook settings in `<socket stem>.claude-settings.json` and the app-launched daemon's log in `<socket stem>.log`, all next to the socket. An installed FTM (`scripts/install.sh`) uses the default socket; `scripts/dev.sh` uses `ftmd-dev.sock` in the same dir and runs the app under the identifier `dev.frazzer.ftm.dev`, so the two run side by side with separate sessions, window state and web storage while sharing `~/.config/ftm` (settings, themes, sounds, repo setup). Panes get `FTM_SOCKET` for their own daemon, so `ftm` inside a pane, whichever binary, talks to the daemon that owns it.

## CLI

```
ftm ping
ftm ls [--json]
ftm spawn terminal|agent [--path P] [--name N] [--worktree BRANCH [--base REF] [--copy-changes]]
                         [--brief FILE] [--parent ID | --detached]
ftm status working|needs-input|idle|done      # id from $FTM_WORKSPACE_ID
ftm close [--remove-worktree]
ftm rename NAME | --clear                     # display title only; "" also clears
ftm pin|unpin [ID]                            # sidebar's Pinned section; top-level workspaces only
ftm capture <pane|workspace> [--raw]          # screen + scrollback as text; --raw: last 1 MiB of output bytes
ftm send <pane|workspace> <text> [--enter]    # type into a pane; a workspace id means its first pane
ftm attach <pane|workspace>                   # this terminal shows the pane; Ctrl+] then d detaches
ftm gc [--yes] [--no-fetch]                   # list merged FTM worktrees; --yes removes them and their branches
```

Planned: `ftm tab`/`ftm split`.

## Worktree creation

Per-repo config at `~/.config/ftm/repos/<repo>.toml` (`$XDG_CONFIG_HOME/ftm/...` when set), plain files the app never holds in memory:

```toml
base = "origin/main"
setup = "direnv allow; rm -rf .venv; uv sync --all-extras"
copy_ignored_exclude = ["data", "*.parquet"]
```

`copy_ignored` picks which gitignored paths of the source checkout a new worktree gets:

| Value | Copies |
|---|---|
| unset or `true` (default) | every gitignored path, minus the built-in excludes and `copy_ignored_exclude` |
| `[".env", "profiles.yml"]` | only paths matching these globs (relative to the repo root); no excludes apply |
| `false` | nothing |

The built-in excludes (`DEFAULT_COPY_IGNORED_EXCLUDE` in `ftm-core/src/config.rs`) are `.venv`, `venv`, `node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.ruff_cache`, `.mypy_cache`, `.tox` and `.DS_Store`: big or rebuildable, and in a Rust or Python checkout often gigabytes. `copy_ignored_exclude` adds to them, never replaces them. Each exclude is a name or glob matched against every component of a path, at any depth, and against the whole repo-relative path, so `node_modules` also skips `app/node_modules` and `data/*.csv` skips CSVs under `data/`.

Steps:

1. `git worktree add <wt> -b <branch> <base>`
2. If `--copy-changes`: `git diff HEAD --binary | git -C <wt> apply` (staged + unstaged edits to tracked files)
3. If `--copy-changes`: copy `git ls-files --others --exclude-standard` (untracked, not ignored)
4. Always: copy gitignored paths as `copy_ignored` says (`git ls-files --others --ignored --exclude-standard --directory`, so an ignored directory is one entry). Files go through `fs::copy`, which on macOS clones on APFS (copy-on-write, near free) and falls back to a byte copy; modes are kept. Symlinks are recreated as symlinks and never followed, so a link out of the repo stays a link. A directory holding `.git` (a nested repo or worktree, e.g. `.claude/worktrees/*`) and special files are skipped. Copying is best effort: a path that cannot be read is logged as a warning and the rest still come over. ftmd logs each skipped path at debug and the totals at info.
5. Run `setup` in the first pane, then start `claude` (or the shell). Setup runs inline before the agent starts, so nothing has to wait on a sentinel file. It is POSIX sh whatever the login shell (it runs as `/bin/sh -c "$FTM_SETUP"`), so environment changes it makes do not reach the agent.

Worktrees live under `~/.local/share/ftm/worktrees/<repo>/<branch>` (`$XDG_DATA_HOME/ftm/worktrees` when set). `--remove-worktree` never forces: it fails on a dirty worktree, deletes the branch only if merged (`git branch -d`), and prunes empty `prefix/` directories.

## Worktree cleanup

`ftm gc` looks only at worktrees under the worktrees dir, so it never touches checkouts FTM did not make. The CLI runs the git checks itself and asks ftmd for the workspace tree, so ftmd must be running. It only knows about the ftmd on its socket, so a dev ftmd's workspaces are not seen by the installed `ftm`, and the other way round. For each repo it fetches the remote of `base` (default `origin/main`) without pruning, unless `--no-fetch` says to use the last fetch. Then it lists each worktree whose branch is done:

- **merged**: the branch tip is an ancestor of `base`. A branch with no commits of its own counts as merged.
- **squash-merged**: the branch's whole diff from where it left `base` matches a commit in `base` by patch id (`git cherry`).
- **upstream deleted**: the remote branch is gone (per `git ls-remote`) and the branch tip equals the last fetched tip of its remote-tracking ref, so everything was pushed. This catches PRs merged in a way the two checks above miss, such as a rebase merge. It cannot tell such a PR from one that was closed unmerged.

A done worktree is skipped, with the reason printed, when any of these hold:

- a workspace's cwd is in it
- `git status --porcelain` shows changes (uncommitted, or untracked and not ignored)
- its upstream was deleted but it has commits since the last fetched tip
- its upstream was already pruned, so what was pushed is unknown

Commits whose changes are not in `base` keep a branch out of the merged and squash-merged sets, so nothing unpushed is lost there.

Without `--yes`, gc only lists. With `--yes`, it runs:

1. `git worktree remove` (never forced; ignored files go with the worktree)
2. `git branch -D` (squash merges are not ancestors, so `-d` would refuse)
3. deletes the stale remote-tracking ref of an upstream-deleted branch
4. prunes empty `prefix/` directories
5. `git worktree prune` per repo

Detached worktrees and unmerged branches are not listed.

## Testing

- `cargo test` runs `crates/ftmd/tests/agents.rs`: ftmd on a private socket with temp `HOME`/`XDG_*` and `FTM_AGENT_CMD` set to a fake agent that drives the real `ftm` CLI (nesting, `--detached`, status, worktree copy + setup, close/reparent/kill, worktree removal); `crates/ftmd/tests/gc.rs` runs `ftm gc` against a repo with a bare origin.
- `scripts/agent-smoke.sh` is an opt-in live check with real Claude (Haiku): an isolated ftmd, a leader agent that spawns a child, and assertions on its results file, `ftm ls --json` and `ftm capture`. It costs a little usage, so it is not part of `cargo test`. Hook-driven status checks get added with milestone 5.

## Claude Code integration

ftmd writes hook settings to `<socket stem>.claude-settings.json` next to its socket (`ftmd.claude-settings.json` for the default one) and starts every agent with `claude --settings <that file>`, so the user's own `~/.claude/settings.json` is untouched (hook entries from both apply). Each hook runs `<ftm next to ftmd> status <s> >/dev/null 2>&1 || true` with a 5s timeout: silent (a `UserPromptSubmit` hook's stdout would be fed to Claude), never failing, and a no-op outside FTM.

| Hook (matcher) | Status |
|---|---|
| `UserPromptSubmit`, `PostToolUse` | `working` |
| `PermissionRequest` (fires as soon as a tool needs approval) | `needs_input` |
| `PreToolUse` (`AskUserQuestion`) | `needs_input` |
| `Notification` (`permission_prompt\|elicitation_dialog\|elicitation_url_dialog`) | `needs_input` |
| `Stop`, `StopFailure` | `idle` |
| `SessionEnd` | `done` (best effort: a killed session may not send it) |

The 60s `idle_prompt` notification is deliberately not mapped, so a finished agent never turns red. `PostToolUse` rather than `PreToolUse` marks work so it cannot race the `AskUserQuestion` hook, and it clears `needs_input` once a prompt is answered. In the app, agent rows show a dot (starting blue, working green, needs input red, idle grey, done hollow grey, failed hollow red, finished but unseen yellow; CSS variables `--status-*`, taken from the theme's palette and nudged until they read against the sidebar); terminals show none. `ftm ls` marks unseen workspaces `(unseen)`.

`needs_input` also raises a desktop notification (agent name, "needs input"), plays the needs-input sound, and badges every ancestor row with the count of agents below it that need input. A workspace turning unseen plays the finished sound, unless it is the selected workspace and the window has focus: then the app marks it seen at once and stays quiet. Selecting an unseen workspace (or focusing the window on one) marks it seen. A session ending after the agent was already idle does not count as finishing again. Agents are started with `claude -n <name> "<brief>"`, so `SendMessage` between leader and workers keeps working as today. `sonarx:spawn-repo-agent` and `sonarx:agent-shutdown` switch from `supacode ...` to `ftm spawn` / `ftm close`.

## Settings

The app's settings live in `~/.config/ftm/config.toml` (`$XDG_CONFIG_HOME/ftm/...` when set) and are edited from the sidebar's ⚙; Save rewrites only the keys the app owns, Cancel reverts any live preview.

```toml
[terminal]
font_family = "Menlo"
font_size = 13
ligatures = false
line_height = 1.0

[theme]
name = "Supacode Dark"   # or follow_system = true with dark/light

[sounds]
needs_input = "Nudge"    # a name from the sound list; "" plays nothing
completed = "Chime"
volume = 0.7             # 0 to 1

[updates]
check = true             # look for a newer release; unset means only on a pacman ftm-bin/ftm-nightly-bin install
channel = "nightly"      # or "stable"; unset means this build's own
```

`[updates]` keys are left out while unset, so Save removes one set back to automatic (and the section if that empties it).

Themes are Ghostty theme files: built in, in `~/.config/ftm/themes`, or found in Ghostty's and Supacode's theme folders. Sounds come from the app's built-ins (`app/src-tauri/sounds`, synthesized by `scripts/gen-sounds.py`), `~/.config/ftm/sounds` (wav, aiff, ogg/oga, mp3, m4a, caf, flac; named by file stem), macOS `/System/Library/Sounds` and freedesktop `/usr/share/sounds/freedesktop/stereo`; on a name clash yours win over built-ins over the system's. The webview plays them (the bytes come over IPC), and the picker hides formats it cannot play: WKWebView has no Ogg Vorbis, and WebKitGTK plays what its GStreamer plugins decode. Picking a sound in the dropdown plays it; ▶ replays it.

## UI

Sidebar with a `+` button: pick terminal or agent, pick a folder; if it is a git repo, optionally tick "worktree" and enter a branch (plus "copy my changes"). Rows nest by parent and show status, branch, and PR link. Right-click a row (or double-click its name) to rename it inline: Enter saves, Esc cancels, an empty name goes back to the spawn name. A rename sets the workspace's `title`, which is shown instead of `name`; `name` stays as spawned because it is `FTM_NAME` for every pane opened later and the agent's Claude session name (the `SendMessage` address). `ftm ls` prints `title (name)` when they differ. Right-click a top-level row (or `ftm pin`/`ftm unpin`) to move it in or out of the Pinned section; a child stays under its parent while that parent exists, so ftmd refuses to pin it, and `ftm ls` marks pinned workspaces `(pinned)`. Each workspace has tabs; each tab splits into panes.

Must run on macOS and Linux as a standalone desktop app (never inside a browser).

**Decision: Tauri v2 + xterm.js.** Tauri renders in the OS webview (WKWebView on macOS, WebKitGTK on Linux) inside its own native window. gpui stays the fallback if xterm.js rendering or webview quirks become a problem.

| Option | Terminal | Notes |
|---|---|---|
| **Tauri v2 + xterm.js** (chosen) | xterm.js WebGL | Rust backend, TS frontend. Proven for TUIs (VS Code). Fastest path to the full sidebar/tabs/splits UI. |
| gpui + alacritty_terminal | alacritty_terminal grid, GPU rendered | All Rust, native feel. Zed's terminal panel is the reference. gpui's API is young and thinly documented. |
| libghostty | Ghostty | Ghostty's own frontends are Swift (macOS) and GTK (Linux): two UIs to build. Not worth it for a cross-platform app. |

The daemon/CLI are independent of this choice; the app is just another socket client plus a PTY stream consumer.

## PTY persistence

Each pane runs under a holder: `ftmd pty-holder`, the same binary in a hidden mode, started in its own session so nothing aimed at ftmd (a signal to its process group, its terminal closing) reaches it. The holder owns the PTY master and the pane's process tree, keeps the last 1 MiB of output, and serves one ftmd at a time on `<ftmd socket>.d/<pane>.sock` with length-prefixed frames (`crates/ftmd/src/frame.rs`): `Hello{version, rows, cols}` then the whole buffer then live output to ftmd; `Input`, `Resize` and `Kill` back. A new connection replaces the old one, which is how a restarted ftmd takes over. A holder whose processes are gone before any ftmd has connected (a startup command that fails at once) keeps its socket and output for up to 5s, so ftmd still gets them. On `Kill` the holder hangs up the shell's process group and the terminal's foreground group (an agent run by an interactive shell has its own group and ignores SIGHUP), SIGKILLs what is left after 2s, and exits once the terminal is empty, removing its socket; ftmd waits for that before `--remove-worktree` deletes anything.

ftmd keeps its own copy of each pane's output and feeds it through a headless `vt100` emulator, so `ftm capture` reads the screen and scrollback as text, `ftm attach` starts with a redraw of the current screen, and `Subscribe` replays raw output to the app. It saves the workspace tree (workspaces, tabs, layouts, statuses) to `$XDG_STATE_HOME/ftm/<socket path>.json` (`~/.local/state/ftm/...`) on every change, keyed by socket so a dev or test daemon never picks up another daemon's panes, and removes the file when the tree is empty. On start it reloads the tree, reconnects to each pane's holder and rebuilds its screen from the holder's buffer. A pane whose holder is gone (its processes exited while ftmd was down, or the machine rebooted) is dropped from its layout; the workspace stays, possibly without tabs. Nothing is restarted after a reboot. Stopping ftmd (SIGTERM) leaves every pane running; closing a pane or workspace is what kills it.

The app is just a client: quitting it changes nothing, and on launch it starts ftmd if needed, then restores its window geometry (`tauri-plugin-window-state`) and the selected workspace and tabs (`localStorage`).

**Upgrading.** `scripts/install.sh`, `scripts/dev.sh` (each on its own socket) or any SIGTERM restarts ftmd on new code without touching panes, but each holder keeps running the binary it was started from until its pane closes; new panes get the new holder. ftmd and holders agree on `frame::VERSION`: a change to the frames bumps it, and a newer ftmd then cannot reattach older holders (it logs `pane holder speaks version N`, drops those panes from the layout and leaves their processes running unattached), so close panes before upgrading across a version bump. The frame protocol is still version 1.

ftmd also upgrades itself (`crates/ftmd/src/upgrade.rs`). It polls the file it was started from (resolved at startup, so a dev ftmd watches `target/debug/ftmd` and a nightly one its own bundle) every 2s for a new inode, size or mtime, and acts once the file has held still for 2s. It runs the new file with `--handshake`, which prints `{"version", "frame", "channel"}`. If that fails, or the frame version differs, it stays put and sends `UpdateBlocked` (replayed on every subscribe), which the app shows as a notice: switching would lose every pane. It also stays put, with an `UpdateBlocked` saying why, when the channel differs (`stable` or `nightly`, from `FTM_CHANNEL`). On Linux the two channels' packages replace each other at the same `/usr/bin` path, and the other channel's ftmd belongs on its own socket, so the old ftmd keeps its panes until it exits and the new channel starts a daemon of its own. A build without `channel` (older than the field) is treated as the same channel. Otherwise it saves the tree and keeps the state lock, then starts the new ftmd in its own session with the listening socket (`FTMD_LISTEN_FD`) and a readiness pipe (`FTMD_READY_FD`). Both are removed from the environment before any thread starts. The new ftmd checks that the fd is a listening stream socket bound to its socket path and that the saved tree parses, then signals ready. The old one exits on that signal, still holding the lock, so none of its pane readers can save over the tree. The new ftmd waits to be reparented and then restores as after any restart. The socket stays bound and listening throughout: clients queue instead of finding nothing and starting a second ftmd. If the new ftmd exits, fails a check or is not ready within 15s, the old one kills it, releases the lock, carries on serving and sends `UpdateBlocked`. On macOS, holders spawned between the swap and the switch run the new binary; on Linux they run the old one through `/proc/self/exe`, since the replaced path reads as `(deleted)` there. On macOS, a new binary with the same frame version is harmless; with a different one the `Hello` check refuses them, the pane fails to open with an error the app shows, and the stray holder is killed. The app pings on each reconnect of its event stream and shows a notice while the daemon's version differs from its own.

An ftmd from before holders existed owns its PTYs directly, so stopping it ends its panes; both scripts detect that (panes without holder sockets) and ask first.

**Update check.** The app's `check_update` command (`app/src-tauri/src/update.rs`) runs about 10s after launch, every 6h, and after a settings change to `[updates]`. It goes through `curl` (unauthenticated, so no new HTTP dependency). `[updates] check` decides whether it runs. Unset, it runs only on an install it can upgrade: a release build under `/usr/bin` that `pacman -Qqo` says belongs to `ftm-bin` or `ftm-nightly-bin`. `[updates] channel` (unset: `paths::CHANNEL`, this build's) picks where to look. Stable reads `releases/latest` and takes it only if it is a published, non-pre-release semver with no pre-release part. Nightly reads `releases?per_page=10` (nightly.yml keeps 5) and takes the highest semver among published pre-releases tagged `-nightly.`, which skips an `-rc.1`. On the build's own channel only a newer version is offered. The other channel's newest is always offered, as a switch (`Update.switch`), whatever its version: the two are different packages, so this is a replacement rather than an upgrade. On an installable build the result carries a script: `mktemp -d`, `curl` the release's `PKGBUILD` asset (`ftm-bin` or `ftm-nightly-bin`), `makepkg -si`. A release without that asset yet (it is attached after publishing) offers nothing until the next check. The build dir is removed on success and kept on failure. The script has no `--noconfirm`. The pane is interactive anyway, and on a switch pacman asks "Remove ftm-bin?", which `--noconfirm` answers with its default, no, failing the install. The notice's Update (Switch for a switch, with a warning that the new channel starts a fresh daemon and the open panes stay behind) spawns an "FTM <version> update" terminal ("FTM Nightly <version> switch" and the like) in `~` with that script as its `command`. After a switch, the old ftmd sees the other channel in `--handshake`, stays put and sends `UpdateBlocked`. It keeps the update pane and every other pane until it exits, and the new channel's app starts its own daemon. sudo can prompt in the pane, and the install then replaces `/usr/bin/ftmd`, which triggers the self-upgrade above; the update pane's holder keeps running through it. Dismiss stores the version in `localStorage` (`ftm.dismissedRelease`). If the daemon's version differs from the app's (an older ftmd would ignore `command` and open a bare shell), the notice shows the script to run by hand instead. Other installs get a notice without Update.

`SpawnRequest.command` needed no protocol or `frame::VERSION` bump. Frames are only spoken between ftmd and its holders, and the command reaches the holder as an environment variable at spawn. On the client socket, `command` is `#[serde(default)]` and serde ignores unknown fields, so an older ftmd reads a newer app's spawn as a plain terminal and an older client's spawn still parses.

## Milestones

1. **Daemon + CLI**: workspace tree, parent from env, status, tree saved to disk.
2. **Worktrees**: repo config, worktree creation with the copy steps, setup script, `ftm gc`.
3. **PTYs**: per-pane holders that survive ftmd restarts, headless terminal state, `ftm attach` as a minimal terminal client.
4. **App**: sidebar, `+` flow, tabs, splits, status dots, notifications.
5. **Claude integration**: hooks, port the sonarx spawn/shutdown skills, retire Supacode.

## Open questions

- Should closing a leader close its children, or reparent them to the top level?
