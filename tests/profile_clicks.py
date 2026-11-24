"""GTK profile gestures. Run with xvfb-run -a python3 tests/profile_clicks.py /path/to/sshi."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time


def check(binary):
    launcher = Path(__file__).resolve().parents[1] / "desktop/linux/sshi-desktop"
    loader = importlib.machinery.SourceFileLoader("sshi_desktop", str(launcher))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    module.SSHI = binary
    Gdk, GLib, Gtk = module.Gdk, module.GLib, module.Gtk
    peer = module.cli("id")
    for name in ("Atlas", "Studio"):
        module.cli("profile", "save", "--name", name, "--endpoint", peer, "--user", "guest",
                   "--relay", "https://example.org/")
    launches = []
    module.open_terminal = lambda args, hold: launches.append(args)
    window = module.Window()
    window.show_all()
    clicks = window.cards
    interval = 160
    window.get_settings().set_property("gtk-double-click-time", interval)
    window.get_settings().set_property("gtk-double-click-distance", 25)

    def pump(ms):
        end = time.monotonic() + ms / 1000
        while time.monotonic() < end:
            while Gtk.events_pending():
                Gtk.main_iteration()
            time.sleep(0.002)

    seat = Gdk.Display.get_default().get_default_seat()

    def click(index, edge=None):
        area = clicks.get_child_at_index(index).get_allocation()
        x = area.x + {"left": 1, "right": area.width - 2}.get(edge, area.width // 2)
        y = area.y + area.height // 2
        for event_type in (Gdk.EventType.BUTTON_PRESS, Gdk.EventType.BUTTON_RELEASE):
            event = Gdk.Event.new(event_type)
            event.button.window = clicks.get_window()
            event.button.button = Gdk.BUTTON_PRIMARY
            event.button.time = int(time.monotonic() * 1000) % (1 << 32)
            event.button.x, event.button.y = x, y
            event.button.x_root, event.button.y_root = x, y
            event.set_device(seat.get_pointer())
            Gtk.main_do_event(event)

    def key(keyval):
        for event_type in (Gdk.EventType.KEY_PRESS, Gdk.EventType.KEY_RELEASE):
            event = Gdk.Event.new(event_type)
            event.key.window = window.get_window()
            event.key.time = int(time.monotonic() * 1000) % (1 << 32)
            event.key.keyval = keyval
            event.set_device(seat.get_keyboard())
            Gtk.main_do_event(event)

    # Answer only actual native dialogs. A surprise or missing dialog fails the check.
    responses, dialogs = [], []
    def answer_dialog():
        for dialog in Gtk.Window.list_toplevels():
            if not isinstance(dialog, Gtk.Dialog) or not dialog.get_visible():
                continue
            dialogs.append(dialog.get_title())
            if responses and dialog.get_title() == "Переименовать подключение":
                name = responses.pop(0)
                if name:
                    grid = dialog.get_content_area().get_children()[0]
                    entry, = [child for child in grid.get_children() if isinstance(child, Gtk.Entry)]
                    entry.set_text(name)
                    dialog.response(Gtk.ResponseType.OK)
                    continue
            dialog.response(Gtk.ResponseType.CANCEL)
        return GLib.SOURCE_CONTINUE

    poll = GLib.timeout_add(10, answer_dialog)
    wait = interval + 65
    try:
        pump(40)
        click(0)  # Programmatic startup selection still requires a first click.
        pump(wait)
        assert not launches and not dialogs
        click(1)
        pump(wait)
        assert window.selected_card().profile["name"] == "Studio"
        assert not launches and not dialogs
        click(1)  # Slow second click could rename; a quick third instead opens once.
        pump(30)
        click(1)
        pump(wait)
        assert launches == [["connect", "Studio"]] and not dialogs

        click(0)
        pump(30)
        click(0)
        pump(wait)
        assert launches == [["connect", "Studio"], ["connect", "Atlas"]] and not dialogs

        click(1)
        pump(wait)
        responses.append("Work studio")
        click(1)
        pump(wait)
        assert dialogs == ["Переименовать подключение"] and not responses
        profiles = json.loads(module.cli("profile", "list", "--json"))
        assert {"name": "Work studio", "endpoint": peer, "user": "guest",
                "relay": "https://example.org/"} in profiles
        assert len(launches) == 2

        click(0)
        pump(wait)
        click(0)
        pump(25)
        click(1)  # Changing selection cancels the pending rename.
        pump(wait)
        assert len(dialogs) == 1 and len(launches) == 2
        key(Gdk.KEY_F2)
        assert len(dialogs) == 2
        key(Gdk.KEY_Return)
        assert launches[-1] == ["connect", "Work studio"] and len(launches) == 3

        # Two nearby clicks across the gap between cards must not activate either.
        pump(wait)
        click(0, "right")
        pump(20)
        click(1, "left")
        pump(wait)
        assert len(dialogs) == 2 and len(launches) == 3
        window.search.set_text("GUEST@" + peer[:8].upper())
        pump(wait)
        assert window.selected_card() is not None
        window.search.set_text("missing")
        pump(wait)
        assert window.selected_card() is None and not window.dock.get_visible()
        window.search.set_text("work studio")
        pump(wait)
        assert window.selected_card().profile["name"] == "Work studio" and window.dock.get_visible()
        print("GTK card selection, double-click, delayed rename, keyboard and filtering passed")
    finally:
        GLib.source_remove(poll)
        window.destroy()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="sshi-profile-clicks.") as root:
        os.environ.update(HOME=root, XDG_STATE_HOME=root + "/state", XDG_CONFIG_HOME=root + "/config")
        check(sys.argv[1])
