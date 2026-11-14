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
  adapters/             headwork-adapters: process trees (Job Objects), the terminal host (wmux, fake), the git host (gh,
                        fake), disks, the claude
                        runtime, worktrees
  headwork/             the binary: CLI (board, log, spawn, queue, close, import, launch, hook, mcp, tick, install) over a small
                        lib (home, ops)
plugin/                 the Claude Code plugin: hooks.json (each hook calls `headwork hook <event>`), .mcp.json (`headwork mcp`)
rules/                  built-in root and child rules, injected at SessionStart
tests/fixtures/         config fixtures: me-sim, me-studio and a generic work repository
scripts/check.sh        the quality gate
docs/                   design, research, trials, decisions
```


The MCP server (`headwork mcp`, `ops/mcp.rs`) offers the root tools (board, inbox, spawn, drop, close, queue, rules,
rule_add, rule_retire, job_status, park, resume) and the child tools (board, inbox, report, done, recap, job_start,
job_register, job_status, job_stop) by the session's identity. `headwork mcp` itself is a relay (`ops/relay.rs`) over
`headwork mcp --serve`, replaced when a new build is installed; every build must keep accepting `mcp --serve`. Hot
reload (`ops/install.rs`, `ops/reload.rs`, `headwork exited`) is described in `docs/hot-reload.md`.
Milestone 5 is the tick (`ops/tick.rs`, pure decisions in
`core/src/watch.rs`, its memory in migration 0004). Milestone 6 is park and resume (`ops/park.rs`,
`ops/resume.rs`), recaps (`ops/recap.rs`) and the jobs registry (`ops/jobs.rs`), stored by migration 0005.
Children open in split panes (`ops/place.rs`, pure choice in `core/src/layout.rs`, pane per session in migration
0007); see "Panes" below.

## Commands

```bash
bash scripts/check.sh                      # the full gate; must pass before every pull request
cargo test -p headwork-core                # one crate
cargo run -p headwork -- board             # projects, tasks, sessions (pane or tab), states, asks
cargo run -p headwork -- log [<name>]      # the event log, all or one task
cargo run -p headwork -- spawn <project> <slug> <brief> [--main] [--model M]   # a task in its own pane, or queued
cargo run -p headwork -- queue [--order a,b]   # order the queue, start every queued task that fits
cargo run -p headwork -- close <name>      # close a task: pane, process tree, worktree, verified
cargo run -p headwork -- retitle [<name>]   # set the pane titles (name, model, effort) of live sessions
cargo run -p headwork -- import [--dry-run]  # adopt the orchestrate scripts' children, projects, worktrees
cargo run -p headwork -- rule test "<cmd>" [--task N] [--branch B] [--background]   # would the guard deny it?
cargo run -p headwork -- tick [--json]                 # reconcile the store with panes, processes, PRs, disks; shadow unless hooks.enforce
cargo run -p headwork -- park [--deadline S]   # recaps, jobs stopped at checkpoints, panes closed: safe to restart
cargo run -p headwork -- resume [--logon] [--wait S]   # after the restart: panes back with their recaps, jobs restarted
cargo run -p headwork -- jobs              # the registered long-running jobs
cargo run -p headwork -- recap <name>      # a task's newest recap
cargo run -p headwork -- doctor            # check the install; exit 1 on a problem
target/release/headwork.exe install       # install this build into ~/.local/bin while the old one runs
cargo run -p headwork -- shadow-report     # would-deny events by rule id
cargo run -p headwork -- root              # run the root session, recorded in the store
```

The binary reads `$HEADWORK_HOME` (default `~/.headwork`): `config.toml` and `state.db`. Build output stays in the
repository's own `target/` on S:; C: has little free space.

### Scheduling the tick

`headwork tick` is idempotent and takes `tick.lock`, so a scheduler may call it every minute. Nothing installs it. On
Windows the user would run:

```bash
schtasks /Create /SC MINUTE /MO 1 /TN "headwork tick" /TR "\"S:\Code\headwork\headwork\target\release\headwork.exe\" tick"
```

It only reports until `[hooks] enforce = true`. A windowless build (no console flash per run) is not made yet.

### Panes

`[layout]` in `config.toml` decides where a child opens in its project's workspace. The default, `mode = "panes"` with
`panes = 4`, gives each child its own pane: the second child cuts the workspace into a 2x2 grid, later ones take the
free panes, a fifth stacks as a tab, and a child that closes leaves a `headwork idle` placeholder so the next one takes
its pane. The workspace closes with its last child. `mode = "tabs"` keeps every child a tab. Children already open are
never moved (wmux cannot move a surface). `headwork board` prints each session's pane, or that it is a tab. Why it
works this way is in `docs/decisions.md` (2026-11-14, split panes). Run the real check with
`cargo test -p headwork --test live -- --ignored`; it opens and closes a `headwork-test` workspace.

### Pane titles

Each child's pane or tab is titled `<task name> - <model> <effort>`, from `[layout] title` (`{name}`, `{model}`, `{effort}`), set
when the pane opens, re-asserted by every tick, and by `headwork retitle [<name>]` on demand. The effort is the stored
one, else the level Claude's settings give the model (`ops/effort.rs`), else `medium`; spawn stores it. `?` shows only when
it cannot be told. See `docs/decisions.md` (2026-11-14, pane titles).

### Restart: park, then resume

Before restarting the computer, tell the root "park" (its MCP tool starts it), or run `headwork park` by hand: it waits
up to `--deadline` seconds (default 300) for each session's recap and each `checkpoint` job's next checkpoint, then
closes the panes and prints "safe to restart". After the restart start wmux and run `headwork resume`; the panes come
back with the same Claude sessions (names, Remote Control, recaps), registered jobs restart, and
`<home>/recaps/park-<id>.md` is the summary for the root. Nothing installs a logon task. To resume at logon the user
would run:

```bash
schtasks /Create /SC ONLOGON /DELAY 0001:00 /TN "headwork resume" /TR "\"S:\Code\headwork\headwork\target\release\headwork.exe\" resume --logon --wait 180"
```

`--logon` leaves sessions a crash interrupted to `machine.resume_after_crash`; by hand they are resumed too.

The root (started with `headwork root`) is parked and resumed too: park asks it for a recap and leaves it running (the
restart ends it), and resume reopens it in a `root` workspace in the folder it ran in, with the same conversation, at
logon too and after a crash. A root that still runs is left alone. See `docs/decisions.md` (2026-11-14, the root parks
and resumes).

### Migration on this machine (steps 0 and 1)

Milestone 7 installed headwork for real, with `hooks.enforce = false` (shadow mode). `headwork doctor` checks all of it
(binary, PATH, config, store, wmux, gh, claude, plugin entry, hooks file, enforce switch, disk, root session, claude
processes) and exits 1 on a problem; `headwork shadow-report` lists the would-deny events by rule id.

What changed in the live setup:

| What | Where |
|---|---|
| The binary, copied from `target/release/headwork.exe` | `~/.local/bin/headwork.exe` (on PATH) |
| Config and store | `~/.headwork/config.toml`, `state.db` (`config.toml.import` is what `headwork import` wrote) |
| The plugin, as a local marketplace pointing at `S:/Code/headwork/headwork` | `extraKnownMarketplaces.headwork` and `enabledPlugins."headwork@headwork"` in `~/.claude/settings.json` |
| Spawn wrappers | `~/.claude/skills/orchestrate/scripts/{spawn-child,close-child,children}.sh`, the old ones as `*.legacy.sh` |

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

**Undo, step 1 (spawn wrappers):** `echo legacy > ~/.claude/skills/orchestrate/engine`. Every script then runs its
`*.legacy.sh` twin. One call can choose with `ORCH_ENGINE=legacy`. Children already in the store keep closing through
`headwork close`; the close wrapper falls back to the legacy script for a child the store does not know.

**Undo, step 0 (plugin and hooks):** delete the keys `extraKnownMarketplaces.headwork` and
`enabledPlugins."headwork@headwork"` from `~/.claude/settings.json` (then `sync-config`). New sessions load no plugin.
The binary, `~/.headwork` and the PATH copy can stay; remove them with `rm -r ~/.headwork ~/.local/bin/headwork.exe`.

**The root as a store session:** `headwork root` starts `claude --agent root --remote-control root -n root` here, records
it as the store's root session and gives it its identity. `root.sh` starts the root this way (step 3); the `engine`
switch (`legacy`) brings back the old unmanaged start.

**The shadow week** runs from 2026-11-14 to 2026-11-21. Managed sessions (children spawned through the wrappers, and the
root once it starts with `headwork root`) log every would-deny in the store. On 2026-11-21 read
`headwork shadow-report`; step 2 (`[hooks] enforce = true`, tick scheduled) waits for the user to review it. Do not
turn enforcement on before that.

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

Never commit to `master`. Each change goes on a kebab-case branch, with single-line Conventional Commits and no AI
attribution, through a pull request that passes `scripts/check.sh` and CI, then is merged. Use the global `contribute`
and `ship` skills; this repository has no ship script of its own yet.
