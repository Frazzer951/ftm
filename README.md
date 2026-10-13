# FTM

Frazzer's Terminal Manager. A terminal workspace manager for running Claude Code agents and shells across repos and git worktrees, with agents nested under the agent that spawned them. macOS and Linux.

See [docs/design.md](docs/design.md) for the design.

## Layout

| Path | What |
|---|---|
| `crates/ftm-core` | Shared model, socket protocol, paths, repo config |
| `crates/ftmd` | Daemon: owns workspaces, PTYs and worktrees |
| `crates/ftm` | CLI client |
| `app/` | Desktop UI (Tauri v2 + xterm.js), see [app/README.md](app/README.md) |

## Run it

```sh
scripts/dev.sh
```

Builds everything, quits a running app, restarts ftmd on the new build (sessions survive: each pane lives in its own
holder process) and runs the app with `tauri dev`. Ctrl+C stops the app; ftmd and your sessions keep running. Rerun it
after pulling or changing code. Panes started by an older build keep running that build's holder until they close; see
[PTY persistence](docs/design.md#pty-persistence).

`FTM_BACKGROUND=1` (for scripted launches) never takes focus: on macOS the app launches with the Prohibited activation
policy, which the system cannot activate, then becomes an accessory without a Dock icon. Known limit: on macOS its window
then stays hidden, so it is no use for screenshots; any launch that shows a window can take focus. Frontend errors are
printed to the terminal running the app as `[frontend] ...`.

## Try the CLI

```sh
cargo build
./target/debug/ftmd &
LEADER=$(./target/debug/ftm spawn agent --name leader)
FTM_WORKSPACE_ID=$LEADER ./target/debug/ftm spawn agent --path ../some-repo --worktree fix/thing
./target/debug/ftm ls
```
