# Migration notes for every machine

Status: the headwork part is built (section 10); the other repositories follow in the tasks of section 8.

Goal: when a change on one machine needs a step on the others (the Mac, a second PC), the change carries a
machine-readable note, and headwork on every machine finds the notes it has not applied, runs the safe steps, and asks the
user for the rest. The hand-written `briefs/mac-adopt-headwork-flow.md` and pasted prompts go away.

## 1. Repositories and per-machine things in scope

| Repository | Runs or is read on other machines | Per-machine state that can need a step |
|---|---|---|
| headwork | yes (binary, plugin) | `~/.headwork` (config, store, `sources.toml`), the tick task (schtasks / launchd), the `headwork resume` logon task, the plugin entry in `settings.json`, the git hook path |
| claude_config (`~/.claude`) | yes | `install.sh` profile (sparse checkout, hooks path), live `settings.json` and `machine.md`, `machines/<m>/headwork.toml`, scheduled Claude tasks, the Box Mac's separate `claude-config-box` |
| headwork shared config (`headwork/config.toml` in claude_config) | yes | covered by `headwork config sync`; only machine-only keys need a step |
| me-studio | the machine that publishes | `%LOCALAPPDATA%\me-studio` (binary from `deploy.sh`, ledger, logs), schtasks "me-studio dashboard", hourly release task, Credential Manager logins |
| me-sim | any machine that renders or trains | `~/.config/me-sim`, `C:/Users/User/me-sim-night/`, `S:/Youtube/` media, toolchains |
| code-home (`S:/Code`) | read by orchestrators | `core.hooksPath hooks`; notes only |
| me-sim-site | no (GitHub Pages) | none: exempt |

Every repository also has the `_local/` rule from `AGENTS.md`; today `scripts/migrate_local.py` is the one migration
mechanism that exists, and it becomes one kind of step.

## 2. What counts as a change that needs a note

