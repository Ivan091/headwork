#!/usr/bin/env bash
set -euo pipefail

pattern='co-authored-by:[[:space:]]*(claude|anthropic)|noreply@anthropic\.com'
pattern+='|generated (with|by)[^[:cntrl:]]*(claude|anthropic)|claude\.ai/code|claude-session:|🤖'
label=${1:-the text}
if grep -n -i -E "$pattern" > /dev/null; then
  echo "$label contains AI attribution (Co-Authored-By: Claude, Claude-Session:, a claude.ai/code link, Generated with Claude Code)," >&2
  echo "which ~/.claude/AGENTS.md bans; remove it and try again" >&2
  exit 1
fi
