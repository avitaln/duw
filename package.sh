#!/usr/bin/env bash
# Build duw.app (see bundle.sh) and wrap it in target/duw-<version>.dmg:
# a disk image showing duw.app next to an Applications shortcut, so people
# install it by dragging one onto the other.
set -euo pipefail
source "$(dirname "$0")/bundle.sh"

DMG=target/$APP_NAME-$VERSION.dmg
STAGE=target/dmg
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

hdiutil create -quiet -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG"
codesign --force --sign "$SIGN_AS" "$DMG"
rm -rf "$STAGE"
echo "Packaged $DMG ($(du -h "$DMG" | cut -f1), version $VERSION, build $BUILD)"
