#!/usr/bin/env bash
# Equinox uninstall script: stops the background, removes binaries, desktop
# file and any autostart/systemd entries (keeps config, cache and history).
# Usage: ./data/uninstall.sh
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"

echo "==> Stopping the background service"

# 1) systemd hosting: stop + disable BEFORE removing the unit file, so a
#    running service is stopped cleanly and its enable-symlink is removed.
if command -v systemctl >/dev/null 2>&1; then
    for unit in equinox-supervisor.service equinox-daemon.service; do
        systemctl --user stop "$unit" 2>/dev/null || true
        systemctl --user disable "$unit" 2>/dev/null || true
    done
fi

# 2) Standalone supervisor/daemon. pkill -x misses the kernel-truncated comm
#    name ("equinox-supervisor" is 17 chars), so match the full command line
#    with -f (this also covers flatpak-hosted `flatpak run --command=…`).
stop_processes() {
    pkill -f 'equinox-supervisor' 2>/dev/null || true
    pkill -f 'equinox-daemon' 2>/dev/null || true
    # SIGTERM lets the supervisor forward to its daemon and the daemon flush
    # its config/history before exiting; wait briefly for a clean exit.
    for _ in 1 2 3 4 5 6; do
        pgrep -f 'equinox-(supervisor|daemon)' >/dev/null 2>&1 || return 0
        sleep 0.5
    done
    # Anything still alive gets the boot.
    pkill -9 -f 'equinox-supervisor' 2>/dev/null || true
    pkill -9 -f 'equinox-daemon' 2>/dev/null || true
}
stop_processes

echo "==> Removing autostart entry and optional systemd unit"
rm -f "$HOME/.config/autostart/equinox-supervisor.desktop"
rm -f "$HOME/.config/systemd/user/equinox-supervisor.service"
rm -f "$HOME/.config/systemd/user/equinox-daemon.service"
# Best-effort: only reload systemd when it is actually present.
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload 2>/dev/null || true
fi

echo "==> Removing binaries"
rm -f "$PREFIX/bin/equinox-supervisor"
rm -f "$PREFIX/bin/equinox-daemon"
rm -f "$PREFIX/bin/equinox-gui"

echo "==> Removing desktop file and icon"
# Legacy name (pre-rename) plus the current one.
rm -f "$PREFIX/share/applications/github.nwkyz.Equinox.desktop"
rm -f "$PREFIX/share/applications/org.equinox.Wallpaper.desktop"
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/equinox.svg"
# Only refresh caches when the theme tree actually has an index.theme; never
# create/delete one here (the user-level hicolor tree is shared with icons
# from many other applications).
HICOLOR_DIR="$PREFIX/share/icons/hicolor"
if [ -f "$HICOLOR_DIR/index.theme" ]; then
    gtk-update-icon-cache "$HICOLOR_DIR" 2>/dev/null || true
fi
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true

echo "==> Done. Cache and history stay in ~/.cache/equinox and ~/.local/share/equinox; remove them manually if you want them gone."
