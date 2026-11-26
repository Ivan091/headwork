# Review flows: reviews on the user's word, run by a reviewer lead with many small reviewers

Status: plan of 2026-11-22. R1 (core, store, `flow check`, the `code` flow), R2 (the trigger, `review_start`, the
reviewer lead's part and rules, `reviewer-write`, `review-unstarted`) and R3 (the loop tools, rounds, the dispute ask,
`review-open`, `review-unfinished`) are built, and so is R4's surface (2026-11-26: reviews on `board` and `status`, the
`plan` flow, `doctor` check `flows`); R4's planned live trial at each level is not run yet. It builds on leads (`docs/leads.md`): a review is a part of the task, and
that part is the reviewer lead.

## 1. The model

The user's words (2026-11-22): "I will say when review is needed (like review [1,2,3], where number is how strong the
review and pushback should be). Review should run with subagents with fresh context. There should be many small
specialized reviewers rather than one big one. Reviewer should have its own lead. In future I plan to have many review
flows (graphs) for different tasks."

| Who | What it is | Where it runs |
|---|---|---|
| implementing lead | the task's lead; the user types `review 2` in its tab; it fixes or disputes the findings | the task's first tab |
| reviewer lead | a part of kind `review`, started for one review; it has never seen the implementation; it runs the flow's nodes, merges their findings, re-checks the fixes; it never edits code | a tab `review` in the task's workspace |
| reviewer | one node of the flow: a small specialist (correctness, tests, project rules, ...) run as a fresh `Agent` subagent of the reviewer lead with only its own prompt and the target | the reviewer lead's process tree, no tab |
| flow | a named graph of nodes in a TOML file (`code`, `plan`, later more), picked by name or by the task's kind | `flows/` in this repository, overridable per home |

One review goes through these steps:

1. The user types `review 2` (or `review [2]`, `review plan 3`, `review 2 focus on park`) in the lead's tab.
2. The `UserPromptSubmit` hook recognises it, stores a `requested` review and tells the lead to call `review_start`.
3. `review_start` pins the commit under review, makes a read-only worktree for it and starts the reviewer lead as a
   part with a brief headwork writes from the flow, the level and the focus.
4. The reviewer lead asks headwork which nodes are ready (`review_next`), runs each as a fresh subagent (in parallel
   where the graph allows), and hands each node's findings back (`review_node_done`).
5. At level 2 and 3 a verify stage checks each finding adversarially before anything reaches the lead.
6. The reviewer lead delivers the confirmed findings (`review_deliver`) and sends the numbered list it returns to the
   implementing lead with `SendMessage`.
7. The implementing lead fixes each finding or disputes it with a reason, commits the fixes, records them
   (`review_resolve`) and tells the reviewer lead with `SendMessage`.
8. The reviewer lead re-checks the fixes and weighs the disputes by the level's pushback; level 3 runs a second full
   round on the new commit. Disputes it cannot settle go to the user as one ask in the review tab.
9. The review closes (`review_finish`), the part closes with it, and the lead tells the user the outcome in one line.

## 2. The user's answers (2026-11-22)

1. The reviewer's own lead is a review part: a fresh session in a tab of the task's workspace.
2. A flow is a TOML graph that headwork tracks: the reviewer lead runs each node as a fresh `Agent` subagent and
   reports each node through MCP tools, so headwork stores the progress, enforces the order and survives a park.
3. The implementing lead fixes the findings or disputes each with a reason; the reviewer lead re-checks; level 3 runs a
   second round; disputes it cannot settle go to the user as one ask.
4. The level sets breadth, depth and pushback together (section 4).

Decided here without asking (recorded in `docs/decisions.md`): the trigger syntax, a read-only detached worktree for
the review, reviews only on the user's word, the tool names, the reviewer lead's default model, and the first two flows.

## 3. The trigger

The `UserPromptSubmit` hook (`ops/hook.rs`, `prompt`) matches the user's own prompt (not a cross-session message) in a
lead's session against:

```
review [<flow>] <level> [<focus>]        level: 1, 2 or 3, optionally in brackets: review [2]
```

