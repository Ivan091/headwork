# Config sync: one config shared through GitHub, overridden per machine

Approved by the user on 2026-11-25 with every recommendation in the decision table (section 8).

## Today

- Each machine reads one file, `~/.headwork/config.toml` (`Home::load_config`, `crates/headwork/src/home.rs:147`), plus each project's `.headwork.toml` overlay.
- The Windows file has 145 lines. About half of it is machine-specific: `[machine]` (host, shell, wmux), all `S:/Code` and `C:/Users/User` paths (`[hooks] orchestrator_writable`, `orchestrator_repos`, `brief_writable`, `[watch] roots`, every `project.*.main`, `read_only_paths`, `extra_writable`). The rest is shared: `[models]`, `[usage.prices]`, `enforce_rules`, `[layout]`, `[leads]`, `[update]`, `[reviews]`, the watch thresholds, and each project's policy (`deliver`, `main_only`, `user_only`, `jobs`, `allowed_repos`, `long_jobs`; `heavy_*` is deprecated and not enforced, `docs/decisions/2026-11-25-heavy-keys-deprecated.md`).
- The Mac gets its config by hand (`briefs/mac-adopt-headwork-flow.md`, plus a pasted prompt for the prices).
- `[machine] notify_ntfy` is a secret: anyone who knows the topic URL can read and send the notifications.

## The design in one paragraph

The shared and per-machine sources live in the private `claude_config` repository, next to the machine profiles it already has (`machines/windows/settings.json`, `machine.md`). headwork keeps reading only `~/.headwork/config.toml`, but that file is now **generated**. A new command, `headwork config sync`, pulls the repository, merges shared + machine + local, validates the result and replaces the file atomically. A broken or conflicting source makes the sync fail; the file the machine runs on stays as it was. Hooks, the tick and the MCP server never read the sources, so a bad pull cannot break a running machine.

## 1. Where it lives

| Option | For | Against |
|---|---|---|
| **claude_config** (`Ivan091/claude_config`, private) | Already cloned on every machine that runs Claude. Already has `machines/<machine>/` profiles and the "versioned copy → install → live file" flow (`settings.json`, `machine.md`). It is a headwork project, with `sync-config` for delivery. | Mixes the headwork config into the Claude config repository. |
| code-home (`Ivan091/code-home`, private) | Holds the briefs, notes and to-do list for these projects. | Its `.gitignore` allowlist and its layout are built around `S:/Code` on Windows. Only the orchestrator writes there. The Mac may not clone it. |
| A new `headwork-config` repository | Clean separation. | One more clone, SSH key setup and delivery flow on each machine, for about 150 lines of TOML. |

**Recommendation: claude_config.** Layout:

```
~/.claude/                          (claude_config)
  headwork/config.toml              shared: models, prices, rules, reviews, layout, leads, update, project policy
  machines/windows/headwork.toml    PC (optional, see "Machine part on the machine"): [machine], paths, project mains, writable lists, watch roots
  machines/mac/headwork.toml        Mac: the same, with cmux and Mac paths
~/.headwork/                        (never committed)
  sources.toml                      which repository and which machine this is (written once)
  config.machine.toml               the machine part kept on the machine; replaces machines/<name>/headwork.toml when present
  config.local.toml                 secrets and one-off local tweaks (notify_ntfy), never synced
  config.toml                       GENERATED effective config; what every headwork command reads
  config.toml.prev                  the previous effective config (one-step rollback)
  config.origin.json                where each effective key came from
```

**Never committed:** `notify_ntfy` and any other token or topic URL, `state.db`, briefs, recaps, jobs, `config.local.toml`, `sources.toml`. Validation **refuses** a shared or machine file that sets `machine.notify_ntfy`, or any key on a short `secret_keys` list in `core`, so a secret cannot reach the repository by accident.

### Machine part on the machine

The machine part holds paths, project mains and hostnames, which may be internal to an employer and must not reach a
repository that is public-facing. `headwork config adopt` therefore writes it to `<home>/config.machine.toml`, never
into the repository, unless `--machine-in-repo` is given or the repository already has `machines/<name>/headwork.toml`
(that file is then used as it is, as before). When `config.machine.toml` exists it takes the machine layer's place; the
merge, origins (`machine`), host check and `unset` rules are the same. It is checked like the tracked layers, so it cannot hold
`machine.notify_ntfy`: that goes in `config.local.toml`. See `docs/decisions/2026-11-26-machine-part-stays-local.md`.

## 2. Merge rule

The order is shared → the machine file (`~/.headwork/config.machine.toml` when it exists, else `machines/<name>/headwork.toml`) → `config.local.toml`. A later layer wins.

