#!/usr/bin/env bash
# Run a CPU-heavy job (simulation, tuning, build) without crowding out the
# live bot's one-second bids: one heavy job at a time (a lock), two worker
# threads, lowest scheduling priority. Usage: ops/heavy.sh <command...>
set -euo pipefail
LOCK="${VICKREY_HEAVY_LOCK:-/tmp/vickrey-heavy.lock}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-2}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
# macOS has no flock(1); use mkdir as the mutex and wait for it.
until mkdir "$LOCK" 2>/dev/null; do
  owner=$(cat "$LOCK/pid" 2>/dev/null || true)
  if [[ -n "$owner" ]] && ! kill -0 "$owner" 2>/dev/null; then rm -rf "$LOCK"; continue; fi
  sleep 5
done
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT
# Run as a child (not exec) so the trap releases the lock when the job ends.
nice -n 19 "$@"
