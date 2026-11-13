# Agent orchestrator: build, adopt or extend?

Research of 2026-11-13. No code was written and nothing was installed.

Marks: **[V]** read today in the tool's own repository, docs or CLI; **[V-local]** checked on this machine (wmux 2.60, Claude Code 2.1.323); **[U]** not verified, from memory or inference.

## 1. Recommendation

**Build a small tool of your own, and build it on Claude Code's own enforcement points. Do not adopt a third-party orchestrator.**

1. **Nothing to adopt.** Every third-party tool that manages terminal panes sits on tmux (WSL on Windows), and every tool that runs natively on Windows is either its own GUI or drives Claude headless. None drives wmux or cmux, none treats `--remote-control <name>` as a setting, and almost none enforces rules in code. Gas Town comes closest on enforcement but brings its own workflow, merges by itself and needs WSL.
2. **Claude Code already has the hard-rule hooks you were missing.** A `PreToolUse` hook can deny a tool call by agent role, a `SessionStart` hook injects the current rules into every session, a hook on `AskUserQuestion` sees every question with its options, and a `Stop` hook can refuse to let a session stop. Today's setup uses none of these; the rules live in prose. This is the "extend" half of the answer.
3. **What is left to build is small**: a state store, a lifecycle state machine, caps, a watchdog that reconciles store, panes and processes, and thin adapters for wmux and cmux. That is the "build" half. A first version is roughly one week of child-session work.
4. **Keep Remote Control and the interactive CLI.** The Agent SDK is headless and its docs point to API-key authentication, so it does not fit sessions you open on the phone under a subscription. Anthropic's background sessions (`claude --bg`, `claude agents`) overlap the most with what you want, but they are a research preview, have no panes, open draft pull requests on their own and stop idle sessions after about an hour. Treat them as a possible second runtime later, not as the base.

One alternative deserves a short trial before the cmux adapter is written: **openwong2kim/wmux** runs on both Windows and macOS, keeps sessions alive in a daemon when the app closes, and has an MCP server and an iOS app. Using it on both machines would give the same host everywhere. It does not replace the tool (no rules, no task state), only the two host adapters.

The name `agentctl` is taken on npm, PyPI and crates.io, and several GitHub projects with the same purpose use it **[V]**. It is fine as a private binary name; pick another before publishing anything.

## 2. How today's problems map to a fix

| Problem seen | Cause | Fix in the tool |
|---|---|---|
| Root read project files instead of spawning | Rule is prose in `root.md` | `PreToolUse` hook denies Edit, Write and project-path reads when the session's role is `root` |
| Children asked open questions | Rule is prose in each brief | Hook on `AskUserQuestion` records the ask, raises the host's blocked state with the same numbered choices and notifies; a `Stop` hook refuses a stop that leaves a question in plain text |
| Standing rules relayed by hand | Rules are copied into briefs at spawn time | One rules file; `SessionStart` hook injects it at start, resume and compaction; a rules-version check on each prompt injects changes into running sessions **[U: the per-prompt path needs a test]** |
| Children not closing, 11 orphaned processes | Closing depends on the child remembering | Children only report `done`; the tool's reaper closes the pane, ends the process tree and verifies it is gone |
| Dropped worktree left on disk | No owner for the worktree after the session | Worktree is a row in the store, removed by the reaper when the task is merged or dropped |
| 132 GB runaway log | No watchdog | Reaper checks free disk space and file growth under tracked folders, ends the owning process tree and alerts |
| State only in root's context | No store | SQLite store; `agentctl board` shows what waits for review, which PRs are blocked, what is queued |

## 3. Comparison

### 3.1 Third-party orchestrators and session managers

All metadata **[V]** from `gh` today; feature cells **[V]** from each README unless marked.

