# Dashboard: a live graph of the agents working together

Status: plan of 2026-11-22, nothing built. The user's request: "a dashboard for headwork, web-based, which should
illustrate a map of agents working together, something like an Obsidian map or rather a graph of agents, like in Neo4j,
so I can see and check how agents are working together." The user's answers are in section 11 and are worked into the
sections above.

## 1. What to answer

- Who is working right now, on what, under whom: the orchestrator, each task's lead, its parts, their subagents and
  helpers.
- Who talks to whom: spawns, session messages, reports to the orchestrator, asks to the user and their answers.
- Where a task is stuck: a session waiting on the user, a review with open findings, a job that runs on, a pull request
  waiting for its gate.
- How a task unfolded: replay the event log and watch the graph grow, talk and shrink.
- What each agent cost (tokens from `usage`, docs/usage.md), drawn as node size or shown on click.

## 2. Shape: `headwork dash`, a page served by the binary

`headwork dash [--port 8740] [--open]` serves one page on `127.0.0.1` only, read only over `state.db` (a second
connection with `query_only`, so the dashboard can never write). No Node, no build step, no extra service; the page
ships inside the binary.

- **Server:** `tiny_http` (synchronous, no async runtime, small) on a thread pool of std threads, in a new module
  `ops/dash/`. JSON endpoints plus the static page. The graph building is pure, in `core/src/graph.rs` (facts in, nodes
  and edges out), so it is table-tested like the rest of `core`.
- **Page:** plain HTML and JavaScript, no framework, embedded with `include_str!`. The graph is drawn by
  **Cytoscape.js** (MIT) with the `fcose` force layout and compound nodes, vendored as files in `dash/` (pinned
  versions, checksums in `dash/VENDOR.md`), never loaded from a CDN, so it works offline.
- **Why Cytoscape.js:** compound nodes (a task box holding its lead and parts), force and hierarchical layouts, styling by
  data, tens of thousands of elements, and an Obsidian/Neo4j-like feel with pan, zoom, drag and focus on a neighbourhood.
  Weighed: vis-network (no compound nodes), sigma.js (fast but weak on compound and labels), D3 (everything by hand),
  a real Neo4j (a database to run and keep in step; kept as a later export, section 10).
- **Lifecycle:** run by hand, or as a registered job (`job_start`) so park and resume carry it. `headwork doctor` gains no
  check: the dashboard is optional.

## 3. Nodes

Every node has an id, a kind, a label, a state (drawn as colour), a start and an end time (for the replay) and a details
panel on click.

| Kind | From | Label | Notes |
|---|---|---|---|
| `user` | one fixed node | you | asks point at it; drawn apart, at the top |
| `orchestrator` | `sessions` with role `orchestrator` | its `rc_name` | the hub; one node across restarts, with each session listed in its details |
| `task` | `tasks` | `<project>-<slug>` | a compound box holding its lead, parts, helpers and satellites; colour by state |
| `lead` / `worker` | the task's sessions | model, effort | the session inside its task box; a task that ran several sessions (resume, interrupt) shows the newest, the others in details |
| `part` | `tasks.parent_id` | the part's slug | its own small box nested in the lead's task box |
| `subagent` | `usage.agent_id`, `agent_type` (already read by U1) plus the new `agents` table (section 5) | agent type and description | a small node on its caller; collapsed into a count badge by default ("7 subagents"), expanded on click |
| `helper` | `helper-session` events | `headwork:helper` | inside its task box |
| `review` | `reviews` | flow and level | the reviewer lead part gets a review badge |
| `review-node` | `review_nodes` | node name (correctness, tests, ...) | under the reviewer part; findings count and verdicts in details |
| `job` | `jobs` | label | satellite of its task; colour by state |
| `pr` | `prs` | `#<number>` | satellite of its task; gate and merge state |
| `ask` | `asks` open or answered | header | edge-node between the session and `user` while open; folds into a line on the edge once answered |

Projects are not nodes: tasks are coloured or grouped by project (a filter, section 7).

## 4. Edges

| Edge | From | Meaning |
|---|---|---|
| `spawned` | orchestrator → task (`task-created` event, actor) | the orchestrator made the task |
| `part-of` | lead → part (`tasks.parent_id`) | drawn as nesting, not a line |
| `depends-on` | task → task (`tasks.depends_on`), part → part (`after`) | dashed |
| `called` | session → subagent (`usage.agent_id` under `session_id`) | thin |
| `opened` | session → helper, session → job | thin |
| `message` | session → session (`SendMessage`) | animated pulse when it happens; weight is the message count |
| `report` | session → orchestrator (`report` events) | carries the last status |
| `asked` | session → user (`ask-opened`), user → session (`ask-answered`) | red while open |
| `reviews` | review → task under review, finding → lead | the review loop |
| `delivered` | task → pr | |

**The one data gap: who a message went to.** Today the `PreToolUse` hook logs `tool-used` with only the tool name, and
the receiver's `prompt` event has `from` but no recipient id for messages that came from outside headwork. Change: the
`tool-used` event for `SendMessage` also stores `to` (the address as sent, after `readdress`) and the message's
`summary` (5 to 10 words, never the body). The receiver side already logs `cross_session` and `from` on `prompt`.
Together they give both ends; a message to a name headwork does not know becomes an edge to an `external` node. No
schema change, an event payload field only.

## 5. Store: one new migration

Everything else is already in the store. One table, at the next free migration number:

```sql
CREATE TABLE agents (
    agent_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    task_id INTEGER REFERENCES tasks(id),
    agent_type TEXT,
    description TEXT,
    model TEXT,
    spawn_depth INTEGER,
    first_at TEXT,
    last_at TEXT
);
CREATE INDEX agents_session ON agents(session_id);
```

