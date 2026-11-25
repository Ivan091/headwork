#!/usr/bin/env bash
set -euo pipefail

usage='usage: scripts/release-migration-notes.sh <commit> [<previous-tag>]'
commit=${1:?$usage}
previous=${2:-}

if [[ -n "$previous" ]]; then
  added=$(git diff --name-only --diff-filter=A "$previous" "$commit" -- 'migrations/*.toml')
else
  added=$(git ls-tree --name-only -r "$commit" -- migrations/ | grep -E '^migrations/[^/]+\.toml$' || true)
fi

[[ -n "$added" ]] || exit 0

printf '## Migration notes\n\n'
while IFS= read -r path; do
  title=$(git show "$commit:$path" | sed -n 's/^title = "\(.*\)"$/\1/p' | head -1)
  printf -- '- %s (`%s`)\n' "${title:-$path}" "$path"
done <<< "$added"
printf '\nRun `headwork migrate list` on each machine after updating.\n'
