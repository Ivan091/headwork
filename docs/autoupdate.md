# Autoupdate: plan

Status: a plan, not built (2026-11-22), reviewed once against the code. Nothing here changes the live setup until the
user approves it and the steps below are implemented.

Goal: after a headwork pull request merges, the machine runs the new build without anyone building and installing by
hand, and a broken master never replaces a working install.

## How updating works today

1. A headwork worker (or lead) merges its pull request with `ship`, which fast-forwards the main checkout
   `S:/Code/headwork/headwork` (`git pull --ff-only`, granted to workers).
2. The same worker runs `cargo build --release -p headwork` in the main checkout and `target/release/headwork.exe
   install` (granted by the project's `install = true`).
3. Hot reload (`docs/hot-reload.md`) does the rest: hooks, the CLI and finishers use the new build from their next
   call, MCP relays swap their inner server before the next request, launchers hand their exit to the installed build.

What goes wrong with that:

- It depends on the worker remembering it. A pull request merged by hand on GitHub, by the orchestrator, or by a
  worker that ended first is never installed; the installed build silently falls behind master.
- Two workers merging close together race on the build and the install; nothing serialises them.
- A worker can install a build from its own worktree that is not master; nothing notices or corrects it.
- The only check of a new build is that `--version` prints `headwork ` (`verify_installed` in `main.rs`). A build that
  fails on config parse, store open or a migration passes it, and since hooks fail open, such a build quietly switches
  the guard off in every session.
- The first open by the new build migrates the live store with no backup; the user makes those by hand
  (`~/.headwork/state.db.pre-0013`).
- A failed build is seen only in that worker's pane, and plugin file changes reach only new sessions without anyone
  being told.

## What stays as it is

Hot reload is the install mechanism and is not changed: `install::install` (rename aside, rename in, check, clean old
copies), the MCP relay, the exit hand-off, build records and the `builds` doctor check. Renaming aside the very binary
that runs the update is safe, as it is for any install. Autoupdate decides when to build, builds where nothing else
runs, proves the build against a copy of the store, and reports. The built-in rules are compiled into the binary
(`include_str!` in `ops/rules.rs`; this machine has no `~/.headwork/rules`), so they update with it. The plugin is a
local marketplace pointing at the main checkout, so its files update when the main checkout is fast-forwarded.

## Design

### Where it builds: a worktree of its own

The update builds in a dedicated worktree, `<main>.autoupdate` (here `S:/Code/headwork/headwork.autoupdate`, on S:
because C: has little room; a slug holds no dot, so no task worktree `{main}-{slug}` can take the name), checked out
detached at `origin/master`, with its own `target/`. So:

- the main checkout's branch, dirty files or a `--main` task working there never block an update;
- no running program holds a build output open (the linker cannot replace a running `.exe` on Windows);
- a worker's `git pull` or `cargo build` in the main checkout never collides with the update.

The main checkout is still fast-forwarded afterwards (`git merge --ff-only origin/master`), because the plugin files
are read from there. That step is best effort: when the checkout is not on `master`, is dirty, has local commits, or
`index.lock` is taken, it is skipped quietly with an event and retried at the next update. `headwork clean` skips this
worktree (no task owns it) and `clean --gc` waits while `update.lock` is held.

### One command: `headwork update`

```text
headwork update [--dry-run] [--force] [--json]
```

Run from the installed binary. Steps:

1. **Lock.** Take `<home>/update.lock`. When it is held, touch `<home>/update.pending` and exit: the holder runs again
   for it. `headwork install` waits up to 60 seconds for the same lock instead of failing.
2. **Decide** (pure, in `core`, below): off, park or resume running, disk at `watch` level `Stop`, or the target commit
   already installed or already failed (deterministically) are skips.
3. **Fetch** `git fetch origin master` in the update worktree and `git checkout --detach origin/master`. Every git and
   cargo call runs with `GIT_TERMINAL_PROMPT=0`, `GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=10"` and a
   hard timeout (the runner in `adapters/src/worktree.rs`), so a missing ssh agent or a hung network ends in a
   transient failure, never a hang.
4. **Build** `cargo build --release --locked -p headwork` with `HEADWORK_COMMIT=<sha>` in the environment, output to
   `<home>/updates/<millis>-<sha>.log` (the newest 20 kept). `--locked`, so a lockfile rewrite fails the build instead
   of dirtying the tree.
5. **Smoke test against a copy of the store.** Make a scratch home holding the live `config.toml` and a copy of the
   store taken with `VACUUM INTO`, and run `<new build> doctor --json` there (`HEADWORK_HOME` pointed at it). That
   proves the new build parses the config, runs its migrations and opens the store, with the live store untouched. A
   doctor problem that the installed build does not also report fails the update. With `[update] verify = "check"`,
   `bash scripts/check.sh` runs in the update worktree as well.
6. **Install** through `install::install`, its `--version` check unchanged.
7. **Hand over to the new build.** The old process stops here and runs `<installed> update --after <old-sha>`, which
   holds the parent's lock (it does not take it again) and does the rest with the new code: fast-forward the main
   checkout (best effort, above), compare `plugin/` between the two commits, record `update-installed`, report, and
   when `update.pending` exists or origin moved meanwhile, start a fresh `headwork update` after the lock is released.
   The old process never touches the store after the swap: once the new build migrates it, the old one would be
   refused (`SchemaTooNew`).

`--dry-run` prints the decision and the commits using `gh api repos/<repo>/commits/master`, changing nothing.
`--force` retries a commit that failed.

### The store backup belongs in the store

Independent of autoupdate, the store takes its own backup: when `Store::open` finds `user_version` below the
migrations it knows, it copies the database with `VACUUM INTO <home>/backups/state-<from>-to-<to>-<millis>.db` before
applying any migration (the newest three kept). That covers hand installs, a hook from a running session that opens
the store first after a swap, and the update alike, and replaces the hand-made `state.db.pre-*` copies.

### What starts it

| Trigger | When | Needs |
|---|---|---|
| The worker after `ship` | right after its merge, the headwork worker runs `headwork update` instead of today's build and install | `update` lifted for `install = true` workers in the guard (below) |
| A task of the project closes | after the `Cleaned` transition in `finish` of any task of `[update] project`, `finish` starts `headwork update` detached (`spawn_detached`); no merged check, the update is idempotent | nothing new |
| The tick | every `[update] poll_min` minutes, `gh api repos/<repo>/commits/master` (no SSH, and `gh` is already the tick's) against the installed commit; when they differ and the decision says go, start `headwork update` detached and return | the tick scheduled (`tick --install` after the user's `Allow`); not scheduled on this machine today |
| By hand | `headwork update`, or the orchestrator's MCP tool `update` | nothing |

The worker trigger is the immediate one. The close trigger catches a worker that forgot or ended first, but it can
come hours later: a lead closes only after the user's go (`[leads] ask_before_close`). The tick catches merges made
outside headwork. The tick never builds in its own process (a build takes minutes, and its poll runs under
`tick.lock`). Autoupdate is not an enforcement action, so it runs under `[update] enabled` alone, whatever `[hooks]
enforce` says.

### Knowing the installed commit

A build's stamp says whether two files are the same build, not which commit it is. The update passes
`HEADWORK_COMMIT=<sha>` to its own build; the binary reads it with `option_env!` and `--version` prints `headwork 0.1.0
(<sha>, schema 14)`, or `(unknown, schema 14)` for a hand build. No `build.rs`: one that runs git would report a clean
commit for a dirty tree (it reruns only when `HEAD` moves) and relink every worktree build on each commit. The update,
the tick and `doctor` read the installed commit by running `<installed> --version`; `unknown` or a build too old to say
always updates, so a hand install of a worktree build is replaced at the next master commit.

### Configuration

```toml
[update]
enabled = false
project = "headwork"
verify = "smoke"
poll_min = 15
```

`enabled` is off until the user turns it on. `project` names the configured project whose main checkout holds
headwork's source; its `allowed_repos` entry is the repository `gh` polls. `verify` is `smoke` (build plus the
scratch-store doctor) or `check` (also `scripts/check.sh`). `poll_min = 0` turns the tick trigger off.

