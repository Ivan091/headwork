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
- 2026-11-18 Jobs survive pid reuse. A job records the identity of its process (image name and start time, read with
  `sysinfo`, migration 0011) when its pid is stored, by `job_register` and at every start. Every liveness check of a
  job (`jobs`, `job_status`, `board`'s close facts, the tick, park, resume, `job_stop`, close) compares it with what runs
  under the pid now: a different image or a start more than a second apart means the job's process is gone, the job is
  recorded `done` with a note naming the program that took the pid, and nothing is signalled. A job recorded before
  this change has no identity and counts as alive only when the pid runs its own program or a shell or launcher
  (`bash`, `sh`, `cargo`, `python`, `py`, `uv`); anything else is left alone and the job ends. `headwork job forget <id>
  --reason` and the `job_forget` MCP tool (a worker's own jobs, any job for the orchestrator) mark a job over without
  touching any process, logged as `job-forgotten`.
- 2026-11-18 Folder trust: the user decided that headwork marks each configured project's `main` folder trusted in
  Claude Code's state file, so no worker stops unseen at "Do you trust this folder?". Spawn, resume and `doctor` set
  `projects["<main>"].hasTrustDialogAccepted = true` when it is not already, and only that field of only those keys.
  The file is re-encoded with `serde_json`, which sorts its keys; Claude Code does not depend on their order and
  rewrites the file in its own order at its next save, and turning on `preserve_order` would change the key order of
  every JSON headwork prints and stores. The read-change-rename window is a few milliseconds, but a Claude session
  that writes in it can still lose its change or ours; ours is retried at the next spawn or doctor run. A missing file
  is created with the one entry. A trust dialog that shows anyway (a write failed, a session started outside headwork's
  checks) becomes a `trust` tool ask with a blocked state whose buttons send the dialog's own keys, so the user, not
  headwork, answers it; an answer given through the store is carried out by sending the same key. Hooks do not run
  before trust, so only sessions still `starting` are read.
- 2026-11-19, a grid of quarters. The user found the shape from splitting the first pane over and over uneven (two
  eighths, a quarter, a half) and wants at most four panes, a 2x2 grid, none smaller than a quarter. What wmux 2.60 can
  do, read from its renderer code and checked on a scratch workspace: `split` always splits the workspace's first leaf
  pane (`__wmux_splitPane` takes `splitTree`'s first leaf, whatever surface calls it), right or `--down`; `layout agents
  --count 1 --coordinator-ratio 0.5 --anchor-pane P --workspace W` splits any pane P side by side into equal halves and
  keeps P's surfaces in the left half (count 1 always gives one column, so never top and bottom); `layout grid` first
  gathers every pane's surfaces into its anchor, so it stays unused. There is still no command to move a surface, to
  resize or equalize panes, or to add a shell tab to a given pane, and a pane whose last surface closes still leaves
  the tree, its sibling taking the space. So the only 2x2 grid those splits reach is two rows of two (one top and
  bottom split of the whole, then each row side by side); two columns of two would need a top and bottom split of the
  right column. The progression is therefore: one worker the whole workspace, two top and bottom halves (not side by
  side as first asked, since that shape can never become a grid), three the top half in quarters, four the bottom half
  too. `core::layout::place` splits only when both halves stay at least half the workspace's width and height: the
  largest such pane, a whole workspace top and bottom, the earlier pane on a tie; otherwise the worker stacks on the
  pane with the fewest sessions, then the largest, then the later one. The fallback for closes, since the freed space
  cannot be kept without an empty pane (which the user rejected on 2026-11-16) and no surface can move into it: the
  neighbour takes the half, and the next worker splits it back, which restores the grid after any single close. When
  both panes of a row close, the other row becomes two columns; the next worker then splits the left column top and
  bottom, the right column is not cut into strips, and later workers stack as tabs until the workspace empties and the
  grid starts over. A `panes` above 4 acts as 4. Workspaces already shaped by hand or by older builds are read as they
  stand: their small panes are never split further, and new workers stack on their largest least busy pane.
- 2026-11-19, two panes side by side. The user set `panes = 2` and wants a vertical divider, left and right halves, not
  top and bottom. Columns first cannot become the 2x2 grid: wmux splits only the first pane top and bottom, so after
  left and right the right column could be cut only into narrow strips, and a 2x2 grid is reachable only rows first.
  So `core::layout::place` splits side by side when `max_panes` is 2 or less, and keeps rows first for 3 and 4. No
  config key: the limit decides the direction, which leaves `panes = 4` and its grid unchanged. With two panes the
  second worker splits the first pane side by side, later workers stack on the pane with the fewest sessions, a closed
  worker's pane goes and the other takes the whole workspace, and the next worker splits it side by side again.
- 2026-11-19, one workspace per worker. The user then preferred one agent per workspace, full screen, over two panes
  side by side. `[layout] mode = "workspaces"` opens each worker in a workspace titled with the task's name
  (`<project>-<slug>`), created fresh with one pane whose shell the worker replaces; nothing splits and nothing stacks, so
  `core::layout::place` is not used. The session records the pane (`PlaceKind::Pane`) and its own workspace, so board,
  tick, retitle, park and resume read it as they read any pane; resume reopens it by the same title. Release closes
  the workspace when no other surface in it holds a session, as `panes` mode does, so a worker opened in a shared
  project workspace before the switch still leaves that workspace to its neighbours. `panes` and `tabs` are unchanged,
  and the side-by-side rule for `panes = 2` stays (it is small and costs nothing). A worker's own helper agent session is
  opened by the worker MCP tool `helper_tab` as a new tab (`Target::Tab`) of the worker's recorded workspace, labelled
  `headwork:helper`; release ignores helper surfaces when deciding whether the workspace is still busy, so the
  workspace closes with its worker. The worker rules say to use it.
- 2026-11-19 Several pull requests per task (second audit, `multi-pr-tasks`). A lead delivers in several pull requests, so
  `review-opened` also applies in `merged` (a later pull request takes the task back to `in_review`), and `merged` →
  `closing` needs every pull request settled: the worker's own close refuses while any is open, in `merged` too. Close
  and the tick share one lookup (`ops/prs.rs`): the URLs recorded for the task, the stored branch and the branch checked
  out in the task's folder, so a renamed branch is found; any open one decides, else any merged, else any closed. The
  tick reconciles `merged` tasks as well. The merged grace counts only once the session has also been quiet for the
  grace with no tool call running (`watch::merged_grace_over`), so an enforced tick never closes a lead that keeps
  working after its first merge. Worktree removal also deletes the empty branch checked out in the folder when the
  worker renamed it. Two finishers no longer run one close: `finish` takes a claim, an exclusive lock on
  `<home>/closing/<task id>.lock` that the system releases when its holder dies; a second finisher logs `close-claimed`
  and leaves. A lock file rather than a store column keeps the schema unchanged and needs no staleness rule.
- 2026-11-19, stop-unfinished waits on background work. Real logs showed the gate firing on workers that had just started a
  background `Bash`, a `Monitor`, a background `Agent` or a `Workflow` and were waiting for its notification. The
  `PostToolUse` hook (matcher widened to `Bash|Monitor|Agent|Task|Workflow`) now records a `tool-used` event with
  `background: true` for those calls: `Bash` with `run_in_background`, every `Monitor` and `Workflow`, an `Agent` or
  `Task` with `run_in_background` or whose response says `async_launched`. `turn_so_far` reports it since the last
  prompt and `StopFacts::waiting_background` lets the stop through, like a running registered job; a plain-text question
  is still denied. The cost is one extra hook call per `Bash` (it records nothing unless the call is background).
- 2026-11-19, `shadow-report --since`. The window is an RFC 3339 date and time, a plain `YYYY-MM-DD` (midnight UTC), or a
  span back from now (`90m`, `24h`, `7d`; units `s`, `m`, `h`, `d`). SQLite's own date parsing reads the absolute
  forms (offsets included) and writes them in the store's event-time format, so no date crate is added. The report and
  its JSON carry the window start (`since`), and the header line says "since <instant>".
- 2026-11-19, `[hooks] enforce_rules`. A list of `RuleId`s (old spellings parse; an unknown id is a config error) whose
  denies block the call even while `enforce = false`; every other rule stays shadow, and `enforce = true` still blocks
  all. Only the hooks' block-or-log decision uses it (`HooksConfig::enforces`); the tick's enforcement
  (closing, removing) stays tied to `enforce`, because a rule list says nothing about it. Listed rules log `deny`
  instead of `would-deny`. `doctor` names the list in its `enforce` check, `rule test` shows the mode of the rule that
  fired, and `shadow-report` marks listed rules `[enforced]` (JSON: `enforced`, `enforce_rules`). Default empty.
- 2026-11-19 Home and temp paths in the guard (second audit, `guard-home-and-tmp`). A claude-config worker's own
  worktree, `~/.claude-<slug>`, counted as outside its folder because the guard treated `~` as an absolute path it never
  expanded. The scope now carries `Places` (the user's home and the temp folder) and expands a leading `~`, `$HOME`,
  `${HOME}`, `/tmp`, `$TMPDIR`, `$TEMP` and `$TMP` before comparing, in edits, `cd` and `git -C`. `/tmp` is the temp
  folder because Git Bash mounts it there. `~user` and other variables stay as written.
- 2026-11-19 Asks follow the pull request (second audit, `ask-lifecycle`). An ask in `in_review` or `merged` is a
  question about a pull request, so `ask-opened` and `ask-answered` apply there and leave the state as it is; the asks
  table tells that the task waits, and close already refuses while one does. Only `working` goes to `needs_user`. An ask
  is abandoned when the user types past its dialog, which fires no `PostToolUse`: the session's next ask, or a prompt the
  user types (not a session message), supersedes its open asks, and a task in `needs_user` goes back to `working`. Asks
  the store raised itself (a topic such as `trust`) are left alone. The push precondition is not required within
  `push_window_secs` of a prompt the user typed in that session, since the user is at the screen: 19 of the 38 shadow
  `ask-push` misses came less than two minutes after one.
- 2026-11-19 Three rule lines (second audit, `ask-and-irreversible-rules`), reviewed and approved by the user: a worker's
  asks have one answer channel (the dialog or the user typing in its pane); a backup before an irreversible step; the
  orchestrator never answers or relays an answer to a worker's ask.
- 2026-11-19 User-only commands (second audit, `user-only-union`). Every session is held to the union of all projects'
  `user_only` (`Config::all_user_only`): a me-sim worker can run me-studio's publisher as easily as its own scripts. A
  pattern whose first word has no slash matches the program's base name under any folder or Windows extension, so
  `me-studio publish` matches `target/release/me-studio.exe publish`. The user chose the per-task approval: the worker
  asks with header `Allow`, naming the command in backticks; when the user picks an option starting with `Allow`, the
  hook records a `user-only-allowed` event per configured pattern that command runs, and the guard skips those patterns
  for that task only. The event log holds the approvals, so no migration was needed (and none could clash with another
  branch's numbered migration).
- 2026-11-19 Lead worktrees (second audit, `lead-worktrees`; the user chose "registered helpers"). A worker makes a helper
  worktree with the MCP tool `worktree_add <slug>`: the project's `worktree_new` command at the folder `worktree_path`
  gives that slug, branch = slug, recorded in `worktrees` with `helper = 1` (migration 0012) so `worktree_of` still means
  the task's own. Helpers are writable for their task only; `finish` removes them with `worktree_remove`, else without
  force when pristine, and a helper that holds work stays, logged `helper-worktree-refused`, never holding up the close
  (the task's own worktree keeps its stricter rule). Subagent worktrees (`<main>/.claude/worktrees`, where Claude Code
  puts `isolation: "worktree"` agents) are writable for every worker of the project: they are made per call and cannot be
  registered. A worker may fast-forward its project's main checkout with `git pull --ff-only` (the ship flow's sync); any
  other git write there stays `worker-git-outside`. The "install kind" is a project flag, `install = true`, rather than a
  task kind, because the worker that merged a headwork change is the one that installs it; only `headwork install` is
  lifted, every other admin command stays the orchestrator's. The orchestrator notes repository is
  `[hooks] orchestrator_repos`: `orchestrator-git` is lifted for a git command whose folder (after `cd` and `-C`, now
  tracked for the orchestrator too) lies inside one, except inside a project's folder there (the folder above its main
  checkout, or the main checkout when that folder is the repository itself). Edits there still come from
  `orchestrator_writable`. All defaults are empty, so the live config must name them.
- 2026-11-20 The user's go carried to a worker (`spawn --allow-ask`, approval-friction plan HW1). The flag (repeatable,
  at most four, as `AskUserQuestion` holds four questions; MCP `allow_ask`) takes user-only commands exactly as the user
  asked for them. Spawn appends a "First action: the user's go" section to the brief with the `Allow` ask pre-filled
  (one question per command, options "Allow it (Recommended)" and "Do not allow it") and logs an `allow-ask` event; the
  grant itself is unchanged, the user's pick in the worker's own pane through the `PostToolUse` hook. The section lives
  in the brief, not in a new table, so no migration is needed and a resumed worker keeps it. An allow-ask that runs no
  user-only command, is blank, spans lines or holds a backtick is refused. A spawn whose brief (commands in backticks or
  fenced code blocks; plain prose names nothing) or allow-asks name a user-only command must have a main-only kind;
  otherwise it is refused before the task exists, naming the main-only kinds whose name the pattern holds (`release`
  for `scripts/release.sh`), else all of them. A brief that only warns against such a command in backticks is refused
  too: the worker rules already keep those commands the user's, so the brief should leave them out. The refusal of a
  worktree kind asked for the main checkout now names the project's main-only kinds as well.
- 2026-11-20, `headwork clean` (plan HW3, case B): a task's leftover worktree goes when the task is `closed`, or
  `merged` with its session ended, and the tree is clean; the project's own remove command still decides (me-sim's
  refuses unmerged work), and a worktree recorded `kept` at close is removed too once it is clean, since a dirty tree
  was the reason to keep it. A Claude Code agent worktree in a main checkout goes when its lock names a dead agent pid
  (any other lock is left), the tree is clean, and its branch is merged into the main checkout's `HEAD` (then deleted
  with `git branch -d`), superseded (every commit beyond `HEAD` has a commit of the same subject in `HEAD`, as a squash
  or re-applied commit leaves it) or gone upstream; the last two keep the branch, so nothing committed is lost. One
  whose folder is gone is unlocked and pruned. Worktrees no task owns (`_master-check`) are listed and left. `--gc` is
  plain `git gc` (default prune window) and is skipped while a registered job of the project runs in the main checkout
  or a main-only task of it is live. The orchestrator gets it as the MCP tool `clean` and its rules tell it to use that
  instead of handing deletions to the user.
- 2026-11-20 `headwork reap` (approval-friction plan HW2). A Job Object of another process cannot be asked for its
  members (and `unsafe` is forbidden), so a session's tree is rebuilt from parent links and start times: a process
  belongs to an ended session when its parent is the session's launcher or `claude` and it started while the session
  ran, or when it descends from one that does; a launcher or `claude` still running counts only if it started within
  two minutes of the session's start (pid reuse). Leftovers wait 60 s after the session's end so a closing launcher can
  finish. `claude`, `me-studio`, `headwork` (launchers, finishers, MCP servers) and every registered job whose recorded
  identity still matches are never ended, nor what they started, nor what started them. Each process is ended alone
  (`taskkill /F` without `/T`) after its image and start time are checked again. Orphans: a `tail` or `bash` whose
  parent is gone, together with everything it started, all of them `tail` or `bash`, each running for over 10 minutes
  and having used under 1 % of a core over its life ("idle"; Windows keeps no last-activity time). Git Bash's `fork` and
  `exec` start shells from a Windows process that exits at once, so the Windows parent of most Git Bash shells is gone;
  the first dry run on this machine chose the car-training `rl-run.sh` wrappers. The snapshot therefore takes each Git
  Bash process's parent from Git Bash's own `ps -el` (MSYS pid, ppid, winpid), and when that list cannot be read no
  shell is judged orphaned. Registered jobs are never reaped, even of closed tasks: `close` already ends those.
- 2026-11-20 `close --force` (HW4) only finishes a task already in `closing` whose pull requests (found as close finds
  them) are all merged and that has no live job; with none found it is refused, so a task without pull requests closes
  only the ordinary way. It runs the close once more and records a worktree the remove command still refuses as `kept`
  (event `worktree-kept`), left on disk for `headwork clean` or the user: never a forced removal, which could lose
  uncommitted files. A pane or process still there keeps refusing.
- 2026-11-20 `tick --install` (HW5) runs `schtasks /Create /SC MINUTE /MO 5 /TN "headwork tick" /TR "\"<exe>\" tick" /F`
  without `/RU`, so the task is the current user's, runs while they are logged on, and needs no admin; `/F` makes a
  second install replace the first. `<exe>` is the installed `~/.local/bin/headwork.exe` when present (hot reload swaps
  it in place), else the running program. A console window still flashes at each run: `conhost --headless` did not
  run a program reliably when tried from a session, and a windowless build is a separate change. Neither the binary nor
  the guard can tell an `Allow` tap of the orchestrator (asks are stored per task), so "only after the user's go" is
  in the injected rules; for workers `tick` stays an orchestrator command.
- 2026-11-21 Extra writable paths for workers. The shadow week showed 35 of 52 would-denies were workers writing and
  committing notes in code-home as their instructions say (car-training's handoff and night results, audit reports).
  Two narrow grants, both checked by the edit rule and the git rule: a project's `extra_writable = [{ path, kinds }]`
  (a file or a folder; `kinds` limits it to task kinds, empty means every task of the project), and `[hooks]
  brief_writable`, folders below which a worker may write exactly the absolute paths its brief lists on a `Writes:`
  line. A line of its own, not every path the brief mentions: briefs name notes they only want read (the audit
  footer names `notes/audit-2026-11-15.md`), and those must stay unwritable. The brief copy in the headwork home is
  read at each hook call, so a worker cannot widen it (that folder is not writable to it). In the repository holding
  a grant (the git folder lies in the grant or the grant in it, so `cd <notes>` and `git -C <repo root>` both count)
  a worker may run `add` and `rm` naming only granted paths (no bare `-A`, `.` of a wider folder, `:/` magic or
  `--pathspec-from-file`), `commit` of what is staged or of granted paths (no `-a`, `--amend`, `-p`), `push` without
  force, deletion, `+` or `:` refspecs, `--mirror` or `--all`, and `pull`; anything else there stays
  `worker-git-outside`, with a reason naming the paths it may add. Such a repository is a notes repository committed
  on its default branch by design, so a granted git write skips `worker-default-branch` and `worker-push-repo`, which
  read the session's own checkout. `git add notes` (the whole folder) is refused although car-training ran it three
  times: it would take in other sessions' notes; the reason tells the worker to name its files. Would-deny events now
  record the session's folder (`cwd`) so a later replay is exact, and `rule test --edit <file>` decides an edit.
- 2026-11-21 The `Stop` gate counts a foreground `Bash` that outlived its timeout and that Claude Code moved to the
  background (its response carries `backgroundTaskId`) as background work, like `run_in_background`. Of the six
  `stop-unfinished` events of 2026-11-19 (20:27 to 21:18 UTC), all ran on the build stamped 19:57 UTC, before the
  background-wait change (PR #38, merged 21:22 UTC, installed 21:42 UTC); four started background work that change
  recognises, and two (`me-sim-audit-master-check` 20:40, `me-studio-audit-reactions` 21:04) were `ship.sh` runs in
  the foreground that hit the 600 s timeout, which it did not, and which this change does.
- 2026-11-21 The cmux host (`host/cmux.rs`, table in `docs/cmux.md`) speaks to cmux through `cmux rpc`, the CLI's
  passthrough to the v2 socket API, instead of the text commands: JSON both ways, UUIDs that stay valid, and no draft
  guard, which refuses keys while a Claude dialog is open. cmux keeps no list of the programs it started, so the label
  rides in the surface's initial command as `HEADWORK_HOST_LABEL=<label>` and `panes` reads it back from
  `surface.list`; a surface closes with its program, so every program found runs and `pid` stays unknown. States come
  from cmux's own Claude hook records (`cmux sessions list`), which carry the session id headwork started `claude`
  with. Closing passes `force`, since headwork's close means close.
- 2026-11-21 cmux has no blocked state with buttons. `show_blocked` puts the question and its keys ("Trust it? (1 Trust
  the folder · 2 Do not trust it)") in a sidebar status of the surface's workspace (key `headwork-<surface>`, bell
  icon, orange, top priority) and sends a cmux notification for the surface; the user answers by typing the key in the
  pane, and `clear_blocked` removes the status. A rename of the tab was not used: the title is the task name, and
  `retitle` and every tick would overwrite it.
- 2026-11-21 A program cmux starts gets cmux's environment, whose `PATH` holds only system folders, and cmux refuses to
  set `PATH` through `startup_environment`. The adapter therefore runs the program through
  `/usr/bin/env "PATH=${PATH%%:*}:<caller PATH>"`, keeping the surface's own shim folder in front so `claude` goes
  through cmux's wrapper and its hooks.
- 2026-11-21 No CI on GitHub: the user removed the workflow, Windows job included, and `scripts/check.sh` run locally is
  the only gate. A trial macOS job (PR #47) got a runner in 30 s and passed in under 2 minutes, but on this private
  repository about 400 runs a month at the 10x macOS multiplier add ~8000 billed minutes to Windows' ~4400. The
  gate's CI-only skips (no master ref, no comment checker) went with it.
- 2026-11-21 The rules and messages name the programs of the OS headwork runs on (`core/src/platform.rs`, picked by
  `std::env::consts::OS`, not by the configured host: the words are about the system, and a Windows machine with cmux
  or a Mac with wmux would still use its own `kill` and shells). The shared `rules/orchestrator.md` and `worker.md` say
  `{stray}`, `{kill}` and `{tick_install}`, which the rules fill: Windows keeps `tail.exe`, `bash.exe`, `taskkill` and
  the scheduled task "headwork tick"; macOS and other unix systems get `tail`, `bash` and `kill`, `pkill` or a
  process-group kill. The macOS words for `tick --install` and `--uninstall` (rules, `tick --help`, the refusal in
  `ops/schedule.rs`) are a neutral placeholder that says no scheduler exists yet and nothing changes.
- 2026-11-21 cmux asks keep having no buttons: the sidebar status and the notification of the earlier 2026-11-21 entry
  are the whole blocked state on the Mac, and the user dropped buttons for good.
- 2026-11-21 This Mac does not use Remote Control. The macOS part of open question 2 in `docs/design.md`, the
  `--remote-control`/`-n` part of question 6 and items 4 and 10 of the Mac test list (`docs/trials/c-cmux/results.md`)
  do not apply. Whether closing a surface ends a shell-run process (the rest of question 6) stays open.
- 2026-11-21 The macOS tick: a launchd LaunchAgent will run `headwork tick` every 5 minutes for the current user,
  installed by `tick --install` after the user's `Allow` tap like the Windows task. It is planned in the follow-up task
  `headwork-macos-launchd-tick`, which also gives `tick --install` its macOS words; until then `tick --install` and
  `--uninstall` refuse on macOS and change nothing.
- 2026-11-21 A reported pull request URL may be on any host, `https://<host>/<owner>/<repo>/pull/<n>`, so workers of
  projects on a GitHub Enterprise host can record theirs. The URL is stored whole, host included, and `<owner>/<repo>`
  alone is checked against `allowed_repos`, which names no host. `gh pr view <url>` takes the host from the URL, so the
  git host adapter passes no `--hostname` or `GH_HOST`; `gh` only has to be logged in to that host. Checked read-only
  against a merged pull request on such a host: `gh pr view` answered `MERGED` from a folder outside any repository.
- 2026-11-21 A task in `closing` whose newest session is over (ended, gone, or never started) frees its worker slot
  (`TaskState::holds_slot`): nothing of it runs, it only waits on its worktree. A closing task whose session still runs
  keeps the slot until its processes are gone. Seen when two dropped tasks with dirty worktrees held two of four slots.
- 2026-11-21 Asks the tick raises (no session, such as the `worktree` ask) are answered by id: `headwork answer <id>
  <choice>` for the user, the orchestrator tool `answer` for the orchestrator, which asks the user with its own dialog
  and passes on only the pick. `choice` is an option's number or its whole text (a prefix such as `Delete` names
  nothing), the `ask-answered` event records who answered, and a worktree answer is carried out at once and the close
  finished. Asks of a session's own dialog stay answered there only; workers cannot answer by id. Not shown on the host
  as a blocked state: the stuck task's pane is gone, and a button would delete changes on one stray click.
- 2026-11-21 `close --force` also finishes a dropped task with no pull request, keeping a worktree the remove command
  refuses (and superseding its waiting `worktree` ask): dropping already gave the task up, and keeping deletes nothing.
  A task that was not dropped and has no pull request is still refused, since its pull request may just not be found.
- 2026-11-21 A dependency counts merged when it passed through `merged`, or when it is `closing` or `closed` with at
  least one recorded pull request and all of them merged: the orchestrator's `close` takes an `in_review` task straight
  to `closing`. Before starting the queue, the tick, `queue` (CLI and tool) and the orchestrator's `close` ask the git
  host again for such dependencies' pull requests, since the recorded state is the worker's report and may be stale.
- 2026-11-21 `headwork mcp --serve` reads `config.toml` again before every request (and records changed projects), so
  a running server sees an edited `max_workers` at its next call instead of the value it started with. A config that no
  longer parses keeps the last good one in use rather than failing every tool call. Watching the file was not needed:
  one read and parse per request costs well under a millisecond against the hook budget.
- 2026-11-21 On unix a process `launchd` adopted counts as an ended session's when it runs in one of the task's
  worktrees and started while the session ran: `claude` starts each `Bash` command as a session of its own, so a
  `nohup cmd &` loses its parent link and escapes every process group kill, and macOS does not show its environment.
  Main checkouts are shared and never used this way.
- 2026-11-21 `reap` judges orphaned idle `tail` and `bash` on Windows only: on macOS every `launchd` job has parent 1
  and looked orphaned, so a printer daemon's `run_service.sh` would have been killed. On unix a leftover is only taken
  when an ended session's folder ties it to that session.
- 2026-11-21 The macOS tick is built (`ops/launchd.rs`): `tick --install` writes
  `~/Library/LaunchAgents/com.headwork.tick.plist` (label `com.headwork.tick`, `ProgramArguments` the installed
  `~/.local/bin/headwork` and `tick`, `StartInterval` 300, stdout and stderr appended to `<home>/tick.log`) and loads it
  with `launchctl bootstrap gui/<uid>`; a loaded agent is booted out first, and a loaded agent or a written plist makes
  the report `replaced`. `bootstrap` is tried up to five times a second apart, since launchd finishes a `bootout` a moment
  later. `--uninstall` boots out `gui/<uid>/com.headwork.tick` when loaded and deletes the plist when written; neither
  there is `absent`. launchd starts jobs with an almost empty environment, so the plist sets `PATH` (the folders of
  headwork, `git`, `gh`, `cmux`, `claude` as found at install time and the bundled cmux CLI, then
  `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`), `HOME`, and `HEADWORK_HOME` (so a home other than
  `~/.headwork` reaches the tick too). A launchd job is not a cmux descendant, so cmux's socket must be in `automation`
  mode (tested, `docs/cmux.md`, "Outside a cmux terminal"); the user switched this Mac to it. The hook's guard has no
  user-only pattern for `schtasks`, so `launchctl` gets none either: `headwork tick` stays a `worker-admin` command and
  the `Allow` tap stays in the rules. `doctor` adds check `tick-agent` on macOS: `launchctl print
  gui/<uid>/com.headwork.tick` says loaded or not, with the last tick's age; not loaded warns only when enforcing.
  Other unix systems still refuse. `tick.log` is not rotated yet.
- 2026-11-21 Leads (plan, `docs/leads.md`): a task's lead, parts and helpers share one workspace as tabs, not a cmux
  workspace group, by the user's choice: tabs work the same on wmux, which has no groups, and need no trait change.
  The user talks to the leads; the orchestrator dispatches and gives one-line status.
- 2026-11-21 Leads, the user's answers (`docs/leads.md`, section 7): status when asked plus a line on finish or failure;
  leads start their own parts; parts count globally with `max_parts = 2` per lead; every top-level task is a lead; a
  lead's close waits for its parts; other projects go through the orchestrator; a part's tab shows only the part.
- 2026-11-21 cmux landed-workspace check: `spawn`, `split` and `split_pane` compare the answer's `workspace_id` (else the tree's) with the asked one; a mismatch force-closes the new surface and fails naming both ids (`docs/cmux.md`, "Tabs in one workspace").
- 2026-11-21 Leads, milestone L1 (store and core). Migration 0013 adds `tasks.parent_id`, `tasks.lead` and the
  `tasks_parent` index, nothing else: every existing row reads as `TaskShape::Plain`. `core/src/lead.rs` holds
  `TaskShape` (`Lead`, `Part { parent }`, `Plain`), built only by `from_columns`, which refuses a parent together with
  `lead = 1`; a stored row like that fails to load (`StoreError::BadShape`) instead of guessing. A part is created only
  under a live lead (not closing or over) of its own project; a part is never a lead, so parts start no parts.
  `[leads] enabled` defaults to **false** in L1, although `docs/leads.md` shows it on: with it on, `spawn` records new
  top-level tasks as leads, but nothing places them or starts parts until L2 and L3, so doctor listed it under
  `unenforced-keys` (no longer, see 2026-11-21 doctor). L3 (or the user) turns it on. A part "is live" for its lead's close until it is `closed`: dropped,
  failed and closing parts still hold a worktree or a tab, so the lead's `done` and the tick's merged-grace close are
  refused with `lead-parts-live` until each part is closed (a lifecycle refusal, not a hook rule id, so
  `enforce_rules` does not name it). For `max_parts` a part counts its siblings that hold a worker slot, the same
  measure as `max_workers`; a refused part stays queued, and `pump` starts the tasks behind it that fit. The queue
  order is pure (`queue_order`): a part sorts by its lead's (rank, queued time, id), right behind the lead and after its
  older siblings, so it is ahead of every task queued after the lead. Drop cascades in one store transaction
  (`Store::drop_task`): the lead and its droppable parts go to `dropped`, each part logged `drop-cascade`; the
  orchestrator's `close` of a lead (forced or by drop) closes its parts that are not closing or closed first, then the
  lead. `part_spawn`, placement, the tick findings and the tree board are L2 to L4.
- 2026-11-21 Leads, milestone L3 (MCP tools and rules). `part_spawn` and `part_drop` are worker tools listed and callable
  only for a session whose task is a lead (`LEAD_TOOLS` in `ops/mcp.rs`); a plain worker, a part and the orchestrator
  neither see nor may call them. `part_spawn` takes the part's own word as `slug` and prefixes `<lead slug>-` unless it
  is already there; the project is always the lead's (a `project` naming another is refused, pointing at the
  orchestrator); kind `task`, worktree, no `keep`, no `allow_ask`, no effort (the settings' level, as for a spawn without
  one). The model goes through the same admission as `spawn`, so `[models] allowed` refuses and Fable raises the user's
  approval ask, the part waiting queued for it. `max_parts` is not a refusal: the part is created and waits in the queue
  behind its lead (L1), and the answer's `queue` says so. `after` names a part of the same lead by its word, slug or
  full name and becomes `depends_on`, so it means "that part's pull request merged", as `--after` does. `part_drop`
  drops one of the lead's parts and closes it through `close_dropped_part`, the one close a worker may run on another
  task (only a dropped part of its own). `board` and `inbox` of a lead or a part show the lead and its parts and no
  task-less asks; a plain worker's and the orchestrator's stay machine-wide. A lead's rules are `worker.md` plus
  `lead.md` (built in, overridable in the rules folder, part of the rules hash); a lead's and a part's `worker.md` drop
  "When your brief makes you a lead", which stays for plain workers while `[leads] enabled` is off. `{parent}` in
  `worker.md` is the lead's task name for a part, else the orchestrator's address. `orchestrator.md` "Briefing a lead"
  became "Dispatching". The stop gate lets a lead stop while a part is not closed (`waiting_part`, the same measure as
  `lead-parts-live`). `[leads] enabled` stays off by default until L2's placement is merged: parts would otherwise open
  in the project's panes instead of the lead's workspace.
