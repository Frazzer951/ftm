#!/usr/bin/env bash
# Build FTM from this checkout and install it as an app; rerun to upgrade:
#
#   scripts/install.sh [--prefix DIR] [--bin-dir DIR] [--no-launch] [--dry-run]
#
# macOS: builds FTM.app with ftm and ftmd inside it, signs it ad hoc and installs it to /Applications (else
# ~/Applications, or --prefix). Linux: installs ftm, ftmd and ftm-app to ~/.local/lib/ftm (or --prefix) with a
# .desktop entry. Both link `ftm` into ~/.local/bin (or --bin-dir).
#
# An upgrade quits the installed app, stops the ftmd on the default socket (its panes survive in their holders, still
# running the old build until they close), replaces the install and opens the app, whose new ftmd adopts every pane.
# A dev build (scripts/dev.sh) runs on its own socket and is left alone.
#
#   --prefix DIR   install into DIR instead of the default
#   --bin-dir DIR  link ftm into DIR instead of ~/.local/bin
#   --no-launch    don't open the app afterwards
#   --dry-run      build and assemble only; print what would be stopped and installed
#   --socket PATH  the daemon to restart instead of the default one (for testing)
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd -P)
# shellcheck source=scripts/lib.sh
. "$REPO/scripts/lib.sh"
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
SIDECARS=$REPO/app/src-tauri/binaries

PREFIX="" BIN_DIR=$HOME/.local/bin LAUNCH=1 DRY=0 SOCKET=$(default_socket)
while [ $# -gt 0 ]; do
  case $1 in
    --prefix) PREFIX=$2; shift 2 ;;
    --bin-dir) BIN_DIR=$2; shift 2 ;;
    --no-launch) LAUNCH=0; shift ;;
    --dry-run) DRY=1; shift ;;
    --socket) SOCKET=$2; shift 2 ;;
    -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1 (see --help)" >&2; exit 2 ;;
  esac
done

run() {
  if [ $DRY -eq 1 ]; then echo "would: $*"; else "$@"; fi
}

link_ftm() {
  local ftm=$1
  echo "==> linking $BIN_DIR/ftm -> $ftm"
  run mkdir -p "$BIN_DIR"
  run ln -sfn "$ftm" "$BIN_DIR/ftm"
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not on PATH; add it to use ftm outside FTM panes" ;;
  esac
  local found
  found=$(command -v ftm || true)
  if [ -n "$found" ] && [ "$found" != "$BIN_DIR/ftm" ]; then
    echo "note: $found comes before $BIN_DIR/ftm on PATH"
  fi
}

restart() {
  local app_exe=$1 ftm=$2
  if [ $DRY -eq 1 ]; then
    find_apps "$SOCKET" "$app_exe" | sed 's/^/would quit app: /'
    lsof -t "$SOCKET" 2>/dev/null | sort -u | sed 's/^/would stop ftmd: /' || true
    return
  fi
  stop_apps "$SOCKET" "$app_exe"
  stop_ftmd "$SOCKET" "$ftm"
}

# The app and ftmd look for ftmd and ftm next to their own executable, so they ship as Tauri sidecars. Pass a bundle
# type to make, or nothing for the bare binaries.
build() {
  echo "==> building"
  "$REPO/scripts/sidecars.sh" "$TRIPLE"
  ensure_app_deps "$REPO/app"
  local args=(--config src-tauri/tauri.bundle.conf.json)
  if [ -n "${1:-}" ]; then args+=(--bundles "$1"); else args+=(--no-bundle); fi
  (cd "$REPO/app" && npm run tauri build -- "${args[@]}")
}

install_macos() {
  if [ -z "$PREFIX" ]; then
    PREFIX=/Applications
    [ -w "$PREFIX" ] || PREFIX=$HOME/Applications
  fi
  local built=$TARGET/release/bundle/macos/FTM.app app=$PREFIX/FTM.app
  build app
  echo "==> signing $built"
  codesign --force --deep --sign - "$built"
  codesign --verify --deep --strict "$built"

  # Stage next to the destination so the swap is two renames on one volume.
  local staged=$PREFIX/.FTM.app.new old=$PREFIX/.FTM.app.old
  run mkdir -p "$PREFIX"
  run rm -rf "$staged" "$old"
  run ditto "$built" "$staged"
  restart "$app/Contents/MacOS/ftm-app" "$staged/Contents/MacOS/ftm"
  echo "==> installing $app"
  [ -e "$app" ] && run mv "$app" "$old"
  run mv "$staged" "$app"
  run rm -rf "$old"
  link_ftm "$app/Contents/MacOS/ftm"
  if [ $LAUNCH -eq 1 ]; then
    echo "==> opening $app"
    run open "$app"
  fi
}

install_linux() {
  local lib=${PREFIX:-$HOME/.local/lib/ftm} f
  build
  run mkdir -p "$lib"
  run cp "$TARGET/release/ftm-app" "$lib/.ftm-app.new"
  for f in ftm ftmd; do run cp "$SIDECARS/$f-$TRIPLE" "$lib/.$f.new"; done
  run cp "$REPO/app/src-tauri/icons/128x128.png" "$lib/ftm.png"
  restart "$lib/ftm-app" "$lib/.ftm.new"
  echo "==> installing $lib"
  # Renames, so running holders keep the old binary.
  for f in ftm ftmd ftm-app; do run mv -f "$lib/.$f.new" "$lib/$f"; done

  local apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
  echo "==> writing $apps/ftm.desktop"
  if [ $DRY -eq 0 ]; then
    mkdir -p "$apps"
    cat >"$apps/ftm.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=FTM
Comment=Terminal workspace manager for Claude Code agents
Exec=$lib/ftm-app
Icon=$lib/ftm.png
Terminal=false
Categories=Development;TerminalEmulator;
DESKTOP
  fi
  link_ftm "$lib/ftm"
  if [ $LAUNCH -eq 1 ]; then
    echo "==> starting $lib/ftm-app"
    [ $DRY -eq 1 ] || (env -u FTM_SOCKET setsid "$lib/ftm-app" >/dev/null 2>&1 &)
  fi
}

case $(uname) in
  Darwin) install_macos ;;
  Linux) install_linux ;;
  *) echo "unsupported OS $(uname)" >&2; exit 1 ;;
esac
echo "==> done"
