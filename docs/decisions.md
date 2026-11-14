# Decisions

Choices the design leaves open, taken without the user and open to revision. Newest last. Each line names the choice
and the reason.

- 2026-11-13 Licence: the repository stays private with no open-source licence ("All rights reserved"); the user can
  pick one later.
- 2026-11-13 The repository was created with `git init` in the existing main folder instead of a fresh clone, because the
  folder was in use; the remote is the same SSH URL.
- 2026-11-13 Names follow the design with `agentctl` read as `headwork`: binary `headwork`, home `~/.headwork`
  (`HEADWORK_HOME`), project-local file `.headwork.toml`, crates `headwork-core`, `headwork-store`.
- 2026-11-13 `docs/` is archived reference material (design, research, trials) and is exempt from the comment ban and
  the line-length check; the trial scripts are kept as they ran.
- 2026-11-13 The comment-ban step needs the user-wide checker in `~/.claude`; CI runners do not have it and skip that
  step, so the local gate stays the authority for comments.
- 2026-11-13 CI runs on Windows only. A macOS job stayed queued with no runner for over 20 minutes on this private
  repository (macOS minutes cost ten times as much); it comes back with the cmux adapter (milestone 8).
- 2026-11-13 `crates/adapters` and `plugin/` are not created in milestone 1; they arrive with the launcher (milestone 2)
  and the hooks (milestone 3), so no empty crates sit in the workspace.
- 2026-11-13 A dropped task keeps its name until it is `closed` (unique index `WHERE state <> 'closed'`, the design
  says `NOT IN ('closed', 'dropped')`), so a new task cannot collide with a worktree or pane still being removed.
- 2026-11-13 `drop` applies to every state except `closing`, `closed` and `dropped`, following the table's "any live"
  over the diagram's four arrows.
- 2026-11-13 `interrupted` also records `state_before_park`, so a resume after a crash returns to where the task was;
  `needs_user` comes back as `working` because the question is asked again after a resume (design 6.1.3).
- 2026-11-13 Process loss applies to `working`, `needs_user`, `in_review` and `merged`; a `spawning` task that never
  starts goes to `failed` by its timeout instead.
- 2026-11-13 `tasks` gets an `in_main` column and a `CHECK` on `state`; whether a task runs in the main checkout is
  stored, not re-derived from a config that may change.
- 2026-11-13 Project config adds `enabled` (default true) and `max_children` (the per-project cap the start guard
  names).
- 2026-11-13 A project-local `.headwork.toml` may set only `long_jobs` and `user_only`; any other key is an error
  rather than silently ignored.
- 2026-11-13 A Fable task that needs approval gets a `tool` ask when it is created; the answer `Allow Fable` lets it
  start, anything else keeps it queued.
- 2026-11-13 Timestamps come from SQLite (`strftime`, UTC with milliseconds), so the store needs no time crate.
  `projects.config_hash` is FNV-1a over the project's JSON form.
- 2026-11-13 `board` and `log` record the configured projects on every run, so the store follows config edits without
  a separate command; a sync is logged only when something changed.
- 2026-11-13 rustfmt keeps its default style (100 columns), as in me-sim.
- 2026-11-13 Milestone 2 ships as two pull requests: the adapters, the launcher and `spawn` with the queue first (the
  launcher test needs a spawned task), then `close`, queue order and `import`.
- 2026-11-13 The `headwork` package has a small library (`home`, `ops`) beside `main.rs` so spawn, the queue and close
  can be tested in-process against the fake host; it is the binary's internals and uses `anyhow` like the binary.
- 2026-11-13 The wmux adapter runs `node wmux.js` directly (from `WMUX_NODE` and `WMUX_CLI`, else `node` and the
  default install folder), because `wmux` and `wmux.cmd` are shims and `cmd` quoting would break arguments.
  `machine.host_bin` overrides the script.
- 2026-11-13 The Job Object comes from the `win32job` crate, since the workspace forbids `unsafe`. The child joins the
  job right after it starts instead of being started suspended (that needs `unsafe`); `claude` starts nothing in that
  first instant. If joining fails the launcher logs `tree: unowned` and the session still runs.
