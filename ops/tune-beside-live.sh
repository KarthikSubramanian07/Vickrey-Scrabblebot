#!/usr/bin/env bash
# Tune next to the live bot. Safe since the anchor bid (an instant bid sent
# before valuation): a slow full valuation can no longer lose an auction.
# A guard still pauses the simulator (SIGSTOP) for a minute whenever the
# bot logs a late start or a late rejection, then resumes it.
# Usage: ops/tune-beside-live.sh  (writes runs/tune/live-tune-<ts>.log)
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export RAYON_NUM_THREADS=${RAYON_NUM_THREADS:-3}
A=./target/release/vickrey-arena
ts=$(date -u +%Y%m%dT%H%M)
log=runs/tune/live-tune-$ts.log
out=runs/tune/live-$ts; mkdir -p "$out"
guard() {
  local last; last=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
  while sleep 20; do
    local now; now=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
    if [ "$now" -gt "$last" ]; then
      echo "guard: bot logged $((now-last)) late event(s); pausing the simulator 60 s" >> "$log"
      pkill -STOP -f vickrey-arena; sleep 60; pkill -CONT -f vickrey-arena
      now=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
    fi
    last=$now
  done
}
guard & GUARD=$!
trap 'kill $GUARD 2>/dev/null; pkill -CONT -f vickrey-arena 2>/dev/null' EXIT
run() { nice -n 19 "$@"; }
{
  echo "== $(date -u +%H:%M) calibrate the Nijello clone (live on our tables: ~12 letters, ~23 leftover, ~86 score)"
  best=""; bd=99
  for k in 1.0 1.4 1.8 2.3 3.0; do
    line=$(VICKREY_NIJELLO_SCALE=$k run $A bench --lineup vickrey,nijello,nijello,ramp,sookra,flat -n 32 --seed 13 | grep "^nijello")
    echo "scale $k: $line"
    l=$(echo "$line" | awk '{print $7}'); d=$(echo "$l - 12" | bc -l | tr -d -); 
    if [ "$(echo "$d < $bd" | bc -l)" = 1 ]; then bd=$d; best=$k; fi
  done
  echo "chosen VICKREY_NIJELLO_SCALE=$best"; export VICKREY_NIJELLO_SCALE=$best
  echo "== $(date -u +%H:%M) champion at a calibrated tournament-eve table (n 48)"
  run $A bench --lineup vickrey,nijello,nijello,ramp,sookra,hunter -n 48 --seed 7
  echo "== $(date -u +%H:%M) heads-up final: duel profile vs calibrated Nijello (n 32)"
  run $A --params config/params_duel.json bench --lineup vickrey,nijello -n 32 --seed 7
  echo "== $(date -u +%H:%M) tune duel vs calibrated Nijello (14 iters x 24)"
  VICKREY_DUEL_RIVAL=nijello run $A --params config/params_duel.json tune --profile duel --iters 14 --matches 24 --out "$out/duel.json" --history "$out/duel.jsonl" --seed 9 2>&1 | grep -E "iter|ACCEPTED"
  if [ -f "$out/duel.json" ]; then
    echo "== $(date -u +%H:%M) duel: tuned vs current, fresh seed (n 64)"
    VICKREY_DUEL_RIVAL=nijello run $A --params "$out/duel.json" --params-b config/params_duel.json compare --profile duel -n 64 --seed 78
  fi
  echo "== $(date -u +%H:%M) tune 6-seat on the calibrated field (16 iters x 48)"
  run $A tune --profile multi --iters 16 --matches 48 --out "$out/multi.json" --history "$out/multi.jsonl" --seed 5 2>&1 | grep -E "iter|ACCEPTED"
  if [ -f "$out/multi.json" ]; then
    echo "== $(date -u +%H:%M) 6-seat: tuned vs champion, fresh seed (n 128)"
    run $A --params "$out/multi.json" --params-b config/params.json compare --profile multi -n 128 --seed 77
  fi
  echo "== $(date -u +%H:%M) live A/B challenger on the calibrated field (n 128)"
  run $A --params config/params_b.json --params-b config/params.json compare --profile multi -n 128 --seed 79
  echo "== $(date -u +%H:%M) done; outputs in $out"
} >> "$log" 2>&1
