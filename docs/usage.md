# Usage: tokens spent by every Claude session

Status: plan of 2026-11-22; U1 built (`headwork usage`, migration 0018); U2 built (hook and tick reads, `board` and `status` tokens, the orchestrator's `usage` tool, `usage-total` at close); U3 built (`[usage.prices]`, the estimate in every view, doctor check `usage-prices`); U4 built (the tick's daily roll-up, helper `claude` sessions named and counted under their task). Done. The user's request: "a logging system to track how much tokens are spent by
type and so on". The user's answers to the open points are in section 10 and are worked into the sections above.

## 1. What to answer

- How many tokens did a task cost, in total and per token type, and how did that split between the lead, its parts,
  their subagents and helpers?
- Which projects, task kinds, models and effort levels spend the most, per day and per week?
- How well does caching work (cache reads against fresh input and cache writes), and where does it break?
- What does the orchestrator itself cost per day?
- What would it cost at API prices (an estimate: the account may be on a subscription)?
- How much do Claude sessions outside headwork spend, by folder?

## 2. Where the numbers come from

Every Claude Code session writes a transcript, `~/.claude/projects/<folder key>/<session id>.jsonl`. Each assistant
line carries the API response's `usage` and the facts around it (checked on this machine, Claude Code 2.1.330):

| Field | What it holds |
|---|---|
| `message.id`, `requestId` | one API response; a response with several content blocks is written once per block, every copy with the same `usage` (checked on five transcripts: no copy differs) |
| `message.model` | the model that answered (`claude-opus-5-5`, ...), so a fallback shows |
| `usage.input_tokens` | uncached input |
| `usage.cache_creation.ephemeral_5m_input_tokens`, `ephemeral_1h_input_tokens` | cache writes, by lifetime (priced differently) |
| `usage.cache_read_input_tokens` | cache reads |
| `usage.output_tokens`, `output_tokens_details.thinking_tokens` | output, and the thinking part of it |
| `usage.server_tool_use.web_search_requests`, `web_fetch_requests` | server tool calls |
| `usage.service_tier`, `speed` | standard or priority, fast mode |
| `effort` | the effort level of that turn |
| `timestamp`, `sessionId`, `cwd`, `isSidechain` | when, which session, where |

Subagents (`Agent`, forks, `Workflow` agents) write their own transcripts beside it,
`<session id>/subagents/agent-<id>.jsonl`, with `agent-<id>.meta.json` giving `agentType`, `description`, `model` and
`spawnDepth`.

headwork already starts every managed session with `--session-id`, so a store `sessions.id` is the transcript's file
name, and each session row names its task. Every number can be joined to project, task, kind, lead or part, and role
without guessing.

**Choice: read the transcripts.** They are exact, already on disk, need no collector, include subagents, and cover the
past as far back as Claude keeps them. Two other sources were weighed:

- Claude Code's OpenTelemetry export (`CLAUDE_CODE_ENABLE_TELEMETRY`, metric `claude_code.token.usage` by `type` and
  `model`) needs a collector process and environment on every launch, carries no subagent split and no history.
- The status line's cost figure is one number per session, no types.

**Gaps transcripts do not cover**, listed by `headwork usage --gaps` so totals are not read as complete:

- the auto mode classifier's own calls (`serverClassifierRequest` names them, no usage is written);
- the summary call of a compaction (the summary lands as a user line, without usage);
- helper tabs: `helper_tab` starts a program without a store session (section 8 fixes that for `claude`);
- transcripts Claude deletes after `cleanupPeriodDays` (default 30) before headwork read them; the tick reads well
  inside that window, but a backfill reaches only what is still on disk.

## 3. Store: one new migration

Numbers and ids only, never message text. The migration is `0018_usage.sql`.

