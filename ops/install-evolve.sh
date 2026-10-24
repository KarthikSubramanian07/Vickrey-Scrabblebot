#!/usr/bin/env bash
# Build the simulator and load the 3-hourly self-improvement LaunchAgent.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="com.vickrey.evolve"
DEST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
(cd "$ROOT" && cargo build --release -p vickrey-sim --bin vickrey-arena)
mkdir -p "$ROOT/runs" "$HOME/Library/LaunchAgents"
# EVOLVE_PROFILES=multi skips the heads-up profile (e.g. while a dedicated duel tuner runs).
PROFILES="${EVOLVE_PROFILES:-multi,duel}"
sed -e "s#__REPO__#$ROOT#g" -e "s#__PROFILES__#$PROFILES#g" "$ROOT/ops/launchd/$LABEL.plist" > "$DEST"
plutil -lint "$DEST" >/dev/null
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$DEST"
echo "loaded $LABEL (every 3 h); logs: $ROOT/runs/evolve.err.log, history: $ROOT/runs/evolution.jsonl"
