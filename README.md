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
| `packaging/` | Homebrew cask and AUR `PKGBUILD` templates the release workflow fills in, see [RELEASE.md](RELEASE.md) |

## Install

Every release ships the app with the `ftm` CLI and the `ftmd` daemon inside it. Upgrade through the package manager
you installed with (`brew upgrade`, your AUR helper, or the newer `.deb`/`.rpm`), then quit and reopen the app.

**macOS** (Apple silicon and Intel), with Homebrew:

```sh
brew install --cask frazzer951/ftm/ftm
```

This installs `FTM.app` and links `ftm` onto `PATH`. The app is signed ad hoc, not notarized; the cask clears the
quarantine flag so it opens. If you take the `.dmg` from the [releases](https://github.com/Frazzer951/ftm/releases)
instead, run `xattr -dr com.apple.quarantine /Applications/FTM.app` after copying it.

**Arch Linux**, from the AUR (x86_64 and aarch64):

```sh
yay -S ftm-bin
```

**Debian, Ubuntu** (22.04 or newer) and **Fedora** (x86_64 and aarch64): take the `.deb` or `.rpm` for your
architecture from the [latest release](https://github.com/Frazzer951/ftm/releases/latest), then

```sh
sudo apt install ./FTM_<version>_amd64.deb    # or arm64
sudo dnf install ./FTM-<version>-1.x86_64.rpm # or aarch64
```

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

## Try the CLI

```sh
cargo build
./target/debug/ftmd &
LEADER=$(./target/debug/ftm spawn agent --name leader)
FTM_WORKSPACE_ID=$LEADER ./target/debug/ftm spawn agent --path ../some-repo --worktree fix/thing
./target/debug/ftm ls
```
