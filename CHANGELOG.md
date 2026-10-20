# Changelog

## [0.1.14] - 2026-10-20

### App

- feat(app): new logo, and a Done sound as the default finished sound

## [0.1.13] - 2026-10-19

### Daemon

- fix(ftmd): force hyperlinks only for agents' Claude Code, not panes
- fix(ftmd): random colors draw from a v4 UUID's random bytes only

### App

- feat(app): open URLs in panes with Cmd/Ctrl+click
- feat(app): color new workspaces from the spawn dialog, a folder's color or a setting

### Docs

- docs: describe pane links

### Other

- feat: custom workspace colors from a picker, and ftm spawn --color
- feat: random workspace colors, and children start near their parent's color

## [0.1.12] - 2026-10-17

### App

- fix(app): give workspace colors fixed values instead of the theme's
- feat(app): sort each sidebar section by manual order, name or color
- feat(app): drag the sidebar's edge to resize it

### Other

- feat: give workspaces a sidebar color with ftm color or the row's menu
- feat: drag workspaces to reorder them in the sidebar
- fix: address review findings on the sidebar order, sort and drag

## [0.1.11] - 2026-10-16

### App

- feat(app): show status for Claude sessions in terminal workspaces

## [0.1.10] - 2026-10-16

### Other

- feat: let ftm spawn agent pass extra arguments to Claude
- feat(ftm): show ftmd's features in ftm ping

## [0.1.9] - 2026-10-16

### Daemon

- test(ftmd): write test executables from a child process to stop ETXTBSY flakes
- test(ftmd): remove each test's temp dirs and hook settings file when it ends
- test(ftmd): give every test daemon its own config and data dirs
- test(ftmd): pass the agent command to script() instead of setting FTM_AGENT_CMD in a test
- test(ftmd): keep the user's git config and login profile out of the tests
- test(ftmd): check the mode of ftmd's state dir by name, not as the first entry
- fix(ftmd): cap holder frame sizes and test malformed frames
- fix(ftmd): keep serving when a pane stops reading its input
- fix(ftmd): make Kill finish off jobs that outlive the shell
- fix(ftmd): reconnect to a holder that gave up on a stalled ftmd
- fix(ftmd): clamp zero-size resizes instead of panicking the emulator
- fix(ftmd): keep accepting after a failed accept instead of exiting
- fix(ftmd): sync saves and start empty past an unreadable state file
- fix(ftmd): notice a binary replaced while ftmd was starting up
- fix(ftmd): create the readiness pipe close-on-exec atomically on Linux
- fix(ftmd): parse kinds, statuses and events from newer daemons as Unknown
- fix(ftmd): send a new pane's first output after the layout that adds it
- fix(ftmd): give a pane back its size when ftm attach detaches
- fix(ftmd): don't leave agents starting when their status hooks are missing
- test(ftmd): wait on conditions instead of fixed sleeps in the robustness tests
- fix(ftmd): kill a holder that failed to attach through its unreaped child
- fix(ftmd): ignore a hand-over whose fds are not open
- test(ftmd): import the logged daemon only in the Linux-only test
- fix(ftmd): keep pane screens at least 2x2 and survive emulator panics
- fix(ftmd): name, configure and track nested worktrees by their main repo
- fix(ftmd): carry changes into a worktree with plumbing diff, immune to the user's diff config
- fix(ftmd): remove the worktree and new branch when a spawn fails after creating them
- fix(ftmd): shorten holder socket names to fit macOS's socket path limit
- fix(ftmd): log the holder's full pane id now that its socket name is shortened
- test(ftmd): call openpty with mutable pointers so the attach tests build on macOS
- feat(worktree): start new branches from the freshly fetched remote default
- fix(ftmd): kill a closed pane's holder over a connection of its own, so the Kill survives a holder hang-up
- fix(ftmd): queue frames for a holder that hung up until ftmd reconnects, and restore the size clients asked for
- perf(ftmd): save the tree off the state lock and coalesce bursts, and skip unchanged statuses
- fix(ftmd): repaint a pane whose emulator started over, and keep the redraw nudge at two columns or more
- test(ftmd): remove the no-hooks settings dir even when the test fails
- fix(ftmd): on macOS, kill every process group in a pane's session, not just the shell's and the foreground one
- fix(ftmd): drop needless std::mem qualifications in the macOS session scan
- test(ftmd): build ftm once per test binary and run a copy, so cargo relinking target/debug/ftm cannot fail a running test

