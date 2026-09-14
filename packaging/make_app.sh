#!/bin/bash
# Builds dist/Snuffler.app -- one universal binary for Apple Silicon and Intel
# -- and dist/Snuffler.dmg, the file that actually gets sent to people.
#
# macOS only: lipo, sips, iconutil, codesign and hdiutil come with the system.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Big Sur is the first release that runs on Apple Silicon; nothing here needs
# anything newer.
export MACOSX_DEPLOYMENT_TARGET=11.0

for target in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$target" >/dev/null
    cargo build --release --locked --target "$target"
done

rm -rf dist
APP=dist/Snuffler.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/Snuffler" \
    target/aarch64-apple-darwin/release/snuffler \
    target/x86_64-apple-darwin/release/snuffler
sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Every size iconutil wants, from the one 1024px master.
ICONSET=dist/AppIcon.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" packaging/AppIcon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) packaging/AppIcon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

# An ad-hoc signature. It is not a Developer ID, so Gatekeeper still asks once
# on the recipient's Mac. But it is not optional either: Apple Silicon will not
# run unsigned code at all, and a bundle whose signature does not cover its
# Info.plist and icon is reported as "damaged" -- with no way past it.
xattr -cr "$APP"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# The disk image. A .dmg carries the bundle and its executable bit intact
# through any transfer -- Google Drive, email, a Windows unzip of the CI
# artifact -- where a bare binary, or a zip made on Windows, would lose them.
STAGE=dist/dmg
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp "packaging/How to open Snuffler.txt" "$STAGE/"
hdiutil create -volname Snuffler -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov dist/Snuffler.dmg
rm -rf "$STAGE"

echo "built dist/Snuffler.app and dist/Snuffler.dmg (version $VERSION)"
