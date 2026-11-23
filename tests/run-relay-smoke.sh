#!/bin/sh
# Run on Linux with Docker, an amd64 package and an installed iroh-relay 1.5 binary.
set -eu
package=${1:?usage: run-relay-smoke.sh PACKAGE.deb [iroh-relay]}
relay=${2:-iroh-relay}
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
pid=
image="sshi-relay-test:$(basename "$work" | tr '[:upper:]' '[:lower:]')"
cleanup() {
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
    docker image rm "$image" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
cp "$package" "$work/sshi.deb"
cp "$here/relay_smoke.py" "$work"
docker build --build-arg "BASE=${SSHI_SMOKE_IMAGE:-debian:bookworm-slim}" -q -t "$image" -f "$here/relay-smoke.Dockerfile" "$work"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
printf 'http_bind_addr = "127.0.0.1:%s"\nenable_metrics = false\n' "$port" > "$work/relay.toml"
env -u IROH_RELAY_ACCESS_TOKEN "$relay" --dev --config-path "$work/relay.toml" > "$work/relay.log" 2>&1 &
pid=$!
for attempt in $(seq 1 50); do
    if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$port"; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then cat "$work/relay.log" >&2; exit 1; fi
    sleep 0.1
done
docker run --rm --network host -e "SSHI_TEST_RELAY_URL=http://127.0.0.1:$port/" "$image"
