# Autoupdate: plan

Status: a plan, not built (2026-11-22), reviewed once against the code. Nothing here changes the live setup until the
user approves it and the steps below are implemented.

Goal: every merge to master becomes a release on GitHub, and each machine (Windows here, the Mac through cmux) installs
it by itself, with no Rust toolchain, no login and no hand steps, while a broken build never replaces a working one.

The user chose (2026-11-22): the repository goes public, and every merge publishes a release. On a private repository
that cadence would cost about 11,000 to 13,000 Actions minutes a month at the current pace (about 7 merges a day,
Windows minutes counted twice and macOS ten times) against 2,000 included; on a public repository standard runners
are free and release files download over plain HTTPS without a login.

## How updating works today

1. A headwork worker (or lead) merges its pull request with `ship`, which fast-forwards the main checkout
   `S:/Code/headwork/headwork` (`git pull --ff-only`, granted to workers).
2. The same worker runs `cargo build --release -p headwork` in the main checkout and `target/release/headwork.exe
   install` (granted by the project's `install = true`).
3. Hot reload (`docs/hot-reload.md`) does the rest: hooks, the CLI and finishers use the new build from their next
   call, MCP relays swap their inner server before the next request, launchers hand their exit to the installed build.

What goes wrong with that:

- It depends on the worker remembering it. A pull request merged by hand, by the orchestrator, or by a worker that
  ended first is never installed; the installed build silently falls behind master. The Mac has to build its own.
- Two workers merging close together race on the build and the install.
- The only check of a new build is that `--version` prints `headwork ` (`verify_installed` in `main.rs`). A build that
  fails on config parse, store open or a migration passes it, and since hooks fail open, such a build quietly switches
  the guard off in every session.
- The first open by the new build migrates the live store with no backup; the user makes those by hand
  (`~/.headwork/state.db.pre-0013`).
- There is no CI: the local gate in the worker's worktree is the only check, and nothing checks master after two
  merges meet.

## What stays as it is

Hot reload is the install mechanism and is not changed: `install::install` (rename aside, rename in, check, clean old
copies), the MCP relay, the exit hand-off, build records and the `builds` doctor check. Installing over the binary that
runs the update is safe, as for any install. The built-in rules are compiled into the binary (`include_str!` in
`ops/rules.rs`; this machine has no `~/.headwork/rules`), so they arrive with the release. The local gate stays the
pull request gate.

## Producing releases: GitHub Actions

`.github/workflows/release.yml`, on `push` to `master` and on `workflow_dispatch`:

1. **Gate and build**, a matrix of `windows-latest` (x86_64) and `macos-latest` (arm64): the toolchain from
   `rust-toolchain.toml`, a cargo cache (`Swatinem/rust-cache`), `bash scripts/check.sh`, then `cargo build --release
   --locked -p headwork` with `HEADWORK_VERSION` and `HEADWORK_COMMIT` in the environment. The gate runs on both
   systems, so the `cfg(unix)` code is checked too, which the Windows-only local gate never does. `check.sh` takes the
   comment checker from `NO_COMMENTS_CHECK`; the workflow fetches or vendors it, or that step is skipped in CI with a
   note.
2. **Publish**, after both: tag `v0.1.<run number>` on the built commit and `gh release create` (the workflow's own
   `GITHUB_TOKEN`, `contents: write`) with `headwork-x86_64-pc-windows-msvc.exe`, `headwork-aarch64-apple-darwin` and
   `SHA256SUMS`, the commit and the merged pull requests in the notes. A run whose gate fails publishes nothing, so a
   broken master never becomes a release.

`concurrency: release` with `cancel-in-progress: false`: one run at a time, and a newer pending run replaces an older
pending one, so a burst of merges yields one release of the newest commit. The workflow never runs on `pull_request`
with write permissions (a public repository takes pull requests from anyone); a separate read-only `gate.yml` on
pull requests may run `check.sh` on both systems, for free.

Repository settings the user sets once: public visibility, immutable releases (so a published asset cannot be
swapped), and Actions allowed to create releases.

Versions: `CARGO_PKG_VERSION` stays `0.1.0`; the release's build sets `HEADWORK_VERSION=0.1.<run>` and
`HEADWORK_COMMIT=<sha>`, read with `option_env!`, so `--version` prints `headwork 0.1.214 (<sha>, schema 14)`. A local
build prints `headwork 0.1.0 (dev, schema 14)`. No `build.rs`.

## Installing releases: `headwork update`

```text
headwork update [--dry-run] [--wait <minutes>] [--to <tag>] [--json]
```

Run from the installed binary. Steps:

1. **Lock.** Take `<home>/update.lock`. When it is held, touch `<home>/update.pending` and exit: the holder runs again
   for it. `headwork install` waits up to 60 seconds for the same lock instead of failing.
2. **Ask GitHub.** `GET https://api.github.com/repos/<repo>/releases/latest` (or the tag of `--to`) without a login,
   with the last `ETag` (a `304` does not count against the 60 requests an hour an address may make). Through `curl`,
   which Windows 10 and later and macOS ship, from the adapters (`adapters/src/download.rs`, a seam for tests), with a
   connect and a total timeout: no new TLS dependency in the binary. `<repo>` is the project's `allowed_repos` entry.
3. **Decide** (pure, in `core`, below): off, park or resume running, disk at `watch` level `Stop`, the release already
   installed, or the release already failed here are skips. A `dev` build installed by hand (a worker trying a build)
   is replaced only by a release published after that file was installed, so a test install is not undone at the next
   poll.
4. **Download** this platform's asset and `SHA256SUMS` into `<home>/updates/<tag>/` and check the hash (the `sha2`
   crate). The hash catches a broken or cut download; the immutable release and HTTPS stand for the origin.