### App

- feat(app): split the settings dialog into tabs
- feat(app): show the current theme above the theme list
- test(app): start the update script test from an empty TMPDIR and show where mktemp went
- fix(app): stop relaunching a failing ftmd on every request and never run a request twice
- fix(app): keep only the end of a launcher's stderr while the launched app runs
- fix(app): treat a fresh repo with no commits as a git repo
- fix(app): say the right thing when the app is newer than ftmd, and offer installs only to a current ftmd
- fix(app): type keypad digits and follow the keyboard layout under the kitty keyboard protocol
- fix(app): play the finished sound when the last background subagent hands back without a new turn
- fix(app): keep the user's typing and pastes during a pane's replay, dropping only replayed query replies
- fix(app): retry subscribing to ftmd and catch the promises that could reject unseen
- fix(app): undo a folder reorder ftmd rejects, and keep window shortcuts out of open dialogs
- test(app): cover the theme color math
- fix(app): show workspaces with a kind or status from a newer ftmd, and ignore events it does not know
- test(app): write the fake ftmd from a child process so its launch cannot hit ETXTBSY
- fix(app): read what ftmd sent before closing on macOS, where a read timeout cannot be set on a closed socket
- fix(app): don't forward events from a newer ftmd that the app does not know
- feat(app): make the Clink and Ding recordings the default alert sounds
- feat(app): paste dropped file paths into the pane under the drop
- chore(app): require import extensions and flag floating promises in biome
- chore(app): turn on noImplicitReturns, noFallthroughCasesInSwitch and noImplicitOverride
- chore(app): typecheck the tests in CI with @types/node
- chore(app): add the .ts extension to the colors and sounds imports
- chore(app): return undefined explicitly from the key handlers
- fix(app): show a notice when the event stream cannot reach or start ftmd
- fix(app): unwind a macOS Meta Option to the key in the active layout
- fix(app): take a shifted symbol's base key from the active layout
- fix(app): keep keys shaped like query replies during a replay

### Docs

- docs: how to write an ftmd integration test that does not flake
- docs: describe the holder's stall, kill and reconnect behavior and the Unknown fallbacks
- docs: file drops onto panes
- docs: describe the new CI checks and how to run them locally
- docs: describe how ftmd kills panes across a holder reconnect, and when it saves

### CI and releases

- fix(release): fail the cask and PKGBUILD render when a download fails
- ci: run clippy and tests with --locked
- ci: fail on rustdoc warnings
- ci: lint the workflows with actionlint and the scripts with shellcheck
- ci: audit Rust and npm dependencies weekly with cargo-deny and npm audit
- ci: build the release bundles and dry-run install.sh on packaging pull requests
- ci: pin shellcheck 0.11.0 and silence two intentional SC2015s

### Other

- fix(ftm-core): ignore empty or relative XDG base dir variables
- fix(ftm-core): list symlinked theme files
- fix(ftm): end attach when stdin is gone instead of spinning
- fix(ftm): restore the terminal when attach is killed by a signal
- fix(ftm): never send ftmd a zero terminal size from attach
- fix(ftm): report gc removal failures on stderr and exit non-zero
- fix(ftm): count a gc worktree as removed even when deleting its branch fails
- fix(ftm): only remove worktrees whose upstream was deleted with --include-upstream-deleted
- fix(ftm): name the --brief file when it cannot be read
- fix(ftm): time out on a hung ftmd and say when it hangs up without replying
- fix(ftm): say when ftmd hangs up on attach without replying
- chore: declare the minimum supported Rust version, 1.90
- chore: write dependabot commits as chore(deps): ...
- chore: enable workspace lints for unsafe, casts and style
- chore: fix style and pointer lints (allow reasons, redundant closures, semicolons, &raw)
- chore: make integer casts explicit or checked
- chore: document every unsafe block with a SAFETY comment
- refactor(core): share one request reply timeout between the CLI and the app
- fix(attach): time out requests to a hung ftmd so the terminal leaves raw mode
- fix(gc): find a worktree's repo the same way as ftmd, via ftm-core
- fix(gc): reject --include-upstream-deleted with --no-fetch
- fix(ftm): gc defaults to the remote's default branch, found the same way ftmd picks a new worktree's base
- fix(ftm): report a failed ls-remote for the default branch again, from ftmd's log and gc's stderr

## [0.1.8] - 2026-10-15

### App

- feat(app): option to hold the finished sound until background subagents are done

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
