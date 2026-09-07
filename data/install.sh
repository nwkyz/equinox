#!/usr/bin/env bash
# Equinox install script: build and install to ~/.local (no root needed).
# Usage: ./data/install.sh
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_DIR="$PREFIX/share/icons/hicolor/scalable/apps"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." # project root

echo "==> Building (release)"
cargo build --release

echo "==> Installing binaries to $BIN_DIR"
mkdir -p "$BIN_DIR"
# equinox-supervisor supervises (and restarts) the daemon; both are needed.
install -m755 target/release/equinox-supervisor "$BIN_DIR/"
install -m755 target/release/equinox-daemon "$BIN_DIR/"
install -m755 target/release/equinox-gui "$BIN_DIR/"

echo "==> Installing desktop file and icon"
mkdir -p "$APP_DIR" "$ICON_DIR"
# GNOME starts apps with the systemd user PATH (no ~/.local/bin), so a bare
# `Exec=equinox-gui` would silently fail to launch. Write the absolute path.
sed "s|^Exec=equinox-gui$|Exec=$BIN_DIR/equinox-gui|" \
    data/github.nwkyz.Equinox.desktop > "$APP_DIR/github.nwkyz.Equinox.desktop"
install -m644 data/equinox.svg "$ICON_DIR/"
# Refresh the icon cache if this hicolor tree has an index.theme. NEVER
# create one here: ~/.local/share/icons/hicolor usually holds icons from many
# other apps, and an incomplete index.theme would hide them all from GTK.
# Without a cache the icon is still found by directory scanning (a re-login
# may be needed for GNOME Shell to pick it up).
HICOLOR_DIR="$PREFIX/share/icons/hicolor"
if [ -f "$HICOLOR_DIR/index.theme" ]; then
    gtk-update-icon-cache "$HICOLOR_DIR" -f 2>/dev/null || \
        echo "    (warning: gtk-update-icon-cache failed; the icon may need a re-login to show)"
fi
# Refresh the desktop-file database so the app appears in the shell.
update-desktop-database "$APP_DIR" 2>/dev/null || true

echo "==> Installing translations (if any)"
if [ -d target/i18n ]; then
    mkdir -p "$PREFIX/share/locale"
    cp -r target/i18n/. "$PREFIX/share/locale/"
fi

# Note: the XDG autostart entry and the optional systemd unit are NOT created
# here. The first GUI launch (OOBE) asks the user whether the background
# should start at login and via which mechanism (supervisor / systemd), and
# writes the matching entry then. This keeps the installer free of any
# systemd dependency.

echo
echo "==> Installation complete."
echo "    Open the UI:        equinox-gui   (or search \"Equinox\" in the app list)"
echo "    The first launch lets you pick sources, autostart and the"
echo "    background mode (standalone supervisor by default, no systemd)."
echo "    Manual background start:   equinox-supervisor"
echo "    Manual background stop:    pkill -f equinox-supervisor"