5. **Smoke test against a copy of the store.** A scratch home with the live `config.toml` and a copy of the store taken
   with `VACUUM INTO`; run `<downloaded> doctor --json` there (`HEADWORK_HOME` pointed at it). That proves the release
   parses this machine's config, runs its migrations and opens the store, with the live store untouched. A doctor
   problem the installed build does not also report fails the update.
6. **Install** through `install::install`, its `--version` check unchanged.
7. **Hand over to the new build.** The old process stops here and runs `<installed> update --after <old version>`,
   which holds the parent's lock and does the rest with the new code: fast-forward the main checkout to the release's
   commit (best effort, below), record `update-installed`, report, and when `update.pending` exists or a newer release
   appeared, start a fresh `headwork update` once the lock is released. The old process never opens the store after
   the swap: the new build may have migrated it, and the old one would be refused (`SchemaTooNew`).

`--wait <minutes>` polls once a minute until a release newer than the installed build appears, then goes on; it is
what the close trigger uses, because a release lands some minutes after its merge. `--to <tag>` installs that
release, newer or older (a rollback), and is refused when that release knows an older schema than the store has.
`--dry-run` prints the decision and changes nothing.

### Plugin files

The plugin is a local marketplace pointing at the main checkout, so its files change only when the main checkout
moves. After an install, the update fast-forwards the main checkout to the release's commit (`git fetch` then
`git merge --ff-only <sha>`, with `GIT_TERMINAL_PROMPT=0`, `ssh -o BatchMode=yes -o ConnectTimeout=10` and a timeout).
When the checkout is not on `master`, is dirty, has local commits, or `index.lock` is taken, that step is skipped
quietly with the event `checkout-not-forwarded` and retried at the next update. When the release changed files under
`plugin/`, the orchestrator is told (below). Shipping the plugin inside the release, and pointing the marketplace at
`~/.headwork/plugin`, would take the checkout out of the path; it changes `settings.json` and can come later.

### The store backup belongs in the store

Independent of autoupdate, `Store::open` backs the store up before it migrates: when `user_version` is below the
migrations it knows, it copies the database with `VACUUM INTO <home>/backups/state-<from>-to-<to>-<millis>.db` before
applying any (the newest three kept). That covers hand installs, a hook from a running session that opens the store
first after a swap, and the update alike, and replaces the hand-made `state.db.pre-*` copies.

### What starts it

