"""Run with xvfb-run -a python3 tests/gtk_forward.py /path/to/sshi."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import Mock, patch


def load_desktop(binary):
    path = Path(__file__).resolve().parents[1] / "desktop/linux/sshi-desktop"
    loader = importlib.machinery.SourceFileLoader("sshi_desktop", str(path))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    module.SSHI = binary
    return module


def check(binary, root):
    module = load_desktop(binary)
    Gtk, GLib = module.Gtk, module.GLib
    module.cli("profile", "save", "--name", "Test station", "--endpoint", module.cli("id"), "--user", "test")
    previous = json.loads(module.cli("profile", "list", "--json"))
    launched = []
    module.open_terminal = lambda args, hold: launched.append((args, hold))
    window = module.Window()
    window.show_all()

    def children(widget):
        yield widget
        if isinstance(widget, Gtk.Container):
            for child in widget.get_children():
                yield from children(child)

    def click_action(caption):
        button, = [widget for widget in children(window) if isinstance(widget, Gtk.Button)
                   and widget.get_label() == caption]
        button.clicked()

    def with_dialog(open_dialog, inspect):
        failures, verified = [], []

        def drive():
            dialogs = [widget for widget in Gtk.Window.list_toplevels()
                       if isinstance(widget, Gtk.Dialog) and widget.get_visible()]
            dialog, = [widget for widget in dialogs if not any(
                child.get_transient_for() == widget for child in dialogs)]
            try:
                response = inspect(dialog)
                verified.append(True)
            except Exception as exc:
                failures.append(exc)
                response = Gtk.ResponseType.CANCEL
            dialog.response(response)
            return GLib.SOURCE_REMOVE

        GLib.idle_add(drive)
        open_dialog()
        if failures:
            raise failures[0]
        assert verified

    def setting(caption):
        item, = [item for item in window.menu_button.get_popup().get_children() if item.get_label() == caption]
        item.activate()

    def choose_english(dialog):
        assert dialog.get_title() == "Настройки"
        combo, = [widget for widget in children(dialog) if isinstance(widget, Gtk.ComboBoxText)]
        assert combo.get_active_id() == "ru"
        link, = [widget for widget in children(dialog) if isinstance(widget, Gtk.LinkButton)]
        assert link.get_uri() == module.RELEASE_URL and link.get_label() == "Что нового в 0.3.0"
        combo.set_active_id("en")
        return Gtk.ResponseType.OK

    with_dialog(lambda: setting("Настройки…"), choose_english)
    assert window.search.get_placeholder_text() == "Search connections"
    assert window.listbox.get_selected_row().profile["name"] == "Test station"
    assert module.LANGUAGE_FILE.read_text().strip() == "en" and load_desktop(binary).language == "en"

    def cancel_settings(dialog):
        assert dialog.get_title() == "Settings"
        combo, = [widget for widget in children(dialog) if isinstance(widget, Gtk.ComboBoxText)]
        assert combo.get_active_id() == "en"
        relay, = [w for w in children(dialog) if isinstance(w, Gtk.Button) and w.get_label() == "Relay settings…"]

        def relay_settings(nested):
            assert nested.get_transient_for() == dialog and nested.get_title() == "Relay settings"
            assert any(isinstance(w, Gtk.Entry) and not w.get_visibility() for w in children(nested))
            return Gtk.ResponseType.CANCEL

        with_dialog(relay.clicked, relay_settings)
        combo.set_active_id("ru")
        return Gtk.ResponseType.CANCEL

    with_dialog(lambda: setting("Settings…"), cancel_settings)
    assert window.search.get_placeholder_text() == "Search connections"

    for kind in ("local", "remote", "dynamic"):
        def inspect(dialog):
            assert dialog.get_title() == "Port forwarding"
            dialog.kind.set_active_id(kind)
            assert dialog.bind_host.get_text() == "127.0.0.1"
            assert dialog.bind_port.get_text() == ("1080" if kind == "dynamic" else "8080")
            assert all(widget.get_visible() == (kind != "dynamic") for widget in dialog.target_widgets)
            start = dialog.get_widget_for_response(Gtk.ResponseType.OK)
            for bad in ("0", "65536", "200/2", "１２", "-L", ""):
                dialog.bind_port.set_text(bad)
                assert not start.is_sensitive() and "65535" in dialog.summary.get_text()
            dialog.bind_port.set_text("65535")
            for bad in ("999.12.10.7", "::1;ls", "--option", "host:77", "x y", "[host]"):
                dialog.bind_host.set_text(bad)
                assert not start.is_sensitive() and "hostname" in dialog.summary.get_text()
            dialog.bind_host.set_text("[::1]")
            dialog.target_host.set_text("host:22")
            assert start.is_sensitive() == (kind == "dynamic")
            dialog.target_host.set_text("server.example.org.")
            dialog.target_port.set_text("443")
            if kind == "remote":
                assert "Test station" in dialog.summary.get_text() and "◀" == dialog.arrows[0][1].get_label()
                assert dialog.ends[0][1].get_text() == "server.example.org.:443"
            elif kind == "dynamic":
                assert "SOCKS5 proxy" in dialog.summary.get_text()
                assert "[::1]:65535" == dialog.ends[0][1].get_text()
            else:
                assert "this computer" in dialog.summary.get_text() and "▶" == dialog.arrows[1][1].get_label()
                assert dialog.ends[1][1].get_text() == "server.example.org.:443"
            assert "[::1]:65535" in dialog.summary.get_text() and start.is_sensitive()
            dialog.bind_host.set_text("::")
            assert "[::1]:65535" in dialog.summary.get_text()
            return Gtk.ResponseType.OK

        with_dialog(lambda: click_action("Forward a port…"), inspect)
        args = ["forward", "--profile", "Test station", "--kind", kind,
                "--bind-host", "::", "--bind-port", "65535"]
        if kind != "dynamic":
            args += ["--target-host", "server.example.org.", "--target-port", "443"]
        assert launched[-1] == (args, False)

    def cancel(dialog):
        dialog.bind_port.set_text("9222")
        dialog.kind.set_active_id("dynamic")
        assert dialog.bind_port.get_text() == "9222"
        return Gtk.ResponseType.CANCEL

    with_dialog(lambda: click_action("Forward a port…"), cancel)
    assert len(launched) == 3
    assert previous == json.loads(module.cli("profile", "list", "--json"))
    backup = Path(root) / "backup.json"
    module.cli("backup", str(backup))
    assert not any(word in backup.read_text() for word in ("bind_port", "target_port", "language"))
    profile = "Test station"
    path = module.cli("socket", "path", "--", profile)
    command = module.cli("socket", "command", "--", profile)
    assert path.startswith("/tmp/") and not Path(path).exists()
    assert path in command and "ProxyCommand=false" in command and previous[0]["endpoint"] in command
    update = {"current": "1.0.0", "latest": "1.0.1", "available": True,
              "download_url": "https://github.com/gadmin2151/sshi/releases/download/v1.0.1/sshi_1.0.1_amd64.deb",
              "release_url": "https://github.com/gadmin2151/sshi/releases/tag/v1.0.1"}
    calls = []
    stopped = True
    report_dirs = []

    def fake_cli(*args, **kwargs):
        calls.append(args)
        if args == ("update", "--json"):
            if not update:
                raise ValueError("offline")
            return json.dumps(update)
        assert args[2:] == ("--", profile)
        if args[1] == "check" and stopped:
            raise ValueError("No active SSH control socket")
        return {"path": path, "command": command, "check": "", "close": ""}[args[1]]

    def complete(work, callback):
        try:
            result = work()
        except ValueError as error:
            callback(None, str(error))
        else:
            callback(result, None)

    def button(dialog, caption):
        selected, = [item for item in children(dialog) if isinstance(item, Gtk.Button) and item.get_label() == caption]
        return selected

    def socket_actions(dialog):
        nonlocal stopped
        assert dialog.get_title() == "SSH control socket" and len(launched) == 3
        assert [args[1] for args in calls] == ["path", "command", "check"]
        assert dialog.status.get_text() == "The socket is stopped." and not dialog.stop_button.get_sensitive()
        assert any(isinstance(item, Gtk.Label) and item.get_text() == command for item in children(dialog))
        with patch.object(Gtk.Clipboard, "get") as clipboard:
            button(dialog, "Copy command").clicked()
            clipboard.return_value.set_text.assert_called_with(command, -1)
            button(dialog, "Copy path").clicked()
            clipboard.return_value.set_text.assert_called_with(path, -1)
        def launch(args, hold, report_dir):
            launched.append((args, hold))
            directory = Path(report_dir)
            report_dirs.append(directory)
            assert directory.stat().st_mode & 0o077 == 0
            (directory / "started").write_text(str(os.getpid()))
            return Mock(poll=lambda: None)
        with patch.object(module, "open_terminal", launch):
            dialog.start_button.clicked()
            assert dialog.report and dialog.timer and not dialog.start_button.get_sensitive()
            (report_dirs[-1] / "result.json").write_text(json.dumps([255, "Permission denied (publickey)"]))
            dialog.poll()
            assert "255" in dialog.status.get_text() and dialog.report is None and not dialog.timer
            assert dialog.details.get_visible() and "Permission denied" in dialog.detail_text.get_text()
            dialog.start_button.clicked()
        assert launched[-1] == (["socket", "start", "--", profile], False)
        stopped = False
        dialog.poll()
        assert "ready" in dialog.status.get_text() and not dialog.start_button.get_sensitive()
        assert not report_dirs[-1].exists() and not dialog.timer
        with_dialog(button(dialog, "Stop socket…").clicked, lambda prompt: Gtk.ResponseType.CANCEL)
        assert calls[-1][1] == "check"
        with_dialog(button(dialog, "Stop socket…").clicked, lambda prompt: Gtk.ResponseType.OK)
        assert calls[-1][1] == "close" and "stopped" in dialog.status.get_text()
        stopped = True
        button(dialog, "Check status").clicked()
        assert dialog.status.get_text() == "The socket is stopped."
        return Gtk.ResponseType.CLOSE

    def reopened(dialog):
        assert dialog.details.get_visible() and "Permission denied" in dialog.detail_text.get_text()
        launches = len(launched)
        with patch.object(module, "open_terminal", return_value=Mock(poll=lambda: None)):
            dialog.start_button.clicked()
            directory = Path(dialog.report.name)
            (directory / "started").write_text(str(os.getpid()))
            dialog.started_at -= 200
            dialog.poll()
            assert not dialog.timer and "then select Check status" in dialog.status.get_text()
            report_dirs.append(directory)
        assert launches == len(launched)
        return Gtk.ResponseType.CLOSE

    def update_actions(dialog):
        button(dialog, "Check for updates").clicked()
        assert window.update_badge.get_visible() and "1.0.1" in window.update_badge.get_label()
        update_button = button(dialog, "Update…")
        with patch.object(Gtk, "show_uri_on_window") as open_url:
            with_dialog(update_button.clicked, lambda prompt: Gtk.ResponseType.CANCEL)
            open_url.assert_not_called()
            with_dialog(update_button.clicked, lambda prompt: Gtk.ResponseType.OK)
            assert open_url.call_args.args[1] == update["download_url"]
            update["download_url"] = None
            button(dialog, "Check for updates").clicked()
            with_dialog(update_button.clicked, lambda prompt: Gtk.ResponseType.OK)
            assert open_url.call_args.args[1] == update["release_url"]
        update.update(available=False, latest=update["current"])
        button(dialog, "Check for updates").clicked()
        assert not window.update_badge.get_visible() and not update_button.get_visible()
        update.clear()
        button(dialog, "Check for updates").clicked()
        assert "offline" in window.update_settings[2].get_text()
        return Gtk.ResponseType.CANCEL

    with patch.object(module, "cli", fake_cli), patch.object(module, "background", complete):
        with_dialog(lambda: click_action("SSH control socket…"), socket_actions)
        with_dialog(lambda: click_action("SSH control socket…"), reopened)
        assert not any(directory.exists() for directory in report_dirs)
        with_dialog(lambda: setting("Settings…"), update_actions)
    with tempfile.TemporaryDirectory(dir=root) as report:
        code = module.report_terminal(report, [sys.executable, "-c",
            "import os, sys; assert os.isatty(2); sys.stderr.write('Test SSH error\\n'); sys.exit(37)"])
        assert code == 37 and json.loads((Path(report) / "result.json").read_text()) == [37, "Test SSH error"]
        # Konsole passes the helper argv directly even for punctuation in profile names.
        special_name = "Lab 'A'; $data"
        launcher = load_desktop(binary)
        with patch.object(launcher.shutil, "which", side_effect=lambda exe: "/usr/bin/konsole" if exe == "konsole" else None), \
                patch.object(launcher.subprocess, "Popen") as spawned:
            launcher.open_terminal(["socket", "start", "--", special_name], False, report)
            argv = spawned.call_args.args[0]
            assert argv[:4] == ["/usr/bin/konsole", "--separate", "-e", "/bin/sh"]
            assert argv[-3:] == ["--report-socket-start", report, special_name]
            assert spawned.call_args.kwargs == {"start_new_session": True}
    window.destroy()
    print("GTK settings, translations, port forwarding, control socket actions and update notifications passed")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="sshi-gtk-forward.") as root:
        os.environ.update(HOME=root, XDG_CONFIG_HOME=root + "/config", XDG_STATE_HOME=root + "/state")
        check(sys.argv[1], root)
