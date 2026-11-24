#!/bin/sh
set -eu

fail() {
    printf 'SSHI: %s\n' "$1" >&2
    exit 1
}

if [ "$(id -u)" -eq 0 ]; then
    set --
else
    command -v sudo >/dev/null || fail 'Run as root or install sudo.'
    set -- sudo --
fi

if command -v apt-get >/dev/null; then
    manager=apt-get
    arch=$(dpkg --print-architecture)
elif command -v pacman >/dev/null; then
    manager=pacman
    arch=$(uname -m)
else
    for manager in dnf dnf5 yum; do
        if command -v "$manager" >/dev/null; then
            break
        fi
    done
    command -v "$manager" >/dev/null || fail 'Available packages use APT, pacman, DNF or Yum.'
    arch=$(rpm --eval '%{_arch}')
fi

repo=https://github.com/gadmin2151/sshi
release=$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$repo/releases/latest")
case "$release" in
    "$repo/releases/tag/v"*) version=${release##*/v} ;;
    *) fail 'Could not find the latest SSHI release.' ;;
esac
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail 'Unexpected release version.'

case "$manager:$arch" in
    apt-get:amd64|apt-get:arm64|apt-get:armhf) package="sshi_${version}_${arch}.deb" ;;
    pacman:x86_64) package="sshi-$version-1-x86_64.pkg.tar.zst" ;;
    dnf*:x86_64|dnf*:aarch64|yum:x86_64|yum:aarch64) package="sshi-$version-1.$arch.rpm" ;;
    *) fail "No package for $manager on $arch." ;;
esac

work=$(mktemp -d /tmp/sshi-install.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
download="$repo/releases/download/v$version"
curl -fsSL "$download/$package" -o "$work/$package"
curl -fsSL "$download/SHA256SUMS" -o "$work/SHA256SUMS"
(cd "$work" && awk -v package="$package" '$2 == package' SHA256SUMS | sha256sum --check -)

case "$manager" in
    apt-get)
        # APT's download sandbox must be able to read the verified package.
        chmod 755 "$work"
        chmod 644 "$work/$package"
        "$@" apt-get update
        "$@" apt-get install -y "$work/$package"
        ;;
    pacman)
        "$@" pacman -U --needed --noconfirm "$work/$package"
        ;;
    *)
        "$@" "$manager" install -y "$work/$package"
        ;;
esac