| Trigger | When | Needs |
|---|---|---|
| The tick | every `[update] poll_min` minutes (default 15) it asks GitHub; when a newer release is there and the decision says go, it starts `headwork update` detached and returns | the tick scheduled (`tick --install` after the user's `Allow`); not scheduled on this machine today |
| A task of the project closes | after the `Cleaned` transition in `finish` of any task of `[update] project`, `finish` starts `headwork update --wait 30` detached (`spawn_detached`) | nothing new; works without the tick |
| By hand | `headwork update`, or the orchestrator's MCP tool `update` | nothing |

With releases the tick is the natural trigger, and every machine, the Mac too, updates by it. Until the user schedules
it, the close trigger covers merges made by headwork's own workers. The tick never downloads in its own process. The
worker no longer builds or installs after `ship`. Autoupdate is not an enforcement action, so it runs under `[update]
enabled` alone, whatever `[hooks] enforce` says.

### Configuration

```toml
[update]
enabled = false
project = "headwork"
poll_min = 15
```

`enabled` is off until the user turns it on. `project` names the configured project whose main checkout carries the
plugin and whose `allowed_repos` entry is the repository releases come from. `poll_min = 0` turns the tick trigger off.

`Config` denies unknown fields, so a build older than this section refuses a config that has it. `[update]` is added
to the live config only after a build that knows it is installed, and `--to` refuses a release older than the first
one that knows `[update]` while the section is there.

The project flag `install = true` keeps its one meaning, a guard grant: its workers may run `headwork install` to try a
build before it merges.

### The guard

`update` joins `ADMIN_COMMANDS` (`core/src/guard/mod.rs`), lifted like `install` for workers of an `install = true`
project for plain `update` only; `--to` stays the orchestrator's and the user's. The orchestrator's MCP tool `update`
starts the same detached command. This goes into `docs/decisions.md` with the change.

### Decisions in `core`

`core/src/update.rs`, pure and table-tested: `decide(&UpdateFacts) -> UpdateStep`. `UpdateFacts` holds enabled, the
installed build (release version, or `dev` with its install time), the newest release (tag, publish time, schema), the
last failure (tag, kind, attempts, time), park or resume running, the disk level and the last poll time.
`UpdateStep` is `UpToDate`, `Skip(SkipReason)`, `Wait { until }` or `Install { tag }`. The runner in
`headwork/src/ops/update.rs` gathers facts and carries out the step, like the tick and `clean`.

Failures are of two kinds. Deterministic (hash mismatch, smoke test, install check) latch on the release: it is not
tried again here until a newer one. Transient (network, GitHub's rate limit, a timeout, disk) never latch: they wait 5,
15, then 60 minutes and retry.

### State

No migration (a migration would itself break pinned launchers, decisions 2026-11-14). The working memory is
`<home>/update.json`, written atomically: last poll, `ETag`, last failure. Findings the user must see go through the
tick's `findings` table (`raise_finding`), which already deduplicates them. Events keep the history: `update-started`,
`update-skipped` (only for a reason worth reading), `update-failed`, `update-installed`, `checkout-not-forwarded`.
Downloads older than the newest three releases are deleted.

### Reporting

The binary itself cannot push to the phone: `[machine] push = "claude"` means the agent's own `PushNotification`.
So:

- **Failure:** `update-failed`, a host notification (`host.notify`, as the tick does), a standing finding
  `update-failed` until a later release installs, the `doctor` check `update` as a warning, and one line in the
  orchestrator's next prompt, so the orchestrator pushes it. The old build keeps running; nothing was installed.
- **Transient failures** say nothing until the third attempt fails.
- **A failed release run on GitHub** is GitHub's to report (its e-mail on a failed workflow); the machines just see
  no new release.
- **Success:** `update-installed`, and a line in the orchestrator's next prompt only when plugin files changed
  ("sessions started before <tag> keep the old hooks until park and resume") or a migration ran.
- `headwork doctor`, new check `update`: enabled or not, the installed version, the newest release, the last result.

### Windows and macOS

- Windows: a file `curl` writes carries no Mark of the Web, so SmartScreen does not stop it. Whether a process the
  scheduled tick starts with `spawn_detached` survives the tick's end depends on Task Scheduler allowing breakaway
  from its job, which `spawn_detached` asks for and silently drops when refused; it is tried before the tick trigger is
  relied on, and `update-started` records whether the breakaway held.
- macOS: `curl` sets no quarantine attribute, and the arm64 linker signs the binary ad hoc, which is all an
  unquarantined binary needs. The launch agent's `PATH` (`TICK_PROGRAMS` in `ops/launchd.rs`) already reaches `curl`
  in `/usr/bin`; `git` stays for the checkout step.

## Before the repository goes public

A scan of the whole history (191 commits on all branches, 72 pull requests, 1 issue; `gitleaks` plus pattern
searches) on 2026-11-22 found **no secrets**: no tokens, keys, passwords or credential files, ever. What would become
public, for the user to decide on:

| What | Where | Options |
|---|---|---|
| No `LICENSE` | repository, `Cargo.toml` | add one (public without one means all rights reserved) |
| Author e-mail addresses | 84 commits with the personal Gmail address, 38 with a work address, 69 GitHub noreply | accept, or rewrite authors with a mailmap (`git filter-repo`), which changes every SHA and breaks links to old commits |
| `Claude-Session:` trailers | 9 commit messages (from before the attribution hook) | accept, or remove in the same rewrite |
| Machine paths | `S:/Code`, `C:/Users/User` in docs, tests, fixtures | low risk; accept |
| The owner's private project names and setup | `tests/fixtures/` (me-sim, me-studio), `CLAUDE.md`, `docs/`, `rules/` | accept, or genericise the fixtures going forward (history keeps them) |
| Raw trial artefacts | `docs/trials/b-hooks/proj/logs/` (hook payloads with session ids and transcript paths), `docs/trials/a-resume/reboot-before/` (pane listings, disk state) | delete from the tree going forward, or from history in the rewrite |
| Files copied from cmux | `docs/trials/c-cmux/` (12 files, a 1.6 MB `tree.txt`) | check cmux's licence, else delete (tree or history) |

The user chose (2026-11-22): rewrite the history before going public, and license the code under MIT. The rewrite
(`git filter-repo`) maps both e-mail addresses to the GitHub noreply address, strips the `Claude-Session:` trailers, and
drops `docs/trials/b-hooks/proj/logs/`, `docs/trials/a-resume/reboot-before/` and `docs/trials/c-cmux/` from history.
It runs from a backup clone kept aside, its force push waits for the user's `Allow`, it happens when no headwork worker
has a branch open, and every worktree and clone (this machine and the Mac) is re-cloned afterwards. Going public is the
user's step in the repository settings, after the rewrite.

## Conventions that change, together with turning it on

- Workers stop building and installing after a merge; the orchestrator's briefs and notes that ask for `cargo build`
  plus `install` change in the same step as `enabled = true`.
- `CLAUDE.md` "Update the installed binary after a merge" becomes "every merge is released by Actions and installed by
  `headwork update` (tick, close, or by hand)"; "There is no CI on GitHub" changes with the workflow.

## Steps

Each a pull request through `scripts/check.sh`:

1. **Store backup before migrating** (`Store::open`). Useful on its own. Built: `crates/store/src/backup.rs`.
2. **Version in the binary** (`option_env!` for `HEADWORK_VERSION` and `HEADWORK_COMMIT`, the schema in `--version`) and
   `doctor` showing it. Built: `crates/headwork/src/version.rs`.
3. **Going public:** a `LICENSE` (MIT) and `license = "MIT"` in `Cargo.toml`, the history rewrite above, then the user
   changes the visibility.
4. **The release workflow** (and the read-only pull request gate); the first release proves it.
5. **`headwork update`:** `[update]` config, `core/src/update.rs` with table tests, `ops/update.rs` (lock and pending,
   GitHub query, download and hash, smoke test, install, `--after`, checkout fast-forward, `--wait`, `--to`),
   `update.json`, events, the `doctor` check, `install` waiting for the lock. CLI tests on a temporary home with a fake
   download seam and fake releases.
6. **Turn it on:** the guard lift, the close trigger, the MCP tool, the convention change, then `[update]` in the live
   config with `enabled = true`, after the user's go; the tick trigger once the user schedules the tick (after the
   breakaway trial).

Steps 1 and 2 need nothing from the user and can start at once.

## Open questions for the user

- Whether to schedule the tick (`headwork tick --install`), which makes updates reach every machine within 15 minutes
  of a release.
