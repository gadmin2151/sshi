#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)/$(uname -m)" != Darwin/arm64 ]]; then
    echo 'Build the macOS installer on an Apple Silicon Mac with Xcode Command Line Tools.' >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=12.0
cargo build --locked --release --bin sshi --target aarch64-apple-darwin
cli=target/aarch64-apple-darwin/release/sshi
version=$("$cli" --version)
version=${version#sshi }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/root/Applications/SSHI.app"
mkdir -p "$app/Contents/"{MacOS,Resources/bin} "$work/root/usr/local/bin" "$work/root/usr/local/share/man/man1" "$work/scripts" dist
install -m 755 "$cli" "$app/Contents/Resources/bin/sshi"
clang -Wall -Wextra -Werror -O2 desktop/macos/socket-start.c -o "$app/Contents/Resources/bin/sshi-socket-start"
swiftc -swift-version 5 -parse-as-library -O -target arm64-apple-macos12.0 \
    desktop/macos/{SSHI,Localization,Forwarding,Invitations,Sockets}.swift -o "$app/Contents/MacOS/SSHI"
install -m 644 desktop/macos/Info.plist "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$app/Contents/Info.plist"
cp packaging/config.toml LICENSE "$app/Contents/Resources/"
icon="$work/SSHI.iconset"
mkdir "$icon"
for side in 16 32 128 256 512; do
    sips -z "$side" "$side" assets/windows/sshi-logo.png --out "$icon/icon_${side}x${side}.png" >/dev/null
    double=$((side * 2))
    sips -z "$double" "$double" assets/windows/sshi-logo.png --out "$icon/icon_${side}x${side}@2x.png" >/dev/null
done
iconutil -c icns "$icon" -o "$app/Contents/Resources/sshi.icns"
codesign --force --sign - "$app/Contents/Resources/bin/sshi"
codesign --force --sign - "$app/Contents/Resources/bin/sshi-socket-start"
codesign --force --sign - "$app"
codesign --verify --deep --strict "$app"
for tool in sshi sshi-copy-iid sshi-copy-id; do
    ln -s /Applications/SSHI.app/Contents/Resources/bin/sshi "$work/root/usr/local/bin/$tool"
done
gzip -nc packaging/sshi.1 > "$work/root/usr/local/share/man/man1/sshi.1.gz"
for tool in sshi-copy-iid sshi-copy-id; do
    ln -s sshi.1.gz "$work/root/usr/local/share/man/man1/$tool.1.gz"
done
cat > "$work/scripts/postinstall" <<'POST'
#!/bin/sh
set -eu
settings="${3%/}/etc/sshi"
mkdir -p "$settings"
if [ ! -e "$settings/config.toml" ]; then
    install -m 644 "${3%/}/Applications/SSHI.app/Contents/Resources/config.toml" "$settings/config.toml"
fi
POST
chmod 755 "$work/scripts/postinstall"
# Install at a stable path so the command-line symlinks always find their bundle.
pkgbuild --analyze --root "$work/root" "$work/components.plist"
/usr/libexec/PlistBuddy -c 'Set :0:BundleIsRelocatable false' "$work/components.plist"
pkgbuild --root "$work/root" --scripts "$work/scripts" --component-plist "$work/components.plist" \
    --identifier com.github.gadmin2151.sshi --version "$version" --install-location / \
    "dist/sshi-$version-macos-arm64.pkg"
