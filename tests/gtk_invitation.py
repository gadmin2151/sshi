"""Exercise the actual GTK new connection form with an isolated CLI store."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time


def run(binary):
    launcher = Path(__file__).resolve().parents[1] / "desktop/linux/sshi-desktop"
    loader = importlib.machinery.SourceFileLoader("sshi_desktop", str(launcher))
    app = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(app)
    app.SSHI = binary
    Gtk = app.Gtk
    own_id = app.cli("id")
    # A different receiver identity using the same CLI and no actual machine credentials.
    old = os.environ["XDG_STATE_HOME"]
    os.environ["XDG_STATE_HOME"] = old + "-receiver"
    receiver = app.cli("id")
    link = app.cli("invite", own_id, "--share", "--user", "guest", "--relay", "https://example.org/")
    os.environ["XDG_STATE_HOME"] = old
    raw = link.split("#", 1)[1]
    assert own_id != receiver
    calls, launches = [], []
    real_cli = app.cli
    failure = "invitation denied"
    fail_save = False

    def cli(*args, stdin=None, **options):
        if args[:2] == ("invitation", "redeem"):
            calls.append((args, stdin))
            if failure:
                raise ValueError(failure)
            return ""
        if fail_save and args[:2] == ("profile", "save"):
            raise ValueError("disk full")
        return real_cli(*args, stdin=stdin, **options)

    def settle(dialog):
        end = time.monotonic() + 8
        while dialog.pending and time.monotonic() < end:
            while Gtk.events_pending():
                Gtk.main_iteration()
            time.sleep(.01)
        assert not dialog.pending

    app.cli = cli
    app.open_terminal = lambda args, hold: launches.append(args)
    manager = app.Window()
    manager.show_all()
    dialog = app.ConnectionDialog(manager)
    try:
        assert dialog.mode.get_active_id() == "invite"
        assert dialog.endpoint.get_visible() and dialog.code.get_visible()
        assert not dialog.submit.get_sensitive()
        dialog.endpoint.set_text(receiver)
        dialog.user.set_text("guest")
        dialog.name.set_text("Office")
        dialog.code.set_text(raw)
        assert dialog.submit.get_sensitive()
        dialog.start()
        settle(dialog)
        assert not launches and json.loads(real_cli("profile", "list", "--json")) == []
        assert dialog.code.get_text() == raw and dialog.endpoint.get_text() == receiver
        assert "отклонил" in dialog.result.get_text()
        failure = "deadline has elapsed"
        dialog.start()
        settle(dialog)
        assert "служба sshi" in dialog.result.get_text() and "relay" in dialog.result.get_text()
        failure = None
        dialog.start()
        settle(dialog)
        assert calls[-1] == (("invitation", "redeem", "--endpoint", receiver), raw + "\n")
        assert launches == [["connect", "Office"]]
    finally:
        dialog.destroy()
    dialog = app.ConnectionDialog(manager)
    try:
        dialog.user.set_text("manual-login")
        dialog.code.set_text(link)
        assert dialog.name.get_placeholder_text() == f"guest@{receiver[:12]}"
        dialog.code.set_text("sshi://invite/v1?broken")
        assert not dialog.submit.get_sensitive() and not dialog.endpoint.get_text()
        dialog.code.set_text(raw)
        assert dialog.user.get_text() == "manual-login" and not dialog.endpoint.get_text()
        clipboard = Gtk.Clipboard.get(app.Gdk.SELECTION_CLIPBOARD)
        clipboard.set_text(link, -1)
        dialog.paste.clicked()
        assert dialog.endpoint.get_text() == receiver and dialog.user.get_text() == "guest"
        assert dialog.relay.get_text() == "https://example.org/" and dialog.submit.get_sensitive()
        dialog.name.set_text("Link profile")
        fail_save = True
        dialog.start()
        settle(dialog)
        assert dialog.result.get_text() == "disk full" and len(launches) == 1
        redemptions = len(calls)
        fail_save = False
        dialog.start()
        settle(dialog)
        assert calls[-1] == (("invitation", "redeem", "--endpoint", receiver, "--relay", "https://example.org/"), link + "\n")
        assert len(calls) == redemptions and launches[-1] == ["connect", "Link profile"]
        assert dialog.code.get_text() == ""
        rows = real_cli("profile", "list", "--json")
        assert raw not in rows and link not in rows and len(json.loads(rows)) == 2
        dialog.mode.set_active_id("manual")
        assert not dialog.code.get_visible()
    finally:
        dialog.destroy()
    inherited = next(entry for entry in manager.profiles if entry["name"] == "Office")
    real_cli("config", "set-relay", "https://example.org/")
    dialog = app.ConnectionDialog(manager, inherited)
    try:
        incompatible = link.replace("guest", "somebody")
        dialog.code.set_text(incompatible)
        assert not dialog.submit.get_sensitive()
        dialog.code.set_text(link.replace("example.org", "different.example.org"))
        assert not dialog.submit.get_sensitive() and "Relay" in dialog.result.get_text()
        dialog.code.set_text(link)
        assert dialog.relay.get_text() == "https://example.org/" and not dialog.relay.get_editable()
        assert dialog.name.get_text() == "Office" and not dialog.name.get_editable()
        dialog.start()
        settle(dialog)
        assert launches[-1] == ["connect", "Office"]
        assert json.loads(real_cli("profile", "list", "--json")) == json.loads(rows)
        real_cli("profile", "remove", "--name", "Office")
        previous_redemptions = len(calls)
        dialog.code.set_text(raw)
        dialog.start()
        settle(dialog)
        assert "изменилось" in dialog.result.get_text() and len(calls) == previous_redemptions
        assert all(p["name"] != "Office" for p in json.loads(real_cli("profile", "list", "--json")))
    finally:
        dialog.destroy()
        manager.destroy()
    print("GTK raw code and receiver ID, retry, complete link and saved profiles: pass")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="sshi-join-ui.") as root:
        os.environ.update(HOME=root, XDG_CONFIG_HOME=root + "/config", XDG_STATE_HOME=root + "/state")
        run(sys.argv[1])