| Case | Rule |
|---|---|
| Table (`[hooks]`, `[project.me-sim]`, `[usage.prices."x"]`) | Merged key by key, recursively. A machine file sets only what differs. |
| Scalar | Replaced. |
| Array (`main_only`, `enforce_rules`, `orchestrator_writable`) | **Replaced as a whole.** No implicit append: what you read in one file is the whole value. |
| Appending to a shared array | Explicit: `extend.hooks.enforce_rules = ["x"]` in the machine file. It is rarely needed, so this is optional for v1. |
| Removing a key or table | `unset = ["project.me-sim-site", "hooks.enforce_rules"]` at the top of the machine or local file. Unsetting a key that no lower layer sets is an error, which catches typos. |
| Projects | Shared holds a project's policy. A project **exists on a machine only when that machine's file gives it a `main`**, so a project the Mac does not have needs no `unset`. A shared project with no `main` is dropped, and the drop is listed in the sync output. |
| Paths | There is no `{code}`-style variable in v1. Keys that hold paths simply live in the machine file. Validation refuses a shared file that sets a machine-only key (`machine.*`, `project.*.main`, `read_only_paths`, `extra_writable`, `hooks.orchestrator_writable`, `orchestrator_repos`, `brief_writable`, `watch.roots`). That keeps the shared file portable. |

The merge works on `toml::Value`, as a pure function in `core` (`core/src/config_layers.rs`) that returns the merged value and a per-key origin map. `Config::parse` (with `deny_unknown_fields`) then checks the result exactly as today.

## 3. How a machine knows which it is

`~/.headwork/sources.toml`, written once during adoption:

```toml
repo = "~/.claude"
machine = "windows"
```

Hostnames are not used: they change, macOS rewrites them, and they say nothing about intent. This is a cross-check, not a guess: the merged `machine.host` must suit the OS (wmux on Windows, cmux on macOS). Otherwise the sync fails, so `windows.toml` can never be applied on the Mac. Without `sources.toml`, headwork behaves exactly as today: a hand-written `config.toml`, no sync.

## 4. How it syncs

| Command | Does |
|---|---|
| `headwork config sync [--no-pull] [--dry-run]` | `git fetch`, then `git merge --ff-only @{upstream}` in the repository (SSH, batch mode, as `clean` does; unrelated unstaged changes, such as wmux's edits to `CLAUDE.md`, never stop it, and a merge that would overwrite a local change is refused with git's message, never stashed; a shared or machine file whose only local difference is its line endings is first checked out from `HEAD`, so a CRLF checkout never blocks it), merge, validate, then write `config.toml` atomically (temp file + rename) and keep `config.toml.prev` and `config.origin.json`. Prints a diff against the old effective file. Logs `config-synced` with the repository commit. |
| `headwork config show [--origin] [--json]` | The effective config, each key tagged `shared` / `windows` / `local` / `default`. |
| `headwork config check` | Merge and validate without writing. Its exit code means pass or fail, so CI or a session can run it before a PR. |
| `headwork config rollback` | Puts `config.toml.prev` back. |
| `headwork config adopt --machine <name>` | One-time migration: splits today's live `config.toml` into shared / machine / local by the machine-only and secret key lists, writes the shared file into the repository for review, the machine part to `~/.headwork/config.machine.toml` (into `machines/<name>/headwork.toml` only with `--machine-in-repo`, which refuses while a local machine file would hide it), the secrets to `config.local.toml`, and `sources.toml`. A shared or machine file that already exists in the repository (a second machine joins; its machine file came through a pull request) is used as it is and never written: the merge of the existing files with the split local part must still reproduce the live config, else nothing is written. `--force` replaces them. |

Triggers:

- **By hand or by the orchestrator** (a new MCP tool, `config_sync`): the main path.
- **`update` and `install`** run `config sync --no-pull` after installing, because a new build may know new keys or new rules. On a failure they keep the old file and report it.
- **The tick does not pull or write.** It only compares the sources' last commit and the merge result with the effective file, and raises a standing finding `config-stale` (the sources changed since the last sync) or `config-drift` (`config.toml` was hand-edited), shown on the board like the other standing findings. No network, no surprise rewrites.
- `sync-config` (the claude_config delivery skill) ends with `headwork config sync` when it shipped a change under `headwork/` or `machines/*/headwork.toml`. A machine with `config.machine.toml` edits that file instead and runs `headwork config sync --no-pull`.

## 5. Failure handling

- The runtime reads only the generated file. A source that fails to parse, an `unset` typo, a machine-only key in the shared file, a secret in a tracked file, a host that does not suit the OS, a duplicate key, a failed pull or a merge conflict in the repository: each fails the sync with the file and key named. `config.toml` is left untouched and the event `config-sync-failed` is logged.
- The candidate is validated the way `update` smoke-tests a build: `Config::parse`, the project-overlay load, and `doctor --json` run on a scratch home holding the candidate. The new file is written only when doctor finds no new failure.
- Sources are read and written with LF line endings only (`adopt --write` writes LF; reads ignore a CR before a line break), so `core.autocrlf` cannot change what a sync builds.
- A pull that is not a fast-forward (local edits in the repository) stops the sync. It never resets or merges.
- `config rollback` gets back to the previous file in one step.

