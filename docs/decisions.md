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
