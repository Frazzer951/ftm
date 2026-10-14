# Releasing FTM

One version covers the whole workspace (`ftm`, `ftmd`, `ftm-core`, the app) and `app/package.json`. The app takes its
version from Cargo, so `tauri.conf.json` has none.

## Cut a release

From an up-to-date `main` with a clean tree:

```sh
cargo release 0.1.0      # dry run: check the version bump, the changelog and the tag
cargo release 0.1.0 -x   # do it
```

Pass a level (`patch`, `minor`, `major`) in place of the version if you like. With `-x`, cargo-release:

1. sets the version in every crate and in `Cargo.lock`,
2. runs `scripts/release.sh`, which syncs `app/package.json` and `app/package-lock.json` and writes `CHANGELOG.md`
   with git-cliff (`cliff.toml` groups commits by their `Component:` prefix),
3. commits `chore(release): prepare for release <version>`, tags `v<version>` and pushes both.

Nothing is published to crates.io.

The tag starts `.github/workflows/release.yml`:

- **draft**: checks the tag matches the workspace version and creates a draft release whose notes are git-cliff's
  section for that tag.
- **build** (`.github/workflows/build.yml`, shared with the nightly): macOS aarch64 and x86_64 (`.dmg`, signed ad hoc), Linux x86_64 and aarch64 on Ubuntu 22.04 (`.deb`,
  `.rpm`). `scripts/sidecars.sh` builds `ftm` and `ftmd` for the target first, and `tauri.bundle.conf.json` bundles
  them next to `ftm-app`: in `FTM.app/Contents/MacOS` and in `/usr/bin`.
