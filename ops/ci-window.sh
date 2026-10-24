#!/usr/bin/env bash
# A CI maintenance window on the 8 GB Mac that also runs the live bot.
# A Rust build next to the bot swaps the machine and makes bids arrive
# late, so: at the next match boundary the bot is swapped for a copy that
# finishes any seated match and does not rejoin the lobby; when it is
# idle the GitHub runner is started, all queued runs complete, the runner
# stops, and the normal bot service comes back. Usage: ops/ci-window.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="com.vickrey.scrabblebot"
DOMAIN="gui/$(id -u)"
RUNNER="$HOME/.vickrey-runner"
cd "$ROOT"
# Never during a tournament.
if curl -s -m 15 -X POST https://maincloud.spacetimedb.com/v1/database/scrabblebot/sql -d "SELECT id FROM tournament" | grep -q '"rows":\[\['; then
  echo "a tournament exists; refusing to open a maintenance window"; exit 1
fi
n=$(wc -l < runs/summary.jsonl)
echo "waiting for the current match to end"
until [ "$(wc -l < runs/summary.jsonl)" -gt "$n" ]; do sleep 0.3; done
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
nohup ./target/release/vickrey run --no-lobby --no-tournaments >> runs/vickrey.out.log 2>> runs/vickrey.err.log &
IDLE_PID=$!
echo "idle bot $IDLE_PID playing out any seated match"
sleep 8
until python3 -c "import json,sys; d=json.load(open('runs/heartbeat')); sys.exit(0 if d['pid']==$IDLE_PID and not d['matches_live'] else 1)" 2>/dev/null; do sleep 2; done
echo "bot idle; starting the runner"
(cd "$RUNNER" && ./svc.sh start >/dev/null 2>&1)
sleep 20
# Wait until no run is queued or in progress (bounded at 40 minutes).
for _ in $(seq 1 160); do
  busy=$(gh run list --limit 10 --json status -q '[.[] | select(.status!="completed")] | length' 2>/dev/null || echo 1)
  [ "$busy" = "0" ] && break
  sleep 15
done
(cd "$RUNNER" && ./svc.sh stop >/dev/null 2>&1)
# Optional extra work while the bot is parked (simulations, tuning).
if [[ -n "${VICKREY_WINDOW_HOOK:-}" ]]; then
  echo "running window hook: $VICKREY_WINDOW_HOOK"
  bash -c "$VICKREY_WINDOW_HOOK" || echo "window hook failed"
fi
echo "runner stopped; restoring the bot"
kill "$IDLE_PID" 2>/dev/null || true
sleep 2
launchctl bootstrap "$DOMAIN" "$HOME/Library/LaunchAgents/$LABEL.plist"
launchctl kickstart -k "$DOMAIN/$LABEL"
sleep 6
cat runs/heartbeat; echo
gh run list --limit 3 --json name,conclusion,headBranch -q '.[] | "\(.headBranch): \(.conclusion)"'
