# Delegation: how it works now, what it costs, what to change

Analysis of 2026-11-23, from `usage` rows since 2026-11-09 (27k responses, about $1,984 at API list prices; an
estimate, never a bill). Question: should the orchestrator, leads and workers hand more of their work to other agents,
and would that save money?

## How delegation works today

| Who | What the rules say | What happens |
|---|---|---|
| Orchestrator | Never does project work; every request becomes a task (`rules/orchestrator.md`) | Already a pure dispatcher. Its own main session is 6 % of spend |
| Lead | "Do the work yourself, with `Agent` subagents"; a part "only when a piece truly needs one" (`rules/lead.md`) | Leads do most of the work in their own context. 4 parts were ever spawned (3 Sonnet, 1 Haiku) out of 69 leads |
| Worker | "Split it, do the parts in your own worktree with `Agent` subagents" (`rules/worker.md`) | Same: the main session does the reading, editing and testing |
| Subagents | `CLAUDE_CODE_SUBAGENT_MODEL=sonnet` in settings | Overridden almost every time: 316 `general-purpose` subagents were started with `model: opus`, 40 with Sonnet. `fork` subagents always run on the parent's model (Opus) |
| Review flows | `flows/code.toml` names a model per node | `verify` (one subagent per finding) and `design`, `challenge-*` are Opus at every level: 217 Opus subagents under review parts |

