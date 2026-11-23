"""Installed-package test. Run in a disposable Debian container with a loopback relay."""
import concurrent.futures
import contextlib
import http.server
import os
import pathlib
import re
import socket
import subprocess
import tempfile
import threading
import time

if not pathlib.Path("/.dockerenv").exists() or os.geteuid() != 0:
    raise SystemExit("Run in a disposable Debian container as root")
relay = os.environ["SSHI_TEST_RELAY_URL"]
if not re.fullmatch(r"http://127[.]0[.]0[.]1:[0-9]+/", relay):
    raise SystemExit("Expected a local, isolated test relay on loopback")
native_env = os.environ.copy()

def text(value):
    return value.decode(errors="replace")

def run(args, *, env=native_env, timeout=30):
    return subprocess.run(args, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout)

def check(result, label, ok=None):
    if ok is None:
        ok = result.returncode == 0
    if not ok:
        raise AssertionError(f"{label} failed ({result.returncode})\n{text(result.stderr)}\n{text(result.stdout[:1000])}")
    return result.stdout

def wait_for(check_ready, label, limit=20):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        if check_ready():
            return
        time.sleep(0.15)
    raise AssertionError(f"Timed out waiting for {label}")

@contextlib.contextmanager
def daemon(args, *, logfile, env=native_env):
    with logfile.open("w+b") as log:
        process = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            yield process
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            if process.returncode not in (-15, 0, 255):
                print(f"{args[0]} exited {process.returncode}: {text(logfile.read_bytes())[-1500:]}", flush=True)

@contextlib.contextmanager
def http_fixture(response):
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(response)
        def log_message(self, *args):
            pass
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            worker.join()

def ready(process, logfile, marker):
    if process.poll() is not None:
        raise AssertionError(f"Process exited early: {text(logfile.read_bytes())}")
    return marker in logfile.read_bytes()

man_path = check(run(["man", "-w", "sshi"]), "find man sshi")
assert b"/sshi.1.gz" in man_path
assert b"iroh relay" in check(run(["man", "-P", "cat", "sshi"]), "render installed man page")
conffiles = check(run(["dpkg-query", "-W", "-f=${Conffiles}", "sshi"]), "package conffiles")
assert b"/etc/sshi/config.toml" in conffiles
assert pathlib.Path("/usr/lib/systemd/user/sshi.service").is_file()
print("Debian apt install, man sshi and configurable /etc conffile: pass", flush=True)

