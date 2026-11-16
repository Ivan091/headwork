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
- 2026-11-14 Effort is `--effort low|medium|high|xhigh|max`, passed to the child as `claude --effort` (the installed
  claude supports it, so no `--settings` workaround). A task stores it in a nullable `tasks.effort` column (migration
  0006); absent is resolved when shown (see 2026-11-14, pane titles) and prints as the level, or `?`. There is no `[effort]` config default and no
  per-project allowed set. Adopted (imported) tasks have no effort. `headwork root --effort` exists too but is not
  stored. The board shows it after the model; `log` shows it on `task-created`; `--json` carries `effort` (null when
  absent) on tasks and in the event data; the MCP `spawn` takes an optional `effort`.
- 2026-11-14 Sub-roots are not built. Large work goes to a lead child (subagents in its own worktree; extra sessions
  proposed to the root, which spawns them with `--after`). Open for the user: whether to build sub-roots later (the
  change set and triggers are in `docs/sub-roots.md`), and whether to add a shadow guard rule (`child-admin`) against
  children running `headwork spawn`/`queue`/`drop`/`park`/`resume`/`import` or closing other tasks through the CLI.
- 2026-11-14 Hot reload (docs/hot-reload.md): no shim or versioned binaries beside `headwork.exe`. Windows lets a
  running program be renamed and the process keeps its original path, so `headwork install` renames the installed
  copy aside (`headwork.old-<millis>.exe`) and renames the new build into place; old copies are deleted once no process
  runs them, at the same install or a later one. A shim would itself be a long-lived file that could never be replaced.
- 2026-11-14 A build is identified by its file's length and modification time (a copy keeps both), not by
  `CARGO_PKG_VERSION`, which stays 0.1.0 across builds, and not by a hash, which costs a full read per check.
- 2026-11-14 The MCP server became a relay over an inner `mcp --serve` that is replaced before the next request when
  the program file changed, instead of a server that restarts itself: Windows has no `exec`, and the process Claude
  started must keep its pipes. The handshake is replayed to the new server and `tools/list_changed` is sent after
  every swap, even if the list is the same; a swap only happens after an install.
- 2026-11-14 The launcher hands its exit work to `headwork exited` from its own path instead of doing it itself, and
  falls back to its own code only when that program cannot run the command (start failure or exit 2). A failure
  inside `exited` is not retried by the launcher, so a close is never handed off twice.
- 2026-11-14 The build record is an event (`build`, with the program path and stamp), not a column: a migration would
  make every launcher running the previous build fail to record its exit, which is the very thing to avoid while
  me-sim-car-training runs.
- 2026-11-14 `headwork install` defaults to `~/.local/bin` and refuses to install the installed copy over itself. It
  opens the store only to report the sessions, so it also runs the new build's migrations, like any other command.
- 2026-11-14 Migration 0006 (task effort) merged before hot reload shipped, so the first install of a hot-reload build
  also moves the store to schema 6 while every running launcher is pinned. Rather than holding the install back, a
  self-close of a session without a build record starts a backup `finish` from the new build before it ends `claude`
  (the old launcher is refused by the schema and hands nothing off), and `finish` records a session end the launcher
  missed. A close from outside already finished by itself once the launcher was gone. A pinned session that exits on
  its own stays `working` until `headwork close` or the tick; nothing is lost.