| Tool | Activity | License | Native Windows | Starts sessions as | Custom `claude` flags | Worktrees | Hard rules, state | Fit |
|---|---|---|---|---|---|---|---|---|
| agent-deck | 1.3k stars, pushed 11-12 | MIT | No, WSL | tmux panes, TUI | Yes, per group | Yes | Conductor session (prompt-driven); phone escalation via Telegram, Slack, ntfy | Partial |
| Gas Town (+ beads) | 18.8k stars, pushed 11-12 | MIT | CLI only; panes need WSL | tmux | [U] | Yes | Witness monitor, merge queue with gates, capacity governor, git-backed state | Partial; heavy and opinionated |
| workmux | 3.0k stars, pushed 11-10 | MIT | Not stated; tmux, zellij, kitty, WezTerm backends | Multiplexer windows | Yes | Full lifecycle with hooks | No queue, caps or task state | Partial; best worktree handling |
| ccmanager | 1.3k stars, pushed 10-27 | MIT | Not supported officially | Own PTY, TUI | Yes, presets | Yes | Status hooks, session restore | Partial |
| claude-squad | 8.8k stars, pushed 11-13 | AGPL-3.0 | No | tmux | Yes | Yes | None; single repo | Poor |
| agent-of-empires | 4.3k stars, pushed 11-13 | MIT | No, WSL2 | tmux | [U] | Yes | Status detection, web dashboard | Partial |
| vibe-kanban | 28.7k stars, pushed 11-12 | Apache-2.0 | Yes, with open bugs | Headless `claude -p` | Broken on Windows | Yes, auto cleanup | Kanban, local DB, MCP | Poor; not interactive |
| emdash | 6.2k stars, pushed 11-10 | Apache-2.0 | Yes, installer | Own Electron app | [U] | Yes | Local SQLite | Partial; replaces wmux instead of using it |
| Paradev (was superset) | 16.3k stars, pushed 11-13 | MIT | No | Desktop app | [U] | Yes | [U] | Poor |
| Conductor | closed source | not stated | No, Mac only | [U] | [U] | Yes | [U] | Poor |
| happy | 24.6k stars, pushed 11-13 | MIT | Yes | Wraps the CLI | [U] | No | Mobile client only | Poor; duplicates Remote Control |
| ruflo (was claude-flow) | 81.6k stars, pushed 11-13 | MIT | Yes | Swarm harness | [U] | [U] | Prompt-level framework | Poor |

Dead or drifting **[V]**: coder/agentapi (archived), sugyan/claude-code-webui (archived), uzi (last push 2025-06), crystal (became Nimbalyst), container-use (slowing), Agent-MCP (stale), claude-swarm (gone), par (idle). opcode and claudecodeui are AGPL viewers, not orchestrators. Cursor background agents and Codex cloud run on the vendor's servers **[U]**.

Worth borrowing: Gas Town's witness, governor and merge-queue gates; workmux's `post_create`, `pre_merge` and `pre_remove` hooks; agent-deck's per-group launch settings.

### 3.2 Anthropic's own features

All **[V]** from code.claude.com docs and the changelog (latest entry 2.1.323) unless marked.

