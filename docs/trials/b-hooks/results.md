# Trials 3 and 4: hooks (claude 2.1.323, Windows, Git Bash hooks)

Setup: proj/ with .claude/settings.json (PreToolUse, UserPromptSubmit, SessionStart -> `bash hook.sh <event>`, logging stdin JSON to proj/logs), agent proj/.claude/agents/trialrole.md.

## Trial 3: agent_type in PreToolUse — WORKS
Cmd: `claude -p --model haiku --agent trialrole --permission-mode acceptEdits "Use Edit ... then Read ..."`
- Every PreToolUse input had `"agent_type":"trialrole"` (also on SessionStart and UserPromptSubmit). `agent_id` absent/null for the main session (set only inside subagents/teammates). Other fields: session_id, prompt_id, transcript_path, cwd, permission_mode, hook_event_name, tool_name, tool_input, tool_use_id.
- Hook printed `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"..."}}` with exit 0 for Edit+role trialrole: Edit was denied even in acceptEdits mode, target.txt unchanged, model saw the reason. Read passed.
- Docs: exit 2 + stderr also blocks, and is the safer choice for enforcement (other non-zero exits don't block). In a subagent, its own type overrides the session's --agent value.
- Not verified: interactive `--agent` (only -p used); auto mode with a deny hook (interactive test in trial 4 used auto but no deny).

## Trial 4: context injection — WORKS
- UserPromptSubmit: stdout JSON `hookSpecificOutput.additionalContext` reached the model (model recited PINEAPPLE-UPS). Plain stdout also works per docs. Fires every prompt, so a rules-version check there reaches running sessions.
- SessionStart: `additionalContext` reached the model (KUMQUAT-SS). Docs: source = startup|resume|clear|compact|fork; cap 10,000 chars (overflow saved to file, 2,000-char preview).
- Cross-session messages: interactive haiku session (`claude --remote-control trial-4-recv -n trial-4-recv --permission-mode auto`) received a SendMessage; UserPromptSubmit FIRED with `prompt` = `<cross-session-message from="uds:..." from-name="..." from-mode="prompting">text</cross-session-message>`, and the hook's context was injected on that turn too (second message: model answered with both values). So a hook can see and act on messages, and a central rule change reaches every child on its next turn, whatever its source.
- Docs also list: scheduled task/loop firing and background subagent reports fire UserPromptSubmit.
- Docs (partial read, doc 293k chars, only first 80k verbatim): UserPromptSubmit output also has `decision:"block"`, `sessionTitle`, `suppressOriginalPrompt`; default timeout 30 s.
- Caveat: a session that is idle gets nothing until some prompt arrives; injection is per turn, not push.

Cleanup: trial-4-recv exited, workspace trial-4-hooks closed.

The raw hook payloads (`proj/logs/`, with session ids and transcript paths) were removed before the repository went
public; the results above quote what mattered from them.