Nothing in code pushes toward delegation; it is rules text only, and the text leans the other way ("do the work
yourself", "only when truly needs one").

## Where the money goes

| Slice | Responses | Cost | Avg context | $/response |
|---|---:|---:|---:|---:|
| Worker main session, Opus | 6,988 | $492 (25 %) | 188k | 0.070 |
| Unmanaged main, Opus | 3,860 | $303 (15 %) | 217k | 0.079 |
| Lead main session, Opus | 3,509 | $257 (13 %) | 200k | 0.073 |
| Worker subagents, Opus | 1,428 | $200 (10 %) | 276k | 0.140 |
| Orchestrator main, Opus | 1,614 | $125 (6 %) | 162k | 0.077 |
| Lead subagents, Opus | 1,465 | $85 (4 %) | 138k | 0.058 |
| Part (review) subagents, Opus | 1,269 | $84 (4 %) | 66k | 0.066 |
| Worker main session, Sonnet | 1,724 | $46 (2 %) | 99k | 0.026 |

- Main sessions of leads and workers on Opus are 38 % of all spend; add unmanaged sessions and it is over half.
- Cost per response is driven by context size, not by output: of the worker main sessions' $492, $263 is cache reads,
  $149 cache writes and $81 output. Every turn re-reads the whole conversation.
- Context grows with the session: median 115k in the first 50 turns, 195k by turn 50-99, 316k by 150-199, 408k by
  250-299. Every file read, build log and test output stays in the context and is paid for again on every later turn
  (a 10k-token read early in a 100-turn Opus session costs about $0.08 to write plus $0.20 in re-reads).
- A Sonnet response is 2.7x cheaper than an Opus one at the same context (0.026 vs 0.070 here), and a subagent runs at
  a smaller context than a long main session.
- Starting a subagent is not free: its first turn already holds about 60k tokens (system prompt, tools, `CLAUDE.md`
  files; headwork's own `CLAUDE.md` is 40 kB). A new session (a part) starts at about 64k, so a part costs about the
  same to start as a subagent, plus a pane and a slot. The median subagent lives 6 responses and returns 2.5k tokens.
- `fork` subagents are expensive: 6 forks in worker sessions made 785 responses for $157, because a fork inherits the
  whole parent context and the parent's model.

## Is more delegation worth it?

Yes, for the right kind of work, and mainly because it keeps the expensive context small, with the cheaper model as a
second gain.

Worth delegating (large reads, compact result):
- exploring a codebase or a log, answering "where is X, how does Y work";
- running builds, tests and the gate and reporting only the failures;
- routine edits that follow a clear spec (a new CLI flag, a doc update, a test table);
- review and verification passes.

Not worth delegating:
- a small edit where the main session already holds the context (the subagent would pay 60k to start and re-read the
  files);
- design decisions and integration, where the summary would lose what matters;
- anything needing back-and-forth with the user.

Rough estimate for the last 14 days, if leads and workers moved 40 % of their main-session turns (the read-heavy ones)
to Sonnet subagents and their main contexts stayed 30 % smaller:

| | Now | After |
|---|---:|---:|
| Lead and worker main sessions (Opus), 10.5k turns | $749 | about $335 (6.3k turns, smaller context) |
| Moved turns on Sonnet subagents, start-up included | | about $150 |
| Subtotal | $749 | about $485 (-35 %, about -$265) |
| Subagents and forks started on Opus that could be Sonnet | $370 | about $185 (-$185) |
| Review `verify` nodes on Sonnet at level 2 | part of $84 | about -$35 |

Together about $450-500 of $1,984, roughly 20-25 % of spend, with no change to which model leads and plans. The
numbers are estimates: the 40 % and 30 % are assumptions, not measured, which is why step 5 measures them.

Risks:
- Quality: a Sonnet subagent can miss things an Opus main session would catch. Mitigation: the main session keeps
  design and the final read of the diff; Opus stays for the ladder's delicate kinds.
- Lost detail in summaries leads to rework, which costs more than it saved. Mitigation: ask subagents for concrete
  output (paths, line numbers, failing test names), not prose.
- Parallel subagents editing the same files. Mitigation: `isolation: "worktree"`, already in the rules.
- Subagent caches live 5 minutes (main sessions' 1 hour), so a slow parent pays the subagent's write again. Minor.

## Plan

1. **Rules text** (`rules/lead.md`, `rules/worker.md`, small): the lead coordinates and reviews; it hands exploration,
   builds and test runs, and routine edits to `general-purpose` subagents on Sonnet (pass `model: "sonnet"`, never
   `opus` without a reason), keeps design, integration and the final diff read, and does not use `fork` for work
   (forks inherit the whole context and model). A short "when to delegate" list from the section above. Same model
   ladder as parts.
2. **Subagent model guard** (code): the `PreToolUse` hook already sees `Agent` calls. Log `subagent-model` (shadow,
   like the other rules) when a worker, lead or part (not a review part) starts an `Agent` subagent on Opus without a
   line `Opus because: <why>` in its prompt, mirroring `part-opus-reason`. Enforceable later through `enforce_rules`.
3. **Context nudge** (code): the usage reader already knows each main session's context size. When a lead's or
   worker's last response passed `[usage] nudge_context_k` (say 200k), the `UserPromptSubmit` or `PostToolUse` hook adds
   one line: hand the next read-heavy step to a subagent, or `/compact`. Once per threshold crossed, not every turn.
4. **Review flow models** (`flows/code.toml`): `verify` on Sonnet at level 2, Opus at 3; it runs once per finding, so it
   is the most repeated node.
5. **Measure** (code): `headwork usage --by delegation`: share of each role's spend and
   responses in subagents versus the main session, and the main session's median context. Read it before and two weeks
   after steps 1-4 to check the estimate.

Not proposed: forcing parts more often. A part costs as much to start as a subagent and adds a pane, a slot and a
lifecycle; it stays for separate reviews, long jobs and second branches. Not proposed either: moving leads off Opus;
the ladder (#50) already governs that.

## Status

All five steps were built on 2026-11-23 (`subagent-opus`, `[usage] nudge_context_k = 200`, `usage --by delegation`).
Baseline from `headwork usage --by delegation --since 14d` on that day:

| Role | Subagent share of cost | Subagent cost on Opus | Median main context |
|---|---:|---:|---:|
| worker | 29 % | 91 % | 152k |
| lead | 26 % | 93 % | 152k |
| orchestrator | 32 % | 5 % | 146k |
| part | 76 % | 99 % | 339k |

Compare after two weeks: the Opus share of subagent cost should fall, the subagent share and the main context should
move the other way.
