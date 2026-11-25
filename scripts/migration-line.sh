#!/usr/bin/env bash
set -euo pipefail

usage='usage: scripts/migration-line.sh <pull-request-body-file> <changed-files-file>'
body=${1:?$usage}
changed=${2:?$usage}

triggers='^(plugin/|crates/headwork/src/ops/(install|schedule|launchd)\.rs$|\.gitignore$)'
line=$(tr -d '\r' < "$body" | grep -m1 -E '^Migration:' || true)

if [[ -z "$line" ]]; then
  echo "the pull request body has no line starting with 'Migration:'" >&2
  echo "write 'Migration: migrations/<id>.toml' or 'Migration: none (<reason>)' (docs/migrations.md)" >&2
  exit 1
fi

if [[ "$line" =~ ^Migration:[[:space:]]*[Nn]one ]]; then
  touched=$(grep -E "$triggers" "$changed" || true)
  if [[ -n "$touched" ]] && ! [[ "$line" =~ ^Migration:[[:space:]]*[Nn]one[[:space:]]*\(.+\)$ ]]; then
    echo "'Migration: none' needs a reason in parentheses: this change touches files that often need a note:" >&2
    echo "$touched" >&2
    exit 1
  fi
  exit 0
fi

path=${line#Migration:}
path=${path## }
if [[ ! -f "$path" ]]; then
  echo "'$line' names a file that does not exist: $path" >&2
  exit 1
fi
