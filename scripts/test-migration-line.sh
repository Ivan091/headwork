#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

expect() {
  local want=$1 body=$2 files=$3 label=$4 got=0
  printf '%b' "$body" > "$dir/body"
  printf '%b' "$files" > "$dir/files"
  bash scripts/migration-line.sh "$dir/body" "$dir/files" 2> /dev/null || got=$?
  if [[ "$got" != "$want" ]]; then
    echo "migration line test failed: $label (want exit $want, got $got)" >&2
    exit 1
  fi
}

expect 1 'Summary\n' 'README.md\n' 'no line'
expect 0 'Summary\nMigration: none\n' 'README.md\n' 'none without triggers'
expect 1 'Migration: none\n' 'plugin/hooks.json\n' 'none on a trigger path'
expect 1 'Migration: none\n' '.gitignore\n' 'none on .gitignore'
expect 0 'Migration: none (comments only)\n' 'plugin/hooks.json\n' 'none with a reason'
expect 0 'Migration: none (x)\r\n' 'crates/headwork/src/ops/install.rs\n' 'carriage return'
expect 0 'Migration: migrations/2026-11-22-public-history.toml\n' 'plugin/hooks.json\n' 'existing note'
expect 1 'Migration: migrations/missing.toml\n' 'README.md\n' 'missing note'
expect 0 'Migration: none\n' 'crates/store/migrations/0001_initial.sql\n' 'store migrations are no trigger'
echo "migration line tests passed"
