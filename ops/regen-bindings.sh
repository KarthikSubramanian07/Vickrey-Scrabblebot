#!/usr/bin/env bash
# Regenerate the Rust client bindings for the live `scrabblebot` module.
#
# Clones the server repo (or reuses $SCRABBLEBOT_SRC), installs the module's
# npm dependencies, and runs `spacetime generate --lang rust` into
# crates/vickey-bot/src/module_bindings. Requires git, npm and the spacetime
# CLI (2.13.x, matching spacetimedb-sdk in crates/vickey-bot/Cargo.toml).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/crates/vickey-bot/src/module_bindings"
REPO_URL="${SCRABBLEBOT_REPO:-https://github.com/clockworklabs/scrabblebot.git}"
SRC="${SCRABBLEBOT_SRC:-}"

for tool in git npm spacetime; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

if [[ -z "$SRC" ]]; then
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  echo "cloning $REPO_URL"
  git clone --depth 1 "$REPO_URL" "$WORK/scrabblebot"
  SRC="$WORK/scrabblebot"
fi

echo "installing module dependencies in $SRC/spacetimedb"
(cd "$SRC/spacetimedb" && npm install --no-audit --no-fund)

echo "generating Rust bindings into $OUT"
spacetime generate --lang rust \
  --module-path "$SRC/spacetimedb" \
  --out-dir "$OUT" -y

echo "done; rebuild with: cargo build --release -p vickey-bot"
