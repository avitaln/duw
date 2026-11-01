#!/usr/bin/env bash
# Build duw.app (see bundle.sh) and install it into /Applications.
set -euo pipefail
source "$(dirname "$0")/bundle.sh"

DEST=${DUW_INSTALL_DIR:-/Applications}

# Quit a running copy so the new one is what opens next.
if pgrep -qf "$DEST/$APP_NAME.app/Contents/MacOS/$APP_NAME"; then
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  sleep 1
fi

rm -rf "$DEST/$APP_NAME.app"
cp -R "$APP" "$DEST/"
echo "Installed $DEST/$APP_NAME.app ($VERSION, build $BUILD)"
