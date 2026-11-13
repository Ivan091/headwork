#!/usr/bin/env bash
set -uo pipefail

tag="${1:?usage: reboot-check.sh before|after}"
out="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/reboot-$tag"
mkdir -p "$out"
wmux_bin=wmux
wmux ping 2>/dev/null | grep -q pong || wmux_bin="C:/Users/User/AppData/Local/Programs/wmux/resources/cli-bin/wmux"

cp ~/AppData/Roaming/wmux/sessions/session.json "$out/wmux-session.json"
claude agents --json 2>/dev/null | tr -d '\r' >"$out/claude-agents.json"
"$wmux_bin" agent list | tr -d '\r' | jq -c '[.agents[] | select(.status == "running") | {label, cmd, surfaceId}]' >"$out/wmux-agents.json"
df -h /c | tail -1 >"$out/disk-c.txt"

: >"$out/panes.txt"
for s in $(jq -r '.. | objects | select(has("claudeSessionId")) | "\(.id)=\(.claudeSessionId)"' "$out/wmux-session.json" | tr -d '\r'); do
  surf="${s%%=*}"
  sid="${s#*=}"
  screen="$("$wmux_bin" read-screen --surface "$surf" 2>/dev/null | jq -r '.text // ""' | tr -d '\r')"
  rc="$(grep -oE 'session_[A-Za-z0-9]+' <<<"$screen" | tail -1)"
  name="$(grep -oE '─ [a-z0-9][a-z0-9-]+ ─$' <<<"$screen" | tail -1 | tr -d '─ ')"
  printf '%s claude=%s rc=%s name=%s\n' "$surf" "$sid" "${rc:--}" "${name:--}" >>"$out/panes.txt"
done
cat "$out/panes.txt" "$out/disk-c.txt"
jq -r '.[] | "\(.kind) \(.name // "-") \(.sessionId) \(.cwd)"' "$out/claude-agents.json"