- 2026-11-21 Leads, milestone L2 (placement). Placement follows the task's shape, not `[leads] enabled`: a task stored
  as a lead opens like `workspaces` mode whatever `[layout] mode` says (its own workspace titled with its name, the
  fresh shell replaced, `PlaceKind::Pane`), a part opens with `Target::Tab` (`PlaceKind::Tab`) and a plain task keeps
  `[layout] mode`. A part's workspace is its lead's newest session's `host_workspace` when the host still lists it, else
  the open workspace titled with the lead's name (`place::lead_workspace`); with neither the part's start fails
  (`launch-error` naming the lead, task `failed`) instead of opening a workspace of its own. `helper_tab` of a lead or a
  part resolves the same way; a plain worker's helper keeps its recorded workspace. `place::release` takes the shape: a
  part closes only its tab; a lead's workspace closes when no other surface holds a session (helpers do not count),
  also in `tabs` mode. A part's tab title is `[leads] part_title` with `{part}` = its slug after `<lead-slug>-` (the
  whole slug when it does not start that way); `rc_name` stays the full task name. Retitle and the tick use the same
  title, so part tabs are covered. Resume orders the tasks it brings back leads (and plain tasks) first in age order,
  then parts by lead and creation (task id); a part whose lead is not back afterwards (any state that holds no slot,
  e.g. failed or skipped at logon) is `skipped` and stays where it was. The cmux landed-workspace check came with #54
  and is relied on as is. The live check is `a_lead_gets_a_workspace_and_its_part_a_tab_in_the_real_cmux` in
  `tests/live.rs`; cmux lists a closed workspace for a moment, so it polls for the close. `[leads] enabled` stays off here as
  well: turning it on changes every new task, and is left to the user.
