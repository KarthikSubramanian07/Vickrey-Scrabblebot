#!/usr/bin/env bash
# Retune against the live leader. Calibrates the data-fitted Lambi Lambiii
# clone to its live numbers (14 letters, ~9 coins left, ~101 points), then
# re-scores the champion, the live challenger and the queued experiments on
# a tournament-strength table, tunes the 6-seat profile on it, and finally
# tunes the heads-up profile against the calibrated leader. Runs beside the
# live bot at the lowest priority with the same late-event guard as
# ops/tune-beside-live.sh. Log: runs/tune/lambi-<ts>.log
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export RAYON_NUM_THREADS=${RAYON_NUM_THREADS:-5} VICKREY_NIJELLO_SCALE=1.4
A=./target/release/vickrey-arena
ts=$(date -u +%Y%m%dT%H%M); log=runs/tune/lambi-$ts.log; out=runs/tune/lambi-$ts; mkdir -p "$out" runs/tune/cands
guard() {
  local last; last=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
  while sleep 20; do
    local now; now=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
    if [ "$now" -gt "$last" ]; then
      echo "guard: $((now-last)) late event(s); pausing 60 s" >> "$log"
      pkill -STOP -f vickrey-arena; sleep 60; pkill -CONT -f vickrey-arena
      now=$(grep -cE "late start|rejected: Auction closed" runs/vickrey.out.log)
    fi
    last=$now
  done
}
guard & GUARD=$!
trap 'kill $GUARD 2>/dev/null; pkill -CONT -f vickrey-arena 2>/dev/null' EXIT
run() { nice -n 19 "$@"; }
TABLE=vickrey,lambi,lambi,nijello,sookra,ramp
{
  echo "== $(date -u +%H:%M) calibrate the Lambi clone (live: 14.0 letters, 8.8 left, 101 pts)"
  best=1.0; bd=99
  for k in 0.6 0.7 0.8 0.9 1.0 1.1; do
    line=$(VICKREY_LAMBI_SCALE=$k run $A bench --lineup $TABLE -n 32 --seed 13 | grep "^lambi")
    echo "scale $k: $line"
    d=$(echo "$line" | awk '{l=$7-14; b=$9-8.8; if(l<0)l=-l; if(b<0)b=-b; print l+b/4}')
    if [ "$(echo "$d < $bd" | bc -l)" = 1 ]; then bd=$d; best=$k; fi
  done
  echo "chosen VICKREY_LAMBI_SCALE=$best"; export VICKREY_LAMBI_SCALE=$best
  echo "== $(date -u +%H:%M) champion at the leader's table (n 64)"
  run $A bench --lineup $TABLE -n 64 --seed 7
  echo "== $(date -u +%H:%M) candidates vs champion on the new field (n 128 each)"
  run $A --params config/params_b.json --params-b config/params.json compare --profile multi -n 128 --seed 41 | sed 's/^/live challenger: /'
  python3 - <<'PY' > "$out/cands.txt"
import json
base=json.load(open('config/params.json'))
cands={
 'min_play_len 6 (cash 6-letter words like the leader)':{'min_play_len':6},
 'min_play_len 5':{'min_play_len':5},
 'income_factor 0.6':{'income_factor':0.6},
 'income_factor 0.8':{'income_factor':0.8},
 'late_window 17 + late_min_play_len 5':{'late_window':17,'late_min_play_len':5},
 'spend_target 1.15':{'spend_target':1.15},
 'premium cap 0.7 all match':{'premium_cap_mult':0.7,'premium_cap_until':1.0},
}
for i,(n,p) in enumerate(cands.items()):
    c=dict(base); c.update(p); f=f'runs/tune/cands/l{i}.json'; json.dump(c,open(f,'w')); print(f"{f}\t{n}")
PY
  while IFS=$'\t' read -r f name; do
    r=$(run $A --params "$f" --params-b config/params.json compare --profile multi -n 128 --seed 42)
    echo "$name: $r"
  done < "$out/cands.txt"
  echo "== $(date -u +%H:%M) tune 6-seat on the leader's field (16 iters x 48)"
  run $A tune --profile multi --iters 16 --matches 48 --out "$out/multi.json" --history "$out/multi.jsonl" --seed 15 2>&1 | grep -E "ACCEPTED"
  [ -f "$out/multi.json" ] && { echo "== $(date -u +%H:%M) tuned vs champion (n 192)"; run $A --params "$out/multi.json" --params-b config/params.json compare --profile multi -n 192 --seed 77; }
  echo "== $(date -u +%H:%M) heads-up vs the calibrated leader (n 24)"
  run $A --params config/params_duel.json bench --lineup vickrey,lambi -n 24 --seed 7
  echo "== $(date -u +%H:%M) tune duel vs the calibrated leader (10 iters x 24)"
  VICKREY_DUEL_RIVAL=lambi run $A --params config/params_duel.json tune --profile duel --iters 10 --matches 24 --out "$out/duel.json" --history "$out/duel.jsonl" --seed 19 2>&1 | grep -E "ACCEPTED"
  [ -f "$out/duel.json" ] && { echo "== $(date -u +%H:%M) duel tuned vs current (n 64)"; VICKREY_DUEL_RIVAL=lambi run $A --params "$out/duel.json" --params-b config/params_duel.json compare --profile duel -n 64 --seed 78; }
  echo "== $(date -u +%H:%M) done; outputs in $out"
} >> "$log" 2>&1