## 6. doctor checks

| Check | Fails / warns when |
|---|---|
| `config-sources` | ok "not synced (hand-written config)" without `sources.toml`; fails when it names a missing repository or machine file (the message names both `~/.headwork/config.machine.toml` and the repository's file when neither exists). |
| `config-machine-shadow` | Both machine files exist, so the local one hides the repository's (warn). |
| `config-stale` | The repository HEAD or the merge result differs from what the last sync recorded (warn: run `headwork config sync`). |
| `config-drift` | `config.toml` differs from the recorded sync output: a hand edit that the next sync would overwrite (warn, naming the keys). |
| `config-secret` | A tracked source holds a secret key (fail). |
| `config-host` | The effective `machine.host` does not suit the OS (fail). |

## 7. What changes elsewhere

- **The Mac adoption brief** (`briefs/mac-adopt-headwork-flow.md`) shrinks to: pull claude_config, `headwork config adopt --machine mac` (which writes the machine part to `~/.headwork/config.machine.toml`, never into the repository), review the shared file, ship it through `sync-config`, then `headwork config sync`. The pasted prices prompt goes away, because the prices are shared.
- **The orchestrator's rules** (`rules/orchestrator.md`): a config change is a claude-config task that edits `headwork/config.toml` or `machines/<m>/headwork.toml` in a PR (machine-only keys of a machine that has `config.machine.toml` are edited there instead, never committed). After the merge, `headwork config sync` runs on each machine (the orchestrator's tool here, a line for the user on the other machine). Never edit `~/.headwork/config.toml` by hand. `orchestrator_writable` may drop `~/.headwork/config.toml` if it is there.
- **The workers' rules** stay as they are (the rules are near the hook's size limit, and a worker cannot write `~/.headwork` anyway).
- **`headwork import`** still writes `config.toml`; on a synced machine move its additions into the sources (a later step may write them there directly).
- **claude_config**: add `headwork/`, add `machines/mac/` (so far only `machines/windows/` exists), and `.gitignore` allowlist entries. `install.sh` needs no change: headwork's own sync does the copy, so the merge logic lives in one place.
- **Docs**: a new `docs/config-sync.md`, one line per choice in `docs/decisions.md`, and the CLAUDE.md summary.

## 8. Decision table

| # | Choice | Options | Recommendation |
|---|---|---|---|
| 1 | Repository | claude_config / code-home / new repository | **claude_config**: already on both machines, already has `machines/<m>/` profiles |
| 2 | Layout | one file with `[machine.windows]` sections / shared + `machines/<m>/headwork.toml` | **Separate files**, beside the existing machine profiles |
| 3 | Runtime | headwork reads the layers on every call / a generated `config.toml` | **Generated file**: a bad pull never breaks hooks or the tick |
| 4 | Arrays | replace / append / replace with explicit `extend` | **Replace**, with `extend.*` optional later |
| 5 | Removal | `unset = [...]` / magic value / none | **`unset` list**, an error when it matches nothing |
| 6 | Projects per machine | everything shared, `unset` the rest / a project exists where its `main` is given | **Exists where `main` is given** |
| 7 | Path variables (`{code}`) | yes / no | **No in v1**: path keys stay machine-only, enforced by validation |
| 8 | Machine identity | `[machine]` name / hostname / `sources.toml` | **`sources.toml`**, cross-checked against the OS through `machine.host` |
| 9 | Secrets | in the repository / `config.local.toml` | **`config.local.toml`**, never synced; validation refuses secrets in tracked files |
| 10 | Sync trigger | manual / install+update / tick auto-apply | **Manual `config sync` + `--no-pull` after `update`/`install`**; the tick only reports `config-stale` / `config-drift` |
| 11 | Validation | parse only / parse + doctor on a scratch home | **Parse + doctor on a scratch home**, as the update smoke test does |
| 12 | Migration | write the files by hand / `config adopt` | **`config adopt`** on each machine, output reviewed in a PR |

## 9. The pieces in the code

- `core/src/config_layers.rs` (pure): `merge(shared, machine, local)` over `toml::Table`s with `unset` and `extend`,
  the machine-only and secret key lists, the per-key origins, the dropped projects. `Config::parse` then checks the
  merged text as before.
- `headwork/src/ops/config_sync.rs`: `sources.toml`, the pull, the scratch-home doctor run, the atomic write with
  `config.toml.prev` and `config.origin.json`, rollback, adopt, show and check.
- `doctor` checks `config-sources`, `config-stale`, `config-drift`, `config-secret`, `config-host`; tick findings
  `config-stale` and `config-drift`; `update` and `install` run the sync with `--no-pull`.
