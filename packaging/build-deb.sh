#!/bin/sh
set -eu

binary=${1:-target/release/sshi}
output=${2:-dist}
version=${3:-}
packaging=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

export LC_ALL=C
elf_header=$(readelf -h -- "$binary")
machine=$(printf '%s\n' "$elf_header" | sed -n 's/^[[:space:]]*Machine:[[:space:]]*//p')
program_headers=$(readelf -lW -- "$binary")
interpreter=$(printf '%s\n' "$program_headers" | sed -n 's/.*\[Requesting program interpreter: \(.*\)\]/\1/p')
case "$machine:$interpreter" in
    'Advanced Micro Devices X86-64:/lib64/ld-linux-x86-64.so.2')
        arch=amd64
        strip_tool=x86_64-linux-gnu-strip
        ;;
    'AArch64:/lib/ld-linux-aarch64.so.1')
        arch=arm64
        strip_tool=aarch64-linux-gnu-strip
        ;;
    'ARM:/lib/ld-linux-armhf.so.3')
        arch=armhf
        strip_tool=arm-linux-gnueabihf-strip
        ;;
    *)
        printf 'Unsupported Debian executable %s (%s, %s)\n' "$binary" "$machine" "$interpreter" >&2
        exit 1
        ;;
esac

if [ "$arch" = "$(dpkg --print-architecture)" ]; then
    strip_tool=strip
elif [ -z "$version" ]; then
    printf 'Pass the package version as the third argument for a cross-built binary.\n' >&2
    exit 1
fi
if [ -z "$version" ]; then
    version=$("$binary" --version)
    version=${version#sshi }
fi
dpkg --validate-version "$version"

# The release baseline is Debian 11. Raise the dependency for builds linked to newer glibc.
glibc_min=2.31
version_info=$(readelf -VW -- "$binary")
glibc_required=$(printf '%s\n' "$version_info" | sed -n \
    -e 's/.*Name: GLIBC_\([0-9][0-9.]*\) .*/\1/p' \
    -e 's/.*Name: GLIBC_ABI_DT_RELR .*/2.36/p' | sort -V | tail -n 1)
if [ -n "$glibc_required" ] && dpkg --compare-versions "$glibc_required" gt "$glibc_min"; then
    glibc_min=$glibc_required
fi

staging=$(mktemp -d)
trap 'rm -rf -- "$staging"' EXIT
chmod 755 "$staging"
install -d -m 755 "$staging/DEBIAN" "$staging/usr/share/man/man1" "$output"
install -Dm 755 "$binary" "$staging/usr/bin/sshi"
"$strip_tool" --strip-unneeded "$staging/usr/bin/sshi"
ln -s sshi "$staging/usr/bin/sshi-copy-iid"
ln -s sshi-copy-iid "$staging/usr/bin/sshi-compi-id"
ln -s sshi-compi-id "$staging/usr/bin/sshi-compy-id"
ln -s sshi "$staging/usr/bin/sshi-copy-id"
install -Dm 755 "$packaging/../desktop/linux/sshi-desktop" "$staging/usr/bin/sshi-desktop"
install -Dm 644 "$packaging/../desktop/linux/sshi.desktop" "$staging/usr/share/applications/sshi.desktop"
install -Dm 644 "$packaging/../assets/windows/sshi-logo.png" "$staging/usr/share/pixmaps/sshi.png"
install -Dm 644 "$packaging/../LICENSE" "$staging/usr/share/doc/sshi/copyright"
install -Dm 644 "$packaging/config.toml" "$staging/etc/sshi/config.toml"
install -Dm 644 "$packaging/sshi.service" "$staging/usr/lib/systemd/user/sshi.service"
install -Dm 644 "$packaging/sshid.service" "$staging/usr/lib/systemd/system/sshid.service"
gzip -n -9 -c "$packaging/sshi.1" > "$staging/usr/share/man/man1/sshi.1.gz"
chmod 644 "$staging/usr/share/man/man1/sshi.1.gz"
ln -s sshi.1.gz "$staging/usr/share/man/man1/sshi-compi-id.1.gz"
ln -s sshi.1.gz "$staging/usr/share/man/man1/sshi-copy-iid.1.gz"
ln -s sshi.1.gz "$staging/usr/share/man/man1/sshi-compy-id.1.gz"
ln -s sshi.1.gz "$staging/usr/share/man/man1/sshi-copy-id.1.gz"

printf '%s\n' /etc/sshi/config.toml > "$staging/DEBIAN/conffiles"
size=$(du -sk "$staging" | cut -f 1)
cat > "$staging/DEBIAN/control" <<EOF
Package: sshi
Version: $version
Section: net
Priority: optional
Architecture: $arch
Maintainer: SSHI Project <gadmin2151@users.noreply.github.com>
Installed-Size: $size
Depends: libc6 (>= $glibc_min), libgcc-s1, ca-certificates, openssh-client
Homepage: https://github.com/gadmin2151/sshi
Recommends: man-db, python3-gi, gir1.2-gtk-3.0, x-terminal-emulator
Suggests: openssh-server
Description: OpenSSH client and receiver over an iroh relay
 Connect to an OpenSSH daemon by iroh Endpoint ID. Includes a persistent
 local identity, GTK desktop client, profiles, backups and port forwarding.
EOF

dpkg-deb --root-owner-group --build "$staging" "$output/sshi_${version}_${arch}.deb"
