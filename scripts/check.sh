#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$1"; }

step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "cargo test"
cargo test --workspace --locked

step "cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

step "line length"
long=$(git ls-files -- ':!docs/' '*.rs' '*.sh' '*.toml' '*.sql' '*.yml' | xargs awk 'length > 140 { print FILENAME ":" FNR }')
if [[ -n "$long" ]]; then
  printf 'lines over 140 columns:\n%s\n' "$long"
  exit 1
fi

step "ai attribution"
bash scripts/test-attribution.sh
base=$(git rev-parse --verify -q origin/master || git rev-parse --verify -q master || true)
if [[ -n "$base" ]]; then
  git log --format=%B "$base..HEAD" | scripts/no-ai-attribution.sh "a commit message on this branch"
else
  printf 'no master ref to compare against\n'
  exit 1
fi

step "comment ban"
checker="${NO_COMMENTS_CHECK:-$HOME/.claude/skills/no-comments/scripts/check.js}"
if [[ -f "$checker" ]]; then
  mapfile -t files < <(git ls-files -- ':!docs/' '*.rs' '*.sh' '*.yml' '*.sql')
  if (( ${#files[@]} )); then
    node "$checker" "${files[@]}"
  fi
else
  printf 'the no-comments checker is missing at %s\n' "$checker"
  exit 1
fi

printf '\nall checks passed\n'
