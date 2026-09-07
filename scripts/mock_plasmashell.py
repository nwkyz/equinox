#!/usr/bin/env python3
# Dev-only mock of org.kde.plasmashell for smoke-testing the KDE wallpaper
# backend without a real Plasma desktop. Registers the session-bus name the
# daemon's KdeBackend talks to, and answers evaluateScript with a desktop
# count of "1" (simulating one written desktop). Prints the received script.
#
# Usage (inside the isolated dbus-run-session of pass-down §8):
#   python3 scripts/mock_plasmashell.py
import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

XML = """<node><interface name="org.kde.plasmashell">
<method name="evaluateScript">
  <arg name="script" type="s" direction="in"/>
  <arg name="result" type="s" direction="out"/>
</method></interface></node>"""


def on_method_call(conn, sender, obj, iface, method, params, invocation):
    if method == "evaluateScript":
        script = params.unpack()[0]
        print(f"MOCK-SHELL evaluateScript len={len(script)}", flush=True)
        print(script, flush=True)
        # Reply = number of desktop containments written (≥1 ⇒ success).
        invocation.return_value(GLib.Variant("(s)", ("1",)))
        return True
    invocation.return_dbus_error(
        "org.freedesktop.DBus.Error.UnknownMethod", "no such method"
    )
    return True


def on_name_acquired(conn, name):
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    iface = node.interfaces[0]
    ok = conn.register_object("/PlasmaShell", iface, on_method_call)
    print(f"MOCK-SHELL registered /PlasmaShell: {ok}", flush=True)


Gio.bus_own_name(
    Gio.BusType.SESSION,
    "org.kde.plasmashell",
    Gio.BusNameOwnerFlags.NONE,
    on_name_acquired,
    None,
    None,
)
print("MOCK-SHELL running", flush=True)
GLib.MainLoop().run()