| Feature | What it gives | Limits | Role in the tool |
|---|---|---|---|
| Hooks | `PreToolUse` deny by `agent_type`; `SessionStart` context (10,000 characters); `Stop` block (8 in a row); `Notification` events `permission_prompt`, `idle_prompt`, `agent_needs_input`; `SessionEnd`; `WorktreeCreate` and `WorktreeRemove`. Handlers: command, http, mcp_tool, prompt | `SessionEnd` has 1.5 s by default and cannot block; a workplace can set `allowManagedHooksOnly` | **Main enforcement point** |
| Per-session flags | `--settings`, `--setting-sources`, `--mcp-config`, `--strict-mcp-config`, `--append-system-prompt-file`, `--permission-mode`, `--disallowedTools`, `--agent`, `--session-id`, `-n` **[V-local]** | No stability promise | Fixed policy per child at spawn |
| Remote Control | `--remote-control [name]`, `remoteControlAtStartup`, server mode with `--spawn worktree` and `--capacity` (default 32) | One remote session per process; no API to create a named session except starting a process; subscription login only; **off by default on Team and Enterprise**, absent on Bedrock, Vertex, Foundry; admins can disable it | Phone access; optional per machine |
| Push | Two switches in `/config`; `PushNotification` tool | No per-event setting; only the agent can call the tool | Backup to the host's "needs you" |
| Cross-session messaging | `SendMessage`, `ListAgents`, addressed by `--name`; local socket or named pipe | An outside program cannot post into a session on Windows **[U: inferred]** | Root to child messages stay as today |
| Background sessions, agent view | `claude --bg`, `claude agents --json`, attach, stop, respawn; supervisor; own worktree; draft PR; restart after sleep | Research preview; no panes; no cap; idle sessions stopped after about an hour; visibility in the mobile app **[U]** | Possible second runtime later |
| Agent teams | Shared task list, teammates | Experimental; split panes need tmux or iTerm2, not Windows Terminal; no nesting | Not a fit |
| Workflows | Scripts that fan out subagents | Subagents, not sessions; no user input mid-run | Inside one child only |
| Agent SDK | Programmatic sessions, `canUseTool`, hooks | Headless; docs direct third-party use to API keys; personal subscription use not addressed **[U]** | Not used |
| Scheduled tasks | Cloud runs on a schedule | Research preview; cloud only | Not used |
| Plugins | One package of hooks, skills, agents, MCP servers and `bin/` executables | Root `CLAUDE.md` in a plugin is not loaded | **Packaging for home and work** |
| Managed settings | Force hooks and deny rules machine-wide | Needs admin rights | Optional hardening at home; the employer's domain at work |

### 3.3 Terminal hosts

| Host | Platforms | License | Spawn, list, read, send, close | Agent features | Sessions survive app close |
|---|---|---|---|---|---|
| amirlehmam/wmux (in use, 2.60; latest 2.61.1) | Windows | MIT | All present **[V-local]**; `agent spawn --cmd` runs a program, not a shell line; no wait-for-exit | Blocked state with clickable choices, subagent lines, notifications, phone Remote Console, browser and markdown panes, `new-workspace --worktree` | No; layout is restored and Claude is restarted with `--resume` |
| openwong2kim/wmux (4.15) | Windows, macOS, Linux experimental | MIT | JSON-RPC over pipe or socket, MCP server, `process.exited` event **[V]**; command at pane creation **[U]** | Fleet view with "needs you", iOS app, worktree fan-out | **Yes**, daemon owns the terminals |
| manaflow-ai/cmux (0.65) | macOS | GPL-3.0-or-later with BUSL parts | All present; `--command` on new panes (direct or shell **[U]**); `wait-for` | `notify`, `set-status`, `claude-hook`, iPhone app in beta; **no blocked-with-choices command** | No by default; optional tmux backing |
| tmux 3.8 | macOS, Linux | ISC **[U]** | Reference surface, `pane-died` hook | None | Yes |
| psmux 3.4 | Windows | MIT | tmux-compatible commands, parity per command **[U]** | None | Yes |
| zellij 0.46, WezTerm CLI | All three | MIT | Enough for an adapter; WezTerm has no key names or exit event | None | Yes; WezTerm needs its mux server |

wmux and cmux share `ping`, `new-workspace`, `list-workspaces`, `list-panes`, `read-screen`, `send`, `send-key`, `notify`, `set-progress`, `browser`, `markdown`. They differ in spawning (`agent spawn --cmd` versus `--command` on a new pane), naming (`--title` versus `--name`) and the whole agent-state vocabulary, which only wmux has.

## 4. Architecture sketch

One binary, `agentctl`, written in Rust: it is the language of both current projects, gives one executable for Windows and macOS with no runtime, and hooks can call it directly. No bash scripts inside the tool.

```
            root session            child sessions
                 |  CLI (later MCP)       |  hooks call `agentctl hook <event>`
                 v                        v
        +------------------------------------------+
        |  agentctl core: state machine and rules  |
        +------------------------------------------+
           |          |            |            |
        SQLite     terminal      git host     notifier
        store      host adapter  adapter      adapter
                   wmux, cmux    gh, plain    host state,
                   tmux later    git          ntfy later
                         ^
                         |  reconcile every minute
                  reaper (watchdog), run by Task Scheduler / launchd
```

