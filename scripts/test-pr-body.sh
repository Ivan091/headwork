#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

body_of() {
  printf '%s' "$1" > "$dir/event.json"
  GITHUB_EVENT_PATH="$dir/event.json" node "$root/scripts/pr-body.js"
}

fail() {
  echo "pr body test failed: $1" >&2
  exit 1
}

got=$(body_of '{"pull_request":{"body":"Summary\nMigration: none"}}')
[[ "$got" == $'Summary\nMigration: none' ]] || fail "body: $got"
[[ -z "$(body_of '{"pull_request":{"body":null}}')" ]] || fail "null body"
[[ -z "$(body_of '{"pull_request":{}}')" ]] || fail "no body"
[[ -z "$(body_of '{"push":{}}')" ]] || fail "no pull request"
printf '%s' '{"pull_request":{"number":42}}' > "$dir/event.json"
number=$(GITHUB_EVENT_PATH="$dir/event.json" node "$root/scripts/pr-body.js" --number)
[[ "$number" == 42 ]] || fail "number: $number"
echo "pr body tests passed"
