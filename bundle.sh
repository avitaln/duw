#!/usr/bin/env bash
# Build duw and assemble a signed target/bundle/duw.app.
# Sourced by install.sh and package.sh; sets APP_NAME, BUNDLE_ID, VERSION, BUILD, APP.
#
# Signing: uses the code-signing identity named in $DUW_SIGN_IDENTITY (default
# "duw Local Signing") when it exists in your keychain, otherwise ad hoc.
# With a stable identity, permissions such as Full Disk Access survive
# updates; with ad hoc signing macOS treats every build as a new app.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
APP_NAME=duw
BUNDLE_ID=com.avitaln.duw
IDENTITY=${DUW_SIGN_IDENTITY:-duw Local Signing}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)

cargo build --release

APP=target/bundle/$APP_NAME.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/$APP_NAME "$APP/Contents/MacOS/$APP_NAME"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>duw</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$BUILD</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSDesktopFolderUsageDescription</key><string>duw measures how much disk space your Desktop uses.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>duw measures how much disk space your Documents use.</string>
  <key>NSDownloadsFolderUsageDescription</key><string>duw measures how much disk space your Downloads use.</string>
  <key>NSRemovableVolumesUsageDescription</key><string>duw measures how much disk space is used on removable drives.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>duw measures how much disk space is used on network drives.</string>
</dict>
</plist>
PLIST

# No -v: a self-signed certificate is "not trusted", but codesign can still use it.
if security find-identity -p codesigning | grep -q "\"$IDENTITY\""; then
  SIGN_AS=$IDENTITY
  echo "Signing with \"$IDENTITY\""
else
  SIGN_AS=-
  echo "Signing ad hoc (no \"$IDENTITY\" identity; permissions reset after each update)"
fi
codesign --force --sign "$SIGN_AS" "$APP"
