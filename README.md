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
| `packaging/` | Homebrew cask and Arch `PKGBUILD` templates the release workflow fills in, see [RELEASE.md](RELEASE.md) |

## Install

Every release ships the app with the `ftm` CLI and the `ftmd` daemon inside it. Upgrade through the package manager
you installed with (`brew upgrade`, or the newer `.deb`, `.rpm` or `PKGBUILD`): within a few seconds the running ftmd
moves itself onto the new build, keeping every session, and the app asks to be reopened.

The app checks GitHub for a newer release about 10 seconds after it starts, then every 6 hours, and shows a notice
when one is out. On an Arch install from a release `PKGBUILD` (`ftm-bin` or `ftm-nightly-bin`, owned by pacman under
`/usr/bin`), the notice has an **Update** button. It opens an "FTM <version> update" terminal workspace that downloads
the new `PKGBUILD` and runs `makepkg -si` there, so you can watch the build, confirm pacman's questions and type your
sudo password. **Dismiss** hides that version until a newer one comes out. Other installs only check when you turn it
on, and then just say a release is out.

The check follows the build's own channel unless you pick one. Pick the other channel and the app offers its newest
build as a **Switch**, whatever its version. Switching replaces the installed package (pacman asks before removing
the other one) and starts a fresh daemon: open panes and sessions stay with the old one and don't carry over (see
below). Set both under ⚙ → Updates, or in `~/.config/ftm/config.toml`:

```toml
[updates]
check = false        # never check; true checks on any install. Unset: only on a pacman ftm-bin/ftm-nightly-bin install
channel = "nightly"  # or "stable". Unset: this build's own channel
```

**macOS** (Apple silicon and Intel), with Homebrew:

```sh
brew install --cask frazzer951/ftm/ftm
```

This installs `FTM.app` and links `ftm` onto `PATH`. The app is signed ad hoc, not notarized; the cask clears the
quarantine flag so it opens. If you take the `.dmg` from the [releases](https://github.com/Frazzer951/ftm/releases)
instead, run `xattr -dr com.apple.quarantine /Applications/FTM.app` after copying it.

A nightly build of `main` installs next to it as `FTM Nightly.app`, with its own daemon and the CLI linked as
`ftm-nightly`:

```sh
brew install --cask frazzer951/ftm/ftm@nightly
```

**Arch Linux** (x86_64 and aarch64): `ftm-bin` is not yet published to the AUR. Each release carries its
`PKGBUILD`, which repackages the release's `.deb`:

```sh
mkdir ftm-bin && cd ftm-bin
curl -fLO https://github.com/Frazzer951/ftm/releases/latest/download/PKGBUILD
makepkg -si
```

Each nightly pre-release carries an `ftm-nightly-bin` `PKGBUILD` the same way. It conflicts with `ftm-bin`, so
installing one replaces the other. The simplest way in is to set `channel = "nightly"` and take the app's Switch.

**Debian, Ubuntu** (22.04 or newer) and **Fedora** (x86_64 and aarch64): take the `.deb` or `.rpm` for your
architecture from the [latest release](https://github.com/Frazzer951/ftm/releases/latest), then

```sh
sudo apt install ./FTM_<version>_amd64.deb    # or arm64
sudo dnf install ./FTM-<version>-1.x86_64.rpm # or aarch64
```

A nightly build of `main` is the `FTM.Nightly_…` `.deb` or `.rpm` from a
[nightly pre-release](https://github.com/Frazzer951/ftm/releases), installed the same way. On Linux you have either
the nightly (package `ftm-nightly`) or the stable release (package `ftm`), never both: they use the same `/usr/bin`
paths, so installing one removes the other. Switching channels starts a fresh daemon with no sessions. The old
channel's ftmd keeps running from its old build with its panes until you stop it or log out; reach them from the app
window that is still open, from `ftm` inside one of those panes, or by pointing `FTM_SOCKET` at the old socket
(`$XDG_RUNTIME_DIR/ftm/ftmd.sock` for stable, `ftm-nightly/ftmd.sock` for nightly). Switch back while those panes
are still running and the daemon picks them up again.

The Linux packages put `ftm-app`, `ftm` and `ftmd` in `/usr/bin` and add a desktop entry. The app needs WebKitGTK 4.1
and, for status sounds, GStreamer's good plugins (the webview plays audio through `autoaudiosink`); the packages
depend on both.

### From source

On Linux, install WebKitGTK 4.1 and GStreamer's good plugins first. On Arch: `pacman -S webkit2gtk-4.1 gst-plugins-good`.

```sh
scripts/install.sh
```

Builds FTM from this checkout in release mode and installs it: on macOS `FTM.app` (with `ftm` and `ftmd` inside it,
signed ad hoc) in `/Applications`, else `~/Applications`; on Linux `ftm`, `ftmd` and `ftm-app` in `~/.local/lib/ftm`
plus a `.desktop` entry. It links `ftm` into `~/.local/bin` and warns if that is not on `PATH`, then opens the app.

Rerun it to upgrade. It quits the installed app, stops its ftmd and replaces the install; your sessions survive, since
each pane lives in its own holder process that keeps running the old build until the pane closes (see
[PTY persistence](docs/design.md#pty-persistence)). The reopened app starts the new ftmd, which picks every pane back
up. It can run from inside an FTM pane. Options: `--prefix DIR`, `--bin-dir DIR`, `--no-launch`, `--dry-run` (build
and print what it would stop and install); see `scripts/install.sh --help`.

## Develop

```sh
scripts/dev.sh
```

Builds everything, quits the running dev app, restarts the dev ftmd on the new build (sessions survive) and runs the
app with `tauri dev`. Ctrl+C stops the app; ftmd and your sessions keep running. Rerun it after pulling or changing code.

Dev and installed FTM run side by side: dev uses its own socket (`ftmd-dev.sock` next to the default one, or
`FTM_DEV_SOCKET`) and app identifier, so it never touches the installed app, its daemon or its sessions, and vice
versa. Both share `~/.config/ftm` (settings, themes, sounds, repo setup).

`FTM_BACKGROUND=1` (for scripted launches) never takes focus: on macOS the app launches with the Prohibited activation
policy, which the system cannot activate, then becomes an accessory without a Dock icon. Known limit: on macOS its window
then stays hidden, so it is no use for screenshots; any launch that shows a window can take focus. Frontend errors are
printed to the terminal running the app as `[frontend] ...`.

## Repo config

`~/.config/ftm/repos/<repo>.toml` sets how FTM makes worktrees of a repo:

```toml
base = "origin/main"                 # where new branches start (default: the remote's default branch)
setup = "direnv allow"               # POSIX sh run in the worktree before the agent starts
copy_ignored_exclude = ["data"]      # more names/globs to leave out of the gitignored-file copy
```

New worktrees get the source checkout's gitignored files (`.env`, `.envrc`, `.claude/`, ...) except `.venv`, `venv`,
`node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.ruff_cache`, `.mypy_cache`, `.tox` and `.DS_Store` at any
depth. `copy_ignored = false` turns that off; `copy_ignored = [".env", "profiles.yml"]` copies only those globs. See
[docs/design.md](docs/design.md#worktree-creation).

## Try the CLI

```sh
cargo build
./target/debug/ftmd &
LEADER=$(./target/debug/ftm spawn agent --name leader)
FTM_WORKSPACE_ID=$LEADER ./target/debug/ftm spawn agent --path ../some-repo --worktree fix/thing
./target/debug/ftm ls
./target/debug/ftm gc          # merged worktrees; --yes removes them and their branches
```
