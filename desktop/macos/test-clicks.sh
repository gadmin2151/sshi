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
    SSHI.swift Localization.swift Forwarding.swift Invitations.swift Sockets.swift test-clicks.swift -o "$bundle/MacOS/SSHI"
python3 -c 'import subprocess,sys; subprocess.run(sys.argv[1:], check=True, timeout=30)' "$bundle/MacOS/SSHI"
