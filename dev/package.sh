#!/usr/bin/env bash
# Usage: dev/package.sh
# Builds the release binary, assembles target/package/Astrolabe.app from it
# and wraps it in target/package/Astrolabe-<version>.dmg. The version is the
# workspace `version`. No code signing or notarization (out of scope for
# 0.0.1). The release workflow runs this same script.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(cargo pkgid -p astrolabe --locked | sed 's/.*[#@]//')
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || { echo "bad version: $version" >&2; exit 1; }

cargo build --release -p astrolabe --locked
bin=$(cargo metadata --format-version 1 --no-deps --locked |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/astrolabe

app=target/package/Astrolabe.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/astrolabe"
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Astrolabe</string>
    <key>CFBundleDisplayName</key><string>Astrolabe</string>
    <key>CFBundleIdentifier</key><string>dev.astrolabe.app</string>
    <key>CFBundleExecutable</key><string>astrolabe</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >&2

dmg=target/package/Astrolabe-$version.dmg
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "Astrolabe $version" -srcfolder "$stage" -format UDZO "$dmg"
echo "$app"
echo "$dmg"
