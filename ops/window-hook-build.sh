#!/usr/bin/env bash
# Window hook: build and test the current tree, then a short clone-field
# bench, so the window closes quickly and the restored bot runs the new
# binary. Use when a code change (not a tune) needs to reach the live bot.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_BUILD_JOBS=1 RAYON_NUM_THREADS=2
log=runs/tune/build-$(date -u +%Y%m%dT%H%M).log
{
  echo "== build"; nice -n 19 cargo build --release --workspace 2>&1 | tail -3 || exit 1
  echo "== tests"; nice -n 19 cargo test --release --workspace 2>&1 | grep -E "^test result|FAILED|panicked" || exit 1
  echo "== bench: tournament-eve table (n 24)"
  nice -n 19 ./target/release/vickrey-arena bench --lineup vickrey,nijello,nijello,ramp,sookra,flat -n 24 --seed 3
  echo "== done"
} > "$log" 2>&1
echo "build hook log: $log"; tail -12 "$log"
