#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

quick=false
case "${1:-}" in
  "") ;;
  --quick) quick=true ;;
  *)
    printf 'usage: scripts/check.sh [--quick]\n'
    exit 2
    ;;
esac

# shellcheck source=scripts/heavy.sh
. scripts/heavy.sh
heavy_hold "check.sh $([[ "$quick" == true ]] && echo --quick || echo full) in $(basename "$PWD")"

step() { printf '\n== %s\n' "$1"; }

step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

if [[ "$quick" == false ]]; then
  step "cargo test"
  cargo test --workspace --locked

  step "cargo doc"
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
fi

step "heavy gate tests"
bash scripts/test-heavy.sh

step "untangle tests"
node dash/untangle.test.js

step "migration notes"
bash scripts/test-migration-line.sh
bash scripts/test-release-migration-notes.sh
bash scripts/test-pr-body.sh
cargo run -q --locked -p headwork -- migrate check .
if [[ "${GITHUB_EVENT_NAME:-}" == "pull_request" && -f "${GITHUB_EVENT_PATH:-}" ]]; then
  body_file=$(mktemp)
  changed_file=$(mktemp)
  number=$(node scripts/pr-body.js --number)
  live=false
  if [[ -n "${GH_TOKEN:-}" && -n "$number" ]]; then
    if gh pr view "$number" --repo "${GITHUB_REPOSITORY:-}" --json body -q .body > "$body_file"; then
      live=true
    fi
  fi
  if [[ "$live" == false ]]; then
    node scripts/pr-body.js > "$body_file"
  fi
  git diff --name-only "origin/${GITHUB_BASE_REF:-master}...HEAD" > "$changed_file"
  bash scripts/migration-line.sh "$body_file" "$changed_file"
  rm -f "$body_file" "$changed_file"
else
  printf 'not a pull request run: the Migration: line of the body is checked in the pull request gate\n'
fi

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
elif [[ -n "${CI:-}" ]]; then
  printf 'skipped in CI: the no-comments checker lives in the user config; the local gate runs it\n'
else
  printf 'the no-comments checker is missing at %s\n' "$checker"
  exit 1
fi

if [[ "$quick" == true ]]; then
  printf '\nquick checks passed; tests and docs run in the pull request gate\n'
else
  printf '\nall checks passed\n'
fi
