#!/usr/bin/env bash
# Window hook: build, test, then measure the champion against the refit
# clone field and run a short multi-seat tune on it. Runs only while the
# bot is parked by ops/ci-window.sh.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_BUILD_JOBS=1 RAYON_NUM_THREADS=2
log=runs/tune/window-$(date -u +%Y%m%dT%H%M).log
{
  echo "== build"; nice -n 19 cargo build --release --workspace 2>&1 | tail -3 || exit 1
  echo "== tests"; nice -n 19 cargo test --release --workspace 2>&1 | grep -E "^test result|FAILED|panicked" || exit 1
  A=./target/release/vickrey-arena
  echo "== champion vs itself on the clone field (sanity, expect ~0)"
  nice -n 19 $A --params config/params.json --params-b config/params.json compare --profile multi -n 24 --seed 11
  echo "== bench: champion at a tournament-eve table"
  nice -n 19 $A bench --lineup vickrey,nijello,nijello,ramp,sookra,flat -n 48 --seed 7
  echo "== bench: heads-up vs nijello"
  nice -n 19 $A --params config/params_duel.json bench --lineup vickrey,nijello -n 48 --seed 7
  echo "== Nijello clone calibration sweep (live Nijello: ~13 letters, ~28 leftover, ~93 score)"
  for k in 1.0 1.3 1.6 2.0; do
    echo "-- scale $k"; VICKREY_NIJELLO_SCALE=$k nice -n 19 $A bench --lineup vickrey,nijello,nijello,ramp,sookra,flat -n 32 --seed 13 | grep -E "^vickrey|^nijello"
  done
  echo "== queued A/B patches on the clone field (n 96 each)"
  python3 - <<'PY'
import json,subprocess,os
base=json.load(open('config/params.json'))
q=json.load(open('config/ab-queue.json'))
os.makedirs('runs/tune/cands',exist_ok=True)
for i,it in enumerate(q):
    c=dict(base); c.update(it['patch'])
    f=f'runs/tune/cands/q{i}.json'; json.dump(c,open(f,'w'))
    r=subprocess.run(['nice','-n','19','./target/release/vickrey-arena','--params',f,'--params-b','config/params.json','compare','--profile','multi','-n','96','--seed','21'],capture_output=True,text=True)
    print(f"q{i} {it['name'][:70]!r}: {r.stdout.strip()} {r.stderr.strip()[-200:]}")
PY
  echo "== tune multi on the clone field (24 iters x 48)"
  nice -n 19 $A tune --profile multi --iters 24 --matches 48 --out runs/tune/multi-clone2.json --history runs/tune/tune-clone2.jsonl --seed 5 2>&1 | tail -30
  echo "== tune duel vs the Nijello clone (16 iters x 32)"
  VICKREY_DUEL_RIVAL=nijello nice -n 19 $A --params config/params_duel.json tune --profile duel --iters 16 --matches 32 --out runs/tune/duel-nijello.json --history runs/tune/tune-duel-nijello.jsonl --seed 9 2>&1 | tail -20
  echo "== duel: tuned vs current duel params, fresh seed"
  VICKREY_DUEL_RIVAL=nijello nice -n 19 $A --params runs/tune/duel-nijello.json --params-b config/params_duel.json compare --profile duel -n 96 --seed 78
  echo "== tuned vs champion, fresh seed"
  nice -n 19 $A --params runs/tune/multi-clone2.json --params-b config/params.json compare --profile multi -n 128 --seed 77
  echo "== done"
} > "$log" 2>&1
echo "window hook log: $log"
