#!/usr/bin/env bash
# Run on x86_64 Arch Linux as an unprivileged user with base-devel installed.
set -euo pipefail
deb=$(realpath -- "${1:?usage: build-arch.sh SSHI_AMD64.deb [OUTPUT_DIR]}")
out=$(realpath -m -- "${2:-dist}")
control=$(bsdtar -xOf "$deb" 'control.tar*' | bsdtar -xOf - ./control)
version=$(sed -n 's/^Version: //p' <<< "$control")
glibc_min=$(sed -n 's/^Depends: libc6 (>= \([0-9.]*\)).*/\1/p' <<< "$control")
if ! grep -qx 'Package: sshi' <<< "$control" || ! grep -qx 'Architecture: amd64' <<< "$control" ||
    ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && $glibc_min =~ ^[0-9]+\.[0-9]+$ ]]; then
    echo 'Expected a stable SSHI amd64 Debian package with a glibc dependency.' >&2
    exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
cd "$stage"
input="sshi_${version}_amd64.deb"
cp -- "$deb" "$input"
checksum=$(sha256sum -- "$input" | cut -d' ' -f1)
{
    printf 'pkgver=%s\n_glibc=%s\nsha256sums=(%s)\n' "$version" "$glibc_min" "$checksum"
    cat <<'PKGBUILD'
pkgname=sshi
pkgrel=1
pkgdesc='SSHI GTK desktop, CLI and OpenSSH receiver over an Iroh relay'
arch=('x86_64')
url='https://github.com/gadmin2151/sshi'
license=('MIT')
depends=("glibc>=${_glibc}" 'libgcc' 'openssh' 'ca-certificates' 'gtk3' 'python-gobject')
optdepends=('man-db: read the SSHI manual' 'systemd: run the per-user receiver')
backup=('etc/sshi/config.toml')
options=('!debug' '!strip' '!zipman')
source=("sshi_${pkgver}_amd64.deb")
noextract=("${source[@]}")

package() {
    set -o pipefail
    bsdtar -xOf "$srcdir/${source[0]}" 'data.tar*' | bsdtar -xpf - --no-same-owner -C "$pkgdir"
    install -Dm644 "$pkgdir/usr/share/doc/sshi/copyright" "$pkgdir/usr/share/licenses/sshi/LICENSE"
}
PKGBUILD
} > PKGBUILD
PKGDEST="$stage" PKGEXT=.pkg.tar.zst PACKAGER='SSHI Project <gadmin2151@users.noreply.github.com>' makepkg --nodeps --noconfirm
mkdir -p "$out"
install -m644 "sshi-$version-1-x86_64.pkg.tar.zst" "$out/"