- 2026-11-21 Leads, milestone L4 (close, tick, board). The drop cascade itself came with L1 (`Store::drop_task`, the
  orchestrator's `drop` and `close` ending a lead's parts first); L4 adds that the enforcing tick takes dropped parts to
  `closing` before their leads. Two standing findings, reported and never acted on: `orphan-part`, a part not closing
  or closed whose lead is `closing`, `closed`, `dropped` or `failed` (`core::lead::orphaned`), and `stray-tab`, a
  running part whose surface runs but not in its lead's workspace (found as `place::lead_workspace` finds it; nothing
  is reported while that workspace is gone). A part's pane-lost check now looks for its surface in any workspace, so a
  moved tab is a stray tab and not an interrupted part. A lead's `UserPromptSubmit` adds one line per part that is
  `needs_user` or has a waiting ask ("part `<name>` waits for the user in its tab"), on every prompt while it waits,
  with or without a rules change. `board` prints each lead with its parts indented under it (`└`), marks leads
  `(lead)`, and a part whose lead is hidden (closed) stays at the top marked `(part of #<id>)`. `status` (CLI and the
  orchestrator's MCP tool, `ops/status.rs`, pure roll-up `core::lead::status`) prints one line per top-level task:
  the lead's end decides first (`dropped`, also for a closed task that was dropped; `failed`; `interrupted`;
  `parked`; `finished` once closed), then an ask of the lead or of a part not closing or closed makes it `needs you`
  (naming the asking parts), then `queued`, `in review` with the open pull request URLs of the lead and its parts,
  `finished` for a merged or closing lead without open parts, else `working`. The words beyond the five of
  `docs/leads.md` (`queued`, `interrupted`, `parked`, `dropped`) are kept because folding them into `working` or
  `failed` would mislead. A part whose lead is not listed gets a line of its own. `[leads] enabled` stays off.
