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

value=${line#Migration:}
value=$(printf '%s' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')

if [[ "$value" =~ ^[Nn]one($|[[:space:]]*\() ]]; then
  if ! [[ "$value" =~ ^[Nn]one$ || "$value" =~ ^[Nn]one[[:space:]]*\([^[:space:]].*\)$ ]]; then
    echo "'$line' is neither 'Migration: none' nor 'Migration: none (<reason>)'" >&2
    exit 1
  fi
  touched=$(grep -E "$triggers" "$changed" || true)
  if [[ -n "$touched" && "$value" =~ ^[Nn]one$ ]]; then
    echo "'Migration: none' needs a reason in parentheses: this change touches files that often need a note:" >&2
    echo "$touched" >&2
    exit 1
  fi
  exit 0
fi

if ! [[ "$value" =~ ^migrations/[^/]+\.toml$ ]]; then
  echo "'$line' is not 'Migration: migrations/<id>.toml' or 'Migration: none (<reason>)'" >&2
  exit 1
fi
if [[ ! -f "$value" ]]; then
  echo "'$line' names a file that does not exist: $value" >&2
  exit 1
fi