with tempfile.TemporaryDirectory(prefix="sshi.") as tmp:
    base = pathlib.Path(tmp)
    receiver = dict(native_env, XDG_STATE_HOME=str(base / "rx"))
    initiator = dict(native_env, XDG_STATE_HOME=str(base / "tx"))
    with concurrent.futures.ThreadPoolExecutor(max_workers=5) as workers:
        ids = list(workers.map(lambda _: check(run(["sshi", "id"], env=initiator), "sshi id").decode().strip(), range(8)))
    initiator_id = ids[0]
    assert re.fullmatch("[0-9a-f]{64}", initiator_id) and len(set(ids)) == 1
    receiver_id = check(run(["sshi", "id"], env=receiver), "receiver sshi id").decode().strip()
    assert initiator_id != receiver_id
    assert check(run(["sshi", "list"], env=initiator), "empty history") == b""
    print("Persistent per-user IDs, including concurrent first use, and empty history: pass", flush=True)

    with contextlib.ExitStack() as stack:
        held_ports = []
        for _ in range(4):
            sock = stack.enter_context(socket.socket())
            sock.bind(("127.0.0.1", 0))
            held_ports.append(sock.getsockname()[1])
    ssh_port, local_port, reverse_port, socks_port = held_ports

    for name in ("host", "login", "wrong"):
        check(run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(base / name)]), f"keygen {name}")
    (base / "known_hosts").write_text(receiver_id + " " + (base / "host.pub").read_text())
    pathlib.Path("/etc/ssh/ssh_config.d/10-sshi-smoke.conf").write_text(
        f"Host *\n  StrictHostKeyChecking yes\n  UserKnownHostsFile {base / 'known_hosts'}\n"
        "  BatchMode yes\n  IdentitiesOnly yes\n  ConnectTimeout 20\n"
    )
    pathlib.Path("/run/sshd").mkdir(exist_ok=True)
    (base / "sshd_config").write_text(
        f"ListenAddress 127.0.0.1\nPort {ssh_port}\n"
        f"HostKey {base / 'host'}\nAuthorizedKeysFile {base / 'login.pub'}\nPidFile {base / 'sshd.pid'}\n"
        "PermitRootLogin prohibit-password\nPasswordAuthentication no\n"
        "KbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n"
        "AllowTcpForwarding yes\nLogLevel VERBOSE\n"
    )
    pathlib.Path("/etc/sshi/config.toml").write_text(
        f'relay_url = "{relay}"\n'
        f'ssh_target = "127.0.0.1:{ssh_port}"\nallow_clients = ["{initiator_id}"]\n'
    )
    sshd_log = base / "sshd.log"
    receiver_log = base / "receiver.log"
    client_log = base / "initiator.log"
    destination = f"root@{receiver_id}"
    sshi = ["sshi", "-i", str(base / "login")]
    with daemon(["/usr/sbin/sshd", "-D", "-e", "-f", str(base / "sshd_config")], logfile=sshd_log) as sshd:
        wait_for(lambda: ready(sshd, sshd_log, b"Server listening"), "sshd")
        with daemon(["sshi", "serve"], env=receiver, logfile=receiver_log) as rx:
            wait_for(lambda: ready(rx, receiver_log, f"Ready: {receiver_id}".encode()), "receiver relay")
            assert check(run(["sshi", "id"], env=receiver), "id while receiving").strip().decode() == receiver_id

            # Redeem a targeted invitation and prove it controls an independent identity.
            guest = dict(native_env, XDG_STATE_HOME=str(base / "guest"))
            guest_id = check(run(["sshi", "id"], env=guest), "guest id").decode().strip()
            code = check(run(["sshi", "invite", guest_id], env=receiver), "issue invitation")
            redeemed = subprocess.run(["sshi-copy-iid", "--invite", destination], env=guest,
                                      input=code, capture_output=True, timeout=30)
            check(redeemed, "redeem over Iroh")
            assert check(run([*sshi, destination, "printf guest"], env=guest), "invited SSH login") == b"guest"
            check(run(["sshi", "revoke-invite", guest_id], env=receiver), "revoke invitation")
            denied = run([*sshi, destination, "true"], env=guest)
            check(denied, "revoked invitation", denied.returncode == 255 and b"not allowed" in denied.stderr)
            print("Targeted invitation, post-enrollment SSH and revocation: pass", flush=True)

            failed = run(["sshi", "-i", str(base / "wrong"), destination, "true"], env=initiator)
            check(failed, "SSH auth rejection", failed.returncode == 255 and b"Permission denied" in failed.stderr)
            assert check(run(["sshi", "list"], env=initiator), "history after failed SSH auth") == b""
            result = run([*sshi, destination, "printf sshi-ok; exit 37"], env=initiator)
            check(result, "sshi root@ID", result.returncode == 37 and result.stdout == b"sshi-ok")
            recorded = (destination + "\n").encode()
            assert check(run(["sshi", "list"], env=initiator), "recorded destination") == recorded
            print("SSH root login, remote exit status, and history of authenticated connections: pass", flush=True)

            with daemon(["sshi", "serve"], env=initiator, logfile=client_log) as tx:
                wait_for(lambda: ready(tx, client_log, f"Ready: {initiator_id}".encode()), "initiator relay")
                result = run([*sshi, "-tt", destination, "test -t 0 && printf tty-ok"], env=initiator)
                check(result, "SSH TTY through the local service", result.returncode == 0 and b"tty-ok" in result.stdout)
                print("Interactive SSH through the shared local endpoint: pass", flush=True)

                with http_fixture(b"receiver") as remote_http, http_fixture(b"initiator") as local_http:
                    forward_log = base / "forward.log"
                    forward = [
                        *sshi, "-N", "-L", f"127.0.0.1:{local_port}:127.0.0.1:{remote_http}",
                        "-R", f"127.0.0.1:{reverse_port}:127.0.0.1:{local_http}",
                        "-D", f"127.0.0.1:{socks_port}", destination,
                    ]
                    with daemon(forward, env=initiator, logfile=forward_log) as ssh:
                        def check_forward():
                            if ssh.poll() is not None:
                                raise AssertionError(f"forwarding ended: {text(forward_log.read_bytes())}")
                            return run(["curl", "-fsS", "--noproxy", "*", "--max-time", "2", f"http://127.0.0.1:{local_port}"], timeout=3).stdout == b"receiver"
                        wait_for(check_forward, "local forwarding")
                        result = run(["curl", "-fsS", "--noproxy", "*", "--max-time", "5", f"http://127.0.0.1:{reverse_port}"])
                        assert check(result, "reverse forwarding") == b"initiator"
                        result = run(["curl", "-fsS", "--noproxy", "", "--socks5-hostname", f"127.0.0.1:{socks_port}", f"http://127.0.0.1:{remote_http}"])
                        assert check(result, "dynamic forwarding") == b"receiver"
                        assert check(run(["sshi", "list"], env=initiator), "history of repeated and active sessions") == recorded
                        result = run([*sshi, destination, "printf concurrent"], env=initiator)
                        assert check(result, "parallel session") == b"concurrent"
                    # The desktop launches this entry point with one argument per field.
                    # Use the host's OpenSSH settings for the test profile, as in the app.
                    with pathlib.Path("/etc/ssh/ssh_config.d/10-sshi-smoke.conf").open("a") as config:
                        config.write(f"Host {receiver_id}\n  IdentityFile {base / 'login'}\n")
                    check(run(["sshi", "profile", "save", "--name", "Workshop", "--user", "root",
                               "--endpoint", receiver_id], env=initiator), "create desktop profile")
                    for kind in ("local", "remote", "dynamic"):
                        with socket.socket() as sock:
                            sock.bind(("127.0.0.1", 0))
                            listen = sock.getsockname()[1]
                        target = local_http if kind == "remote" else remote_http
                        action = ["sshi", "forward", "--profile", "Workshop", "--kind", kind,
                                  "--bind-host", "127.0.0.1", "--bind-port", str(listen)]
                        request = ["curl", "-fsS", "--max-time", "2"]
                        if kind == "dynamic":
                            request += ["--noproxy", "", "--socks5-hostname", f"127.0.0.1:{listen}",
                                        f"http://127.0.0.1:{target}"]
                        else:
                            action += ["--target-host", "127.0.0.1", "--target-port", str(target)]
                            request += ["--noproxy", "*", f"http://127.0.0.1:{listen}"]
                        expected = b"initiator" if kind == "remote" else b"receiver"
                        with daemon(action, env=initiator, logfile=forward_log) as tunnel:
                            def profile_ready():
                                if tunnel.poll() is not None:
                                    raise AssertionError(text(forward_log.read_bytes()))
                                return run(request, timeout=3).stdout == expected
                            wait_for(profile_ready, f"desktop {kind} forwarding")
                print("Local, reverse and SOCKS port forwarding via iroh, with concurrent SSH: pass", flush=True)

print("Installed Debian package passed the SSH and forwarding tests.", flush=True)
