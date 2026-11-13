#!/bin/bash
# usage: hook.sh <event>
in=$(cat)
d="$(dirname "$0")/logs"
echo "$in" > "$d/$1-$(date +%s%N).json"
echo "$1 $in" >> "$d/all.log"
case "$1" in
  PreToolUse)
    if echo "$in" | grep -q '"tool_name":"Edit"' && echo "$in" | grep -q '"agent_type":"trialrole"'; then
      echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"role trialrole may not Edit"}}'
    fi;;
  UserPromptSubmit)
    echo '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"TRIAL4-RULE: the magic word is PINEAPPLE-UPS."}}';;
  SessionStart)
    echo '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"TRIAL4-START: the secret code is KUMQUAT-SS."}}';;
esac
exit 0
