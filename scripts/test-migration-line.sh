#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/work/migrations"
: > "$dir/work/migrations/example.toml"

expect() {
  local want=$1 body=$2 files=$3 label=$4 got=0
  printf '%b' "$body" > "$dir/body"
  printf '%b' "$files" > "$dir/files"
  (cd "$dir/work" && bash "$root/scripts/migration-line.sh" "$dir/body" "$dir/files" 2> /dev/null) || got=$?
  if [[ "$got" != "$want" ]]; then
    echo "migration line test failed: $label (want exit $want, got $got)" >&2
    exit 1
  fi
}

expect 1 'Summary\n' 'README.md\n' 'no line'
expect 0 'Summary\nMigration: none\n' 'README.md\n' 'none without triggers'
expect 1 'Migration: none\n' 'plugin/hooks.json\n' 'none on a trigger path'
expect 1 'Migration: none\n' '.gitignore\n' 'none on .gitignore'
expect 1 'Migration: none\n' 'crates/headwork/src/ops/schedule.rs\n' 'none on schedule.rs'
expect 1 'Migration: none\n' 'crates/headwork/src/ops/launchd.rs\n' 'none on launchd.rs'
expect 0 'Migration: none (comments only)\n' 'plugin/hooks.json\n' 'none with a reason'
expect 0 'Migration: none (x)\r\n' 'crates/headwork/src/ops/install.rs\n' 'carriage return'
expect 0 'Migration:   none   \n' 'README.md\n' 'none with surrounding spaces'
expect 0 'Migration:\tnone (reason)  \n' 'plugin/x\n' 'tab and trailing spaces'
expect 1 'Migration: none-such.toml\n' 'README.md\n' 'none is a whole word'
expect 1 'Migration: nonesuch\n' 'README.md\n' 'none prefix of another word'
expect 1 'Migration: none ()\n' 'README.md\n' 'empty reason'
expect 1 'Migration: none (\n' 'README.md\n' 'broken reason'
expect 0 'Migration: migrations/example.toml\n' 'plugin/hooks.json\n' 'existing note'
expect 0 'Migration:  migrations/example.toml  \n' 'README.md\n' 'existing note with spaces'
expect 1 'Migration: migrations/missing.toml\n' 'README.md\n' 'missing note'
expect 1 'Migration: migrations/sub/example.toml\n' 'README.md\n' 'note in a subfolder'
expect 1 'Migration: ./migrations/example.toml\n' 'README.md\n' 'path that is not canonical'
expect 1 'Migration: migrations/example.txt\n' 'README.md\n' 'not a toml note'
expect 1 'Migration: <migrations/<id>.toml, or none (<reason>)>\n' 'README.md\n' 'the pull request template placeholder'
expect 0 'Migration: none\n' 'crates/store/migrations/0001_initial.sql\n' 'store migrations are no trigger'
echo "migration line tests passed"
