# Changelog

## [0.1.7] - 2026-10-15

### App

- fix(app): smooth wheel scrolling in full-screen apps such as Claude Code's fullscreen mode

## [0.1.6] - 2026-10-15

### App

- feat(app): drag to rearrange sidebar folders, persisted by ftmd
- fix(app): play status sounds with Web Audio so FTM never takes the media keys
- fix(app): suspend the audio context once a status sound finishes

### Docs

- docs: folder reordering

## [0.1.5] - 2026-10-15

### Daemon

- fix(ftmd): never signal a recycled process group, and log pane exits
- test(ftmd): check pane exits are logged with their cause

### App

- fix(app): show the font preview under the font fields in Settings
- feat(app): kitty keyboard protocol in panes, and Shift+Enter as ESC CR without it
- feat(app): Ctrl+Shift+C/V and Shift+Insert copy and paste in Linux panes
- feat(app): word editing keys in Linux panes
- feat(app): Option as Meta on macOS, with a setting for one side or off
- fix(app): don't answer terminal queries replayed from the daemon's buffer

### Docs

- docs: add issue forms, a PR template and CONTRIBUTING.md

### CI and releases

- fix(release): group changelog entries by a conventional commit's scope

### Other

- Deps: Bump core-text from 20.1.0 to 22.0.0
- Deps: Bump typescript from 5.9.3 to 7.0.2 in /app

## [0.1.4] - 2026-10-14

### App

- feat(app): check for a newer release and install it from its PKGBUILD
- feat(app): pick the update channel and switch between stable and nightly
- fix(app): say how to finish a channel switch, and clear the notice when nothing is offered
- fix(app): make the update script's build dir under TMPDIR on BSD mktemp too

### Docs

- docs: describe the update check and a terminal's spawn command

## [0.1.3] - 2026-10-14

### Daemon

- feat(worktree): copy gitignored files into new worktrees by default
- fix(worktree): copy gitignored files best effort

### CLI

- fix(cli): press Enter in its own write in ftm send --enter
- feat(cli): add ftm pin and ftm unpin
- feat(cli): add ftm gc for merged worktrees

### App

- feat(app): open a workspace's folder in VS Code, GitKraken or Finder

### CI and releases

- fix(packaging): use postflight_steps in the Homebrew casks

## [0.1.2] - 2026-10-14

### Daemon

- fix(ftmd): start pane holders from the running build on Linux

### App

- feat(app): macOS text-editing keys in terminal panes

### Docs

- docs: on Linux, install the nightly or the stable package, not both

### CI and releases

- ci(nightly): build a nightly on every push to main
- fix(release): let set-version.sh resolve the index on a cold runner
- feat(packaging): make the Linux nightly and stable packages replace each other

## [0.1.1] - 2026-10-14

### Daemon

- fix(ftmd): repaint panes when a terminal attaches at their current size
- feat(ftmd): move onto a replaced binary by itself, keeping every pane

### App

- fix(app): stop the macOS Siri / Writing Tools glyph over the terminal
- App: FTM Nightly identity: FTM_CHANNEL=nightly builds default to an ftm-nightly socket dir, tauri.nightly.conf.json names the app and bundle id
- fix(app): log panics and Objective-C exceptions to ftm-app.log
- fix(app): dedupe logged Objective-C exceptions and cap ftm-app.log

### CI and releases

- Release: daily nightly pre-release and ftm@nightly cask, sharing the build with release.yml; keep nightly tags out of release notes

## [0.1.0-rc.2] - 2026-10-14

### Daemon

- ftmd: daemon-owned PTYs, tabs and splits, output streaming
- ftmd: stop a pane's foreground job on close; tidy removed worktrees
- ftmd: unseen flag for finished turns, MarkSeen request
- ftmd, app: name hook settings and the daemon log after the socket
- ftmd: run repo setup in sh, keep a failed agent start on screen
- ftmd: keep a fast-dying pane's output until ftmd attaches; apply the replay before a pane is visible
- ftmd: mark the agent pane before applying the holder's replay

### CLI

- ftm: canonicalize --path

### App

- app: Tauri v2 + xterm.js desktop skeleton
- App: keep a restored window reachable
- Settings: terminal font, size, line height and ligatures
- Settings: font picker with installed families and a live preview
- Settings: decide 'not installed' from the font list
- Themes: Ghostty theme parser, bundled themes, theme listing
- Settings: theme picker
- Sounds: built-in tones, system and user sound listing, [sounds] settings
- App: yellow dot for finished-but-unseen agents, status sounds with a settings picker

### Scripts

- scripts/dev.sh: run the app on this checkout's code, keeping sessions
- dev.sh: reinstall app dependencies when package.json or the lockfile change
- Scripts: install.sh builds and installs FTM as an app; dev.sh gets its own socket

### Docs

- Docs: unseen status, sounds and the settings file

### CI and releases

- CI: fmt, clippy and tests on Linux and macOS, frontend build
- CI: Dependabot for cargo, npm and actions; Biome lint for the frontend
- Release: tauri-action workflow, ftm and ftmd as sidecars, Homebrew cask and AUR ftm-bin, cargo-release and git-cliff
- Release: start the workspace at 0.0.0, RELEASE.md, install instructions
- Release: AUR push behind the AUR_PUBLISH variable; attach the PKGBUILD to each release
- Release: take the draft's id from the create call; publish by id

### Other

- Initial FTM scaffold: core types, daemon, CLI, and design doc
- Agent e2e tests: FTM_AGENT_CMD, ftm capture/send, fake-agent test
- Live agent smoke test; capture reads a headless terminal screen
- Status dots driven by Claude Code hooks
- Sessions survive ftmd restarts: per-pane holders, saved tree, ftm attach
- Owner-only socket, holder and state dirs
- Rename workspaces from the sidebar, the CLI and the agents themselves
- Readable sidebar rows; unnamed agents name themselves; FTM_BIN; FTM_BACKGROUND
- Sidebar sections: Pinned, Active Terminals, Active Agents, Folders
- New-workspace dialog lists recent folders
- FTM_BACKGROUND never takes focus; frontend errors reach the log
- Apply the theme to terminals and the app chrome
- Linux fixes: fish-safe agent startup, WebKitGTK DMA-BUF workaround, DOM renderer on Linux
- cliff: blank line after the changelog header
- Frontend: declare Vite client types so CSS side-effect imports type-check

<!-- generated by git-cliff -->