`core/src/review/command.rs` parses it (pure, table-tested): `review 2`, `review [3]`, `review plan 1`, `review 2 focus on the
park path` match; `review the docs`, `please review 2 things`, `reviewer`, `review 4` do not (a near miss gets a hint
in the hook's context, no review). The flow defaults through `[reviews] flows` by the task's kind, then `code`.

On a match the hook stores a review in state `requested` (event `review-requested`, with flow, level, focus) and adds
to the lead's context: "The user asked for a level-N review with the flow F. Call `review_start` now." Hooks do not
start sessions (a spawn takes about 20 seconds); the lead's tool call does. The stop rule `review-unstarted` refuses a
lead's stop while a requested review has not started.

A review starts only from a `requested` row: `review_start` is refused without one (`review-unrequested`). The row comes
from the user's word or from a plan: since 2026-11-23 the orchestrator or a lead may plan a level per task at spawn
(`review`, with `decided_by` and a reason), and headwork requests it at the lead's first pull request report or at its
`done` (`review-planned`), exactly as the user's command would. The user's words win: `review N` replaces the plan and
`no review` clears it. A part's tab and the orchestrator's do not take the command;
a later `review N` while one runs is refused with the running one's state (`review-running`).

## 4. Levels

Every flow defines its levels; the built-in `code` flow:

| | 1 light | 2 standard | 3 strict |
|---|---|---|---|
| Reviewers | correctness, tests, project rules | + error handling, design and cohesion, docs and decisions | + state machine and concurrency, security, approach challenger, scope challenger |
| Models | Sonnet | Opus for correctness and design, Sonnet for the rest | Opus |
| What counts as a finding | a defect with a concrete failing scenario | defects, plus risks and nits marked `nit` | everything at 2, plus doubts about the approach and the scope |
| Verify stage | none | one adversarial verifier per finding; only `confirmed` and `plausible` go on | same |
| Rounds | one; the fixes are re-checked | one; the fixes are re-checked | two: a second full round on the fixed commit |
| A dispute | accepted when it gives a reason | accepted only with evidence (a test, a code reference, a doc line) | same as 2, and the reviewer lead argues back once first |
| A dispute not settled | accepted | goes to the user in one ask | goes to the user in one ask |

The pushback text of each level is part of every node's prompt at that level, so a specialist at level 3 presses harder
than the same specialist at level 1, not only more specialists run.

## 5. Flows

A flow is a TOML file. Built-in flows live in `flows/` in this repository and are embedded in the binary like the rules
(`include_str!`); a file `<home>/flows/<name>.toml` overrides or adds one by name, its prompts read from beside it.

```toml
name = "code"
description = "A branch's diff against the default branch"
target = "diff"                     # diff: base..head of the task's branch; files: the paths in focus or the brief

[levels.1]
pushback = "Report only defects you can show with a concrete failing scenario. Accept any reasoned dispute."
dispute = "reason"                  # reason | evidence
unsettled = "accept"                # accept | ask
rounds = 1

[levels.2]
pushback = "Report defects and risks; mark nits. Accept a dispute only with evidence."
dispute = "evidence"
unsettled = "ask"
rounds = 1

[levels.3]
pushback = "Challenge the approach and the scope as well as the code. Argue back once before accepting a dispute."
dispute = "evidence"
unsettled = "ask"
rounds = 2

[[node]]
id = "correctness"
prompt = "code/correctness.md"
from_level = 1
model = { 1 = "sonnet", 2 = "opus", 3 = "opus" }

[[node]]
id = "tests"
prompt = "code/tests.md"
from_level = 1
model = "sonnet"

[[node]]
id = "challenge-approach"
prompt = "code/challenge-approach.md"
from_level = 3
model = "opus"

[[node]]
id = "verify"
prompt = "code/verify.md"
from_level = 2
after = ["*"]                       # every node of a lower stage that ran
each = "finding"                    # one subagent per finding of its inputs
model = "opus"
```

The graph:

- Nodes form a directed acyclic graph through `after` (node ids, or `*` for every earlier node that runs at this level).
  A node without `after` is ready at once. `each = "finding"` fans a node out into one subagent per finding of its
  inputs; that is how the verify stage works, and later flows can use it for other per-item steps.
- `from_level` drops a node below that level; `after` edges to dropped nodes vanish.
- Each node returns findings in one shape: `file`, `line`, `severity` (`important`, `nit`, `doubt`), `summary`,
  `scenario`; a verify node returns a `verdict` (`confirmed`, `plausible`, `rejected`) per finding.
- `headwork flow check [<name>]` validates every flow (ids unique, acyclic, prompts present, models allowed by
  `[models]`, levels 1 to 3 defined) and `doctor` runs it (check `flows`). A broken override is an error at load and
  `review_start` refuses it.

The first two flows:

| Flow | For | Nodes |
|---|---|---|
| `code` | a branch's diff | correctness, tests, project rules (`CLAUDE.md`, `AGENTS.md`: no comments, lints, errors), error handling, design and cohesion, docs and decisions, state machine and concurrency, security, approach challenger, scope challenger, verify |
| `plan` | a plan document such as this one | feasibility against the code, consistency with `docs/design.md` and `docs/decisions.md`, open points left unasked, scope challenger, effort sanity, verify |

Later flows (the user's "many review flows for different tasks") are new TOML files: a me-sim scene flow (physics
plausibility, video pacing, render settings), a docs flow, a security flow. Node kinds beyond subagents are deferred:
a `command` node (run `scripts/check.sh` in the review worktree) and a `gate` node (ask the user mid-flow).

## 6. Fresh context

- The reviewer lead is a new session: its brief holds the flow, the level, the focus, the base and head commits and the
  implementing lead's address, nothing of the implementing conversation.
- Each node is an `Agent` subagent of type `general-purpose`, never a fork (`subagent_type: "fork"` would inherit the
  reviewer lead's context). `review_next` returns, per ready node, the exact prompt to pass: the node's prompt, the
  level's pushback, the target (base and head commit, the worktree path, the changed files), the focus, and for an
  `each` node the one finding. The reviewer rules forbid adding anything else, so reviewers do not anchor on each other
  or on the reviewer lead's opinion.
- Re-checks and the second round are new subagents too, given the finding, the lead's resolution and the new diff.

## 7. Store: migration 0019

```sql
CREATE TABLE reviews (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    part_id INTEGER REFERENCES tasks(id),
    flow TEXT NOT NULL,
    level INTEGER NOT NULL CHECK (level BETWEEN 1 AND 3),
    focus TEXT NOT NULL DEFAULT '',
    base_sha TEXT,
    head_sha TEXT,
    round INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL CHECK (state IN ('requested', 'running', 'delivered', 'rechecking', 'asking', 'done', 'cancelled')),
    requested_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE TABLE review_nodes (
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    round INTEGER NOT NULL,
    node TEXT NOT NULL,
    item INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('ready', 'running', 'done', 'failed')),
    started_at TEXT,
    finished_at TEXT,
    PRIMARY KEY (review_id, round, node, item)
);
CREATE TABLE review_findings (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    round INTEGER NOT NULL,
    node TEXT NOT NULL,
    file TEXT, line INTEGER,
    severity TEXT NOT NULL,
    summary TEXT NOT NULL,
    scenario TEXT NOT NULL,
    verdict TEXT,
    resolution TEXT CHECK (resolution IN ('open', 'fixed', 'fix-accepted', 'disputed', 'dispute-accepted', 'fix-rejected', 'asked', 'user-kept', 'user-dropped')),
    note TEXT,
    argued INTEGER NOT NULL DEFAULT 0
);
```

`core/src/review/` holds the pure parts: the command parser, the flow model and its validation, which nodes are ready
given the level and the finished ones, the review's state machine (`requested` → `running` → `delivered` →
`rechecking` → `running` (round 2) or `asking` or `done`; `cancelled` from any state when the lead closes or the user
drops the part), and the dispute decision by level. Table tests per transition and guard, `proptest` over random node
completions (a node never runs before its inputs; a review never delivers with a ready node).

## 8. Tools and rules

| Tool | Who | What |
|---|---|---|
| `review_start {}` | implementing lead | takes the `requested` review: refuses with uncommitted changes (`review-dirty`), pins `head_sha` (HEAD) and `base_sha` (merge base with the default branch), makes the review worktree, spawns the part `review` (`review-2` for a later review) of kind `review` with the generated brief |
| `review_next {}` | reviewer lead | the ready nodes with their full prompts and models, marked `running`; empty when the round waits on nothing |
| `review_node_done {node, item?, findings[]}` | reviewer lead | stores a node's findings (or verdicts); a malformed one is refused with the shape |
| `review_deliver {summary}` | reviewer lead | refused while a node is ready or running (`review-nodes-open`); returns the findings numbered (confirmed and plausible only after a verify stage) and the implementing lead's address, for the reviewer lead to send with `SendMessage` |
| `review_resolve {items: [{id, fixed or disputed, note}]}` | implementing lead | one entry per open finding; refused while one is missing or the fixes are uncommitted; pins the new `head_sha` and returns the reviewer lead's address, for a `SendMessage` that wakes it |
| `review_verdict {items: [{id, fix accepted or rejected, dispute accepted or kept}]}` | reviewer lead | the re-check, from fresh subagents; a kept dispute at `unsettled = "ask"` makes the review `asking` |
| `review_finish {}` | reviewer lead | refused while a finding is open, a round is due, or an ask waits; closes the review, then the part closes itself |

Rules:

- `rules/reviewer.md` (new), added to a part of kind `review`: you are the reviewer lead; never edit, commit or push;
  run every node `review_next` gives you as its own fresh `general-purpose` `Agent` subagent with exactly the prompt
  given, in parallel; hand each result back with `review_node_done`; never drop or soften a node's finding yourself
  (the verify stage does that); deliver, re-check, ask the user about unsettled disputes in one `AskUserQuestion`
  headed `Dispute` (one question per finding, options "Keep the finding" and "Drop it"), finish, then close.
- `rules/lead.md` gets a section "Reviews": when the user asks for one, call `review_start`; wait for the findings (the
  stop rule lets a lead wait on a part); fix or dispute every finding, commit, then `review_resolve`; tell the user the
  outcome in one line when the review finishes; never argue with the reviewer outside `review_resolve`.
- Guard: a `review` task's session writes nowhere but the scratchpad and runs no `git commit`, `push`, `checkout` or
  builds (rule `reviewer-write`); its worktree is read-only to it. The implementing lead's `done` is refused while a
  review is open (`review-open`). The stop rule refuses a reviewer lead's stop while `review_next` would return nodes
  or a resolution waits on its verdict (`review-unfinished`).

## 9. The review worktree and placement

- headwork itself makes the review worktree with `git worktree add --detach <worktree_path>-review-<n> <head_sha>`, not
  the project's `worktree_new` (no branch, no build setup needed for reading), and moves it to the new `head_sha` at
  each round (`git checkout --detach`, by headwork, not the reviewer). It is removed when the part closes; `clean`
  learns the shape.
- The part opens as a tab of the task's workspace titled by `[leads] part_title` (`review`). It counts against
  `max_parts` and the machine's slots like any part; when they are full it waits in the queue right behind its lead.
- The reviewer lead runs on `[reviews] lead_model` (default Sonnet: it coordinates, the nodes carry the judgement).

## 10. Config

```toml
[reviews]
enabled = false                         # until R3 is in
lead_model = "sonnet"
flows = { task = "code", plan = "plan" } # default flow by task kind; else "code"
parallel = 6                            # at most this many nodes running at once per review
```

## 11. Board, status, log

`board` shows a running review under its task (`review 2 code, round 1: 5/7 nodes, 3 findings`), `status` adds
`in review (level 2)` to the task's line, and the event log carries `review-requested`, `review-started`,
`review-node`, `review-delivered`, `review-resolved`, `review-verdict`, `review-asked`, `review-finished`.

`status --json` gives each task's entry a `review` field: `null` when no review is open, else
`{"level": 2, "flow": "code", "round": 1, "state": "running", "focus": "", "findings_awaiting_lead": 0}`
(`level` 1 to 3; `state` one of `requested`, `running`, `delivered`, `rechecking`, `asking`). The text `line` is unchanged.

## 12. Milestones (worker-days, one worker each)

| # | Milestone | Effort |
|---|---|---|
| R1 | Core and store: `core/src/review/` (parser, flow model and validation, ready-node scheduling, state machine, dispute rule), migration 0019, store queries, `flow check`, the built-in `code` flow and its prompts; table and proptest cases | 1.5 |
| R2 | Start: the hook's trigger, `review_start`, the review worktree, the part of kind `review`, `rules/reviewer.md`, the `reviewer-write` and `review-unstarted` rules, `[reviews]` config | 1 |
| R3 | The loop: `review_next`, `review_node_done`, `review_deliver`, `review_resolve`, `review_verdict`, `review_finish`, rounds, the dispute ask, `review-open` and `review-unfinished`, the lead's "Reviews" section | 1.5 |
| R4 | Surface and trial: board, status, log, the `plan` flow, `doctor` check `flows`, one real review at each level on a headwork task; results in `docs/trials/` (surface built 2026-11-26; the trial remains) | 1 |
| | Total | about 5 |

R1 first, then R2, then R3; R4 last. A review can be used by hand from R2 on (the reviewer lead walks the flow from its
brief), and is enforced from R3 on.

## Deferred

`command` and `gate` nodes; reviews that a project requires before merge; reviews of another project's task; the Claude Code Workflow tool as the engine, set aside
because headwork would see only a run's end result and a park would lose a run in flight (`docs/decisions.md`,
2026-11-22).
