"""Exercise an installed SSHI client/receiver using loopback, OpenSSH and iroh-relay 1.5.

Run as the macOS runner user: python3 tests/mac_relay_smoke.py
"""
import contextlib
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from login_pty import first_login


def command(args, env, data=b"", check=True):
    result = subprocess.run(args, env=env, input=data, capture_output=True, timeout=30)
    if check and result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} exited {result.returncode}: {result.stderr.decode(errors='replace')}")
    return result


@contextlib.contextmanager
def background(args, env, logfile, stdin=subprocess.DEVNULL):
    with logfile.open("wb") as output:
        process = subprocess.Popen(args, env=env, stdin=stdin, stdout=output, stderr=subprocess.STDOUT)
        try:
            yield process
        finally:
            if process.stdin:
                process.stdin.close()
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=4)


def wait(label, predicate, process=None, log=None, timeout=20):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if process is not None and process.poll() is not None:
            break
        if predicate():
            return
        time.sleep(0.1)
    detail = log.read_text(errors="replace")[-1800:] if log else ""
    raise RuntimeError(f"{label} did not become ready. {detail}")


def free_ports():
    with socket.socket() as first, socket.socket() as second:
        for item in (first, second):
            item.bind(("127.0.0.1", 0))
        return first.getsockname()[1], second.getsockname()[1]


def listening(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.3):
            return True
    except OSError:
        return False


