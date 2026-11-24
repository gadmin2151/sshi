#!/bin/bash
# Run after installing the matching CLI on an Apple Silicon Mac with a GUI session.
set -euo pipefail
cd "$(dirname "$0")"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export XDG_STATE_HOME="$work/state" XDG_CONFIG_HOME="$work/config"
cli=$(command -v sshi)
peer=$("$cli" id)
"$cli" profile save --name First --user one --endpoint "$peer"
"$cli" profile save --name Second --user two --endpoint "$peer" --relay "https://relay.example.org/"
bundle="$work/SSHI.app/Contents"
mkdir -p "$bundle/MacOS" "$bundle/Resources/bin"
cp Info.plist "$bundle/"
ln -s "$cli" "$bundle/Resources/bin/sshi"
clang -Wall -Wextra -Werror socket-start.c -o "$bundle/Resources/bin/sshi-socket-start"
swiftc -swift-version 5 -parse-as-library -DPROFILE_CLICK_TEST -target arm64-apple-macos12.0 \
    SSHI.swift Localization.swift Forwarding.swift Invitations.swift Sockets.swift FileBrowser.swift test-clicks.swift -o "$bundle/MacOS/SSHI"
python3 - "$bundle/MacOS/SSHI" "$work/sshi-sample.txt" <<'PY'
from pathlib import Path
import subprocess
import sys

program, report = sys.argv[1:]
with subprocess.Popen([program]) as process:
    try:
        sys.exit(process.wait(timeout=30))
    except subprocess.TimeoutExpired:
        print("Timed out in the SSHI AppKit tests. Sampling the main thread.", file=sys.stderr)
        try:
            subprocess.run(["/usr/bin/sample", str(process.pid), "1", "1", "-file", report],
                           check=False, timeout=5)
            sample = Path(report)
            if sample.exists():
                lines = sample.read_text(errors="replace").splitlines()
                start = next((index for index, line in enumerate(lines) if "Call graph:" in line), 0)
                print("\n".join(lines[start:start + 80]), file=sys.stderr)
        except (OSError, subprocess.TimeoutExpired) as error:
            print(error, file=sys.stderr)
        finally:
            process.kill()
            process.wait()
        sys.exit(1)
PY
