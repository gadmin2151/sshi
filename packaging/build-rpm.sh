#!/bin/sh
set -eu
deb=${1:?usage: build-rpm.sh PACKAGE.deb [OUTPUT_DIR]}
out=${2:-dist}
packaging=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
version=$(dpkg-deb -f "$deb" Version)
case "$(dpkg-deb -f "$deb" Architecture)" in
    amd64) cpu=x86_64 ;;
    arm64) cpu=aarch64 ;;
    *) echo "Expected Debian amd64 or arm64 package" >&2; exit 1 ;;
esac
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/SOURCES" "$out"
dpkg-deb --fsys-tarfile "$deb" > "$stage/SOURCES/payload.tar"
rpmbuild --define "_topdir $stage" --define "sshi_version $version" \
    --target "$cpu" -bb "$packaging/sshi.spec"
install -m 644 "$stage/RPMS/$cpu/sshi-$version-1.$cpu.rpm" "$out/"