def smoke(base):
    sshi, relay = shutil.which("sshi"), shutil.which("iroh-relay")
    if not sshi or not relay:
        raise RuntimeError("Install sshi and iroh-relay 1.5 and put them on PATH")
    user = pwd.getpwuid(os.getuid()).pw_name
    relay_port, ssh_port = free_ports()
    url = f"http://127.0.0.1:{relay_port}/"
    relay_config = base / "relay.toml"
    relay_config.write_text(f'http_bind_addr = "127.0.0.1:{relay_port}"\nenable_metrics = false\n')
    config = base / "config.toml"
    config.write_text(f'relay_url = "{url}"\nssh_target = "127.0.0.1:{ssh_port}"\nallow_clients = []\n')
    env = {
        "PATH": os.environ["PATH"], "HOME": str(base), "TMPDIR": str(base),
        "SSHI_CONFIG": str(config), "XDG_CONFIG_HOME": str(base / "config"),
        "USER": user, "LOGNAME": user, "LC_ALL": "C",
    }
    ssh_config = base / "sshd_config"
    ssh_config.write_text(
        f"ListenAddress 127.0.0.1\nPort {ssh_port}\nHostKey {base}/host\n"
        f"AuthorizedKeysFile {base}/client.pub\nPidFile {base}/sshd.pid\nAllowUsers {user}\n"
        "PasswordAuthentication no\nKbdInteractiveAuthentication no\nAuthenticationMethods publickey\n"
        "UsePAM no\nPermitUserRC no\nStrictModes no\nDisableForwarding yes\nPrintLastLog no\n"
    )
    for name in ("host", "client", "interactive"):
        command(["/usr/bin/ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", base / name], env)
    with (base / "client.pub").open("a") as keys:
        keys.write('command="echo sshi-pty-authed; exit 36" ' + (base / "interactive.pub").read_text())
    rx = dict(env, XDG_STATE_HOME=str(base / "rx"))
    tx = dict(env, XDG_STATE_HOME=str(base / "tx"))
    receiver = command([sshi, "id"], rx).stdout.decode().strip()
    client = command([sshi, "id"], tx).stdout.decode().strip()
    assert receiver != client
    destination = f"{user}@{receiver}"
    (base / "known_hosts").write_text(f"{receiver} " + (base / "host.pub").read_text())

    # The installed CLI invokes ssh via PATH. Supply a private OpenSSH config
    # to it without accessing the user's real identity files or known_hosts.
    options = base / "client.config"
    options.write_text(
        f"Host *\n IdentityFile {base}/client\n UserKnownHostsFile {base}/known_hosts\n"
        " GlobalKnownHostsFile /dev/null\n StrictHostKeyChecking yes\n UpdateHostKeys no\n"
        " IdentityAgent none\n IdentitiesOnly yes\n BatchMode yes\n ConnectTimeout 15\n"
    )
    wrapper = base / "ssh"
    wrapper.write_text('#!/bin/sh\nexec /usr/bin/ssh -F "$SSHI_SMOKE_SSH_CONFIG" "$@"\n')
    wrapper.chmod(0o700)
    tx.update(PATH=f"{base}:{env['PATH']}", SSHI_SMOKE_SSH_CONFIG=str(options))
    profile = "Native Mac"
    command([sshi, "profile", "save", "--name", profile, "--user", user, "--endpoint", receiver, "--relay", url], tx)
    connect = [sshi, "connect", profile]
    with contextlib.ExitStack() as jobs:
        relay_log = base / "relay.log"
        daemon = jobs.enter_context(background([relay, "--dev", "--config-path", relay_config], env, relay_log))
        wait("loopback relay", lambda: listening(relay_port), daemon, relay_log)
        ssh_log = base / "sshd.log"
        daemon = jobs.enter_context(background(["/usr/sbin/sshd", "-D", "-e", "-f", ssh_config], env, ssh_log))
        wait("loopback sshd", lambda: listening(ssh_port), daemon, ssh_log)
        rx_log = base / "receiver.log"
        daemon = jobs.enter_context(background([sshi, "serve"], rx, rx_log))
        wait("receiver", lambda: f"Ready: {receiver}" in rx_log.read_text(), daemon, rx_log)

        denied = command(connect + ["true"], tx, check=False)
        assert denied.returncode != 0 and b"not allowed" in denied.stderr, denied.stderr
        terminal_config = base / "interactive.config"
        terminal_config.write_text(options.read_text().replace(f"IdentityFile {base}/client", f"IdentityFile {base}/interactive")
                                   .replace("BatchMode yes", "BatchMode no").replace("ConnectTimeout 15", "ConnectTimeout 45"))
        grant = command([sshi, "invite", client], rx).stdout
        first_login([sshi, destination], dict(tx, SSHI_SMOKE_SSH_CONFIG=str(terminal_config)), grant, client,
                    shell_prompt=False)
        command([sshi, "revoke-invite", client], rx)
        wait("interactive broker shutdown", lambda: not (base / "tx/sshi/proxy.sock").exists(), timeout=15)
        print("First interactive login prompted, accepted a hidden code and authenticated", flush=True)
        code = command([sshi, "invite", client], rx).stdout
        command([sshi, "invitation", "redeem", "--endpoint", receiver], tx, data=code)
        assert command(connect + ["true"], tx).returncode == 0
        command([sshi, "revoke-invite", client], rx)
        wait("first broker shutdown", lambda: not (base / "tx/sshi/proxy.sock").exists(), timeout=15)
        link = command([sshi, "invite", client, "--share", "--user", user], rx).stdout
        preview = command([sshi, "invitation", "inspect"], tx, data=link)
        assert json.loads(preview.stdout) == {"user": user, "endpoint": receiver, "relay": url}
        alternate = dict(tx, IROH_RELAY_URL="http://127.0.0.1:1/")
        command([sshi, "invitation", "redeem"], alternate, data=link)
        print("Separate code and complete invitation link redeemed through the local relay", flush=True)
        baseline = rx_log.read_text().count(f"Shared iroh connection from {client}")

        session_log = base / "session.log"
        remote = """sh -c 'echo first-ready; read -r reply && echo "received-$reply"'"""
        with background(connect + [remote], tx, session_log, stdin=subprocess.PIPE) as first:
            wait("first SSH session", lambda: "first-ready" in session_log.read_text(), first, session_log)
            who = command(connect + ["id -un"], tx)
            assert who.stdout.decode().strip() == user, who.stdout
            first.stdin.write(b"second-finished\n")
            first.stdin.flush()
            assert first.wait(timeout=15) == 0, session_log.read_text()
            assert "received-second-finished" in session_log.read_text()
        assert command([sshi, "list"], tx).stdout.decode().splitlines() == [destination]
        assert rx_log.read_text().count(f"Shared iroh connection from {client}") == baseline + 1, rx_log.read_text()
        wait("broker shutdown", lambda: not (base / "tx/sshi/proxy.sock").exists(), timeout=15)
        print("Two overlapping named SSH sessions authenticated, shared one Iroh connection and closed cleanly", flush=True)

        control = [sshi, "socket"]
        path = Path(command(control + ["path", "--", profile], tx).stdout.decode().strip())
        native = shlex.split(command(control + ["command", "--", profile], tx).stdout.decode())
        assert native[0] == "ssh" and native[-1] == destination and str(path) in native
        assert path.parent == Path(f"/tmp/sshi-{os.geteuid()}") and path.name.startswith(receiver[:12] + "-")
        assert not path.exists() and command(native + ["true"], tx, check=False).returncode != 0
        baseline = rx_log.read_text().count(f"Shared iroh connection from {client}")
        auth = ssh_log.read_text().count(f"Accepted publickey for {user} ")
        try:
            # File-backed output lets a background master finish detaching even if
            # an OpenSSH version leaves diagnostic descriptors open in its proxy.
            start_log = base / "socket-start.log"
            with start_log.open("wb") as output:
                start = subprocess.run(control + ["start", "--persist", "1m", "--", profile], env=tx,
                                       stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, timeout=30)
            assert start.returncode == 0, start_log.read_text(errors="replace")
            assert path.is_socket() and path.parent.stat().st_mode & 0o077 == 0
            command(control + ["check", "--", profile], tx)
            first = command(native + ["printf via-local-socket; exit 37"], tx, check=False)
            assert first.stdout == b"via-local-socket" and first.returncode == 37, first
            assert command(native + ["id -un"], tx).stdout.decode().strip() == user
            assert rx_log.read_text().count(f"Shared iroh connection from {client}") == baseline + 1
            assert ssh_log.read_text().count(f"Accepted publickey for {user} ") == auth + 1
        finally:
            if path.is_socket():
                command(control + ["close", "--", profile], tx)
                wait("control socket removal", lambda: not path.exists(), timeout=10)
            # The fixture owns this unique endpoint and has no concurrent clients.
            path.with_suffix(".lock").unlink(missing_ok=True)
        assert command(control + ["check", "--", profile], tx, check=False).returncode != 0
        assert command(native + ["true"], tx, check=False).returncode != 0
        wait("socket broker shutdown", lambda: not (base / "tx/sshi/proxy.sock").exists(), timeout=15)
        print("Managed socket started through Iroh, reused one SSH login, and closed cleanly", flush=True)


if __name__ == "__main__":
    # Darwin's Unix-domain socket pathname limit is 104 bytes.
    with tempfile.TemporaryDirectory(dir="/tmp", prefix="sshi-mac.") as work:
        try:
            smoke(Path(work))
        except Exception:
            sshd_log = Path(work) / "sshd.log"
            if sshd_log.is_file():
                print("Ephemeral OpenSSH server log:\n" + sshd_log.read_text(errors="replace")[-2500:],
                      file=sys.stderr, flush=True)
            raise
