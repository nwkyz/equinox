#!/usr/bin/env bash
# Dev helper: build + restart the DEBUG daemon & GUI with logs.
# Usage: ./dev.sh [page]
#   page (optional): wallpaper | sources | gallery | history | timeline
#                     -> opens the GUI directly on that page/window.
# Logs: /tmp/eq-daemon.log  /tmp/eq-gui.log
set -u
cd "$(dirname "$0")"

PAGE="${1:-}"

if [ "$PAGE" = "stop" ]; then
    pkill -x equinox-gui 2>/dev/null && echo "==> GUI stopped"
    pkill -x equinox-daemon 2>/dev/null && echo "==> daemon stopped"
    exit 0
fi

echo "==> Building"
cargo build --quiet || exit 1

echo "==> Stopping old instances (incl. installed versions)"
pkill -x equinox-daemon 2>/dev/null
pkill -x equinox-gui 2>/dev/null
sleep 0.5

echo "==> Starting daemon"
./target/debug/equinox-daemon > /tmp/eq-daemon.log 2>&1 &
sleep 1

echo "==> Starting GUI${PAGE:+ (page: $PAGE)}"
if [ -n "$PAGE" ]; then
    EQ_DEBUG_PAGE="$PAGE" ./target/debug/equinox-gui > /tmp/eq-gui.log 2>&1 &
else
    ./target/debug/equinox-gui > /tmp/eq-gui.log 2>&1 &
fi
sleep 1.5

if pgrep -x equinox-daemon > /dev/null && pgrep -x equinox-gui > /dev/null; then
    echo "==> Running ✔   logs: tail -f /tmp/eq-daemon.log /tmp/eq-gui.log"
else
    echo "==> FAILED to start:"
    tail -n 8 /tmp/eq-daemon.log /tmp/eq-gui.log 2>/dev/null
    exit 1
fi