```sql
CREATE TABLE usage (
    message_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id TEXT,
    agent_type TEXT,
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL CHECK (role IN ('orchestrator', 'lead', 'part', 'worker', 'helper', 'unmanaged')),
    dir TEXT NOT NULL,
    at TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT,
    speed TEXT,
    service_tier TEXT,
    input INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    output INTEGER NOT NULL,
    thinking INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    web_fetches INTEGER NOT NULL
);
CREATE INDEX usage_task ON usage(task_id, at);
CREATE INDEX usage_at ON usage(at);

CREATE TABLE usage_daily (
    day TEXT NOT NULL,
    session_id TEXT NOT NULL,
    agent_type TEXT NOT NULL DEFAULT '',
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL,
    dir TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL DEFAULT '',
    responses INTEGER NOT NULL,
    input INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    output INTEGER NOT NULL,
    thinking INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    web_fetches INTEGER NOT NULL,
    PRIMARY KEY (day, session_id, agent_type, model, effort)
);

CREATE TABLE usage_files (
    path TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id TEXT,
    read_to INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL,
    read_at TEXT NOT NULL
);
```

- `task_id` and `role` are fixed when the row is written, from the session row (role from `tasks.lead` and
  `parent_id`), so a later rename or close does not move old numbers. A subagent row takes its parent session's task
  and role and adds `agent_id` and `agent_type`.
- `dir` is the session's folder (`cwd`), the only grouping an `unmanaged` row has.
- `message_id` as the key makes every read idempotent: copies of one response, a reread after a crash, or a backfill
  over rows already there change nothing (`INSERT ... ON CONFLICT DO NOTHING`).
- Size: a busy day here is some 30 sessions of a few hundred responses each, about 10 000 rows, under 2 MB.
- Retention: `usage` keeps one row per response for `[usage] keep_days` (default 90). Once a day the tick moves older
  rows into `usage_daily`, summed per day, session, subagent type, model and effort, in one transaction (insert the
  sums, delete the rows), and records `usage-rolled-up` with the cut-off time and the counts. Every query reads both
  tables, so totals never change when rows roll up; only per-response figures (output per response, speed, service
  tier) stop for those days. A reread of a transcript skips lines older than the cut-off, so rolled-up responses are
  never counted twice.

## 4. Reading: `ops/usage.rs`, pure parsing in `core/src/usage.rs`

- `core::usage::parse_line(&str) -> Option<UsageLine>`: pure, takes one transcript line, returns the numbers or none
  (not an assistant line, no usage, a half-written last line). Table tests on real lines with the text stripped.
- `ops::usage::ingest(store, files)`: for each transcript and its subagent files, read from `usage_files.read_to` to
  the last complete line, insert, move `read_to`. A file that shrank or whose mtime went back is read from the start
  (the key and the cut-off keep that safe).
- A transcript whose session id is in the store is tagged with its task and role; any other is `unmanaged`. When the
  store learns the id later (`headwork import` adopts it), its rows are re-tagged.
- When it runs:
  - the `Stop` hook, for the session's own transcript and its subagents: a few kilobytes per turn, next to the 30 ms a
    hook takes today; a failure is logged as `usage-error` and never blocks;
  - `SessionEnd`, a last read;
  - the tick, for every live and recently ended store session (subagents that ran in the background, sessions whose
    hook did not run), and for every other transcript under `~/.claude/projects` changed since its `read_to`;
  - `headwork usage --backfill`, once, for every transcript still on disk, managed or not.

## 5. What to break it down by

| Dimension | From |
|---|---|
| token type | input, cache write 5m, cache write 1h, cache read, output (thinking shown as a part of output), web searches and fetches |
| project, task, kind | the session's task |
| lead tree | a lead's own rows, each part's, and the roll-up (as `status` rolls up states) |
| role | orchestrator, lead, part, worker, helper, subagent (`agent_type`: general-purpose, Explore, fork, workflow-subagent, ...), unmanaged |
| model | the answering model; set beside the task's requested model, a fallback is visible |
| effort, speed | per response |
| time | day, hour, or `--since` like `shadow-report` |
| session | a task's sessions apart (a resume starts a new one) |
| folder | `dir`, for `unmanaged` sessions |

Derived figures: cache hit ratio (`cache_read / (input + cache_read + cache writes)`), output per response, and tokens
per merged pull request for a task.

## 6. Money: an estimate from a price table

