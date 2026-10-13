# app

Desktop UI: Tauri v2 + xterm.js. The frontend is vanilla TypeScript (`src/`); the Rust side (`src-tauri/`) is a workspace
member, so the binary lands in `target/` next to `ftmd`.

```sh
npm install
npm run tauri dev      # builds ftmd + ftm, starts Vite, launches the window
```

On launch the app connects to `ftmd`, starting it (from next to the app binary, else `$PATH`) if the socket does not
answer. The daemon is detached in its own process group and logs to `<socket stem>.log` next to the socket (`ftmd.log` for
the default one), so panes survive the app quitting. Set `FTM_SOCKET=/tmp/ftm-dev.sock` to run against a separate daemon;
`scripts/dev.sh` does that for you.

| Shortcut | macOS | Linux |
|---|---|---|
| New tab | Cmd+T | Ctrl+Shift+T |
| Split right | Cmd+D | Ctrl+Shift+D |
| Split down | Cmd+Shift+D | Ctrl+Shift+E |
| Close pane | Cmd+W | Ctrl+Shift+W |

Settings (⚙ in the sidebar) cover the terminal font, theme and the two status sounds; they are saved to
`~/.config/ftm/config.toml`, see [Settings](../docs/design.md#settings). Drop your own sounds in `~/.config/ftm/sounds`.
