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
  headwork/             the binary: CLI (board, log)
tests/fixtures/         config fixtures: me-sim, me-studio and a generic work repository
scripts/check.sh        the quality gate
docs/                   design, research, trials, decisions
```

Later milestones add `crates/adapters` (hosts, git, runtime, processes) and `plugin/` (hooks, MCP registration).

## Commands

```bash
bash scripts/check.sh                      # the full gate; must pass before every pull request
cargo test -p headwork-core                # one crate
cargo run -p headwork -- board             # projects, tasks, sessions, states, asks
cargo run -p headwork -- log [<name>]      # the event log, all or one task
```

The binary reads `$HEADWORK_HOME` (default `~/.headwork`): `config.toml` and `state.db`. Build output stays in the
repository's own `target/` on S:; C: has little free space.

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