- 2026-11-14 Detached processes (finishers, park and resume started from MCP, jobs) start with `CREATE_NO_WINDOW`
  instead of `DETACHED_PROCESS`. A process without any console makes every console program it runs (`tasklist` in the
  finisher's 250 ms launcher poll, `taskkill`, `wmux`, `git`) open its own window; with a hidden console of its own the
  children inherit it and no window appears, and the process still leaves the pane's console and job.
- 2026-11-14 Children open in split panes: `[layout] mode = "panes" | "tabs"`, `panes = 4`; the code default is panes
  with 4, so this machine needs no config line, and `tabs` is the old behaviour exactly. Per project workspace, the
  first child replaces the fresh workspace's shell, the second cuts that lone pane into the whole grid at once
  (`wmux layout grid --count N --anchor-pane`), later children replace the free panes' shells, and children beyond N
  stack as tabs on the pane with the fewest sessions (the later pane on a tie). The grid is made at the second child,
  not the first, so a single child keeps the whole screen; until N children run, the grid shows idle shells.
- 2026-11-14 What wmux 2.60 allows decided the design (checked on scratch workspaces): `layout grid` with an anchor
  first gathers every pane's surfaces into the anchor as tabs, so it runs only on a workspace with one pane; `split`
  (`pane.split`) always splits the first pane of the caller's workspace (`WMUX_SURFACE_ID` picks the workspace, an
  unknown surface means the workspace on screen); there is no CLI to move a surface or to add a shell tab to a given
  pane (`surface.create` takes a pane id, but `new-surface` does not pass it). So children already open never move,
  and a pane must not disappear: a child closing alone in its pane first starts `headwork idle <project>` there as a
  tab (label `headwork:free`, no `/`, so tick and import ignore it), and the next child replaces it. A pane closed by
  hand leaves the grid short; the next child then splits the first pane (the only split wmux does), and a split that
  lands outside the workspace is closed again and the child stacks.
- 2026-11-14 A pane is free when none of its surfaces holds a session: a live store session, a running host program
  other than a placeholder, or a surface where the host saw a Claude session. In panes mode a closing child closes the
  workspace when nothing else there holds a session (placeholders and grid shells do not count); tabs mode keeps the
  old rule (the workspace closes when the surface is all it holds).
- 2026-11-14 Migration 0007 adds `sessions.host_pane` and `sessions.host_place` (`pane` or `tab`), as placed; the
  board prints "in its own pane <id>", "as a tab of <id>" or "as a tab" after the surface. Sub-roots, planned as 0007
  in `docs/sub-roots.md`, takes the next free number. The split direction uses a window aspect of 16:9.
- 2026-11-14 Pane titles. A child's pane or tab is titled `<name> - <model> <effort>` (`[layout] title`, placeholders
  `{name}`, `{model}`, `{effort}`) with wmux `rename-surface`, which sets a custom title that Claude's own terminal title
  (an escape sequence) does not replace; checked live. It is set when a pane opens (spawn, resume), re-asserted by every
  tick, and `headwork retitle [<name>]` sets it for live sessions on demand. The agent label stays `<project>/<slug>`,
  because the tick and import read it. Idle placeholders keep their own title. A failing rename never fails a spawn.
- 2026-11-14 The word "default effort" is gone. `headwork spawn` without `--effort` stores the level Claude runs with
  (`ops/effort.rs`): per model id (`claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`, `claude-fable-5-2`) the
  highest settings layer that names one wins, local project (`.claude/settings.local.json`), shared project
  (`.claude/settings.json`), then user (`<claude dir>/settings.json`, `CLAUDE_CONFIG_DIR` honoured); in a layer
  `modelSettings.<id>.effortLevel` beats the top-level `effortLevel`, and the user layer's top level is skipped for the
  5.5 models, as the Claude Code docs say. Nothing set means `medium`, the documented default of the 5.5 models. The
  `CLAUDE_CODE_EFFORT_LEVEL` variable and `/effort` picks are session-only and ignored. Tasks stored with no effort are
  resolved the same way when shown (`board`, `log`, `--json`, titles); the store is not rewritten. `?` shows only when a
  level cannot be told. The built-in level comes from the docs, not from the claude binary.
