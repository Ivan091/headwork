# Trials 1 and 2: resume and Remote Control naming

Run on 2026-11-13 with Claude Code 2.1.323 and wmux 4.15.0 (app-4.15.0) on Windows 11, from Git Bash. Throwaway sessions used `--model haiku`, ran in `S:\Code\agentctl\trials\a-resume`, and were closed afterwards.

## Trial 1: fixed id at spawn, then resume with name and Remote Control in one launch

### 1a. Start with a fixed `--session-id`

```bash
U=$(node -e "console.log(require('crypto').randomUUID())")   # 2dca4b8d-580c-4185-8f33-15b25a9f0f10
wmux agent spawn --workspace <agentctl ws> --cwd 'S:\Code\agentctl\trials\a-resume' --label trial-1-start \
  --cmd "claude --session-id $U --remote-control trial-1-resume -n trial-1-resume --model haiku -- \"Reply with exactly: TRIAL1-START-OK\""
```

- It replied `TRIAL1-START-OK`. The banner said `/remote-control is active ... https://claude.ai/code/session_01FMR26z3V2xmjipwxwzDW8U`, and the prompt box showed the name `trial-1-resume`.
- The transcript was written to `~/.claude/projects/S--Code-agentctl-trials-a-resume/2dca4b8d-580c-4185-8f33-15b25a9f0f10.jsonl`. The fixed id is the transcript name. **Works.**
- `/exit` printed Claude's own resume hint: `claude --remote-control trial-1-resume --model haiku --resume 2dca4b8d-...`. It keeps `--remote-control <name>` and the model, but not `-n` or the permission mode.

### 1b. Resume in a new pane, in one launch

```bash
wmux agent spawn ... --label trial-1-resumed \
  --cmd "claude --resume $U --remote-control trial-1-resume -n trial-1-resume --model haiku -- \"Reply with exactly: TRIAL1-RESUMED-OK and the earlier word you replied with\""
```

- It replied `TRIAL1-RESUMED-OK TRIAL1-START-OK`, so the old conversation came back. The prompt ran as the first new turn.
- **The Remote Control URL was the same as before:** `session_01FMR26z3V2xmjipwxwzDW8U`. The name `trial-1-resume` was in the prompt box again.
- It kept the same transcript file and the same session id. `claude agents --json` listed `sessionId 2dca4b8d-..., name trial-1-resume`.
- `ListAgents` from another session listed it as `trial-1-resume`. `SendMessage to: "trial-1-resume"` was delivered, and it answered `TRIAL1-MSG-OK`.
- Verdict: **works**, for both `--session-id` at spawn and resume by that id with `--remote-control <name> -n <name> "<prompt>"` in one launch. Phone check: see the "Phone check" section below.

### 1c. What wmux keeps for a restart (read from its state and code; wmux was not restarted)

- wmux saves a layout snapshot every 5 minutes (`sessionSnapshotMinutes: 5`) to `~/AppData/Roaming/wmux/sessions/session.json`. `restoreClaudeSessions: true` is set in `~/AppData/Roaming/wmux/settings.json`.
- Panes made by `wmux agent spawn` are saved as only `{"id","type":"terminal","claudeSessionId":"<uuid>"}`. There is **no cwd and no command line**. Shell-started panes also save `cwd`, `currentCwd` and the shell.
- In the bundled code (`resources/app.asar`, functions `Wn`/`Bn`), the restore command is built from the pane's recorded command list. If a `claude ...` command is found there, ` --resume <id>` is appended to it; otherwise the command is plain `claude --resume <id>`. Saved agent panes keep no command, so we expect the plain form: **no `--remote-control`, no `-n`, no `--model`, no `--permission-mode`** [U: inferred from code, not seen].
- If wmux appended to our spawn command, ` --resume <id>` would land after the `-- "<prompt>"` separator and be read as prompt text [U: inferred].
- Without a saved cwd, the folder-locked resume (2.1.321) may fail or open the wrong project [U]. Test 1d checks this.
- `restoreClaudeSessions` is a global switch only. Nothing per pane was found, so agentctl must either turn it off or adopt the restored panes, as research §5 says.
- wmux could not be restarted from here: that ends every pane, including `root` and this session.

### 1d. Resume from a different folder

```bash
cd S:/Code/agentctl   # the parent folder, not the one the session started in
claude -p --resume 2dca4b8d-580c-4185-8f33-15b25a9f0f10 --model haiku "Reply with exactly: TRIAL1-CWD-OK"
```

- It replied `TRIAL1-CWD-OK` with exit code 0. The new turns were appended to the original transcript under `S--Code-agentctl-trials-a-resume/`.
- **The working directory, however, became the launch folder:** `cwd: S:\Code\agentctl` was recorded on the new turns. The 2.1.321 "locked to the start folder" note did not show up as a refusal here.
- What this means: if wmux restores a pane without a saved cwd, the conversation resumes, but a worktree child may then work in the wrong checkout. agentctl has to start each resume with `--cwd <task worktree>` itself. Verdict: **partly**. Resume works from any folder, but the folder is not restored.

