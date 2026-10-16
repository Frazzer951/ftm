# Contributing to FTM

Issues and pull requests are welcome. For anything bigger than a small fix, open an issue first so we can agree on
the approach before you write the code. [docs/design.md](docs/design.md) explains how the daemon, panes, worktrees
and agents fit together.

## Setup

You need a stable Rust toolchain (1.90 or newer) and Node 24 with npm. On Linux, also install the Tauri system
libraries, WebKitGTK 4.1 and GStreamer's good plugins. On Debian or Ubuntu:

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
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
```

The workspace lints in the root `Cargo.toml` apply to every crate. Every `unsafe` block needs a `// SAFETY:`
comment that says why it is sound, and an `#[allow]` or `#[expect]` needs a `reason = "..."`.

ftmd's tests start real pane holders and shells. `cargo test -p ftm-core` alone is quick when you only touched
shared code. Before adding one, read the notes at the top of `crates/ftmd/tests/common/mod.rs` on keeping it from
flaking.

The app's frontend lives in `app/`:

```sh
cd app
npm ci
npm run lint       # biome, warnings fail
npm run typecheck  # tsc over src/ and tests/
npm test           # node --test
npm run build      # tsc --noEmit and vite build
```

Biome only lints here; its formatter is off, so match the surrounding style. Rust formatting follows
`rustfmt.toml`.

Shell scripts and workflows are linted too. With [actionlint](https://github.com/rhysd/actionlint) and
[ShellCheck](https://www.shellcheck.net) installed (actionlint runs ShellCheck over each workflow's `run:` blocks):

```sh
actionlint
shellcheck scripts/*.sh scripts/tests/*.sh
for test in scripts/tests/*.sh; do "$test"; done
```

`scripts/tests/` covers the release scripts (`render-package.sh`, `nightly-version.sh`) against local fixtures.

A pull request that touches `packaging/`, `scripts/`, the Tauri configs or the workflows also builds the release
bundles (a Linux `.deb` and `.rpm`, a macOS nightly `.dmg`) and dry-runs `scripts/install.sh` on both, so packaging
breakage shows up before the nightly. To try the install dry run yourself without touching your installed FTM:

```sh
scripts/install.sh --dry-run --no-launch --socket /tmp/ftm-dry/ftmd.sock --prefix /tmp/ftm-dry/prefix \
  --bin-dir /tmp/ftm-dry/bin
```

A weekly audit runs [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) against `deny.toml` (advisories,
licenses, banned sources) and `npm audit --omit=dev --audit-level=high` in `app/`. It also runs on pull requests
that change `Cargo.lock`, `deny.toml` or `app/package-lock.json`. Locally: `cargo deny check`. A new dependency
under a license that `deny.toml` doesn't list fails it; add the license only if it is MIT-compatible.

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
