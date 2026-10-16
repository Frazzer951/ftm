# Contributing to FTM

Issues and pull requests are welcome. For anything bigger than a small fix, open an issue first so we can agree on
the approach before you write the code. [docs/design.md](docs/design.md) explains how the daemon, panes, worktrees
and agents fit together.

## Setup

You need a stable Rust toolchain and Node 24 with npm. On Linux, also install the Tauri system libraries, WebKitGTK
4.1 and GStreamer's good plugins. On Debian or Ubuntu:

```sh
sudo apt install build-essential pkg-config libssl-dev libxdo-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev gstreamer1.0-plugins-good
```

On Arch: `pacman -S webkit2gtk-4.1 gst-plugins-good`.

## Run your changes

```sh
scripts/dev.sh
```

It builds `ftmd`, `ftm` and the app, restarts the dev daemon on the new build and runs the app with `tauri dev`.
Ctrl+C stops the app; the dev ftmd and its panes keep running. Rerun it after changing code.

Dev runs on its own socket, `ftmd-dev.sock` next to the default `ftmd.sock`, and its own app identifier, so an
installed FTM keeps its daemon and sessions. It refuses to run on the default socket. To run more than one dev
instance, give each its own socket:

```sh
FTM_DEV_SOCKET=$XDG_RUNTIME_DIR/ftm/ftmd-mine.sock scripts/dev.sh
```

Dev and installed FTM share `~/.config/ftm` (settings, themes, sounds, repo config). The dev daemon logs to
`ftmd-dev.log` next to its socket.

To try the CLI against the dev daemon, point it at the same socket:
`FTM_SOCKET=$XDG_RUNTIME_DIR/ftm/ftmd-dev.sock ./target/debug/ftm ls`. On macOS the socket folder is
`~/Library/Caches/ftm` unless `XDG_RUNTIME_DIR` is set.

## Checks

CI runs these on every pull request (Rust on Linux and macOS). Run them before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

ftmd's tests start real pane holders and shells. `cargo test -p ftm-core` alone is quick when you only touched
shared code. Before adding one, read the notes at the top of `crates/ftmd/tests/common/mod.rs` on keeping it from
flaking.

The app's frontend lives in `app/`:

```sh
cd app
npm ci
npm run lint    # biome, warnings fail
npm run build   # tsc --noEmit and vite build
```

Biome only lints here; its formatter is off, so match the surrounding style. Rust formatting follows
`rustfmt.toml`.

If a change alters behavior, settings or the protocol, update the README, `app/README.md` or `docs/design.md` in the
same pull request.

## Commits

Write commits as `type(scope): summary`, in the imperative and lower case:

```
feat(app): open a workspace's folder in VS Code, GitKraken or Finder
fix(ftmd): start pane holders from the running build on Linux
docs: describe the update check
```

- **type**: `feat`, `fix`, `docs`, `ci`, `chore`, `refactor`, `test`
- **scope**: the part it touches: `ftmd`, `cli`, `app`, `worktree`, `packaging`, `scripts`, `release`, `nightly`

[git-cliff](https://git-cliff.org) builds `CHANGELOG.md` from these at release time (see [cliff.toml](cliff.toml)
and [RELEASE.md](RELEASE.md)), grouping entries by scope and using each commit's first line as the entry, so make
that line read well on its own. Keep one logical change per commit.

## Pull requests

Branch from `main` (`feature/...`, `fix/...`, `chore/...`) and open the pull request against `main`. Fill in the
template: what and why, how you tested it, and `Refs #<issue>`. A first-time contributor's CI run waits for a
maintainer to approve it.

By contributing you agree that your work is released under the [MIT license](LICENSE).