- **publish**: once every asset is up, makes the release public.
- **homebrew**: renders `packaging/homebrew/ftm.rb` with the `.dmg` checksums and pushes it to
  [Frazzer951/homebrew-ftm](https://github.com/Frazzer951/homebrew-ftm) as `Casks/ftm.rb`.
- **aur**: renders `packaging/aur/ftm-bin/PKGBUILD` with the `.deb` checksums and attaches it to the release as
  `PKGBUILD`. It pushes it, with a fresh `.SRCINFO`, to [ftm-bin](https://aur.archlinux.org/packages/ftm-bin) only
  when the repository variable `AUR_PUBLISH` is `true`. It isn't yet: there is no AUR account (see below).

A pre-release tag (`v0.1.0-rc.1`) runs the same build and publishes a GitHub pre-release, but the homebrew and aur jobs
push nothing: they render the cask and PKGBUILD and upload them as workflow artifacts (the PKGBUILD is attached to the
pre-release too). Use one to try the pipeline.

If a build fails, fix it on `main`, delete the draft release and the tag (`git push origin :v<version>`), and cut the
release again.

## Nightlies

`.github/workflows/nightly.yml` builds **FTM Nightly** from `main` on every push to it. Pushes queue: one runs at a
time, and only the newest waiting push is kept. A release pushes its version-bump commit to `main` along with the tag,
so it also starts a nightly, built at the next patch. Run it by hand from Actions → Nightly → Run workflow (on `main`).
A run skips the build when `main` is still at the commit of the last nightly; tick *force* to build it anyway.

- **Version**: the patch after the committed version, the UTC date and the workflow's run number,
  `0.1.2-nightly.20261014.7`, so each nightly sorts above the last release and every nightly before it, however many
  run in a day. A committed pre-release keeps its core version: `0.2.0-rc.1` gives `0.2.0-nightly.<date>.<run>`.
  `scripts/set-version.sh` stamps it into the build; nothing is committed.
- **Identity**: the build sets `FTM_CHANNEL=nightly`, read by `ftm-core` at compile time, and merges
  `tauri.nightly.conf.json`: the app is `FTM Nightly.app` (`dev.frazzer.ftm.nightly`) and `ftm`, `ftmd` and the app
  default to `<cache or XDG_RUNTIME_DIR>/ftm-nightly/ftmd.sock`, so it runs its own daemon, panes and session state
  next to the stable one. `FTM_SOCKET` still overrides it. `~/.config/ftm` (settings, themes, repo setup) and the
  worktrees dir are shared.
- **Release**: a GitHub pre-release tagged `v<version>`, never marked latest. Only the newest 5 are kept; older ones
  and their tags are deleted, as are drafts left by failed runs.
- **Homebrew**: renders `packaging/homebrew/ftm@nightly.rb` and pushes it to the tap as `Casks/ftm@nightly.rb`
  (`brew install --cask frazzer951/ftm/ftm@nightly`), which links the CLI as `ftm-nightly`. It never touches
  `Casks/ftm.rb`.
- **PKGBUILD**: renders `packaging/aur/ftm-nightly-bin/PKGBUILD` with the nightly `.deb` checksums and attaches it to
  the pre-release as `PKGBUILD`, after publishing, as release.yml does for `ftm-bin`. It is never pushed to the AUR;
  it is what the app's update check installs on the nightly channel. pacman's version can't contain `-`, so
  `0.1.4-nightly.20261014.11` becomes `0.1.4_nightly.20261014.11`. `vercmp` orders that after every earlier nightly and
  after the previous stable release. `ftm-nightly-bin` and `ftm-bin` both provide `ftm` and conflict with each other,
  so installing one asks to remove the other.

The Linux `.deb` and `.rpm` are attached too. They install the same `/usr/bin` paths as stable, so on Linux you have
one channel or the other, never both. The package is `ftm-nightly` against stable's `ftm`, and each declares the other
in `Conflicts` plus `Replaces` (deb) or `Obsoletes` (rpm) (`bundle.linux` in `tauri.conf.json` and
`tauri.nightly.conf.json`). So `apt install` or `dnf install` of either one removes the other and doesn't fail on
the shared files. Switching channels gives you a fresh daemon and fresh sessions. The running ftmd sees the other
channel's build in `--handshake` and doesn't hand over to it. It keeps serving its panes on its own socket until it
exits, and the new channel's app and CLI start a daemon on theirs.

To build one locally: `scripts/set-version.sh <version>`, then with `FTM_CHANNEL=nightly` exported run
`scripts/sidecars.sh` and `npx tauri build --config src-tauri/tauri.bundle.conf.json --config
src-tauri/tauri.nightly.conf.json` in `app`. Revert the stamped manifests afterwards.

## Secrets

Repository secrets in Settings → Secrets and variables → Actions:

| Secret | What |
|---|---|
| `HOMEBREW_TAP_TOKEN` | A fine-grained token with Contents: read and write on `Frazzer951/homebrew-ftm` only, used by the release and nightly workflows |

## Publishing to the AUR (not yet on)

`ftm-bin` is not on the AUR yet: there is no account to maintain it, and AUR registration is closed. Until it is, each
release carries a `PKGBUILD` asset to build from (see the README). To turn publishing on:

1. Make an account at <https://aur.archlinux.org/register> once registration reopens.
2. Make a key just for this: `ssh-keygen -t ed25519 -C ftm-bin-ci -f ~/.ssh/aur_ftm -N ''`. Paste
   `~/.ssh/aur_ftm.pub` into My Account → SSH Public Key, and `~/.ssh/aur_ftm` into a new repository secret,
   `AUR_SSH_PRIVATE_KEY`.
3. Set the repository variable `AUR_PUBLISH` to `true` (Settings → Secrets and variables → Actions → Variables).
4. Create the package. The AUR creates a package the first time anyone pushes to a new name, so the next release's aur
   job normally does it. To do it now from the latest release, or if that push is refused:

   ```sh
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur_ftm' git clone ssh://aur@aur.archlinux.org/ftm-bin.git
   cd ftm-bin
   curl -fLO https://github.com/Frazzer951/ftm/releases/latest/download/PKGBUILD
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO && git commit -m "ftm-bin <version>"
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur_ftm' git push origin master
   ```

   Later releases update it from CI. Then change the README's Arch section to `yay -S ftm-bin`.

## Signing

macOS builds are signed ad hoc, not notarized. The cask strips the quarantine attribute after install, so Gatekeeper
lets it open. A `.dmg` downloaded by hand needs `xattr -dr com.apple.quarantine /Applications/FTM.app` after copying
the app.