`Config` denies unknown fields, so a build older than this section refuses a config that has it. `[update]` is added
to the live config only after a build that knows it is installed, and nothing installs an older build afterwards.

The project flag `install = true` keeps its one meaning, a guard grant: its workers may run `headwork install` (to try a
build before it merges) and, new, plain `headwork update`. `[update] project` alone decides which project's closes
trigger an update.

### The guard

`update` joins `ADMIN_COMMANDS` (`core/src/guard/mod.rs`), lifted like `install` for workers of an `install = true`
project, plain `update` only (`--force` stays the orchestrator's and the user's). The orchestrator cannot run `cargo`
or `git merge` through Bash, and does not need to: its MCP tool `update` starts the same detached command. This goes
into `docs/decisions.md` with the change.

### Decisions in `core`

`core/src/update.rs`, pure and table-tested: `decide(&UpdateFacts) -> UpdateStep`. `UpdateFacts` holds enabled, the
installed commit, the target commit, the last failure (commit, kind, attempts, time), park or resume running, the disk
level, the last poll time, and `force`. `UpdateStep` is `UpToDate`, `Skip(SkipReason)`, `Wait { until }` or
`Build { target }`. The runner in `headwork/src/ops/update.rs` gathers facts and carries the step out, like the tick
and `clean`.

