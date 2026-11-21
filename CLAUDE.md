# headwork

headwork manages Claude Code sessions with hard rules in code: one Rust binary that keeps tasks, sessions, worktrees,
pull requests, asks, rules, jobs and events in one SQLite store, and enforces the lifecycle through guarded state
transitions, Claude Code hooks, an MCP server and a watchdog tick. It replaces the prose-only `orchestrate` skill step
by step (`docs/design.md`, section 7.3).

- `docs/design.md` is the approved design. It still says `agentctl`; read it as `headwork`. Section 8 lists the
  milestones.
- `docs/research.md` and `docs/trials/` hold the research and trial results the design builds on.
- `docs/decisions.md` records choices the design left open. Add a line there whenever you take one.

## Layout

```
Cargo.toml              workspace: shared package settings, dependencies, lints, profiles
crates/
  core/                 headwork-core: lifecycle state machine, guards, config model; pure, no I/O
  store/                headwork-store: SQLite (rusqlite, bundled), numbered migrations, queries
  adapters/             headwork-adapters: process trees (Job Objects), the terminal host (wmux, cmux, fake), the git host (gh,
                        fake), disks, the claude
                        runtime, worktrees
  headwork/             the binary: CLI (board, log, spawn, queue, close, import, launch, hook, mcp, tick, install) over a small
                        lib (home, ops)
plugin/                 the Claude Code plugin: hooks.json (each hook calls `headwork hook <event>`), .mcp.json (`headwork mcp`)
rules/                  built-in orchestrator and worker rules, injected at SessionStart
tests/fixtures/         config fixtures: me-sim, me-studio and a generic work repository
scripts/check.sh        the quality gate
docs/                   design, research, trials, decisions
```


