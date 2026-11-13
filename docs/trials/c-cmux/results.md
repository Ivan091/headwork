# Trial 5: cmux on macOS (source and docs only)

Date 2026-11-13. Repo manaflow-ai/cmux, pushed 2026-11-13. Nothing was run: this PC is Windows.
Marks: **[V]** read in the repo (files saved beside this one), **[U]** not verified, needs the Mac.
Sources saved here: `skills_cmux_SKILL.md`, `skills_cmux_references_panes-surfaces.md`, `docs_cli-contract.md`, `docs_agent-hooks.md`, `docs_feed.md`, `docs_notifications.md`, `docs_agent-messages.md`, `docs_agent-recovery.md`, `claude-wrapper.sh` (Resources/bin/cmux-claude-wrapper), `tree.txt`.

## Verdict (research.md trial 5 question)

**CLI `--command` goes through a shell. [V]** `new-workspace`, `new-split`, `new-pane`, `new-surface` accept `--command <text>`: cmux starts the normal interactive shell and types the text plus Enter into it. Quoting, `&&`, pipes and `$VARS` are interpreted by that shell, and the shell stays alive after the command exits (skills/cmux/SKILL.md, panes-surfaces.md, cli-contract "Initial terminal command").

**Socket has both forms. [V]** `initial_input` (raw text, caller appends `\r`; same as the CLI) and the older `initial_command`, which *replaces the shell with a one-shot process*: direct run, pane closes on exit. `initial_command` is mentioned only in passing in cli-contract (~line 682); whether the CLI exposes it is **[U]**. Direct run needs the socket (v2 JSON frames), not the CLI.

Consequence for the adapter: with the CLI, quote the whole `claude ...` line for the user's login shell (zsh on Mac); a finished child leaves a live shell prompt, so "pane gone = child ended" does not hold; wmux `agent spawn --cmd` (direct) behaves the other way.

## Capability map

| Need | wmux today (scripts/skill) | cmux | Verdict |
|---|---|---|---|
| Ping | `wmux ping` -> pong | `cmux ping` | same |
| Start pane with command | `agent spawn --workspace --cwd --label --cmd` (direct, no shell) | `new-workspace/new-split/new-pane/new-surface --command` (via shell); socket `initial_command` direct. cwd: `--cwd` on new-workspace, `--working-directory` on new-surface | partly: shell, not direct |
| Workspace per project | `list-workspaces` + `new-workspace --title --cwd --panes 1 --layout single` | `list-workspaces`, `new-workspace` (cwd, description, `--env`, layout), `rename-workspace`, `workspace-action set-description/set-color` | works |
| Label a pane | `--label project/slug` at spawn, filtered by `agent list` | `rename-tab`, `rename-workspace`; no label at creation found **[U]** | works, two calls |
| List panes and state | `agent list` (status, label, agentId, surfaceId, workspaceId); `agent-state --surface` -> state, blockedReason | `list-workspaces`, `list-panes`, `list-pane-surfaces`, `tree`, `--json`; socket `surface.input_state`: `lifecycle` unknown/running/idle/needsInput, `waiting_on_human`, `state` empty/draft/dialog | partly: lifecycle only for hook-integrated agents; no `agent list` equivalent, so the adapter keeps its own surface-to-task map |
| Read pane text | `read-screen --surface` | `read-screen`, `capture-pane --scrollback --lines n` | works |
| Send text / keys | `send`, `send-key enter --surface` | `send`, `send-key`, `send-panel`; draft guard refuses to type over an agent prompt draft or open dialog unless `--force` | works; guard is safer but can refuse |
| Message an agent | `SendMessage` by session name | `cmux agent message <ws/surface/title> "text"` via the agent's hooks, never keystrokes; `agent inbox` | extra, better than `send` for Claude |
| "Needs attention" with choices | `report-agent --blocked "<q>" --choices '[{id,label,key}]'`; click answers the prompt | **No equivalent command.** `notify --title --body` (badge, pane ring, banner, returns id; `--desktop false`), `set-status` pill, `trigger-flash`, `log`. Feed has choices only for permission requests, ExitPlanMode and AskUserQuestion, driven by agent hooks (`feed.push`), answered by `feed.*.reply` | partly: AskUserQuestion shows in Feed with its real options automatically; arbitrary choices not possible |
| Metadata | `report-agent`, `report-subagent` | `set-status`, `clear-status`, `list-status`, `set-progress`, `log`, `sidebar-state`, workspace todos | works (no subagent lines) |
| Close | `agent kill <id>` + `close-surface` | `close-surface`, `close-workspace` | works; process end for a shell-run child is **[U]** |
| Wait for exit | none | `wait-for` named sync point (`-S` to signal), tmux-compat `set-hook` | cmux better |
| Events | none | `cmux events --category feed --category agent --cursor-file --reconnect`: `feed.item.received/completed`, `agent.hook.<Event>` | cmux better; useful for a watchdog |

## Restart behavior [V]

- cmux restores windows, workspaces, panes, scrollback and browser state on relaunch.
- Session hooks write `~/.cmuxterm/<agent>-hook-sessions.json`: agent session id, workspace, surface, cwd, pid, lifecycle, sanitized launch command. On relaunch cmux rebuilds each workspace and runs `claude --resume <id>`. The sanitizer keeps model, sandbox, config and cwd flags and drops prompts, credentials, old session selectors and non-interactive commands. Whether `--remote-control <name> -n <name>` survive it is **[U]** (test 10).
- Off switch: `terminal.autoResumeAgentSessions: false` in `~/.config/cmux/cmux.json` (global, not per pane): layout comes back, agent panes stay idle. This is what agentctl needs if it does its own resume.
- `cmux surface resume set --shell <cmd> | -- <argv...>` attaches a custom resume command to a surface. A CLI or socket write is stored but **not auto-run** until the user approves it in the UI (reply has `approval_required: true`). agentctl cannot silently make a custom resume sticky. Agent hooks publish bindings with auto-resume already granted.
- Agent Hibernation (off by default except under memory pressure): SIGTERM an idle off-screen agent and resume on revisit; `cmux agent hibernate/wake surface:N`. Matters if agentctl counts running processes.
- Other running processes do not survive an app quit. Optional tmux backing from research.md was not re-checked.