## Trial 2: `--bg` background session

```bash
cd S:/Code/agentctl/trials/a-resume
claude --bg --remote-control trial-2-bg -n trial-2-bg --model haiku "Reply with exactly: TRIAL2-BG-OK"
# -> backgrounded · 38ab026c · trial-2-bg
```

- It returned immediately. `claude logs 38ab026c` showed `● TRIAL2-BG-OK` and `/remote-control is active ... session_01FMR26zPXYZX6VkntGBxQLC`. **`--remote-control <name>` works together with `--bg`.**
- `claude agents --json` listed it as `kind: background, name: trial-2-bg, id 38ab026c`. `ListAgents` showed `trial-2-bg · bg`.
- `SendMessage to: "trial-2-bg"` was delivered, and it replied `TRIAL2-MSG-OK` to the sender. **Messaging by name works.**
- `claude --help` says a background session can be resumed with `--bg --resume <id>` under the same id, and a supervisor restarts sessions after sleep or updates. That is not tested here.
- Verdict: **works** locally. Phone check: see below.

## Side finding

- Git Bash turns `/exit` sent through `wmux send` into `C:/Program Files/Git/exit`, so the session receives it as a prompt. Use `MSYS_NO_PATHCONV=1 wmux send ...` for any slash command. agentctl's host adapter has to do this.

## Phone check (answered by the user on 2026-11-13)

- Both `trial-1-resume` and `trial-2-bg` showed by name in the Claude mobile app. **Trial 2 works: a `--bg` session is reachable from the phone by name.**
- `trial-1-resume` was a single entry. Its thread showed `TRIAL1-START-OK` and then `TRIAL1-RESUMED-OK TRIAL1-START-OK`. **Resuming with the same `--remote-control <name>` reattaches to the same phone session with full history; no duplicate is created.**
- Mix-up to avoid: the user first opened `agentctl-trial-resume` (this child) instead of `trial-1-resume`. Names that share a suffix are easy to confuse on the phone, so agentctl should give test sessions a distinct prefix.

## Verdicts

| Question | Verdict |
|---|---|
| `--session-id <uuid>` at spawn fixes the transcript id | works |
| `claude --resume <id> --remote-control <name> -n <name> "<prompt>"` in one launch | works |
| Phone shows the resumed session under the same name, same thread | works (user confirmed) |
| `--bg` session with `--remote-control <name>` shows by name on the phone | works (user confirmed) |
| `--bg` session can be messaged by name (`SendMessage`) | works |
| wmux restore keeps the name, Remote Control, model, mode and cwd of agent panes | does not, by its code and state: only `claudeSessionId` is saved [U until a wmux restart or reboot] |
| wmux Claude restore can be turned off per pane | does not: only the global `restoreClaudeSessions` setting exists [U: no per-pane switch found] |
| Resume from another folder | partly: works, but runs in the launch folder |
| Background sessions come back after a reboot | not verified (needs the reboot) |

## Reboot checklist (for the user's next reboot)

The user chose to reboot later. A reboot ends `root` and every child, including car training, so do it when the children are idle.

1. Just before rebooting, run `bash S:/Code/agentctl/trials/a-resume/reboot-check.sh before`. It writes `reboot-before/` with the wmux snapshot, `claude agents --json`, the running wmux agents with their commands, each Claude pane's session id, Remote Control session and name, and free space on C:. A baseline from 2026-11-13 is already there; the fresh run replaces it.
2. Reboot, log in, and open wmux. Give the panes about a minute to restore.
3. Run `bash S:/Code/agentctl/trials/a-resume/reboot-check.sh after` and compare `reboot-after/panes.txt` with `reboot-before/panes.txt`. Check:
   - **Panes:** which Claude panes came back, and whether each one shows the same Claude session id as before.
   - **Name:** for each restored Claude pane, whether the prompt-box name (`name=`) is the old one (`me-sim-...`, `root`) or empty. Empty means wmux ran a plain `claude --resume <id>` without `-n`.
   - **Remote Control:** whether each pane shows `rc=session_...` and whether the phone lists it under its old name, or not at all, or under a hostname-generated name.
   - **Folder:** the folder each restored pane runs in. In the pane, ask `pwd`, or look at the cwd in its transcript. A worktree child must be in its worktree, not in `S:\Code` or the wmux default folder.
   - **Mode and model:** the status bar shows auto mode and the model. Children were started with `--permission-mode auto` and a set model.
   - **Background sessions:** whether `root` (kind `background` in `claude agents`) came back by itself after the reboot, and under the same session id.
   - **Duplicates:** whether any conversation is running twice, for example restored by wmux and also by the background supervisor (`root` has both a wmux pane and a background entry today).
   - **Disk:** whether C: has more free space than the 2.1 GB before (`reboot-after/disk-c.txt`).
4. Paste the two `panes.txt` files and the answers into this file, or give them to a session working on agentctl.