The MCP server (`headwork mcp`, `ops/mcp.rs`) offers the orchestrator tools (board, status, inbox, spawn, drop, close, answer, queue, clean, reap, rules,
rule_add, rule_retire, job_status, job_forget, park, resume) and the worker tools (board, inbox, report, done, recap, job_start,
job_register, job_status, job_stop, job_forget, helper_tab, worktree_add, and for a lead part_spawn and part_drop) by the session's identity. A lead's or part's board and inbox show only the lead and its parts; a lead's rules add `rules/lead.md`, and `{parent}` in `rules/worker.md` names a part's lead (L3, `docs/leads.md`). `headwork mcp` itself is a relay (`ops/relay.rs`) over
`headwork mcp --serve`, replaced when a new build is installed; every build must keep accepting `mcp --serve`. Hot
reload (`ops/install.rs`, `ops/reload.rs`, `headwork exited`) is described in `docs/hot-reload.md`.
Milestone 5 is the tick (`ops/tick.rs`, pure decisions in
`core/src/watch.rs`, its memory in migration 0004). A running task whose session ended or whose launcher is gone
becomes `interrupted` through `ops/lost.rs`, called by the `SessionEnd` hook, `board` and the tick alike.
A task may deliver in several pull requests: close and the tick find them through one lookup (`ops/prs.rs`: recorded
URLs, the stored branch, the branch checked out), a later pull request takes a `merged` task back to `in_review`, and
`finish` takes a close claim (`<home>/closing/<id>.lock`) so two finishers never run one close.
A lead gets helper worktrees for its parts from the worker tool `worktree_add` (`ops/helpers.rs`, migration 0012): made by
the project's `worktree_new` at `worktree_path` with the part's slug, writable for the task, removed at close (one that
holds work stays and is logged). Workers may also write `<main>/.claude/worktrees` (subagent worktrees) and fast-forward
the main checkout with `git pull --ff-only`; `install = true` on a project lets its workers run `headwork install`;
`[hooks] orchestrator_repos` names notes repositories the orchestrator may commit and push in (project checkouts inside
them excluded); `orchestrator_writable` gives it the edits there. A worker may write paths outside its folder only
when granted (`core/src/guard/grants.rs`): its project's `extra_writable = [{ path, kinds }]` for its task kind, and
under `[hooks] brief_writable` the files its brief lists on a `Writes:` line; in their repository it may then `add`,
`rm`, `commit`, `push` (no force) and `pull` those paths only, whatever the branch.
`headwork clean` (`ops/clean.rs`, pure decisions in `core/src/clean.rs`) removes the worktrees of merged (session
ended) and closed tasks through the project's own `worktree_remove`, and Claude Code agent worktrees inside main
checkouts (`.claude/worktrees/agent-*`) whose locking agent is gone and whose branch is merged (branch deleted with
`git branch -d`), superseded by commits of the same subjects, or gone upstream (branch kept). It never touches a dirty
tree, a live task's, a running agent's or one no task owns; it lists each with the reason. `--gc` runs plain `git gc`
in a main checkout only when no registered job runs there and no main-only task is live.
`headwork reap` (`ops/reap.rs`, pure decisions in `core/src/reap.rs`) ends what ended sessions left running (the
processes their launcher and `claude` started, rebuilt from parent links and start times; on unix also those `launchd`
adopted that run in the task's worktrees, which `close` ends at once) and, on Windows only, orphaned `tail` and
`bash` idle for over 10 minutes (on unix every `launchd` job looks orphaned), never `claude`, `me-studio`, `headwork`
or a registered job, nor what they started or what started them; Git Bash shells take their parents from Git Bash's `ps -el`. The orchestrator's `close` takes
`force` for a task stuck in `closing` whose pull requests are all merged and that has no live job (`close_forced` in
`ops/close.rs`), and for a dropped task with none: a worktree the remove command refuses is recorded `kept` instead of
blocking. A closing task whose session ended frees its worker slot. Asks the tick raises have no dialog: `headwork
answer` or the orchestrator's `answer` tool answers them by id (`ops/answer.rs`), recording who answered.
Leads (`docs/leads.md`) start with L1: a task's shape (`core/src/lead.rs`: `Lead`, `Part { parent }`, `Plain`, from
`tasks.parent_id` and `tasks.lead`, migration 0013), `[leads]` (`enabled`, off for now; `max_parts`; `part_title`), a
part waiting behind its lead in the queue and for `max_parts` at start, a lead's `done` refused while a part is not
closed (`lead-parts-live`), and a lead's drop or close ending its parts first (`Store::drop_task`, `ops/close.rs`).
L2 places them (`ops/place.rs`): a lead gets a workspace of its own titled with its name whatever `[layout] mode`
says, its parts and helpers open as tabs of the lead's workspace (by recorded id, then by title; a part is refused when
it is gone), a part's tab is titled by `[leads] part_title`, its release closes only its tab, and resume brings each
lead back before its parts.
L4 adds the tick findings `orphan-part` (a part open while its lead is over or failed) and `stray-tab` (a part's tab
outside its lead's workspace), a line in a lead's next prompt per part waiting on the user, the tree `board`, and
`status` (`ops/status.rs`, roll-up in `core/src/lead.rs`), a CLI command and an orchestrator MCP tool.
Milestone 6 is park and resume (`ops/park.rs`, `ops/resume.rs`), recaps (`ops/recap.rs`) and the jobs registry (`ops/jobs.rs`), stored by migration 0005.
Workers open in split panes (`ops/place.rs`, pure choice in `core/src/layout.rs`, pane per session in migration
0007); see "Panes" below.

## Commands

```bash
bash scripts/check.sh                      # the full gate; must pass before every pull request
cargo test -p headwork-core                # one crate
cargo run -p headwork -- board             # projects, tasks (parts indented under their lead), sessions (pane or tab), states, asks
cargo run -p headwork -- status [--all] [--json]   # one line per top-level task, rolled up from its parts
cargo run -p headwork -- log [<name>]      # the event log, all or one task
cargo run -p headwork -- spawn <project> <slug> <brief> [--main] [--model M] [--allow-ask "<cmd>"]...   # a task in its own pane, or queued
cargo run -p headwork -- queue [--order a,b]   # order the queue, start every queued task that fits
cargo run -p headwork -- close <name> [--force]   # close a task, verified; --force: stuck in closing, PRs merged (or dropped, none), no job
cargo run -p headwork -- answer <ask-id> <choice>  # answer an ask the tick raised (e.g. keep or delete a worktree) by option number or text
cargo run -p headwork -- retitle [<name>]   # set the pane titles (name, model, effort) of live sessions
cargo run -p headwork -- import [--dry-run]  # adopt the orchestrate scripts' children (workers), projects, worktrees
cargo run -p headwork -- rule test "<cmd>" [--task N] [--branch B] [--background]   # would the guard deny it?
cargo run -p headwork -- tick [--json]                 # reconcile the store with panes, processes, PRs, disks; shadow unless hooks.enforce
cargo run -p headwork -- tick --install | --uninstall  # create or remove the Windows task "headwork tick" or the macOS launch agent (after the user's Allow tap)
cargo run -p headwork -- reap [--dry-run] [--json]     # end what ended sessions left running and orphaned idle tail/bash
cargo run -p headwork -- park [--deadline S]   # recaps, jobs stopped at checkpoints, panes closed: safe to restart
cargo run -p headwork -- resume [--logon] [--wait S]   # after the restart: panes back with their recaps, jobs restarted
cargo run -p headwork -- jobs              # the registered long-running jobs
cargo run -p headwork -- job forget <id> --reason "..."   # mark a job over without touching any process
cargo run -p headwork -- recap <name>      # a task's newest recap
cargo run -p headwork -- doctor            # check the install; exit 1 on a problem
target/release/headwork.exe install       # install this build into ~/.local/bin while the old one runs
cargo run -p headwork -- clean [--dry-run] [--gc]   # remove worktrees of merged/closed tasks and finished agent worktrees; gc quiet main checkouts
cargo run -p headwork -- shadow-report [--since 24h|7d|2026-11-19|<RFC 3339>]   # would-deny events by rule id, optionally from a time on
cargo run -p headwork -- orchestrator      # run the orchestrator session, recorded in the store (old name: root)
```

On macOS the terminal host is cmux: `[machine] host = "cmux"`; `docs/cmux.md` maps every host operation to its cmux call.

The binary reads `$HEADWORK_HOME` (default `~/.headwork`): `config.toml` and `state.db`. Build output stays in the
repository's own `target/` on S:; C: has little free space.

### Scheduling the tick

`headwork tick` is idempotent and takes `tick.lock`, so a scheduler may call it every few minutes. `headwork tick
--install` creates (or replaces) the one Windows scheduled task "headwork tick": every 5 minutes, the current user,
no admin, running the installed `~/.local/bin/headwork.exe tick`; `headwork tick --uninstall` removes it
(`ops/schedule.rs`). On macOS the same commands write and load, or unload and delete, the user launch agent
`~/Library/LaunchAgents/com.headwork.tick.plist` (`launchctl bootstrap`/`bootout gui/<uid>`, every 5 minutes, output in
`<home>/tick.log`, `ops/launchd.rs`); cmux's socket must be in `automation` mode for it (`docs/cmux.md`, "Outside a cmux
terminal"), and `headwork doctor` (check `tick-agent`) says whether it is loaded. Sessions run either only after the
user's `Allow` tap naming that command; nothing installs it silently.

It only reports until `[hooks] enforce = true`. A windowless build (no console flash per run) is not made yet.

### Panes

`[layout]` in `config.toml` decides where a worker opens in its project's workspace. The default, `mode = "panes"` with
`panes = 4`, gives each worker its own pane, at most a grid of four equal quarters, two rows of two, and never a pane
smaller than a quarter: the first worker holds the whole workspace, the second takes the bottom half, the third the top
right quarter, the fourth the bottom right; a fifth and later stack as tabs on the pane with the fewest sessions (then
the largest, then the later one). Rows, not columns, because wmux splits only the first pane top and bottom; any other
pane it splits only side by side (`layout agents --anchor-pane`). A worker takes a free pane when there is one, and no
empty pane is cut ahead of time. A worker that closes takes its pane away and the pane beside it takes the space; the
next worker splits that half back into quarters. When both workers of a row close, the other row grows into two
columns; then the next worker splits the left column, and the right one is never cut into narrow strips, so further
workers stack until the workspace empties. The workspace closes with its last worker. A pane that still holds a session
(a stacked worker, the orchestrator) stays. `mode = "tabs"` keeps every worker a tab. Workers already open are never
moved (wmux cannot move a surface). The pure choice is `core/src/layout.rs`. `headwork board` prints each session's
pane, or that it is a tab. Why it works this way is in `docs/decisions.md` (2026-11-14, split panes; 2026-11-16, panes
close with their worker; 2026-11-19, a grid of quarters). Run the real check with
`cargo test -p headwork --test live -- --ignored`; it opens and closes `headwork-test` workspaces.

With `panes = 2` (or 1) the one split is side by side instead: the second worker takes the right half, left and right
with a vertical divider, later workers stack as tabs, and a closed worker's pane gives the whole workspace to the other.
A larger limit stays rows first because only rows first reaches the grid (`docs/decisions.md`, 2026-11-19, two panes
side by side).

`mode = "workspaces"` gives every worker a workspace of its own, titled with the task name (`<project>-<slug>`) and
holding one full-screen pane: no splits, no stacked tabs, and the workspace closes with its worker. Workers opened
earlier in a project workspace stay where they are. A worker opens a short-lived helper session of its own (a separate
program in wmux, not an `Agent` subagent) with the `helper_tab` MCP tool: always a new tab in its own workspace, labelled
`headwork:helper`, which does not keep the workspace open when the worker closes. See `docs/decisions.md` (2026-11-19, one workspace per worker).

### Pane titles

Each worker's pane or tab is titled `<task name>`, from `[layout] title` (default `{name}`; `{model}` and `{effort}` also work), set
when the pane opens, re-asserted by every tick, and by `headwork retitle [<name>]` on demand. The effort is the stored
one, else the level Claude's settings give the model (`ops/effort.rs`), else `medium`; spawn stores it. `?` shows only when
it cannot be told. See `docs/decisions.md` (2026-11-14, pane titles).

### Folder trust

Claude Code keeps folder trust per repository in its state file (`~/.claude.json`, `projects["<path>"].hasTrustDialogAccepted`);
a task worktree inherits its main repository's trust, a parent folder's does not reach a repository inside it. So before a
task's session starts (spawn and resume) and in `headwork doctor` (check `trust`), headwork sets that one field for the
`main` folder of the task's project, keyed the way Claude Code writes it (forward slashes, upper-case drive letter), and
logs `trust-granted` (or `trust-error`, which never stops the spawn). Only configured `main` folders, never another key or
setting. The write is atomic (a temp file renamed over it), the previous version goes to `~/.claude.json.headwork-backup`,
and nothing prints the file's contents. The file is `$HEADWORK_CLAUDE_JSON`, else `$CLAUDE_CONFIG_DIR/.claude.json`, else
`~/.claude.json`; tests point `HEADWORK_CLAUDE_JSON` (or `Home::with_claude_json`) at a temporary file. If a starting
worker still shows the trust dialog, the tick raises a `trust` ask, a blocked state on the pane whose buttons answer the
dialog (`1` yes, `2` no), a notification and a `trust-prompt` finding, and holds the spawn timeout until the dialog is gone.
See `docs/decisions.md` (2026-11-18, folder trust).

### Restart: park, then resume

Before restarting the computer, tell the orchestrator "park" (its MCP tool starts it), or run `headwork park` by hand: it waits
up to `--deadline` seconds (default 300) for each session's recap and each `checkpoint` job's next checkpoint, then
closes the panes and prints "safe to restart". After the restart start wmux and run `headwork resume`; the panes come
back with the same Claude sessions (names, Remote Control, recaps), registered jobs restart, and
`<home>/recaps/park-<id>.md` is the summary for the orchestrator. Nothing installs a logon task. To resume at logon the user
would run:

```bash
schtasks /Create /SC ONLOGON /DELAY 0001:00 /TN "headwork resume" /TR "\"S:\Code\headwork\headwork\target\release\headwork.exe\" resume --logon --wait 180"
```

`--logon` leaves sessions a crash interrupted to `machine.resume_after_crash`; by hand they are resumed too.

The orchestrator (started with `headwork orchestrator`) is parked and resumed too: park asks it for a recap and leaves
it running (the restart ends it), and resume reopens it in an `orchestrator` workspace in the folder it ran in, with
the same conversation and its recorded name, at logon too and after a crash. An orchestrator that still runs is left
alone. See `docs/decisions.md` (2026-11-14, the root parks and resumes).

### Migration on this machine (step 0)

Milestone 7 installed headwork for real, with `hooks.enforce = false` (shadow mode). `headwork doctor` checks all of it
(binary, PATH, config, store, wmux, gh, claude, plugin entry, hooks file, enforce switch, disk, deprecated config keys,
orchestrator session, claude processes) and exits 1 on a problem; `headwork shadow-report` lists the would-deny events by rule id.

What changed in the live setup:

| What | Where |
|---|---|
| The binary, copied from `target/release/headwork.exe` | `~/.local/bin/headwork.exe` (on PATH) |
| Config and store | `~/.headwork/config.toml`, `state.db` (`config.toml.import` is what `headwork import` wrote) |
| The plugin, as a local marketplace pointing at `S:/Code/headwork/headwork` | `extraKnownMarketplaces.headwork` and `enabledPlugins."headwork@headwork"` in `~/.claude/settings.json` |

The plugin does nothing in a session headwork did not start: its hooks run `${HEADWORK_EXE:-true}`, and only the
launcher sets `HEADWORK_EXE`. Its MCP server connects with no tools in such a session. A hook that fails (missing
binary: exit 127, broken config: exit 1) never blocks a call, and a hook takes about 30 ms.

Update the installed binary after a merge, safely while sessions run it (docs/hot-reload.md):

```bash
cargo build --release -p headwork
target/release/headwork.exe install    # renames the running copy aside, puts the build in ~/.local/bin, cleans old copies
```

Hooks, the CLI and finishers use the new build from their next call. Sessions launched by a hot-reload build switch
their MCP server at the next tool call and hand their exit to the new build. Sessions launched before hot reload
(`headwork doctor`, check `builds`, "pinned") keep their old MCP server until `/mcp` → headwork → Reconnect in that
session, or park and resume. Plugin files (`hooks.json`, `.mcp.json`) need a new session. A pinned launcher cannot
record its exit once a newer migration ran; `headwork close <name>` finishes such a session (docs/hot-reload.md).

**Legacy orchestrate scripts retired (2026-11-16).** The scripts in `~/.claude/skills/orchestrate/scripts` that headwork
replaced (the spawn, close, list and root scripts, with their `*.legacy.sh` twins and the `engine` switch) are gone, and so is
the undo path back to them. Spawning, closing and listing go through `headwork spawn`, `close` and `board` only.
`projects.conf` stays; `headwork import` still reads it. See `docs/decisions.md` (2026-11-16, legacy scripts retired).

**Undo, step 0 (plugin and hooks):** delete the keys `extraKnownMarketplaces.headwork` and
`enabledPlugins."headwork@headwork"` from `~/.claude/settings.json` (then `sync-config`). New sessions load no plugin.
The binary, `~/.headwork` and the PATH copy can stay; remove them with `rm -r ~/.headwork ~/.local/bin/headwork.exe`.

**The orchestrator as a store session:** `headwork orchestrator` starts `claude --agent <agent> --remote-control
orchestrator -n orchestrator` here, records it as the store's orchestrator session and gives it its identity. `--name`
changes the name; the agent is `orchestrator` (user or launch-folder `.claude/agents`), `root` only when just `~/.claude/agents/root.md` exists, and
`--agent` overrides both.

**Root and children became orchestrator and workers (2026-11-14).** The coordinating session was called the root and the
sessions it spawns children; `docs/decisions.md` (2026-11-14, orchestrator and workers) lists every renamed name. What
still takes the old names:

| Old | New | The old one |
|---|---|---|
| `headwork root` | `headwork orchestrator` | a hidden alias |
| session name `root` | `orchestrator` | an address: a worker's `SendMessage` to `root` or `orchestrator` goes to the live orchestrator's name (`PreToolUse` hook) |
| `HEADWORK_ROLE=root`, `child` | `orchestrator`, `worker` | still parses: running sessions keep their environment |
| roles, actors, rule ids (`root-git`, `child-edit-outside`, ...) in the store | `orchestrator-git`, `worker-edit-outside`, ... | migration 0009 rewrites them; the old spellings still parse |
| `machine.max_children`, `project.<name>.max_children`, `hooks.root_writable` | `max_workers`, `orchestrator_writable` | still read; `headwork doctor` warns (check `config-keys`) |
| rules files `root.md`, `child.md` in the rules folder | `orchestrator.md`, `worker.md` | read when the new file is missing |
| rule scopes `root`, `child` (`rule_add`) | `orchestrator`, `worker` | accepted and stored as the new word |

The rules a worker gets name the orchestrator's live address: the store's live orchestrator session (`rc_name`), else
the `-n` name of an unmanaged `claude -n orchestrator` or `-n root` that wmux runs, else `orchestrator`. The address is
part of the rules hash, so when it changes every running worker is told the new one at its next prompt.

Moving the running root (a store session named `root`, launched by a hot-reload build) to the new name:

1. Install this build (`target/release/headwork.exe install`). Migration 0009 runs at the first store open. Nothing
   changes for the running sessions: the root stays `root`, the rules keep naming `root`, and `headwork doctor` shows
   check `orchestrator` as ok.
2. When the root is idle, end it and start it again under the new name from `S:/Code`: `headwork orchestrator`. Until the config task adds `~/.claude/agents/orchestrator.md`
   it runs the `root` agent, and with that file the `orchestrator` agent, with no change here. Its earlier
   conversation does not come along; give it a recap first or let it read `HANDOFF.md`.
3. From then on the rules name `orchestrator`; every running worker gets them at its next prompt. A worker started
   with this build (plugin `hooks.json` with `SendMessage` in the `PreToolUse` matcher) that still writes to `root`
   is sent on to `orchestrator` by its hook. A worker started before this build has no such hook: a `SendMessage` to
   `root` it sends before its next prompt fails, and it reaches `orchestrator` after that prompt. Restart the root when
   few workers run, or tell those workers the new name.

**The shadow week** runs from 2026-11-14 to 2026-11-21. Managed sessions (workers spawned through the wrappers, and
the orchestrator once it starts with `headwork orchestrator`) log every would-deny in the store. On 2026-11-21 read
`headwork shadow-report`; step 2 (`[hooks] enforce = true`, tick scheduled) waits for the user to review it. Do not
turn enforcement on before that.

`headwork shadow-report --since <when>` reads only events at or after `<when>` (an RFC 3339 time, a date `YYYY-MM-DD`
as midnight UTC, or a span back from now such as `24h`, `7d`); text and `--json` both carry the window. `[hooks]
enforce_rules = ["stop-unfinished", ...]` makes denies by the listed rules block even while `enforce = false` (every other
rule stays shadow; unknown ids are a config error; the tick's closing and removing still wait for `enforce`);
`doctor` and `shadow-report` show the list. It is empty by default. A worker that started background work (a background
`Bash`, `Monitor`, `Agent` or `Workflow`) since its last prompt may stop: it waits for the notification.

## Code rules

The user's global rules in `~/.claude/AGENTS.md` apply. The points that matter most here:

- **No comments.** Only doc comments (`///`, `//!`) are allowed, in Rust, shell, SQL migrations and workflows alike. A
  user-wide hook blocks edits that add other comments, and `scripts/check.sh` runs the same checker.
- **Errors:** libraries (`core`, `store`) define typed errors with `thiserror`; only the binary uses `anyhow`. No
  `unwrap`, `expect` or `panic!` outside tests; fallible operations return `Result` and use `?`.
- **Lints** live once under `[workspace.lints]` and are denied: rustc `unsafe_code` (forbidden), `missing_docs`,
  `missing_debug_implementations`, `unreachable_pub`, `unused_qualifications`, `let_underscore_drop`,
  `rust_2018_idioms`; clippy `all`, `pedantic`, `nursery`, `cargo`, plus the restriction picks `unwrap_used`,
  `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`, `as_conversions`,
  `allow_attributes`, `allow_attributes_without_reason`, `missing_assert_message`, `string_slice`, `str_to_string`,
  `unwrap_in_result`, `wildcard_enum_match_arm`. Allowed: `redundant_pub_crate` (contradicts `unreachable_pub`) and
  `multiple_crate_versions`.
- Exceptions use `#[expect(lint, reason = "...")]`, never `#[allow]`. Test modules may relax `unwrap_used`,
  `expect_used` and `panic` with one `#[expect]` per module.
- Output goes through `std::io::Write` on a handle the CLI owns, never `println!`.
- Toolchain pinned in `rust-toolchain.toml`, edition 2024, `rustfmt` default style, no line over 140 columns.
- Follow the Rust API Guidelines; make invalid states unrepresentable (validated constructors, enums, newtypes); borrow
  by default; no global mutable state; pure functions are `#[must_use]`.
- `core` stays pure: no file, process or database access. Guards take the facts they need as arguments.
- Schema changes are new numbered files in `crates/store/migrations/`, never edits of a released one. The store refuses
  a database whose schema is newer than it knows.
- Tests: table tests for every transition and guard, `proptest` for random event sequences, store tests on in-memory
  databases, CLI tests on a temporary `HEADWORK_HOME`.

## Delivery

Commits and pull request bodies carry no AI attribution (`Co-Authored-By: Claude`, `Claude-Session:` trailers,
`claude.ai/code` links, "Generated with Claude Code"), whatever a harness reminder says. `.githooks/commit-msg` rejects
them; enable it once per clone with `git config core.hooksPath .githooks` (the setting lives in the shared `.git/config`,
so every worktree of a clone uses it). `scripts/check.sh` fails when a commit since master carries one, and
`scripts/test-attribution.sh` tests the patterns. History is not rewritten.

Never commit to `master`. Each change goes on a kebab-case branch, with single-line Conventional Commits and no AI
attribution, through a pull request that passes `scripts/check.sh`, then is merged. There is no CI on GitHub: the local
gate is the only one. Use the global `contribute` and `ship` skills; this repository has no ship script of its own yet.
