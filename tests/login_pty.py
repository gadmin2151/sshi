"""Drive an interactive first SSHI login in an isolated test terminal."""
import errno
import os
import pty
import re
import select
import signal
import termios
import time


def first_login(args, env, grant, client_id, prompt=b"Invitation code or link (Enter to cancel): ", shell_prompt=True):
    """Exercise a controlling terminal, including hidden entry inside ProxyCommand."""
    pid, master = pty.fork()
    if pid == 0:
        os.execvpe(args[0], args, env)
    output = bytearray()
    sent = exited = reaped = False
    deadline = time.monotonic() + 40
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], .12)[0]:
                try:
                    output.extend(os.read(master, 4096))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            if not sent and prompt in output and not (termios.tcgetattr(master)[3] & termios.ECHO):
                assert client_id.encode() in output
                os.write(master, grant.strip() + b"\n")
                sent = True
            if shell_prompt and sent and not exited and re.search(rb"# (?:\x1b\[[0-9;?]*[a-zA-Z])*\Z", output):
                os.write(master, b"exit 36\n")
                exited = True
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                reaped = True
                assert sent and (exited if shell_prompt else b"sshi-pty-authed" in output) and os.waitstatus_to_exitcode(status) == 36, "first interactive login failed"
                assert grant.strip() not in output, "terminal echoed the invitation"
                return
        raise AssertionError(f"first interactive login timed out (prompt answered={sent}, shell opened={exited})")
    finally:
        os.close(master)
        if not reaped:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