- 2026-11-13 The launcher ignores Ctrl+C and Ctrl+Break (`ctrlc` with an empty handler), so an interrupt typed into a
  pane only reaches Claude. Closing the pane still ends the launcher and, through the job, the whole tree.
- 2026-11-13 Until the `SessionStart` hook exists (milestone 3), the launcher fires `session-started` itself once
  `claude` runs; when `claude` exits on its own it moves a `spawning` task to `failed` and a running one to
  `interrupted`.
- 2026-11-13 New triggers: `launch-failed` (`spawning` → `failed`, the pane or `claude` did not start) and
  `force-close` (any live state → `closing`, the root's `close` of a keep task or a stuck one, without pretending it
  was dropped).
- 2026-11-13 The permission mode is a closed set (`auto`, `plan`, `default`, `acceptEdits`) stored in
  `tasks.permission_mode`, which milestone 1's schema already had.
- 2026-11-13 `machine.shell` names the shell for command templates (worktree scripts), `bash` from `PATH` when absent;
  on this machine `PATH` also holds WSL's `bash`, so the config should name Git Bash.
- 2026-11-13 A pane runs `<absolute headwork.exe> launch --task N --home <home>`: wmux starts panes with its own
  environment, so neither `PATH` nor `HEADWORK_HOME` can be relied on. The launcher moves its own working folder to the
  home, so only `claude` stands in the worktree and the worktree can be removed once `claude` is gone.
- 2026-11-13 In a workspace headwork just created, the first pane replaces the workspace's shell tab
  (`--replace-tab`), so the workspace holds only its children and can close with the last one.
- 2026-11-13 `spawn --main` defaults the kind to the slug (`car-training` in main is kind `car-training`); other tasks
  default to kind `task`.
- 2026-11-13 Each queue run tries every queued task in order and logs a refusal for each one that still waits; the log
  shows why a task has not started, at the price of repeated lines.
