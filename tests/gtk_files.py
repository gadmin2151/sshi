"""Check the GTK file action, list navigation and both transfer directions."""
import json
import os
from pathlib import Path
import sys
import tempfile
import time
from unittest.mock import Mock, patch

from gtk_forward import load_desktop


def test(binary, directory):
    module = load_desktop(binary)
    Gtk, Gdk = module.Gtk, module.Gdk
    module.cli("profile", "save", "--name", "File station", "--endpoint", module.cli("id"), "--user", "test")
    root = module.Window()
    root.show_all()

    fixture = directory / "fake-sshi"
    log = directory / "requests.jsonl"
    fixture.write_text(f"""#!{sys.executable}
import json
import sys
with open({str(log)!r}, "a") as log:
    for line in sys.stdin:
        request = json.loads(line)
        log.write(line)
        log.flush()
        side = request["side"]
        if request["action"] == "list":
            path = request["path"] or ("/local" if side == "local" else "/remote")
            name = "photo test.bin" if side == "local" else "отчёт.bin"
            event = {{"type": "listing", "listing": {{"path": path, "side": side, "parent": "/",
                "entries": [{{"name": "Reports", "kind": "directory", "size": None}},
                            {{"name": name, "kind": "file", "size": 1024}}]}}}}
        else:
            event = {{"type": "copied", "files": len(request["names"])}}
        print(json.dumps(event), flush=True)
""", encoding="utf-8")
    fixture.chmod(0o700)
    module.SSHI = str(fixture)

    def relevant(widget):
        yield widget
        if isinstance(widget, Gtk.Container):
            for child in widget.get_children():
                yield from relevant(child)

    files, = [w for w in relevant(root) if isinstance(w, Gtk.Button) and w.get_label() == "Файлы"]
    files.clicked()
    pane, = [w for w in Gtk.Window.list_toplevels() if isinstance(w, module.FileWindow)]

    def wait(condition):
        deadline = time.monotonic() + 5
        while not condition():
            while Gtk.events_pending():
                Gtk.main_iteration()
            assert time.monotonic() < deadline, pane.status.get_text()
            time.sleep(0.02)

    def requests():
        return [json.loads(line) for line in log.read_text().splitlines()]

    wait(lambda: not pane.busy and pane.panes["remote"].path == "/remote")
    local, remote = pane.panes["local"], pane.panes["remote"]
    assert local.path == "/local"
    local.tree.get_selection().select_path(Gtk.TreePath.new_from_indices([1]))
    assert pane.upload.get_sensitive()
    pane.upload.clicked()
    wait(lambda: not pane.busy and len(requests()) >= 4)
    assert requests()[2]["names"] == ["photo test.bin"] and requests()[2]["destination"] == "/remote"

    remote.tree.get_selection().select_path(Gtk.TreePath.new_from_indices([1]))
    data = Mock()
    atom = Gdk.Atom.intern("application/x-sshi-pane-selection", False)
    data.get_target.return_value = atom
    remote.drag_data(remote.tree, None, data, 0, 0)
    data.get_data.return_value = data.set.call_args.args[2]
    with patch.object(Gtk, "drag_get_source_widget", return_value=remote.tree), patch.object(Gtk, "drag_finish") as finish:
        pane.drop(local.tree, object(), 0, 0, data, 0, 0, "local")
        assert finish.call_args.args[1:3] == (True, False)
    wait(lambda: not pane.busy and len(requests()) >= 6)
    assert requests()[4]["side"] == "remote" and requests()[4]["names"] == ["отчёт.bin"]

    remote.tree.row_activated(Gtk.TreePath.new_from_indices([0]), remote.tree.get_column(0))
    wait(lambda: not pane.busy and remote.path == "/remote/Reports")
    assert requests()[-1]["path"] == "/remote/Reports"
    pane.destroy()
    pane.worker.wait(timeout=5)
    root.destroy()
    print("GTK file pane entry point, upload, remote-to-local drag and folder navigation passed")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="sshi-gtk-files-") as work:
        os.environ["XDG_STATE_HOME"] = str(Path(work) / "state")
        os.environ["XDG_CONFIG_HOME"] = str(Path(work) / "config")
        test(sys.argv[1], Path(work))