- 2026-11-14 The root parks and resumes. Park 1 (2026-11-14 13:19) showed the cause: the root was a store session
  (`headwork root` recorded it at 09:52), but park and resume only walk tasks, and the root has none, so it was neither
  asked for a recap nor reopened. Now park records the live root on the park (migration 0008: `parks.root_session`),
  sends it `ROOT_PARK_PROMPT` and waits for its `recap` (the tool is now the root's too; a root recap has no task and is
  found by session) within the same deadline, else writes a mechanical one. The root is **not** stopped: it may be the
  session that started the park, and it tells the user when it is safe to restart; the restart ends it. Resume brings
  back the root the park recorded, or, without a park, a root the store counts as running whose processes are gone
  (a crash); it is left alone while any root still runs. It comes back at logon too, whatever `resume_after_crash`
  says, because the root is the session that asks about the rest. Resume closes the pane wmux restored for it, then
  opens `headwork root --session <id> [--resume] --summary <file>` in a workspace titled `root`, in the folder the root
  ran in (migration 0008: `sessions.dir`, recorded by every `headwork root`; a root started before it has none and is
  reported as failed, to be started by hand). The same conversation continues when Claude still has its transcript;
  otherwise a new root session starts from `HANDOFF.md` and the summary, whose `## Root` part holds its newest recap.
  The summary is written before the root's pane opens, so it lists the tasks and jobs but not the root's own outcome;
  `headwork resume` prints that. `headwork root` without `--model` no longer passes the machine default (sonnet) but
  leaves the model to claude, so the root keeps the user's settings model (opus) as `root.sh` always gave it; a
  resumed root starts the same way, with the default permission mode. Model and mode are not stored per session.
- 2026-11-14 `root.sh` starts the root with `headwork root` (step 3), passing its arguments through; the `engine`
  switch (`echo legacy > ~/.claude/skills/orchestrate/engine`, or `ORCH_ENGINE=legacy`) keeps the old unmanaged start.
- 2026-11-14 Closing checks `git worktree prune` and `git worktree list --porcelain` before the remove command. A task
  folder git does not list is deleted when empty and recorded `removed`; with files it is left, and the close is refused
  with a `worktree-refused` event naming it, so the user decides. The check applies to custom remove commands too.
- 2026-11-14 Orchestrator and workers. The user renamed the root to the **orchestrator** and its children to
  **workers**, not "master" (the default branch, and master/slave). Entries above keep the old words; they record what
  was decided then. What was renamed: the CLI command (`headwork orchestrator`, `headwork root` a hidden clap alias);
  the default session name (`orchestrator`); `Role` and `Actor` (`orchestrator`, `worker`); the rule ids
  (`orchestrator-edit`, `-git`, `-build`, `-delivery`; `worker-edit-outside`, `-read-only`, `-default-branch`,
  `-push-repo`, `-global-skill`, `-fable`, `-long-job`); `MergeSource::Worker`; the config keys (`max_workers`,
  `orchestrator_writable`); the rules files (`rules/orchestrator.md`, `rules/worker.md`) and their text; the rule scopes;
  the park and resume reports (`orchestrator` instead of `root` in their JSON), the resume summary heading
  (`## Orchestrator`) and the resumed orchestrator's workspace (`orchestrator`); doctor's check (`orchestrator`); the
  event types `park-orchestrator` and `orchestrator-resume-failed`; the column `parks.orchestrator_session`; the first
  prompt of a worker ("You are the worker session …"). The event log's actor column is 12 wide to fit `orchestrator`.
- 2026-11-14 Old spellings keep working, written never. `text_enum!` takes aliases after `|`, so `root`, `child` and
  the old rule ids parse from the store, the event log, an old session's `HEADWORK_ROLE` and serde; `as_str` writes the
  new word. Migration 0009 rewrites the store anyway, so queries by text (`role = 'orchestrator'`, shadow-report by rule
  id) see one spelling: it rebuilds `sessions` for its `CHECK (role IN ...)` (SQLite cannot alter a check; foreign keys
  are deferred for the copy), renames the column, and rewrites actors, event types, `$.role`, `$.rule` and
  `$.result.rule` in event data (skipping data that is not JSON), and rule scopes and sources. The shadow report also
  normalizes rule ids, so the shadow week's events count under one id. Config keys use serde aliases, which
  `deny_unknown_fields` accepts; `Config::deprecated_keys` names them for doctor's `config-keys` warning instead of a
  warning on every load, since commands print through owned handles and hooks print nothing. User rules files
  `root.md` and `child.md` are read when `orchestrator.md` and `worker.md` are missing. `import` matches both "worker
  session" and "child session" prompts, since the orchestrate scripts still write the latter.
