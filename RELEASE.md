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
- **build**: macOS aarch64 and x86_64 (`.dmg`, signed ad hoc), Linux x86_64 and aarch64 on Ubuntu 22.04 (`.deb`,
  `.rpm`). `scripts/sidecars.sh` builds `ftm` and `ftmd` for the target first, and `tauri.bundle.conf.json` bundles
  them next to `ftm-app`: in `FTM.app/Contents/MacOS` and in `/usr/bin`.
- **publish**: once every asset is up, makes the release public.
- **homebrew**: renders `packaging/homebrew/ftm.rb` with the `.dmg` checksums and pushes it to
  [Frazzer951/homebrew-ftm](https://github.com/Frazzer951/homebrew-ftm) as `Casks/ftm.rb`.
- **aur**: renders `packaging/aur/ftm-bin/PKGBUILD` with the `.deb` checksums and pushes it, with a fresh `.SRCINFO`,
  to [ftm-bin](https://aur.archlinux.org/packages/ftm-bin).

A pre-release tag (`v0.1.0-rc.1`) runs the same build and publishes a GitHub pre-release, but the homebrew and aur jobs
only render the cask and PKGBUILD and upload them as workflow artifacts; they don't push. Use one to try the pipeline.

If a build fails, fix it on `main`, delete the draft release and the tag (`git push origin :v<version>`), and cut the
release again.

## Secrets

Repository secrets in Settings → Secrets and variables → Actions:

| Secret | What |
|---|---|
| `HOMEBREW_TAP_TOKEN` | A fine-grained token with Contents: read and write on `Frazzer951/homebrew-ftm` only |
| `AUR_SSH_PRIVATE_KEY` | The private half of the SSH key registered on the AUR account (below) |

## One-time AUR setup

1. Make an account at <https://aur.archlinux.org/register>.
2. Make a key just for this: `ssh-keygen -t ed25519 -C ftm-bin-ci -f ~/.ssh/aur_ftm -N ''`. Paste
   `~/.ssh/aur_ftm.pub` into My Account → SSH Public Key, and `~/.ssh/aur_ftm` into the `AUR_SSH_PRIVATE_KEY` secret.
3. Create the package. The AUR creates a package the first time anyone pushes to a new name, so the first release's
   aur job normally does it. If that push is refused (say, the key isn't registered yet), push the rendered PKGBUILD
   by hand once the release's assets are downloadable:

   ```sh
   # the PKGBUILD the release's aur job rendered, from its workflow artifacts
   gh run download <run-id> -n aur-pkgbuild -D /tmp/ftm-bin-pkgbuild
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur_ftm' git clone ssh://aur@aur.archlinux.org/ftm-bin.git
   cd ftm-bin
   cp /tmp/ftm-bin-pkgbuild/PKGBUILD .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO && git commit -m "ftm-bin <version>"
   GIT_SSH_COMMAND='ssh -i ~/.ssh/aur_ftm' git push origin master
   ```

   Later releases update it from CI.

## Signing

macOS builds are signed ad hoc, not notarized. The cask strips the quarantine attribute after install, so Gatekeeper
lets it open. A `.dmg` downloaded by hand needs `xattr -dr com.apple.quarantine /Applications/FTM.app` after copying
the app.