- 2026-11-21 Doctor and `[leads] enabled`. `unenforced-keys` no longer reports `leads.enabled`: placement (L2), `part_spawn` (L3)
  and close, tick and board (L4) are built, so the key is enforced.
- 2026-11-21 The orchestrator's agent defaults to `orchestrator`; `root` only when `~/.claude/agents/root.md` exists and no
  `orchestrator.md` is in `~/.claude/agents` or the launch folder's `.claude/agents`. The agent file may live in the project
  (`~/Work/.claude/agents`), so the old "else `root`" fallback made a bare `headwork orchestrator` fail there.
- 2026-11-21 Bare `headwork`. With no arguments the binary runs `headwork orchestrator` with its defaults, instead of printing the usage. `--help` and `--version` are unchanged.
- 2026-11-21 Leads ask before closing. The user asked that a lead ask them before its task closes, until they say
  otherwise: `[leads] ask_before_close`, default `true`. The go is an answered `AskUserQuestion` headed `Close` whose
  pick starts with `Close`, stored as a `close-go` event (no migration: events hold it). It holds the lead's own close
  and the tick's merged-grace close (`go: CloseGo` in `CloseFacts` and `FinishFacts`), not a drop or the orchestrator's
  close, which already follow the user's word. A go stays valid for the task once given. The setting is not part of
  the rules hash, so a running lead sees a change at its next session.
