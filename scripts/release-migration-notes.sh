#!/usr/bin/env bash
set -uo pipefail

usage='usage: scripts/release-migration-notes.sh <commit> [<previous-tag>]'
commit=${1:?$usage}
previous=${2:-}

if [[ -n "$previous" ]]; then
  if ! added=$(git diff --name-only --diff-filter=A "$previous" "$commit" -- 'migrations/*.toml' 2> /dev/null); then
    echo "warning: cannot diff $previous..$commit; the release gets no migration notes section" >&2
    exit 0
  fi
else
  if ! added=$(git ls-tree --name-only -r "$commit" -- migrations/ 2> /dev/null | grep -E '^migrations/[^/]+\.toml$'); then
    added=
  fi
fi

[[ -n "$added" ]] || exit 0

title_of() {
  local raw
  raw=$(git show "$commit:$1" 2> /dev/null | sed -n -E 's/^title[[:space:]]*=[[:space:]]*(.*)$/\1/p' | head -1)
  case "$raw" in
    \"*\") raw=${raw#\"}; raw=${raw%\"}; printf '%s' "$raw" | sed -e 's|\\"|"|g' -e 's|\\\\|\\|g' ;;
    \'*\') raw=${raw#\'}; raw=${raw%\'}; printf '%s' "$raw" ;;
    *) ;;
  esac
}

printf '## Migration notes\n\n'
while IFS= read -r path; do
  title=$(title_of "$path")
  printf -- '- %s (`%s`)\n' "${title:-$path}" "$path"
done <<< "$added"
printf '\nRun `headwork migrate list` on each machine after updating.\n'