**State store.** SQLite in `~/.agentctl/state.db`: `projects`, `tasks`, `sessions` (pane id, process id, Claude session id, Remote Control name), `worktrees`, `asks`, `slots`, and an append-only `events` log. The store is the single source of truth; the root session reads it instead of remembering.

**Lifecycle.**

```
queued -> spawning -> working <-> needs_user
                         |
                         v
                     in_review  (PR open or render awaiting verdict)
                         |
               merged or dropped -> closing -> closed
   any state -> failed | orphaned | interrupted (set by the reaper)
   working, needs_user, in_review -> parking -> parked -> back to the state before (section 5)
```

Every transition is a command with a guard. Examples: `spawn` refuses over the cap or on a duplicate name; `done` refuses while an ask is open or a PR is unmerged; `closed` is set only after the reaper has seen the process gone and the worktree removed or parked. Standing roles (training, night queue) are a task flag, not a special case in prose.

**Enforcement points.**

1. *Spawn time.* Caps (children overall, per project, heavy-job slots), unique names, worktree creation through the project's own command, and a generated `--settings` file with deny rules for the role. The launch line carries `--remote-control <name> -n <name>` when the machine allows it.
2. *Hooks*, shipped as a plugin so the same package installs at home and at work:
   - `SessionStart`: inject the current rules and the task brief pointer.
   - `PreToolUse`: role guard. Root may not edit or read project trees. A child may not write outside its worktree, push to the default branch, or use the global commit skills where the project names a delivery script.
   - `PreToolUse` on `AskUserQuestion`: store the ask, raise the host's blocked state with the same numbered choices, notify, and reject asks that break the format (an option like "needs changes").
   - `Notification`: permission and idle prompts go to the store and the host.
   - `Stop`: refuse to stop while the task is `working` with no report and no ask.
   - `SessionEnd`: mark the session ended. Cleanup is the reaper's job, because this hook has 1.5 seconds.
   - Hooks only allow or deny. They never rewrite a tool input, since a rewriting hook makes auto mode re-review the call (seen on this machine with the `rtk` hook).
3. *Reaper.* `agentctl reap`, run every minute by Task Scheduler or launchd, so it works when no agent is running. It reconciles the store with the host's pane list and the operating system's process list; ends process trees whose task is closed; removes worktrees of merged or dropped tasks; checks free disk space and file growth in tracked folders; flags children that have been silent too long. It is a program with fixed rules, so it can do what auto mode refuses agents (ending processes, deleting folders) within exactly the cases the rules name.
4. *MCP server*, second version. Typed tools (`task_spawn`, `task_board`, `task_report`, `task_done`) would let the root run without Bash at all. The CLI is enough to start.

**Adapters.**

- *Terminal host*: required `ping`, `spawn(argv, cwd, title)`, `list`, `read_screen`, `send_text`, `send_key`, `close`; optional capabilities `blocked_with_choices`, `notify`, `subagent_line`, `wait_exit`, `persistent`. wmux first, cmux second, tmux and psmux later. Where a host lacks blocked choices (cmux), the adapter falls back to `notify` plus status.
- *Git host*: `gh` for pull-request state; plain git when `gh` is absent. A workplace on GitLab or Bitbucket gets its own adapter.
- *Agent runtime*: the interactive `claude` CLI. Background sessions or another agent CLI could be added behind the same interface.
- *Notifier*: host state first; an optional ntfy or chat webhook for machines without Remote Control.

**Per-project config.** A global `~/.agentctl/config.toml` for caps and the host, and one entry per project:

```toml
[project.me-sim]
path = "S:/Code/me-sim/me-sim"
worktree = "scripts/worktree.sh new {slug}"
worktree_remove = "scripts/worktree.sh remove {slug}"
deliver = "scripts/ship.sh"
main_only = ["car-training", "pro-render", "publish", "release"]
heavy_slots = 2

[project.work-service]
path = "~/work/service"
worktree = "builtin"
deliver = "pr"            # normal branch, push, pull request; a human merges
remote_control = false
```