Filled by the same reader that fills `usage` (`ops/usage.rs`): when it reads a subagent transcript it also reads its
`agent-<id>.meta.json` once (description, type, model, depth) and keeps `first_at` and `last_at` from the lines it
reads. Descriptions are the short labels the caller gave the `Agent` call, never prompts or output. `spawn_depth` lets
nested subagents (a fork's agents, a workflow's) hang off their parent agent rather than the session.

## 6. Endpoints

All `GET`, JSON, read only:

| Path | Returns |
|---|---|
| `/` | the page |
| `/api/graph?at=<time>&scope=<live\|all\|task:N>` | nodes and edges as Cytoscape elements, the state at that time (default now) |
| `/api/events?since=<event id>` | new events since an id, each mapped to a graph change (node added, state changed, edge pulse) |
| `/api/node/<id>` | details for the side panel: state history, last report, recap, tokens by type, prs, asks, findings |
| `/api/range` | first and last event times, for the slider |
| `/api/stream` | server-sent events: the same changes as `/api/events`, pushed as they land |

**Live:** the page opens `/api/stream`; the server polls the `events` table every second for ids above the last one it
sent (cheap: the primary key) and pushes the mapped changes. A dropped stream falls back to polling `/api/events`.

**Replay:** `/api/graph?at=T` builds the graph from rows whose start is at or before `T` and whose end is after it, and
states from the newest `transition` event at or before `T` (`core/src/graph.rs`, `state_at`). The slider asks for
snapshots while dragged and plays forward from a point by applying `/api/events` in order at 1x, 10x or 100x, so
messages pulse and nodes appear in the order they happened.

## 7. The page

- **Canvas:** the graph, force layout with the user node pinned top and the orchestrator in the middle; each task a box
  around its lead and parts. Toggle a hierarchical layout (orchestrator → tasks → parts → subagents).
- **Colour by state:** working green, needs_user red, in_review amber, queued grey, closed faded (hidden by default
  in live view). Node size by tokens (log scale) when "size by cost" is on.
- **Side panel on click:** the node's details from `/api/node/<id>`; a "focus" button keeps only its neighbourhood (two
  hops), like Neo4j Browser's expand.
- **Filters:** project, task state, kinds (hide subagents, jobs, prs), time window, "only live".
- **Slider:** bottom bar from the first event to now, play and speed buttons, "live" snaps back to now.
- **Badges:** open asks to the user counted on the user node; tick findings (`findings`) as warning badges on the node
  they name.
- **Search:** type a task or session name to jump to it.
- Light and dark theme from the system setting. No external requests.

## 8. Security

- Binds `127.0.0.1` only; a `--bind` other than loopback is refused.
- Read only: `query_only` connection, no endpoint changes anything (section 10 has the later actions).
- No message bodies, prompts or transcripts are ever served, only what the store already holds (reports, recaps,
  ask questions, finding summaries) plus the new message summaries.
- `Host` header checked against `127.0.0.1:<port>` and `localhost:<port>` (blocks DNS rebinding).

## 9. Milestones (worker-days, one worker each)

| | What | Done when |
|---|---|---|
| D1 | `core/src/graph.rs` (facts → nodes and edges, `state_at`), the `agents` migration and its fill in the usage reader, `to` and `summary` on `SendMessage` events, `headwork graph --json [--at T]` printing the graph | table tests for every node and edge kind; `headwork graph --json` on this machine's store shows the review-flows lead with its parts and subagents |
| D2 | `headwork dash`: `tiny_http` server, `/api/graph`, `/api/node`, the page with Cytoscape.js vendored, layout, colours, side panel, filters, search | the page shows the live board as a graph and matches `headwork board` |
| D3 | live and replay: `/api/events`, `/api/stream`, `/api/range`, the slider, play speeds, message pulses | a task's whole life replays from its first event; a new spawn appears within two seconds |
| D4 | cost and health: size by tokens, tokens by type in the panel, finding and ask badges, review nodes with findings, the hierarchical layout; a trial with a few live tasks, results in `docs/trials/` | the user has watched a real lead, its parts and a review on the page |
| | Total | about 4 |

D1 first; D2 and D3 in order; D4 last. Each milestone is usable on its own: D1 gives a JSON graph any tool can draw.

## 10. Later (not in this plan's milestones)

- **Actions** (the user's answer: read only first): answer an ask, send a message to a session, close or drop a task
  from the page, each a `POST` through the same guarded ops the CLI and MCP tools use (`ops/answer.rs`,
  `ops/close.rs`), behind a per-run token printed by `headwork dash` and required on every `POST`, recorded with actor
  `dash`.
- **Neo4j export:** `headwork graph export --cypher` writes the D1 graph as Cypher `MERGE` statements for ad-hoc queries
  in Neo4j Browser.
- **Obsidian export:** one note per task with `[[links]]`, for a static map in Obsidian's graph view.
- An `orchestrator` MCP tool that returns the dashboard's URL; a line in `board` when the dashboard runs.

## 11. The user's answers (2026-11-22)

1. Where: built into headwork, a local page served by the binary (section 2); a Neo4j or Obsidian export is later
   (section 10).
2. Time: live, plus a slider that replays the event log (section 6).
3. Nodes: the orchestrator, leads and parts; subagents and helpers; reviews and their nodes; jobs, pull requests and
   asks to the user, with the user as a node (section 3).
4. Actions: read only first, actions in a later milestone (sections 8 and 10).

## Deferred

- Several machines on one page (the store is per machine).
- Sessions outside headwork (`unmanaged` in `usage`) as nodes; they have no task and no edges beyond their subagents.
- A hosted or shared dashboard; it stays on loopback.