Prices change and the account may pay by subscription, so dollars are an estimate at API list prices, never a bill.
They live in `config.toml`, not in code:

```toml
[usage]
keep_days = 90

[usage.prices."claude-opus-5-5"]
input = 0.0
cache_write_5m = 0.0
cache_write_1h = 0.0
cache_read = 0.0
output = 0.0
web_search_per_1000 = 0.0
```

- Dollars per million tokens per type (the zeros are placeholders; the user fills in the list prices). A model name
  matches exactly, else the longest configured prefix (`claude-opus` covers every Opus).
- The estimate is worked out when a report is made, never stored, so a corrected price re-prices the past.
- A model with no price shows `?` in the dollar column and is listed under the table; `headwork doctor` gains a check
  `usage-prices` that warns about models seen in the last 7 days with no price.
- `service_tier` and `speed` multipliers are left out until a priced tier is used here; `--gaps` says so.

## 7. Where it shows

- `headwork usage [--since 7d] [--by project|task|kind|model|effort|role|agent|folder|day] [--task <name>]
  [--unmanaged] [--json]`: a table, one row per group, a column per token type, a token total and an estimated dollar
  total; `--gaps` lists what section 2 cannot see. Without `--unmanaged`, sessions outside headwork show as one
  `unmanaged` line.
- `board` and `status`: one short column per task with total tokens (`1.2M`) and the estimate (`$4.10`), a lead showing
  its roll-up.
- The orchestrator MCP tool `usage` (same arguments), so "what did today cost" is answered in one call; a worker's
  `board` shows its own task only, as today.
- At close: an event `usage-total` with the task's figures, and the line in the close report, so the orchestrator's
  "finished" line can carry it.

## 8. Helper tabs and sessions outside headwork

- `helper_tab` running a fresh `claude`: headwork adds `--session-id <uuid>` and records the id in a `helper-session`
  event under the task (not a store session, which would stand in for the worker's own), so its transcript is read like
  any other and counts as role `helper`. A `claude --resume` keeps the resumed session's tag; other programs spend no
  tokens.
- Sessions headwork did not start (a plain `claude` in some folder) are read as role `unmanaged` with no task, grouped
  by folder (section 4).

## 9. Milestones (worker-days, one worker each)

| | What | Done when |
|---|---|---|
| U1 | the new migration, `core::usage` parser, `ops::usage::ingest` for managed and unmanaged transcripts, `headwork usage` with `--by`, `--since`, `--unmanaged`, `--backfill` | the backfill fills the table from this machine's transcripts; totals for one task match a hand count of its transcript |
| U2 | `Stop`, `SessionEnd` and tick reads; `board` and `status` columns (tokens); orchestrator `usage` tool; `usage-total` at close | a fresh task's tokens show on `board` within one turn |
| U3 | `[usage.prices]`, the dollar estimate in every view, doctor check `usage-prices` | `headwork usage` prints dollars for every priced model and `?` with a list for the rest |
| U4 | `keep_days` roll-up into `usage_daily` in the tick; helper sessions (`--session-id`) | totals for a period are the same before and after its roll-up; a helper's tokens count under its task |

Tests follow the code rules: table tests for the parser (every field, missing fields, partial lines, copies of one
response, sidechain lines), store tests on in-memory databases for idempotent inserts, roll-ups and totals across both
tables, CLI tests on a temporary `HEADWORK_HOME` with fixture transcripts.

## 10. The user's answers (2026-11-22)

1. Money: tokens plus an estimate at API prices from a price table in `config.toml` (section 6); no budgets.
2. Scope: every Claude session on the machine; those headwork did not start count as `unmanaged`, by folder (sections 4
   and 8).
3. Detail: one row per response for 90 days, then a daily roll-up per session, subagent type, model and effort
   (section 3).
4. Where it shows: `headwork usage`, the orchestrator's `usage` tool, a column on `board` and `status`, and the total at
   close (section 7); no HTML page.

## Deferred

- Budgets per task, with an `over-budget` tick finding and an ask.
- An HTML report page.
- Claude Code's OpenTelemetry export, should the classifier's or compaction's tokens ever appear there.