**How sessions use it.** Root: `agentctl spawn me-sim crusher --brief b.md --model opus`, `agentctl board`, `agentctl queue add`, `agentctl drop`. Child: `agentctl report`, `agentctl done`; asking is automatic through the hook. The user: `agentctl board` in any terminal, or the same view on the phone through the root session.

**Cross-platform.** Rust with `rusqlite` (bundled SQLite); paths normalised in one place; process trees through Job Objects on Windows and process groups on macOS; hooks call the executable, so no shell differences.

**First version** (about one week of child-session work, three to five working days plus review):

- store, state machine, `spawn`, `ls`, `board`, `report`, `done`, `drop`;
- wmux adapter;
- four hooks: `SessionStart` rules, `PreToolUse` role guard, `AskUserQuestion` forwarder, `SessionEnd`;
- reaper with process, worktree and disk checks;
- `agentctl doctor`: checks that the installed Claude Code still has the flags and hook events the tool relies on;
- `park` and `resume` for sessions (section 5): session ids fixed at spawn, handoff files, the stop gate while parking. This adds about two days, so the first version is closer to a week and a half.

Then: the long-job registry for park (training checkpoints, rerun of renders), cmux adapter and PR-state tracking (three to four days); queue, heavy slots and MCP server (about a week); tmux and psmux adapters, GitLab adapter as needed.

## 5. Planned restart: park and resume

Requirement added during the research: one command prepares everything for a computer restart, and one brings it back.

### What exists

| Tool | What it does on restart | Mark |
|---|---|---|
| Claude Code | `--resume <id or name>`, `--continue`, `--session-id <uuid>` to fix the id at start, `--fork-session`. Transcripts live in `~/.claude/projects` and survive a reboot. A session's background shells and subagents do not. | [V-local] flags; background loss [U] |
| Claude Code background sessions | Supervisor restarts sessions after sleep and updates. After a reboot: not stated. | [V], reboot [U] |
| amirlehmam/wmux | Restores layout, tabs and folders; shells start again; "running processes do not survive a restart". Claude panes get session resume. Reboot behaviour is not described separately; `HANDOFF.md` still lists it as open. | [V], reboot [U] |
| openwong2kim/wmux | Daemon keeps sessions through app quit. After a crash or reboot each recovered pane offers Resume with the exact conversation when known; panes declared in `wmux.json` with a restart policy restart by themselves. | [V] |
| cmux | Restores layout; resumes Claude sessions whose id its hooks saved (`autoResumeAgentSessions`); `cmux surface resume set` attaches a custom resume command to a pane. | [V] |
| workmux | `resurrect` reopens worktree windows that had agents and continues their conversations. | [V] |
| ccmanager | Offers to rerun each session's command in its worktree; the conversation is not restored. | [V] |
| agent-deck | Restart and resume commands keep the session name. | [V] |
| Gas Town | Work state lives in git and survives; a "handoff" refreshes an agent's context; `gt up --restore`. | [V] |

Every one of these recovers *after* the fact. None of the READMEs describes a coordinated "prepare for restart": handoffs written first, long jobs checkpointed, then a clean stop **[U: absence not proven]**. That part has to be built, and it fits the tool well because it needs exactly the store and the lifecycle.

### Design

Two commands and two new states (`parking`, `parked`).

`agentctl park`:

1. Set the global state to `parking`. `spawn` refuses and the queue pauses.
2. Message every running session through the host: reach a safe point and run `agentctl handoff` (what it was doing, what is left, what waits on the user). The `Stop` hook refuses a stop without a handoff while parking. A deadline applies (five minutes by default).
3. Stop long jobs through their registered commands. Each job is a row with a kind: `checkpoint` (car training: signal it, wait for the checkpoint), `rerun` (renders, gates: end it, mark for rerun), `finish` (must complete: park waits or the user overrides).
4. Snapshot tasks, sessions with their Claude session ids, worktrees (branch, head, uncommitted files) and pull requests.
5. Close panes, end the process trees, check they are gone, print "safe to restart". A session that misses the deadline gets a mechanical handoff (last screen, `git status`) and is listed for the user, who decides whether to force it.