A note is needed when a machine that only runs `git pull` (and headwork's autoupdate) would end up broken or different.

| Change | Note? | Step kind |
|---|---|---|
| Code, docs, tests, skills, `AGENTS.md`, rules | no | pull is enough |
| headwork store migration | no | automatic, backed up (`backups/state-*.db`) |
| A store migration that needs a data fix by hand | yes | agent |
| Shared headwork config key added or changed | no | `config sync` carries it |
| Machine-only config key added, renamed, required | yes | confirm (edit `~/.headwork/config.machine.toml`, or `machines/<m>/headwork.toml` by PR on a machine that has no local file) |
| Config key removed or renamed with the old one still read | no | `doctor` `config-keys` warns |
| Plugin files (`hooks.json`, `.mcp.json`) | yes | agent (restart sessions; park and resume) |
| Ignored files move (`_local/`) | yes | agent (`scripts/migrate_local.py`) |
| New tool or dependency (brew, cargo tool, node, Python package) | yes | confirm |
| New or changed secret, login, token | yes | user |
| Scheduled task created, changed, removed (schtasks, launchd, Claude scheduled task) | yes | user (Allow ask naming the command) |
| `machines/<m>/settings.json` changed | yes, for that machine | user (`install.sh machines/<m> --settings`) |
| `install.sh` or sparse profile changed | yes | agent |
| Rewritten history, force-pushed default branch | yes | confirm (backup branch, then reset) |
| Binary deployed outside the repo (me-studio `deploy.sh`) | yes, machines that run it | agent |
| Ledger, media or data location moved | yes, machines that hold it | confirm |

## 3. Where the notes live on GitHub

| Option | Machine-readable | Travels with the change | Verdict |
|---|---|---|---|
| **`migrations/` folder per repository on GitHub, one TOML file per note** | yes | yes: merged with the change that needs it | **recommended: the source of truth, read from GitHub's default branch** |
| Release notes | partly | headwork only (others have no releases) | **generated from the folder** for headwork: `release.yml` adds a "Migration notes" section listing notes added since the last tag |
| PR description | no | yes | **a pointer line**: `Migration: migrations/<id>.toml` or `Migration: none (<reason>)` |
| `MIGRATIONS.md` single file | weak | yes | no: merge conflicts, hard to mark per note |
| Commit trailer | weak | yes | no: the `commit` skill keeps commits single-line |
| GitHub issue label | no | no | optional: ship scripts add label `migration` so `gh pr list -l migration` finds them; not relied on |
| Central index repository | yes | no (drifts) | no: headwork builds the index on the fly from every configured repository |

Note format (`migrations/2026-11-25-config-sync-adopt.toml`):

```toml
id = "2026-11-25-config-sync-adopt"
title = "Take headwork's config from claude_config"
since = "v0.1.55"
os = ["windows", "macos"]
machines = []
requires = ["headwork"]
why = "Config moved to shared layers (docs/config-sync.md)."
check = "headwork config check"

[[step]]
kind = "agent"
run = "headwork config adopt --machine {machine} --repo ~/.claude"

[[step]]
kind = "confirm"
run = "headwork config adopt --machine {machine} --repo ~/.claude --write"
backup = "cp ~/.headwork/config.toml ~/.headwork/config.toml.pre-adopt"

[[step]]
kind = "user"
say = "Run `headwork tick --install` after the Allow ask."
```

Fields: `id` (the file name without `.toml`), `title`, `why`; optional `since` (a release tag or a date, for the reader
only: headwork does not compare it with the installed release), `os` (`windows`, `macos`, `linux`; empty means all),
`machines` (names from `sources.toml`; empty means every machine), `requires` (projects whose main checkout the machine
must have), `check` (a command that exits 0 when the note is already applied, so a fresh machine skips it; only `apply`
runs it) and `pull` (default `true`; `false` skips the implicit `git pull --ff-only` of section 4, for a note that
repairs a checkout a fast-forward cannot reach). A `[[step]]` is `agent` (idempotent, runs without asking), `confirm` (asks the
user first, with an optional `backup` that runs before it) or `user` (the user does it). `agent` and `confirm` steps need
`run`, `user` steps need `say`; `{machine}` in a step is replaced by the machine's name.

## 4. How a machine finds what it has not applied

- `headwork migrate` (new command, also MCP tools for the orchestrator): `list` (pending, applied, skipped,
  not applicable, invalid), `show <id>`, `apply <id>` (runs agent steps; stops at the first confirm or user step and
  prints the ask), `mark <id> applied|skipped --reason`, `check [<path>]`.
- Source of truth is GitHub: the notes are read from each repository's default branch on GitHub, never from a
  local checkout. `headwork migrate` runs `git fetch` (SSH, batch mode, as `clean` does) in the local clone and reads
  `origin/<default>:migrations/` with `git show`, so a machine sees a new note before it pulls and whatever its
  checkout is on. Repositories: every configured project, the claude_config repo from `sources.toml`,
  `orchestrator_repos` (code-home). headwork's own notes come from `Ivan091/headwork` the same way. A note's `since` is documentation only:
  headwork never compares it with the installed release, so a note can be pending before `update` installs the change
  it needs; the note's `check` decides, and only `apply` runs it.
- A note whose commit is not in the local `HEAD` yet gets an implicit first agent step: `git pull --ff-only` in that
  checkout (a confirm step when the checkout is dirty or not on the default branch; once confirmed it first checks out
  the default branch when the tree is clean, and refuses when it is not); `pull = false` in a note skips it. Filters
  (`os`, `machines`, `requires`) decide where a note applies.
- The applied record is per machine and local: a store table (new migration: `migrations_applied` with repo, id,
  state, who, when, output) and events `migration-applied` / `migration-skipped`. A note whose `check` passes is
  recorded as applied without running, so a fresh machine or a lost store rebuilds its record from the checks.
- Pending ignores `check` on purpose: `doctor` and the tick never run a note's command, because a check may be slow,
  need the network or have side effects, and the tick only reads. A note whose `check` would pass stays pending until
  `apply` runs it, which records it applied without running a step; `mark` does the same by hand.
- Surfacing: `doctor` check `migrations-pending` (warn); tick finding `migration-pending` (reports only, never runs
  steps, like `config-stale`; a repository that cannot be fetched or read is a `tick-error` finding with the reason,
  and one whose notes cannot be read keeps its standing findings); `update` and `config sync` leave a line in `update.json` notices, so the orchestrator's
  next prompt says "2 migration notes pending: ...".
- Who applies them: the orchestrator spawns one main-folder task per repository (`<project>-migrate`), which runs
  `headwork migrate apply`, asks the user for each confirm and user step, and reports. Without headwork (a bare new
  machine) `docs/other-machine.md` stays the bootstrap; its last step is `headwork migrate list`.

## 5. Steps that need the user

| Kind | Who runs it | How |
|---|---|---|
| agent | the agent | without asking; must be idempotent |
| confirm | the agent after the user's go | `AskUserQuestion` headed `Allow` naming the exact command, backup first and named in the report |
| user | the user | the agent asks with the command or action; marks the note applied only on the user's answer |

The validator refuses a note that puts a login, secret, `schtasks`, `launchctl`, `sudo`, `tick --install`,
`--settings`, `push --force` or `reset --hard` in an `agent` step.

## 6. Enforcing the rule from now on

| Where | What | Verdict |
|---|---|---|
| `headwork migrate check [<path>]` | validates the note files (schema, ids, step kinds, banned commands in `agent`) | **yes**, in headwork's `check.sh` and the others' ship scripts |
| PR body line `Migration:` | `create-pr`, `contribute`, `ship-pr` skills and me-sim / me-studio `ship.sh` refuse a PR without it | **yes** |
| Trigger heuristic | `scripts/migration-line.sh` fails when the PR says `none` without a reason while the change touches `plugin/`, `crates/headwork/src/ops/install.rs`, `schedule.rs`, `launchd.rs` or `.gitignore` at the root (the store's own `migrations/` is no trigger) | **yes**, in headwork's CI gate (a pull request run, also when the body is edited); locally the line is not checked |
| headwork guard rule | a worker's `done` with a merged PR whose body lacks `Migration:` | **later**: shadow first, after the line exists everywhere |
| `AGENTS.md` | the rule in one place; `migrate_local.py` becomes a step of a note | **yes** |

## 7. How this fits config sync

`config sync` stays the automatic channel for headwork config: shared changes need no note. Notes cover what sync
cannot: a machine-only key each machine must add to its own machine file (`~/.headwork/config.machine.toml`, or `machines/<m>/headwork.toml`), a first `adopt`, a new
secret in `config.local.toml`. A note's step may be `headwork config sync`. `migrate list` runs after `config sync`
and `update`, so one notice covers both.

## 8. Per repository: what changes, and the follow-up tasks

| Task | Repository | Changes | Review |
|---|---|---|---|
| `headwork-migrate` (first; the others wait for its format) | headwork | `docs/migrations.md` (format, decision table); pure model and validator in `core/src/migrate.rs`; fetch and read from GitHub's default branch; store migration for the local applied record; `headwork migrate` CLI and MCP tools; `doctor`, tick, update notices; `release.yml` "Migration notes" section; `check.sh` step and gate heuristic; `migrations/` with backfilled notes (rewritten history, config adopt) | 2 |
| `claude-config-migrations` | claude_config | `AGENTS.md` rule (section 2 table in short, the `Migration:` line); `commit`, `create-pr`, `contribute`, `ship-pr`, `sync-config` skills; `docs/other-machine.md` ends with `headwork migrate list`; `migrations/` backfilled from the Mac brief (install profile, `--settings`, mac machine file); the Box Mac's repo noted as out of scope | 1 |
| `me-studio-migrations` | me-studio | `ship.sh` `Migration:` line and `migrate check`; `migrations/`; notes for the dashboard task, hourly release, ledger location, `deploy.sh` | 1 |
| `me-sim-migrations` | me-sim | `ship.sh` the same; `migrations/`; `_local/` adoption if any ignored folders exist | 1 |
| (orchestrator, no task) | code-home | mark `briefs/mac-adopt-headwork-flow.md` superseded once the notes exist | — |

me-sim-site: exempt, no machine state.

## 9. Choices to confirm

| Choice | Recommendation | Alternative |
|---|---|---|
| Source of truth | GitHub: `migrations/*.toml` on each repository's default branch, fetched, never the local checkout | release notes or PR text only |
| Note format | TOML (headwork already parses TOML flows) | Markdown with front matter (nicer to read, harder to validate) |
| Applied record | local: the headwork store (user's choice, 2026-11-25) | `machines/<m>/migrations.toml` on GitHub |
| Agent steps | run without asking when `kind = "agent"` | ask before every step |
| Who applies | orchestrator spawns `<project>-migrate` tasks on a notice | the tick runs agent steps itself |
| Enforcement | `Migration:` line in every PR + validator + trigger heuristic in headwork's gate | line only |

## 10. What is built (headwork)

| Part | Where |
|---|---|
| Note model, validator, filters, pending computation, `{machine}` expansion | `crates/core/src/migrate.rs` (pure; table tests) |
| Applied record: table `migrations_applied` (repo, note id, state, actor, at, output, reason), events `migration-applied` and `migration-skipped` | `crates/store/migrations/0023_migrations_applied.sql`, `crates/store/src/migrations.rs` |
| Reading a repository: `git fetch` of the default branch in batch ssh mode, `git ls-tree` and `git show` on `origin/<default>:migrations/`, never the working tree | `crates/adapters/src/notes.rs` |
| Sources, list, apply, mark, check, announcements | `crates/headwork/src/ops/migrate.rs` |
| Surfacing | `doctor` check `migrations-pending` (no fetch), tick stage `migrations` with the standing finding `migration-pending` (fetches, runs nothing), `update.json` notices after `update` and `config sync` (each note told once) |
| MCP | `migrate_list` (orchestrator and workers), `migrate_mark` (orchestrator) |
| Gate | `scripts/check.sh` step "migration notes": `migrate check .`, and on a pull request the `Migration:` line (`scripts/migration-line.sh`, `.github/pull_request_template.md`) |
| Release | `scripts/release-migration-notes.sh`, called by `release.yml`, appends a "Migration notes" section |
| Backfilled notes | `migrations/2026-11-22-public-history.toml`, `migrations/2026-11-25-config-sync-adopt.toml` (from `briefs/mac-adopt-headwork-flow.md`) |

Commands:

```bash
headwork migrate list [--json] [--no-fetch]
headwork migrate show <note>
headwork migrate apply <note> [--from N --confirmed] [--machine NAME] [--json]
headwork migrate mark <note> applied|skipped --reason "..."
headwork migrate check [<path>] [--json]
```

A note is addressed as `<repo>/<id>`; a bare id works while only one repository has it. Repository keys: a project's name;
`claude-config` for the `sources.toml` repository; the lower-case folder name of each `orchestrator_repos` entry. A
folder already listed is skipped. When a key is already taken by another folder it gets a suffix, in this order of
sources: `-config` for the config repository, `-notes` for an orchestrator repository, then `-2`, `-3`, so one key never
names two repositories (a project named `notes` and an orchestrator folder `Notes` give `notes` and `notes-notes`).

`apply` fetches first and refuses to run when the note's repository failed to fetch or read (an applied note must come from
the newest notes; `--from` resumes from the refs already fetched). It exits 1 and prints the problems for a note that is
not valid, and for `--from` past the last step; a note that does not apply on this machine exits 0 as not applicable.
It runs the note's `check` first; exit 0 records the note as applied without running a step. Otherwise it runs the
`agent` steps in order and stops at the first `confirm` step (exit 3) or `user` step (exit 4), printing what to ask and
the line that goes on once the user agreed: `headwork migrate apply <note> --from <step> --confirmed` (a confirm step runs
its `backup`, then `run`; a user step counts as done). Exit 1 means a step failed (or ran past ten minutes) and nothing is recorded; the output of the earlier steps and of the
failing one is printed; when every step
ran the note is recorded as applied. When the note's file is not in the checkout's `HEAD` yet, `apply` first runs
`git pull --ff-only` there (step 0): by itself on a clean checkout of the default branch, else it stops with a confirm
ask. A note with `pull = false` skips step 0.

`migrate show` prints the steps as they would run, with `{machine}` left in place when the machine's name is unknown, and
how a settled note was settled. `migrate check` validates `<path>/migrations/*.toml` (the extension in any case) in the
working tree, with the same parser and validator as the list: ids match file names and are unique, `agent`
steps and `confirm` steps carry `run`, `user` steps carry `say`, and an `agent` step holds none of `login`, `secret`,
`token`, `schtasks`, `launchctl`, `sudo`, `tick --install`, `tick --uninstall`, `--settings`, `push --force`, `push -f`,
`reset --hard`, `rm -r`.

The `Migration:` line of a pull request is either `Migration: migrations/<id>.toml` (the file must exist) or
`Migration: none`; `none` is a whole word, optionally followed by a reason in parentheses (anything else, such as the
placeholder of the pull request template, fails), and needs the reason when the change touches `plugin/`,
`crates/headwork/src/ops/install.rs`, `schedule.rs`, `launchd.rs` or `.gitignore`.