Failures are of two kinds. Deterministic (compile error, smoke test, check) latch on the commit: it is not tried again
until a new commit or `--force`. Transient (fetch, network, crates.io, `index.lock`, a lock timeout, a
`rust-toolchain.toml` bump whose download fails, disk) never latch: they wait 5, 15, then 60 minutes and retry.

### State

No migration (a migration would itself break pinned launchers, decisions 2026-11-14). The working memory is
`<home>/update.json`, written atomically: last poll, last failure (commit, kind, step, attempts, time). Findings the
user must see go through the tick's existing `findings` table (`raise_finding`), which already deduplicates them.
Events keep the history: `update-started`, `update-skipped` (only for a reason worth reading, not every up-to-date
poll), `update-failed`, `update-installed`, `checkout-not-forwarded`.

### Reporting

The binary itself cannot push to the phone: `[machine] push = "claude"` means the agent's own `PushNotification`.
So:

- **Failure:** `update-failed`, a host notification (`host.notify`, as the tick does), a standing finding
  `update-failed` until a later commit installs, the `doctor` check `update` as a warning with the log path, and one
  line in the orchestrator's next prompt, so the orchestrator pushes it. The old build keeps running; nothing was
  installed, so nothing is rolled back.
- **Transient failures** say nothing until the third attempt fails.
- **Success:** `update-installed`, and a line in the orchestrator's next prompt only when the plugin files changed
  ("sessions started before <sha> keep the old hooks until park and resume") or a migration ran.
- `headwork doctor`, new check `update`: enabled or not, installed commit, origin's master, last result.

### Rollback

No command. The normal answer to a bad build is a fix forward, which the next merge installs; a build that fails its
smoke test never gets installed. By hand, when needed: check out the last good commit in a worktree, build, `install`,
and, only when a migration ran, stop sessions and put back the store backup from `<home>/backups/`. This recipe goes
into `docs/hot-reload.md`.

### Windows and macOS

- Windows: whether a process the scheduled tick starts with `spawn_detached` survives the tick's end, or keeps the
  "headwork tick" instance alive so later ticks are skipped, depends on Task Scheduler allowing breakaway from its job,
  which `spawn_detached` asks for and silently drops when refused. It is tried before the tick trigger is built, and
  `update-started` records whether the breakaway held.
- macOS: the launch agent builds its `PATH` from `TICK_PROGRAMS` (`ops/launchd.rs`); `cargo` joins them. `install`
  works the same there.

## Conventions that change, together with turning it on

- The headwork worker's last delivery step becomes `headwork update`; the orchestrator's briefs and notes that ask for
  `cargo build` plus `install` change in the same step as `enabled = true`, so the hand build and the update do not both
  run for every merge.
- `CLAUDE.md` "Update the installed binary after a merge" becomes "the worker runs `headwork update`; closes and the tick
  catch what it missed".

## Steps

Each a pull request through `scripts/check.sh`:

1. **Store backup before migrating** (`Store::open`). Useful on its own.
2. **Commit in `--version`** (`option_env!("HEADWORK_COMMIT")`, the schema) and `doctor` showing the installed commit.
3. **`headwork update` by hand:** `[update]` config, `core/src/update.rs` with table tests, `ops/update.rs` (lock and
   pending, update worktree, fetch, build, smoke test, install, `--after`, best-effort fast-forward), `update.json`,
   events, the `doctor` check, `install` waiting for the lock, `clean` skipping the update worktree. Tests: CLI tests on
   a temporary home with a local bare repository as origin and a fake builder (the build command is a seam, as `verify`
   is in `install`).
4. **Turn it on:** the guard lift, the close trigger, the MCP tool `update`, the convention change, then `[update]` in the
   live config with `enabled = true`, after the user's go.
5. **The tick trigger:** first the Task Scheduler breakaway trial, then the `gh` poll, findings, backoff. Live only once
   the user schedules the tick.

Steps 1 to 4 give the useful core: every merged headwork change installs itself, proven against a copy of the store
first. Step 5 can wait for the tick to be scheduled.

## Open questions for the user

- `verify`: the plan says `smoke` (build plus the scratch-store doctor, about a minute) and keeps `check` as an option.
- The update worktree at `S:/Code/headwork/headwork.autoupdate` costs one more release `target/` (about 400 MB) on S:
  and a cold first build. The alternative is building in the main checkout, which brings back the branch and dirty
  preconditions and collisions with `--main` tasks.
- Success says nothing unless plugin files changed or a migration ran.