- 2026-11-21 An orchestrator running outside headwork. After a reboot wmux reopened the orchestrator's pane with a plain
  `claude --resume <id>`: the conversation went on, but the store had seen it end (`SessionEnd`) and nothing set
  `HEADWORK_EXE`, so from 2026-11-17 it ran unmanaged, the park of 2026-11-21 recorded no orchestrator and the resume
  reopened none. Now park, when the store has no running orchestrator, looks for a surface whose Claude session id (from
  the host's surface states) is a known orchestrator session (`Store::orchestrator_session`, any state) and records it
  on the park with a mechanical recap: it is not sent the park notice, since without headwork tools it cannot call
  `recap`. Resume after a crash adopts such a session the same way when the store has no orchestrator to bring back.
  Either way resume closes that pane (as it closes every pane the host restored) and reopens the conversation with
  `headwork orchestrator --session <id> --resume`, so it is managed again; event `orchestrator-unmanaged`. Only
  sessions the store knows are adopted: a `claude -n orchestrator` started by hand has no recorded folder, so it stays
  the user's. Because such an orchestrator runs `headwork resume` through Bash in the very pane resume closes, the CLI
  checks `WMUX_SURFACE_ID` (or `CMUX_SURFACE_ID`) first (`resume::would_close`) and, when resume would close its own
  surface, starts itself again detached without that variable (event `detached`) and exits, so closing the pane cannot
  cut it off halfway.
- 2026-11-22 Resume keeps each session's permission mode. After a restart the orchestrator came back in `default` (the
  `headwork orchestrator` default) although it ran in auto when parked, and a worker came back in its task's spawn
  mode whatever the user had switched it to with shift+tab. Migration 0014 adds `sessions.permission_mode`, the mode a
  session was last seen in: every hook records the `permission_mode` of its input (event `session-mode` on a change),
  and `headwork orchestrator` records the mode it starts with. Resume starts each session, workers and the orchestrator
  alike, in the last top-level `permissionMode` its Claude transcript records (Claude writes one on every mode switch
  and on each prompt, and the transcript exists for an orchestrator running outside headwork, which has no hooks),
  else the stored one, else as before (the task's mode; `default` for the orchestrator). A worker gets it through its
  session row (`claude_command` prefers it to the task's), the orchestrator through `--mode`. A mode headwork cannot
  start a session in (`bypassPermissions`, `dontAsk`) counts as none, so the fallback applies rather than an older mode.
- 2026-11-22 Park asks the orchestrator, and takes the recap it already wrote. Park 4 (2026-11-22) wrote a mechanical
  recap for a managed orchestrator and did not wait: an orchestrator started with `headwork orchestrator` recorded no
  host surface, so park looked for its pane in the host's surface states, did not find it there, skipped the notice and
  logged nothing about why. Park also ignored the recap the orchestrator had written 36 ms before calling the `park`
  tool (whose description asks for one first), because only recaps tagged with the park counted, and the resume summary
  then showed the newer mechanical recap instead of it. Now a new `headwork orchestrator` records the surface the host
  names in its environment (`HostKind::surface_var`: `WMUX_SURFACE_ID`, `CMUX_SURFACE_ID`; event `session-surface`;
  a session brought back gets its place from the resume as before). Park first adopts the orchestrator's newest own
  recap that is in no park and was written at or after its latest `prompt` event, the turn that started the park
  (`Store::adopt_orchestrator_recap`, event `recap-adopted`), and then neither asks nor waits; a recap from an earlier
  turn does not count. Otherwise it sends the notice and waits, and logs `orchestrator-asked`, or `orchestrator-unasked`
  with the reason, which the mechanical recap repeats.
- 2026-11-22 The silent kind. The user asked for a kind of worker that closes without the user's confirmation. Kinds were
  already free strings (`spawn --kind`, the `spawn` tool's `kind`), so `silent` is a reserved kind name
  (`SILENT_KIND`, `LeadsConfig::asks_before_close`) rather than a new flag or migration: a `silent` lead's close and the
  tick's merged-grace close need no `close-go`, and its rules leave out "Ask before you close". Every other kind keeps
  `[leads] ask_before_close`. A `silent` kind in the main checkout still needs to be listed as main-only like any kind.
- 2026-11-22 Review flows, the plan (`docs/review-flows.md`, nothing built). The user asked for reviews started by
  their word (`review 1` to `review 3`, the number setting how strong the review and the pushback are), run by many
  small specialized reviewers as fresh-context subagents under a reviewer lead of its own, with more flows (graphs)
  for other kinds of tasks later. The user chose: the reviewer lead is a part of kind `review` in the task's workspace;
  a flow is a TOML graph headwork tracks node by node through MCP tools (not Claude Code Workflow scripts, which
  headwork would see only at the end and a park would lose mid-run); the implementing lead fixes or disputes each
  finding, the reviewer lead re-checks, level 3 runs a second round and unsettled disputes go to the user in one ask;
  the level sets breadth, depth and pushback together. Taken without asking: the trigger is the `UserPromptSubmit`
  hook matching `review [<flow>] <level> [<focus>]` in a lead's own prompt, and `review_start` is refused without such
  a request, so reviews start only on the user's word; the review reads a detached worktree headwork makes at the
  pinned commit, not one from the project's `worktree_new`; the reviewer lead runs on Sonnet by default while the nodes
  carry the judgement; sessions pass findings and resolutions by `SendMessage`, the tools returning the text and the
  address; the first flows are `code` and `plan`.
- 2026-11-22 Closed sessions leave the Claude app. Closed workers stayed listed in the Claude app (Remote Control)
  because `close` ended `claude` with `taskkill /T /F`, which skips the teardown where Claude Code archives its Remote
  Control session at claude.ai (`[remote-bridge] Torn down (archive=200)` in its debug log). A trial with throwaway
  `--remote-control` sessions showed that `/exit` typed into the pane archives the session when idle, mid-turn and
  mid-tool-call alike, and exits within a second. So `close` (`ask_exit` in `ops/close.rs`) types `/exit` and Enter into
  the session's pane first, waits up to `[watch] exit_grace_s` (default 15, 0 skips it) for `claude` to be gone, and
  kills the tree only when it stays (events `exit-asked`, `exit-ignored`, `exit-unasked`). Calling the archive endpoint
  ourselves was rejected: it is undocumented and needs the user's OAuth token. Park still ends sessions without `/exit`,
  so resume brings them back unarchived. Sessions closed before this change are archived by hand (⋯ → Archive in
  claude.ai/code or Desktop).
- 2026-11-22 Autoupdate (`docs/autoupdate.md`, a plan): every merge to master is released by GitHub Actions (Windows
  x86_64 and macOS arm64, gated by `check.sh` on both) and each machine installs the newest release with `headwork
  update`, smoke-tested against a copy of the store first, rather than building origin/master locally. The user chose a
  public repository so the runners are free (a release per merge on a private one would need about 11,000 to 13,000
  minutes a month against 2,000 included) and downloads need no login; the history is rewritten first (author e-mail
  addresses, `Claude-Session:` trailers, raw trial artefacts and the copied cmux files) and the code is licensed MIT.
- 2026-11-22 The store copies itself before migrating (autoupdate step 1): `Store::open` takes the copy, not the
  installer, because any process may be the first to open an older store after a swap (a hook usually wins). `VACUUM
  INTO` rather than a file copy, because the store is in WAL mode and other processes write it. Two processes may race;
  a copy whose `user_version` is no longer the old one is thrown away. A failed copy is reported (`Store::backup`,
  printed by `install`) but does not stop the migration: a store the new build refuses would stop every hook. No event
  is logged: no actor fits a store open, and a new actor word would break older builds reading the log.
- 2026-11-22 The version in the binary (autoupdate step 2, `crates/headwork/src/version.rs`): `--version` prints
  `headwork <version> (<commit>, schema <n>)`, the version and commit from `HEADWORK_VERSION` and `HEADWORK_COMMIT` read
  with `option_env!` at build time (cargo rebuilds when they change), `0.1.0 (dev, ...)` when unset. Environment
  variables the release workflow sets rather than a `build.rs` that runs git: a `build.rs` reruns only when `HEAD` moves,
  so it would call a dirty tree clean, and it would relink every worktree build on each commit. The string is built at
  run time (clap's `string` feature), so the CLI parses through `Cli::command().version(..)` and `from_arg_matches`;
  usage errors still exit 2, which the launcher's exit hand-off relies on. `doctor`'s `binary` check shows it.
- 2026-11-22 Token usage (plan in `docs/usage.md`): read Claude's transcripts, one row per API response keyed by
  `message.id`, instead of Claude Code's OpenTelemetry export or the status line, because transcripts are exact, need no
  collector, split out subagents and reach back as far as Claude keeps them. The user chose an API-price estimate from
  `config.toml`, every session on the machine (others as `unmanaged`), 90 days of per-response rows then a daily
  roll-up, and `usage`, `board`, `status` and close as the views.
- 2026-11-22 headwork went public (autoupdate step 3) as a new repository with the rewritten history rather than by a
  force push and a visibility switch: GitHub keeps every pull request's `refs/pull/<n>/head`, which would have kept the
  old commits (private e-mail addresses, session trailers) fetchable. The old repository is the private
  `Ivan091/headwork-archive`; pull request numbers before the move refer to it. The code is MIT licensed.
- 2026-11-22 The release workflow (autoupdate step 4): `release.yml` on every push to `master` (and by hand) runs
  `check.sh` and a release build on `windows-latest` and `macos-latest` with `HEADWORK_VERSION=0.1.<run number>` and
  `HEADWORK_COMMIT`, checks the binary prints both, and a publish job on `ubuntu-latest`, the only one with `contents:
  write`, writes `SHA256SUMS` and runs `gh release create v0.1.<run> --generate-notes`. One run at a time (`concurrency:
  release`, newer pending runs replace older pending ones). `gate.yml` runs `check.sh` read-only on pull requests. The
  comment checker is the user's (`~/.claude/skills/no-comments`), not vendored into a public repository, so `check.sh`
  skips that step when `CI` is set and the local gate keeps enforcing it.
- 2026-11-22 `headwork update` (autoupdate step 5). Taken without asking: `[update]` is checked only while `enabled`
  (its project configured with an `allowed_repos` entry; the first entry is the repository), so the default
  `project = "headwork"` never breaks a config without that project. `--to` is the user's explicit word: it installs
  with updates off, during a back-off and after the release failed here, but not during a park, a resume or below
  `disk_stop_gb`. The decision runs once before GitHub is asked (off, machine, disk, back-off need no network), then
  with the release. A transient failure is reported (event, `doctor` warning) only from its third attempt in a row; a
  deterministic one at once, and it stays latched in `update.json` until a newer release installs. The API request
  runs `curl -sSL --include` (status and `ETag` read from the headers, `If-None-Match` sent when an `ETag` is kept),
  not `-f`, so a `304`, `404` and `403`/`429` are told apart; file downloads use `--fail`. The smoke test runs the
  installed build's `doctor --json` first, then the release's, on the same scratch home (the config and a `VACUUM
  INTO` copy of the store, `HEADWORK_CLAUDE_JSON` pointed into it so the trust check never writes the live file), so
  the copy is still at the installed schema for the first; only checks that fail with the release and pass with the
  installed build fail the update. The hand-over `update --after` takes the lock afresh (waiting up to 60 s) once the
  old process released it, rather than inheriting it, since a file lock ends with its process. No new actor, finding
  or event word that older builds would fail to parse: events are logged as `user`; the host notification, the
  standing finding and the orchestrator's prompt line wait for step 6. `update` takes `--dir` like `install`, which
  lets tests point it away from `~/.local/bin`.
- 2026-11-22 The GitHub gate is the required pull request check; this replaces "No CI on GitHub" (2026-11-21), whose
  cost reason went with the private repository: standard runners are free for a public one. A ruleset on `master`
  requires `gate (windows-latest)` and `gate (macos-latest)`, and delivery waits for them with `gh pr checks <url>
  --watch --fail-fast` before `gh pr merge`, since `ship` would otherwise merge before CI ends (PR #3 merged green, but
  nothing would have held a red one). Locally a worker runs `scripts/check.sh --quick` (fmt, clippy, line length,
  attribution, comment ban) and the tests of the crates it changed; tests and docs on both systems run in CI. The
  comment ban stays local only, since its checker lives in the user config. `gate.yml` cancels an older run of the same
  pull request.
- 2026-11-22 Autoupdate turned on in code (step 6). The guard lifts `update` for workers of an `install = true` project
  only with `--dry-run`, `--json` and `--wait <minutes>`; `--to`, `--dir` and `--after` stay denied (`--dir` could
  install elsewhere, `--after` is the hand-over's). The close trigger fires after the `Cleaned` transition of any task
  of `[update] project`, dropped and forced closes included, and starts `env.exe` (the program finishers run) with
  `HEADWORK_HOME` set, since a finisher found its home through `--home`; a start that fails is logged in
  `update-triggered` and ignored. The tick's `poll_due` reuses the decision's checks that need no network (park or
  resume, disk below stop, a transient failure's back-off) and also waits while the update lock is held, so a tick
  does not start a process every five minutes during a back-off or a `--wait 30`; it runs whatever `[hooks] enforce`
  says, and `TickEnv` gained an `UpdateStarter` seam and `TickReport` an `update` field. The orchestrator's tool
  `update` answers a dry run in-process (as the CLI would, without a host) and otherwise starts the detached update and
  returns its pid; a start that fails is a tool error.
- 2026-11-22 How update failures are told. The standing finding is a new `FindingId::UpdateFailed` (`update-failed`):
  older builds tolerate it because no build reads the `findings` rows back into a `FindingId` (the `rule` column is
  written, and matched as text by `resolve_findings`), and the `finding` events carry the rule inside their JSON data,
  read as text. `update` raises and clears it itself, not the tick (which may not be scheduled): one key per failed
  release (`update-failed:v0.1.9`) or `update-failed:transient`, a new failure resolving the others; the new build's
  `update --after` clears it, and so does an update that finds nothing to install after a transient failure. The host
  notification and the orchestrator's line go out only when the finding is new, which makes each told once. The lines
  wait in `update.json` (`notices`, at most ten), not the event log, which has no index on the event type and would be
  scanned at every orchestrator prompt, and not in a new table, which needs a migration; the `UserPromptSubmit` hook of
  an orchestrator session takes them under the update lock and leaves them for the next prompt while an update runs.
  An install leaves a line when its checkout step saw plugin files change, or when the new build's schema (from the
  version lines) is above the replaced one's. An older build that saves `update.json` (a rollback) drops waiting
  notices.

- 2026-11-22, turns and notifications (audit G3): the `UserPromptSubmit` hook marks a prompt whose text starts with
  `<task-notification` as `data.notification = true` (event data is JSON, no migration). A turn starts at the last
  prompt that is not a notification, and a notification does not supersede the session's asks. Background work counts
  as running while the background starts since that prompt outnumber the notifications since it: the payload names no
  task id the start event could be matched on, so the stop is allowed while any is open, and a surplus notification
  never reopens anything. Work started in an earlier turn is not tracked, as before.
- 2026-11-22, notifications are no user prompts (audit G3): `user_prompted_within` skips notification prompts, so a
  subagent finishing no longer lets `ask-push` think the user just spoke.
- 2026-11-22, the lead's Close ask names the push (audit G8): `rules/lead.md` "Ask before you close" now says to send a
  `PushNotification` just before the `AskUserQuestion`, as the worker rules do; most `ask-push` would-denies came from
  this step.
