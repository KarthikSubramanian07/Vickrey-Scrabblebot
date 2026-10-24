#!/usr/bin/env bash
# Build the release binary and (re)load the Vickey LaunchAgent for this user.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="com.vickey.scrabblebot"
DEST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
TOKEN="${VICKEY_TOKEN_FILE:-$HOME/.config/vickey/token}"

if [[ ! -s "$TOKEN" ]]; then
  echo "no bot token at $TOKEN; run: target/release/vickey setup --team <TEAM> --bot <BOT>" >&2
  exit 1
fi

echo "building release binary"
(cd "$ROOT" && cargo build --release -p vickey-bot)

mkdir -p "$ROOT/runs" "$HOME/Library/LaunchAgents"
sed -e "s#__REPO__#$ROOT#g" -e "s#__HOME__#$HOME#g" \
  "$ROOT/ops/launchd/$LABEL.plist" > "$DEST"
plutil -lint "$DEST" >/dev/null

# Replace any running instance.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$DEST"
launchctl enable "$DOMAIN/$LABEL"
launchctl kickstart -k "$DOMAIN/$LABEL"

echo "loaded $LABEL"
echo "  logs:      tail -f $ROOT/runs/vickey.err.log"
echo "  heartbeat: cat $ROOT/runs/heartbeat"
echo "  stop:      ops/uninstall-launchd.sh"
