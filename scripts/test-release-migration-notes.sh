#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cd "$dir"
git init -q -b main
git config core.autocrlf false
git config user.name t
git config user.email t@example.com
git config commit.gpgsign false
mkdir migrations
printf 'id = "old"\ntitle = "Old note"\n' > migrations/old.toml
git add .
git commit -q -m first
git tag v1
printf 'id = "new"\ntitle = "A \\"quoted\\" title"\n' > migrations/new.toml
printf "id = \"lit\"\ntitle = 'Literal title'\n" > migrations/lit.toml
printf 'id = "bare"\n' > migrations/bare.toml
printf 'not a note\n' > migrations/readme.md
git add .
git commit -q -m second

notes() { bash "$root/scripts/release-migration-notes.sh" "$@"; }

fail() {
  echo "release migration notes test failed: $1" >&2
  exit 1
}

since=$(notes HEAD v1 2> /dev/null)
grep -qF -- '- A "quoted" title (`migrations/new.toml`)' <<< "$since" || fail "escaped quotes: $since"
grep -qF -- '- Literal title (`migrations/lit.toml`)' <<< "$since" || fail "literal string: $since"
grep -qF -- '- migrations/bare.toml (`migrations/bare.toml`)' <<< "$since" || fail "missing title: $since"
grep -qF 'Old note' <<< "$since" && fail "a note of the previous release is listed: $since"
grep -qF 'readme.md' <<< "$since" && fail "a non-toml file is listed: $since"

first=$(notes HEAD)
grep -qF 'Old note' <<< "$first" || fail "without a previous tag every note is listed: $first"

none=$(notes HEAD HEAD)
[[ -z "$none" ]] || fail "no new note must print nothing: $none"

status=0
broken=$(notes HEAD v-missing 2> "$dir/err") || status=$?
[[ "$status" == 0 ]] || fail "an unknown previous tag must not fail the job (exit $status)"
[[ -z "$broken" ]] || fail "an unknown previous tag must print no section: $broken"
grep -qi warning "$dir/err" || fail "an unknown previous tag must warn"
echo "release migration notes tests passed"