- 2026-11-14 The orchestrator's address follows the live session instead of a fixed name. headwork routes no messages;
  Claude Code delivers `SendMessage` by the session's `-n` name, and one session has one name. So: the worker rules say
  `{orchestrator}`, filled with the live orchestrator's `rc_name` from the store, else the `-n` name of a running
  unmanaged `claude -n orchestrator` or `-n root` in wmux's panes (only asked when the store has none), else
  `orchestrator`. The address is hashed with the rules, so a restart under another name re-tells every running worker
  at its next prompt. A `PreToolUse` hook (now matching `SendMessage`) rewrites a message to `root` or `orchestrator`
  to the live address with `updatedInput` and `permissionDecision: allow`, logged as a `readdressed` event; it applies in
  shadow mode too, since it denies nothing. Sessions started before this build lack the matcher (plugin files are read
  at session start) and rely on the re-told rules. `claude agents --json` was not used to find an unmanaged
  orchestrator: it is a research preview and too slow for a hook.
- 2026-11-14 The orchestrator's agent is `orchestrator` when `~/.claude/agents/orchestrator.md` (or under
  `CLAUDE_CONFIG_DIR`) exists, else `root`, so `headwork orchestrator` works before and after the config task renames
  the agent file; `--agent` and `--name` override. A session brought back by resume keeps its recorded name, so a
  parked `root` comes back as `root`.
- 2026-11-14 `docs/research.md`, `docs/trials/` and `docs/sub-roots.md` keep "root" and "child": they record research
  and a proposal as written. `docs/design.md` gets a note at the top instead of a rewrite of every section.
- 2026-11-14 A session that ends moves its task to `interrupted` at once, not only at the next tick (`ops/lost.rs`).
  Before, the `SessionEnd` hook only marked the session `ended`, and the launcher's own exit record never ran when the
  pane was closed under it (`reason: other`), so with no tick scheduled in the shadow week a `--keep` worker showed
  `working` for hours. `interrupted` is the design's state for a live session gone without `done` or a park (section
  2.3), and `headwork resume` brings it back, so keep and normal workers alike take it; `failed` and `closing` would
  lose the conversation or the worktree. Three places now apply it: the `SessionEnd` hook (except the reasons `clear`
  and `resume`, after which `claude` runs on), `headwork board` and the MCP `board` tool, which first reconcile every
  running task whose session ended or whose launcher pid is gone (no host call, so the board stays fast with wmux
  down), and the tick, which also catches a vanished pane. A parking machine and tasks already closing are left alone.
  This is bookkeeping, so it runs in shadow mode too; the tick still records the `pane-lost` finding only for what it
  found itself.
- 2026-11-15 Lead worker adopted (rules/orchestrator.md, rules/worker.md, docs/sub-roots.md); sub-roots not built. The `child-admin` shadow rule stays a separate task after the shadow week.
- 2026-11-15 Concurrent writers to `state.db` wait instead of failing with `database is locked`. Every store
  connection sets a 30-second busy timeout before anything else, and starts every transaction `IMMEDIATE`: a deferred
  transaction that read first could not upgrade to a write while another process wrote, and SQLite returns
  `SQLITE_BUSY` at once there whatever the timeout. Switching a new file to WAL ignores the busy handler too, so
  `Store::open` retries that one pragma until the timeout. Migrations read `user_version` inside their immediate
  transaction, so two processes creating the store at once never apply one twice. This was the cause of two spawns
  back to back failing and of the `park_resume` flake (its recap writer raced `park`).
