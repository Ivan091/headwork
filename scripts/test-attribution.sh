#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail=0
expect() {
  local want=$1 text=$2 got=0
  printf '%s\n' "$text" | scripts/no-ai-attribution.sh "sample" > /dev/null 2>&1 || got=$?
  if [[ "$want" == reject && "$got" == 0 ]] || [[ "$want" == accept && "$got" != 0 ]]; then
    printf 'expected %s: %s\n' "$want" "$text"
    fail=1
  fi
}

expect reject $'feat: x\n\nCo-Authored-By: Claude <noreply@anthropic.com>'
expect reject $'feat: x\n\nClaude-Session: https://claude.ai/code/session_01'
expect reject $'feat: x\n\nhttps://claude.ai/code/session_01'
expect reject $'feat: x\n\n🤖 Generated with Claude Code'
expect accept $'feat(guard): refuse attribution\n\nA clean body.'

msg=$(mktemp)
trap 'rm -f "$msg"' EXIT
printf 'feat: x\n\nClaude-Session: https://claude.ai/code/session_01\n' > "$msg"
if .githooks/commit-msg "$msg" > /dev/null 2>&1; then
  echo "commit-msg hook accepted an attribution line"
  fail=1
fi
printf 'feat: x\n' > "$msg"
if ! .githooks/commit-msg "$msg" > /dev/null 2>&1; then
  echo "commit-msg hook rejected a clean message"
  fail=1
fi
exit "$fail"