`agentctl resume`, run by hand or at logon by Task Scheduler / launchd:

1. Wait for the host. For each parked task, open its pane and start `claude --resume <session id> --remote-control <name> -n <name>` with a first prompt that points at the handoff file. If the resume fails, start a fresh session with the brief plus the handoff.
2. Restart long jobs with their resume commands, within the heavy-job slots.
3. Start the root last and give it one recap from the store: what came back, what did not, what waits on the user.

The same path covers a crash or power cut: the reaper finds sessions gone without a park, marks them `interrupted`, and `agentctl resume` restores them from the transcript without a handoff.

Two rules follow. The tool fixes each session's id at spawn with `--session-id`, so resume never guesses which conversation a pane held. And only one party resumes: the host's own Claude restore (wmux, cmux) is turned off for panes the tool owns, or the tool adopts the panes the host restored; otherwise a conversation gets resumed twice.

## 6. Risks

- **Claude Code changes fast.** Seven releases in ten days; recent breaking changes include a removed Remote Control flag (2.1.300) and sessions locked to their start folder (2.1.321). No stability promise for flags or hooks was found. Mitigation: `agentctl doctor` after each update, all Claude specifics in one module.
- **Work policies.** Remote Control is off by default on Team and Enterprise and absent on Bedrock, Vertex and Foundry. `allowManagedHooksOnly` would disable the plugin's hooks; auto mode can be disabled. The tool must run with Remote Control off and degrade to spawn-time settings when hooks are blocked. Check the employer's rules on third-party terminal apps (cmux is GPL with BUSL parts) before relying on them.
- **Hook limits.** `Stop` blocks at most 8 times in a row, `SessionEnd` has 1.5 seconds, `SessionStart` context is capped at 10,000 characters. The design does not depend on any of them alone; the reaper is the backstop.
- **Hosts.** wmux and cmux lose running processes when the app closes, and both are young, fast-moving projects (wmux went from 2.60 to 2.61.1 during this research). The adapter stays thin and the store survives a host restart.
- **Scope creep.** The existing tools show where this goes: Gas Town grew a vocabulary of its own. Keep the tool to state, rules, lifecycle and adapters; leave the thinking to the agents.

## 7. To verify with short trials before building

1. Does `claude --resume <id> --remote-control <name> -n <name> "<prompt>"` work in one launch, and does the phone show the session under the same name again? What does wmux restore after a real reboot, and can its Claude resume be turned off per pane?
2. Does a `--bg` background session appear by name in the mobile app? If yes, the runtime choice deserves a second look.
3. Does `agent_type` reach `PreToolUse` hooks in a session started with `--agent root` on Windows, as the docs say?
4. Can a `UserPromptSubmit` hook add context, so rule changes reach running sessions?
5. Does cmux's `--command` run a program directly or through a shell?
6. Half a day with openwong2kim/wmux on Windows: does `claude --remote-control` work in it, and do sessions really survive closing the app?

## Sources

- Claude Code docs, code.claude.com/docs/en: remote-control, hooks, agent-view, cross-session-messaging, agent-teams, worktrees, workflows, routines, channels, settings, managed-settings, permission-modes, cli-reference, plugins, agent-sdk, changelog.
- Repositories on github.com: amirlehmam/wmux, openwong2kim/wmux, manaflow-ai/cmux, tmux/tmux, psmux/psmux, zellij-org/zellij, wezterm/wezterm, asheshgoplani/agent-deck, gastownhall/gastown, gastownhall/beads, raine/workmux, kbwo/ccmanager, smtg-ai/claude-squad, BloopAI/vibe-kanban, generalaction/emdash, slopus/happy, ruvnet/ruflo.
- Local: `~/.claude/skills/orchestrate`, `~/.claude/agents/root.md`, `wmux --help`, `claude --help`, the projects' `scripts/ship.sh` and `scripts/worktree.sh`.
