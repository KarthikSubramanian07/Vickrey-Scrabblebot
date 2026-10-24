#!/usr/bin/env bash
# Stop and remove the Vickrey LaunchAgent. Leaves runs/ and the token alone.
set -euo pipefail

LABEL="com.vickrey.scrabblebot"
DEST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || echo "$LABEL was not loaded"
rm -f "$DEST"
echo "removed $LABEL"