- 2026-11-15 Closing a task deletes its branch once the worktree is removed, but only when the branch holds no
  commits beyond the main checkout's `HEAD` (`git merge-base --is-ancestor`); a branch with work of its own is kept,
  and the outcome is logged (`branch-deleted`, `branch-kept`, `branch-refused`; a git failure never fails the close).
  A `builtin` spawn whose branch already exists reuses it with `git worktree add -B` when it is empty in the same
  sense, resetting it to `HEAD`; a branch with commits is never reset, and the spawn fails as before. The exit code 2
  reported with a "closed" close came from the `ls` that followed it in the same shell line, not from `headwork`.
- 2026-11-15 Guard false positives from the 2026-11-15 audit. A configured command pattern (`user_only`, `scripts`,
  `long_jobs`) now matches only at the program position: `cat`, `wc` or `grep` of `scripts/publish-now.sh` no
  longer counts as running it, while `bash script.sh` still does because the parser already makes the script argument
  the program. Here-document bodies (`<<EOF`, `<<'EOF'`, `<<-EOF`) are skipped up to their terminator line instead of
  being parsed as commands; a here-string (`<<<`) is not a here-document. The `Stop` gate takes a `running_job` fact,
  true when the worker's task has a `running` job whose process is alive (or has no pid to check); such a worker may
  end its turn without asking or reporting, but a plain-text question is still denied.
- 2026-11-15 Tick fixes from the 2026-11-15 audit. The merged grace reads `running_job` from the store: a `running`
  job of the task whose process is alive (or has no pid) holds the close, through the same `jobs::has_running_job`
  the `Stop` gate uses. The runaway rule gives no growth verdict to a file without a previous sample (only the size
  limit applies to it; its one run had judged every first-seen file from zero), and judges growth per minute since the
  last tick, spreading it over at least `MIN_INTERVAL_S` (60 s) so a manual tick seconds after the last does not
  inflate the rate. A file that crosses the sample floor is therefore judged on growth one tick later. `headwork
  doctor` has a `tick` check: a tick older than 5 minutes, or none, warns only with `[hooks] enforce = true`; in shadow
  mode it is reported as ok, since nothing schedules the tick until enforcement is turned on.
- 2026-11-15 Closing a task ends what it owned in the store. `finish` records every open job of the task `done`
  ("the task closed"), ending a process that still runs, and once the task's processes are gone sets its `starting`
  or `live` sessions `ended`, so `doctor` and the board no longer count a closed worker's session as live. The rows
  left stale by earlier closes are cleaned once by migration 0010 (sessions and open jobs of `closed` tasks), chosen
  over a `doctor --fix` because it runs by itself at the next store open and is idempotent; it changes no schema. Like
  every migration it stops a pinned launcher (docs/hot-reload.md) from recording its exit; `headwork close` finishes
  such a session.
- 2026-11-16 Spawns of me-sim and me-studio hung because the default shell was a bare `bash`, which Windows resolved to
  WSL's `System32\bash.exe`; WSL's git has no SSH keys, so the `git fetch` in `scripts/worktree.sh new` waited on a
  prompt forever. The default (`[machine] shell` absent) is now Git Bash on Windows, looked up as `bin/bash.exe` in an
  ancestor of `git --exec-path`, then in Git's install folders (`ProgramW6432`, `ProgramFiles`,
  `ProgramFiles(x86)`, `LOCALAPPDATA/Programs`), then on `PATH`, never WSL's `bash` (`System32`, `Sysnative`,
  `WindowsApps`); plain `bash` only when none is found, and elsewhere. `headwork import` uses the same default.
  `headwork doctor` has a `shell` check: WSL's bash or a missing file fails, a bare name warns on Windows. Every
  worktree command (scripts and git) now runs with stdin closed and at most `COMMAND_TIMEOUT` (300 s, long enough for
  a fetch and a large checkout); a command past it has its tree ended and fails with what it printed so far, so a
  prompt fails loudly instead of hanging the spawn.