- 2026-11-13 A child closes itself by ending its own `claude` tree, which takes the calling `headwork close` with it.
  The launcher sees `claude` exit with the task `closing`, starts a detached `headwork finish` (no console, breaking
  away from wmux's job, verified on this machine), and closes its own pane, or the workspace it leaves empty, as its
  last act; `finish` waits for the launcher to be gone, removes the worktree, verifies and marks `closed`. Measured:
  about 4 s from the child's call to `closed`.
- 2026-11-13 `close` from outside ends `claude`'s tree and waits up to 60 s for the launcher's hand-off, then finishes
  itself; tasks without a launcher (adopted ones) get today's order: the pane or the empty workspace, then
  `agent kill`, then leftover processes, then the worktree.
- 2026-11-13 A worktree whose remove command refuses (dirty, unpushed) is never forced: the task stays `closing` with a
  `worktree-refused` event, and the next `spawn`, `queue` or `close` reaps it once it can go. The user's "keep or
  delete" ask arrives with the tick (milestone 5).
- 2026-11-13 `spawn` and `queue` first reap `closing` tasks whose launcher is gone, a small part of the tick, so a
  self-closed child frees its slot even before milestone 5.
- 2026-11-13 Queue order is a nullable `tasks.queue_rank` (migration 0002): `queue --order a,b` ranks those first, the
  rest follow oldest first.
- 2026-11-13 Every brief except a keep task's ends with "Closing yourself", naming `"<headwork.exe>" close <name>`,
  as `spawn-child.sh` does today with `close-child.sh`; milestone 3 moves this into `rules/child.md`. `close` deletes
  only briefs under the headwork home, so adopted tasks keep the orchestrate copies.
- 2026-11-13 `import` finds children two ways: running wmux agents labelled `<project>/<slug>`, and panes wmux
  restored after a restart (no longer agents), named from the first prompt of the transcript that `agent-state`'s
  session id points to. Only surfaces that still exist in a workspace count, because wmux keeps stale agents and
  states after their panes close. Adopted tasks enter `working` without the start guard, logged as `adopted`, with the
  session id from wmux (else `wmux:<surface>`), `keep` when the orchestrate brief has no "Closing yourself", the model
  from the agent's command line or the transcript.
- 2026-11-13 `import` appends projects of `projects.conf` missing from `config.toml` (creating it when absent): the
  project's own `scripts/worktree.sh` and `scripts/ship.sh` when present, else `builtin` and `pr`, and its `origin`
  as the only allowed repository. `--dry-run` shows the plan and writes nothing.
- 2026-11-13 Hooks live in a `[hooks]` config section: `enforce` (default false, shadow mode: every rule a call
  breaks is logged as `would-deny` with its rule id and the call goes through), `plugin_dir`, extra `writable` paths
  for every session (`~/.claude/projects`, `~/.claude/plans`), `root_writable` (`~/.claude/cache/orchestrate`), and
  the windows in which a push (120 s) or a sent file (300 s) counts as preceding an ask.
- 2026-11-13 The plugin is loaded per session with `--plugin-dir`, only when `hooks.plugin_dir` is set, never
  installed into `~/.claude`. Each hook runs `"${HEADWORK_EXE:-true}" hook <event>`; the launcher sets
  `HEADWORK_EXE`, so a session without it runs `true` and is untouched.
- 2026-11-13 With the plugin, the SessionStart hook, not the launcher, fires session-started, and the brief holds only
  the task: the rules arrive as SessionStart context. Without the plugin, spawn appends the rendered child rules to
  the brief. The tick's spawn timeout (milestone 5) is the backstop for a session whose hook never fires.
- 2026-11-13 Rules are `rules/root.md` and `rules/child.md`, built into the binary and overridable in `rules_dir`,
  plus optional `global.md` and `<project>.md`. Their hash is `machine.rules_hash` (migration 0003); a session whose
  seen hash differs gets the rules again once, on its next prompt.
- 2026-11-13 Only denies are logged (`deny`, `would-deny`), plus prompts and the tool calls the ask and Stop checks
  need; allowed calls are not, to keep the log readable and hooks fast.
- 2026-11-13 The root may not run build tools anywhere, not only in project folders: it owns no code, and a cwd test
  is easy to dodge with `cd`.
- 2026-11-13 The Stop gate's plain-question check applies even after an ask in the same turn. The gate gives up after
  3 blocks in one turn instead of trusting `stop_hook_active`, so a stuck model cannot loop.
- 2026-11-13 `(Recommended)` on an ask option is optional, but when present it must be on the first option.
- 2026-11-13 In shadow mode an ask is still recorded, moves the task to `needs_user` and raises the blocked state:
  that is bookkeeping, not enforcement.
- 2026-11-13 A hook that fails exits 1, which Claude Code shows but never treats as a block; only a rule deny in
  enforce mode exits 2.
- 2026-11-14 The ask check wants at least 2 options and has no upper bound of its own (the user wants any reasonable
  number). `AskUserQuestion`'s own schema caps a question at 4 options, so the tool's cap still applies where it
  exists; the check stays aligned with the tool by adding nothing stricter. The rules files say the same.
- 2026-11-14 `headwork mcp` is registered in `plugin/.mcp.json` as `${HEADWORK_EXE} mcp`; the launcher sets
  `HEADWORK_EXE`, `HEADWORK_SESSION` and `HEADWORK_ROLE`, so the server needs no arguments. A session without them is
  unmanaged: `tools/list` is empty and every call is refused.
- 2026-11-14 Roles gate twice: `tools/list` shows only the role's tools, and each call checks the role again. A child's
  tools also need the session's row with a task, so a forged `HEADWORK_ROLE` alone gets nothing. The root has no
  session row yet (its launcher arrives later), so root tools trust `HEADWORK_ROLE=root`, which only a launcher sets.
- 2026-11-14 Milestone 4 offers the tools that do not need the tick, parking or jobs: root `board`, `inbox`, `spawn`,
  `drop`, `close`, `queue`, `rules`, `rule_add`, `rule_retire`; child `board`, `inbox`, `report`, `done`. `park`,
  `resume`, `park_status`, `handoff` and the job tools arrive with their milestones.
- 2026-11-14 `report(status, summary, links?, pr?)` has statuses `progress`, `review`, `blocked`, `done`. It is stored
  as a `report` event and as the task's `last_report`. `review` needs a GitHub pull request URL, records it in `prs`
  and moves the task to `in_review` when the repository is in `allowed_repos`; otherwise the lifecycle refuses, the
  report stays, and the reply carries the refusal as a note.
- 2026-11-14 The Stop gate lets a working child end a turn when it reported `blocked` or `done` since its last prompt,
  as it already did after an ask or a message to another session. `progress` does not count.
- 2026-11-14 `done` records the report, then closes the task through the same `close` as the CLI, after its response is
  written, because closing ends the session that called it. It refuses a keep task and an open ask. It does not check
  pull request state or "report sent to root" yet: the tick (milestone 5) is what learns that a pull request merged.
- 2026-11-14 `rule_add` and `rule_retire` write the `rules` table; those rules join the files' hash and render under
  "Rules added by the root" for `global`, the role, or the project, so the version bumps at once and each session gets
  the change on its next prompt. Scopes are `global`, `root`, `child` or a configured project.
- 2026-11-14 `spawn` through MCP takes the brief as text, writes it to `<home>/incoming/` for the spawn operation and
  removes it afterwards; MCP spawns run in `auto` permission mode. `drop` records the user's words as a `drop-reason`
  event, then closes.
- 2026-11-14 The tick splits bookkeeping from enforcement. Recording what already happened always runs: spawn
  timeout, lost panes and launchers, pull request states, answers to its own asks, the queue. Ending things runs only
  with `hooks.enforce`: killing leftover processes, killing a runaway writer, closing a merged task past its grace,
  starting `dropped` tasks closing, closing a timed-out spawn's pane. In shadow mode each of those is a `shadow`
  finding (`headwork tick` prints it) and nothing is touched, as design 7.3 step 0 says ("the tick only reports").
- 2026-11-14 A finding that stands (orphan pane, disk, runaway, stuck, stale rules, would-close) is a row in the new
  `findings` table (migration 0004): it is logged once as `finding`, and as `finding-cleared` when the next tick no
  longer sees it, so a condition that lasts for hours does not write an event a minute. One-shot actions (a
  transition, an ended process, a reminder) are written as `finding` events directly.
- 2026-11-14 A session counts as lost when its launcher pid is dead or its pane is gone from the host; a session
  without a launcher pid (adopted) is judged by its pane alone. Lost panes are not judged when the host does not
  answer `ping`, so a restarting wmux never turns every task `interrupted`; they are not judged while the machine is
  `parking`, as the lifecycle guard says.
- 2026-11-14 Spawn timeout backstop: a `spawning` task whose session was recorded more than `watch.spawn_timeout_s`
  (120) ago goes `failed`, its session `gone`; with enforcement its processes end and its pane closes. The clock is
  SQLite's, like every timestamp.
- 2026-11-14 Stuck check, two looks: a `working` session quiet for `stuck_after_min` with no tool in flight has its
  screen hashed (FNV-1a, the rules hash's function) and remembered; only an unchanged hash on a later tick makes it
  `stuck`. The first look never reports. A moving session drops its remembered screen.
- 2026-11-14 Stale asks: an `open` ask older than `watch.ask_remind_min` (120) gets a host notification, then another
  every interval until answered (design says one reminder; repeating is cheap and the user is the bottleneck).
- 2026-11-14 Rules: every tick hashes the rules files through the same loader the hooks use and bumps the version on a
  change; live sessions whose `rules_seen` is behind are reported (`rules-stale`, standing) until their next prompt
  delivers the rules. Nothing acts on it.
- 2026-11-14 Merged tasks close after `watch.merged_grace_min` (10) with the lifecycle's own guard (not `keep`, no
  waiting ask, report sent). "Report sent" is a recorded `SendMessage` of any session of the task; the job guard is
  false until jobs exist (milestone 6).
- 2026-11-14 Pull requests are found with `gh pr view <branch> --json number,state,url` run in the task's folder
  (the main checkout when the worktree is gone), recorded in `prs`. An open one moves `working` to `in_review`, a
  merged one on to `merged` (`working` goes through `in_review` in one tick), a closed one sends `in_review` back to
  `working`. A repository outside `allowed_repos` never opens a review. Tasks in the main checkout have no branch and
  are skipped. A `gh` failure is a standing `tick-error`, not a failed tick.
- 2026-11-14 A worktree the project's remove command refuses becomes one `tool` ask (topic `worktree`, options
  "Keep the worktree (Recommended)" and "Delete it"), left `pending` for whoever talks to the user (the root, with
  milestone 4's `inbox`). The tick carries out the answer on a later run: keep marks the worktree `kept`, delete
  runs `git worktree remove --force` for `builtin` projects (a project's own script decides for itself), then the
  close finishes.
- 2026-11-14 Disks are checked per volume, once, at the first existing path of the home, temp, `~/.claude`, project
  mains and live task folders. Free space comes from `fs4` (a safe wrapper; the workspace forbids `unsafe`). Below
  `disk_warn_gb` is a notification; below `disk_stop_gb` the queue is not run that tick.
- 2026-11-14 Runaway output scans the configured roots (`{temp}` expands) and every live task's folder for files of
  at least a quarter of the growth limit, remembers their sizes (`file_samples`), and flags a file over `file_limit`
  or one that grew by `growth_limit` since the last tick (an unsampled file counts from zero). The scan visits at
  most a million entries. The writer is found by path, not by handle: the task whose folder holds the file. A live
  task is only alerted (and ended only where its volume is below `disk_stop` and enforcement is on); a closing,
  failed or interrupted task's processes are ended with enforcement; a file under no task is alerted.
- 2026-11-14 Not built: the design's scan for `claude`/`node` processes that descend from no launcher. Windows has no
  safe, cheap process list without `unsafe` or a PowerShell call, and the Job Object already ends each launcher's
  tree; the orphan check covers panes instead. Also not built: log rotation (headwork writes no log file yet; the
  event log is the record) and the windowless `headworkw.exe` target.
- 2026-11-14 `headwork tick` takes `tick.lock` in the home (`File::try_lock`), so overlapping scheduler runs exit at
  once with "another tick is running". Its thresholds are a `Limits` value in bytes and seconds that tests build
  directly, so no test needs a gigabyte.
- 2026-11-14 Milestone 6 names. The design's `handoff` tool is `recap` (the brief says recaps). A recap is a row in the
  new `recaps` table (migration 0005, kinds `session`, `mechanical`, `close`, `interrupted`) and, on a park, the file
  `handoffs/<name>.md`: the session's own text first, then the facts the tool recorded (state, branch, head,
  uncommitted files, last report, open questions, jobs). A session recap written while the machine is `parking` belongs
  to that park; that is how `park` knows it arrived. The tick writes an `interrupted` recap when it finds a session
  gone, and `close` writes a `close` one, so recaps also serve the day-to-day case (`headwork recap <name>` shows the
  newest).
- 2026-11-14 Jobs: `job_start` starts the command detached (`spawn_detached_to`, output appended to
  `jobs/<task>-<label>.log`, which is not capped yet) and registers it, because the `long_jobs` rule denies background
  shells and a session needs an allowed way to run one; `job_register` records a process started elsewhere; `job_status`
  and `job_stop` complete the set. The park policy comes from the call, else the project's `jobs.<label>`, else `rerun`.
  A configured checkpoint path with a `{placeholder}` (me-sim's `{out}`) cannot be expanded here, so the session must pass
  `checkpoint_path`. `done` refuses while a job of the task runs. `heavy` is recorded; heavy slots are not enforced.
- 2026-11-14 Park waits for three things, polling the store: a session recap for each session that can write one, a
  checkpoint file newer than the park's start for each `checkpoint` job, and the end of each `finish` job. `rerun` jobs
  stop at once. At the deadline (default 300 s, `park --deadline`) a missing recap becomes a mechanical one, a
  `checkpoint` job is stopped and its note says work since the last checkpoint is lost, and a `finish` job is left
  running and reported (`NOT safe to restart`, exit code 1). The machine is `parked` either way. Not built: the Stop gate
  that refuses a stop until the recap exists, the extension of the deadline for a running tool call, and the tool ask
  "wait or force"; the prompt typed into the pane and the deadline carry the park.
- 2026-11-14 A session waiting on the user is not asked for a recap (it cannot answer): its asks become `superseded`,
  its blocked state is cleared, and its mechanical recap lists the question as "ask this again after the resume"; the
  resume prompt tells the session to ask it again. Uncommitted tracked changes are saved with `git stash create` as
  `refs/headwork/park/<slug>` (worktree untouched).
- 2026-11-14 Resume is a command, by hand or from a logon task the user installs (`headwork resume --logon --wait 180`);
  nothing is installed here. By hand it also resumes `interrupted` tasks (a crash); with `--logon` those wait unless
  `machine.resume_after_crash = "always"` (`ask` leaves them for the root, `never` leaves them). Each task gets a pane
  running `headwork launch --resume`, which starts `claude --resume <same id>` with the same name, model and mode in the
  task's folder. When Claude's transcript for the id is gone (checked as `<id>.jsonl` under `projects/*` of
  `CLAUDE_CONFIG_DIR` or `~/.claude`) the task starts a new session from its brief and recap (`--fresh`). A pane that
  cannot be opened is retried once, then the task is `failed`. Before relaunching, panes the host restored by itself
  for those session ids (the reboot trial: wmux reopens plain `claude --resume <id>` panes) are closed, so no
  conversation runs twice. The user may also switch `restoreClaudeSessions` off in wmux.
- 2026-11-14 After the sessions, jobs in `parked` that asked to be restarted run again, `checkpoint` ones first; a job
  that cannot start is `failed`. The summary for the root is `recaps/park-<id>.md` (or `crash-<time>.md`), also set on
  the park row, plus a host notification. The root is not a store session yet (no `launch --root`), so park and resume
  act on children; the root writes its own recap before calling `park`, and reads the summary after. The MCP `park` and
  `resume` tools start the CLI detached and return at once, because park outlives the call.
- 2026-11-14 Left for milestone 7: the scan for `claude` and `node` processes that descend from no launcher (still no
  safe, cheap process list on Windows without `unsafe` or PowerShell; park and resume end only the trees they know).
- 2026-11-14 Milestone 7 install: the plugin is installed as a local marketplace (`extraKnownMarketplaces.headwork`, a
  `directory` source on the main checkout, plus `enabledPlugins."headwork@headwork"`), because two keys in
  `settings.json` are the cleanest thing to delete. `hooks.plugin_dir` stays unset so a managed session does not load
  the plugin twice; doctor warns when both are set. The binary is copied to `~/.local/bin/headwork.exe`, so the build
  folder of a task worktree can go.
- 2026-11-14 The plugin's MCP command is `${HEADWORK_EXE:-headwork}`: with `HEADWORK_EXE` unset (every unmanaged
  session) the server used to be `failed` in each one; now it connects and offers no tools.
- 2026-11-14 Doctor fails (exit 1) on: a missing binary, config, store schema newer than the build, a silent host, gh or
  claude missing, no plugin entry, a hooks file without one of the six events, almost no disk. It only warns on PATH,
  a missing store file, no root session, low disk, and a `claude plugin list` that does not show the plugin.
- 2026-11-14 The orphan scan stays in doctor and is informational. Windows offers no parent process ids without
  PowerShell or `unsafe`, so doctor lists `claude.exe` processes the store does not track and says it cannot call any of
  them an orphan; tracked means a live session's launcher or `claude` pid.
- 2026-11-14 The root is a store session through a new command, `headwork root` (role `root`, no task, Remote Control
  name `root`, the agent `root`). It does not replace `root.sh` yet; that is step 3.
- 2026-11-14 Step 1 wrappers keep the old arguments. `spawn-child.sh` prints the same JSON (agentId, surfaceId and
  workspaceId come from `wmux agent list`, cwd from `projects.conf`); a spawn the start guard holds back exits 3 with
  the reason, where the old script exited 1 at the cap. The "Closing yourself" footer is the `child.md` rule now.
  `close-child.sh` falls back to the legacy script for a child the store does not know. `children.sh` prints the old
  table and then the queue and asks from the board. `~/.claude/skills/orchestrate/engine` holds `headwork` or `legacy`.
- 2026-11-14 The config written on this machine is the import's, with `me-sim` and `me-studio` replaced by the fixture's
  fuller sections (main-only kinds, user-only scripts, heavy slots, long jobs), since the import cannot know those.
