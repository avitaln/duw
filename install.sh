#!/usr/bin/env bash
# Build duw, package it as duw.app and install it into /Applications.
#
# Signing: uses the code-signing identity named in $DUW_SIGN_IDENTITY (default
# "duw Local Signing") when it exists in your keychain, otherwise ad hoc.
# With a stable identity, a Full Disk Access grant survives reinstalls; with
# ad hoc signing macOS treats every build as a new app.
set -euo pipefail

cd "$(dirname "$0")"
APP_NAME=duw
BUNDLE_ID=com.avitaln.duw
DEST=${DUW_INSTALL_DIR:-/Applications}
IDENTITY=${DUW_SIGN_IDENTITY:-duw Local Signing}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)

cargo build --release

STAGE=target/bundle/$APP_NAME.app
rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp target/release/$APP_NAME "$STAGE/Contents/MacOS/$APP_NAME"

cat > "$STAGE/Contents/Info.plist" <<PLIST
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

if security find-identity -v -p codesigning | grep -q "\"$IDENTITY\""; then
  echo "Signing with \"$IDENTITY\""
  codesign --force --options runtime --sign "$IDENTITY" "$STAGE"
else
  echo "Signing ad hoc (no \"$IDENTITY\" identity; Full Disk Access must be re-granted after each install)"
  codesign --force --sign - "$STAGE"
fi

# Quit a running copy so the new one is what opens next.
if pgrep -qf "$DEST/$APP_NAME.app/Contents/MacOS/$APP_NAME"; then
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  sleep 1
fi

rm -rf "$DEST/$APP_NAME.app"
cp -R "$STAGE" "$DEST/"
echo "Installed $DEST/$APP_NAME.app ($VERSION, build $BUILD)"
