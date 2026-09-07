#!/usr/bin/env bash
# Debug helper script: start/stop/restart/inspect the equinox daemon and GUI
# (installed versions).
#
# Usage:
#   ./debug.sh start     Start daemon and GUI (logs go to /tmp/eq-{daemon,gui}.log)
#   ./debug.sh stop      Stop both (by process name, all instances)
#   ./debug.sh restart   Restart both
#   ./debug.sh logs      Show the tail of both logs
#   ./debug.sh status    Show run state and D-Bus name ownership
#
# Note: runs the installed versions under ~/.local/bin; after changing the
# source, reinstall with ./data/install.sh first, then ./debug.sh restart.
set -u

DAEMON_BIN="$HOME/.local/bin/equinox-daemon"
GUI_BIN="$HOME/.local/bin/equinox-gui"
DAEMON_LOG=/tmp/eq-daemon.log
GUI_LOG=/tmp/eq-gui.log

is_up() { pgrep -x "$1" > /dev/null; }

start_one() {
    local name="$1" bin="$2" log="$3"
    if is_up "$name"; then
        echo "[$name] already running"
    elif [ -x "$bin" ]; then
        "$bin" >> "$log" 2>&1 &
        echo "[$name] started (pid $!): log $log"
    else
        echo "[$name] installed binary not found: $bin"
        echo "[$name] please run: ./data/install.sh"
    fi
}

stop_one() {
    local name="$1"
    local pids
    pids=$(pgrep -x "$name")
    if [ -n "$pids" ]; then
        echo "[$name] stopping: $pids"
        # shellcheck disable=SC2086
        kill $pids
    else
        echo "[$name] not running"
    fi
}

start_all() { start_one equinox-daemon "$DAEMON_BIN" "$DAEMON_LOG"; start_one equinox-gui "$GUI_BIN" "$GUI_LOG"; }
stop_all()  { stop_one equinox-gui; stop_one equinox-daemon; }

case "${1:-start}" in
    start)
        start_all
        ;;
    stop)
        stop_all
        ;;
    restart)
        stop_all
        sleep 1
        start_all
        ;;
    logs)
        echo "=== daemon log ($DAEMON_LOG) ==="
        tail -n 30 "$DAEMON_LOG" 2>/dev/null || echo "(none)"
        echo
        echo "=== GUI log ($GUI_LOG) ==="
        tail -n 30 "$GUI_LOG" 2>/dev/null || echo "(none)"
        ;;
    status)
        echo "-- processes --"
        for name in equinox-daemon equinox-gui; do
            if is_up "$name"; then
                echo "$name: $(pgrep -x "$name" | tr '\n' ' ')"
            else
                echo "$name: not running"
            fi
        done
        echo "-- D-Bus name ownership --"
        busctl --user status org.equinox.Daemon1 2>&1 | grep -E '^PID|Name' | head -3
        echo "-- latest wallpaper --"
        busctl --user call org.equinox.Daemon1 /org/equinox/Daemon1 org.equinox.Daemon1 GetStatus 2>&1
        ;;
    *)
        echo "Usage: $0 {start|stop|restart|logs|status}" >&2
        exit 1
        ;;
esac