- 2026-11-16 Two spawn and close failures seen on a busy night. The wmux CLI gives up after 5 s without a reply, and a
  wmux with about ten panes took longer to list its workspaces, failing a spawn after its worktree was made; headwork
  now runs the CLI with `WMUX_RPC_TIMEOUT_MS=30000` (wmux's own floor variable, so it only lengthens the deadline)
  unless the caller set it. A close whose project remove script refuses a worktree (me-sim's `worktree.sh remove`
  keeps one whose work it cannot see merged, which a task that never started also is) falls back to `git worktree
  remove` without force when the worktree holds no work: its branch has no commits beyond the main checkout's `HEAD`
  and git itself refuses changes or untracked files. It logs `worktree-removed-pristine` with the script's error and
  deletes the empty branch as before. A worktree with commits or changes still stays and keeps the task `closing`.
- 2026-11-16 A worker closes its own task through the checked path, never force-close (the audit counted 25 of 25
  closes as `force-close`, and no task ever reached `in_review` or `merged`). The `done` tool and `headwork close
  <own name>` run from the worker's own session (`HEADWORK_ROLE=worker`, `HEADWORK_SESSION` the task's session) both
  call `close_own`: it asks the git host now for every pull request recorded for the task (by URL, new
  `GitHost::pr_by_url`), the worktree's branch and the branch checked out in the task folder, because workers often
  open their pull request from a branch other than the worktree's. A merged one moves the task `working` →
  `in_review` → `merged` → `closing` (merge reported as the git host's, so `merge = "human"` projects work too); a
  task with no pull request, or whose pull request was closed unmerged, takes the new `finish` trigger `working` →
  `closing` with the same guard as `close` (no open ask, no running job, a `SendMessage` report) plus no open pull
  request; an `in_review` task whose pull request closed goes back through `changes-requested` first; a dropped task
  begins closing. Every step is checked against the lifecycle before any is applied, so a refusal leaves the task as
  it was and logs `close-refused`. A `done` that is refused records no report. Force-close (`ForceClose`) stays for
  the user (no role) and the orchestrator, and each transition is logged with the caller's actor instead of always
  `user`. A worker's `headwork close` of another task is refused outright, not shadowed: the close is a lifecycle
  guard like `done`'s, not a hook rule. A worker the checked path refuses and that cannot fix it tells the
  orchestrator, whose `close` force-closes.
- 2026-11-16 Two shadow guard rules for workers. `worker-admin` (also parsed as `child-admin`): a worker running the
  headwork CLI (`headwork`, `headwork.exe`, any path to it, quoted or not, and `cargo run -p|--package|--bin headwork
  -- ...`) with a state-changing subcommand: `spawn`, `queue`, `drop`, `close`, `park`, `resume`, `import`, `tick`,
  `retitle`, `orchestrator`, `root`, `launch`, `finish`, `exited`, `install` (`ADMIN_COMMANDS`). Read-only ones
  (`board`, `log`, `doctor`, `jobs`, `recap`, `shadow-report`, `rule test`), the hidden plumbing a session's own
  plugin runs (`hook`, `mcp`), `idle`, and any call with `-h`, `--help`, `-V` or `--version` stay allowed. `close`
  of the worker's own task (`<project>-<slug>` or `<project>/<slug>`) is allowed; `Scope.task_name` carries the name,
  filled by `scope_for`, so hooks and `rule test --task` agree. `worker-git-outside`: a git write whose target folder
  is not within `Scope.writable`. Writes are `GIT_WRITES` (`commit`, `push`, `pull`, `reset`, `checkout`, `switch`,
  `restore`, `merge`, `rebase`, `cherry-pick`, `revert`, `am`, `clean`, `add`, `rm`, `mv`, `init`), `branch` and
  `tag` with a delete, move, copy, force or annotate flag or with a name and no listing flag, `worktree`
  `add`/`remove`/`prune`/`move`/`lock`/`unlock`/`repair`, and `stash` except `list` and `show`; everything else
  (`status`, `log`, `diff`, `show`, `fetch`, listings, `rev-parse`, `remote`) is a read. The target is the effective
  folder with each `-C` applied in turn, as git stacks them; `--git-dir` or `--work-tree` makes it unknown and
  allowed. The effective folder starts at the session's folder and follows `cd` and `pushd` across the simple
  commands of one line (`cd X && git commit`, `cd X; git push`), in order, ignoring subshell boundaries; `cd` alone
  goes to `~`, and `cd -` or `popd` make it unknown, which allows. It is checked before the default-branch and
  push-repo checks, which still read the branch and remotes of the session's own folder.
- 2026-11-16 Planned versus implemented. `heavy_slots` stays informational: a job's `heavy` flag is recorded and the
  config key is read, but nothing waits for a slot, because the one real heavy-slot owner is the project's own
  `heavy_gate` (me-sim's `scripts/heavy.sh`) and a second, separate counter would only disagree with it.
  `notify_ntfy` stays unimplemented (no notifier exists; Claude's own push covers it). `headwork doctor` check
  `unenforced-keys` warns while either is set. Job logs rotate when a job starts or restarts: over 10 MB the log moves
  to `<log>.1`, older copies shift up, five are kept (`jobs::rotate_log`); headwork writes no other log file of its
  own (the design's `logs/agentctl.log` does not exist: the event log lives in the store). `design.md` carries a
  rename note at its top instead of a rewrite.

- 2026-11-16, job shell: a job whose program is a bare `bash`/`sh` (or WSL's `bash.exe`) runs with the configured
  `[machine] shell` (Git Bash by default), as spawn does. Job logs are opened write-and-seek-to-end instead of
  append-only, because Git Bash (MSYS) processes lose their output on an append-only handle. No new doctor check: the
  `shell` check already covers the configured shell.
- 2026-11-16 The legacy orchestrate scripts (`spawn-child.sh`, `close-child.sh`, `children.sh`, `root.sh`, their
  `*.legacy.sh` twins and the `engine` switch) are retired at the user's decision. `CLAUDE.md` no longer documents an undo
  path to them; the entries above that mention them stay as history. `headwork import` still reads `projects.conf`.
- 2026-11-16, panes close with their worker. The user does not want empty panes on screen, so the 2026-11-14 grid and
  placeholders are gone. A new worker takes a free pane (a fresh workspace's shell, a pane the user left) or splits the
  workspace's first pane, up to `layout.panes`, then stacks as a tab; `layout grid` is no longer used and left the host
  trait. A closing worker's surface simply closes: checked live on wmux 2.60, a pane whose last surface closes leaves
  the tree and its sibling takes the space, the first pane included, and a later `split` still lands in the workspace.
  A pane that keeps another surface (a stacked worker, a shell the user opened, the orchestrator) stays. Splitting only
  the first pane makes the shape uneven: with four workers the panes are two eighths, a quarter and a half (side by
  side, then top and bottom, then side by side, by the 16:9 aspect), because wmux splits no other pane. `headwork idle`
  is retired: nothing starts a placeholder any more. Placeholders older builds left (label `headwork:free`) are closed
  whenever a worker opens or closes in their workspace; the label stays in the code only to find them. A pinned older
  process that still runs `headwork idle` on this build gets a usage error, so its placeholder exits at once and is
  closed the same way. The two live tests now use separate workspaces (`headwork-test`, `headwork-test-tick`), so they
  no longer collide when run in parallel.
- 2026-11-16 Pane titles show the task name only: `[layout] title` defaults to `{name}`; the model and effort are in Claude's status line. The template and its placeholders stay for anyone who sets it. The orchestrator is not a task, so headwork never titles its tab.