## Claude Code integration [V]

- A `claude` wrapper (Resources/bin/cmux-claude-wrapper) sits on PATH inside cmux terminals and injects `--session-id` and `--settings` (hooks). It skips its own `--session-id` when the caller passes `--session-id`, `--resume`/`-r` or `--continue`/`-c` (lines ~689, 764, 1826), and it lists `--remote-control` among args it recognizes (line ~1526), so pass-through is expected. Gated by a Settings toggle "Claude Code integration".
- Hooks feed: running/idle/needsInput lifecycle, sidebar status, notifications, session id for resume, the `PushNotification` tool bridged into cmux notifications, PermissionRequest/AskUserQuestion/ExitPlanMode into Feed with inline buttons, optional workspace auto-naming.
- `cmux claude-hook install` writes the same hooks into `~/.claude/settings.json` (documented for remote hosts and pre-existing tmux). Do not run it where agentctl owns hooks without checking: two hook sets double-fire.
- Hook coexistence: cmux injects via `--settings`; agentctl plugin hooks come from user or plugin settings; Claude merges both, so both run. Ordering and the Feed's up-to-120 s hook park are **[U]**.
- Socket trust modes: `cmuxOnly` (process ancestry; anything started in a cmux terminal is trusted), `password` (`CMUX_SOCKET_PASSWORD`), `allowAll`, off; `cmux socket-status` shows the effective mode. Children inherit `CMUX_WORKSPACE_ID`, `CMUX_SURFACE_ID`, `CMUX_SOCKET_PATH`.

## What the adapter must do differently from wmux

1. Spawn: one shell line `cd <cwd> && claude --remote-control N -n N --model M --permission-mode auto -- "<prompt>"` as `--command`, then `rename-tab`.
2. State: no `agent-state`; use `surface.input_state` over the socket, or `events` for `agent.hook.*`; fall back to `read-screen`.
3. Blocked: no choices command; rely on the AskUserQuestion Feed hook plus `notify` and `set-status "Needs you: ..."`. Keep the numbered-options rule in the prompt.
4. Close: `close-surface`, then have the reaper verify the process tree is gone (the shell wrapper makes this necessary).
5. Resume: set `autoResumeAgentSessions` false and resume from agentctl, or adopt cmux's restore and only reconcile.
6. License: GPL-3.0-or-later with BUSL parts; use as an external CLI only.

## 10-minute test list for the Mac

Use `--model haiku` and labels `trial-5-*`; close afterwards.

1. `cmux --version; cmux ping; cmux socket-status --json`: version, pong, socket mode.
2. `cmux new-workspace --cwd ~ --command 'echo shell=$0 pid=$$; sleep 600'`, then `cmux read-screen --surface <ref>`: confirms a login shell runs it. In another terminal `ps -o pid,ppid,command`: shell with a `sleep` child?
3. `cmux close-surface --surface <ref>`, then `pgrep -f 'sleep 600'`: child gone? **Key point: closing ends the process.**
4. Spawn Claude the wmux way: `cmux new-workspace --cwd ~/somerepo --command 'claude --remote-control trial-5-a -n trial-5-a --model haiku --permission-mode auto -- "Reply ok and wait"'`. Does Claude start, does the phone show `trial-5-a`, does `~/.cmuxterm/claude-hook-sessions.json` list the session? **Key point: wrapper plus `--remote-control`/`-n` work.**
5. `cmux rename-tab --surface <ref> "trial-5/a"`, then `cmux list-panes --json` and `cmux tree --json`: title present? pid or lifecycle field present?
6. `cmux capabilities`: look for `surface.input_state`, `feed.*`, `agent.*`. Try the socket call: `printf '{"id":1,"method":"surface.input_state","params":{"surface_id":"<uuid>"}}\n' | nc -U "$CMUX_SOCKET_PATH"` while Claude works and while idle (auth may be needed).
7. `cmux send --surface <ref> "hi"; cmux send-key enter --surface <ref>`; then leave a half-typed draft and repeat: does the guard refuse, does `--force` override?
8. In that Claude: "use AskUserQuestion with options 1 yes 2 no". Does Feed show the options with buttons, does the ring and badge appear, does `cmux events --category feed` print it? **Key point: blocked with choices.**
9. `cmux agent message trial-5/a "ping"`: does the idle child wake with it?
10. Restart: with `autoResumeAgentSessions` true (default) quit cmux (Cmd+Q) and reopen. Does the pane return with `claude --resume <id>`, and does the phone still show `trial-5-a` (are `--remote-control`/`-n` kept)? Repeat with `false`: the pane should stay idle. **Key point: restart.**
11. Cleanup: `cmux close-workspace`; `pgrep -fl claude`.

If short on time: 2, 3, 4, 8, 10.

## Not verified

All runtime behavior (no cmux here): process end on close, direct-run `initial_command` from the CLI, `--remote-control` surviving the restore sanitizer, hook ordering with agentctl hooks, exact `list-panes --json` fields, tmux backing.
