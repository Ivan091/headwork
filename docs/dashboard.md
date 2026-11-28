# Dashboard: a live graph of the agents working together

Status: plan of 2026-11-22; D1 built 2026-11-23 (`headwork graph`), D2 built 2026-11-23 (`headwork dash`), D3 built
2026-11-23 (live stream and replay slider), D4 built 2026-11-23 (cost and health; trial in
`docs/trials/f-dashboard/results.md`); see `docs/decisions.md`. The user's request: "a
dashboard for headwork, web-based, which should illustrate a map of agents working together, something like an
Obsidian map or rather a graph of agents, like in Neo4j, so I can see and check how agents are working together." The
user's answers are in section 11 and are worked into the sections above.

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

Open by default: starting the orchestrator (`headwork orchestrator`, `resume`) runs `ops::dash::ensure`. With `[dash] enabled`
(default on) and nothing listening on `[dash] port` (8740) it starts a detached `headwork dash` and, with `[dash] open`
(default on), opens the page in the browser. A server already running is left alone and the page is not opened again.

- **Server:** `tiny_http` (synchronous, no async runtime, small) on a thread pool of std threads, in a new module
  `ops/dash/`. JSON endpoints plus the static page. The graph building is pure, in `core/src/graph.rs` (facts in, nodes
  and edges out), so it is table-tested like the rest of `core`.
- **Page:** plain HTML and JavaScript, no framework, embedded with `include_str!`. The graph is drawn by
  **Cytoscape.js** (MIT) with the `fcose` force layout and compound nodes, vendored as files in `dash/` (pinned
  versions, checksums in `dash/VENDOR.md`), never loaded from a CDN, so it works offline.
- **Why Cytoscape.js:** force and hierarchical layouts, styling by
  data, tens of thousands of elements, and an Obsidian/Neo4j-like feel with pan, zoom, drag and focus on a neighbourhood.
  Weighed: vis-network, sigma.js (fast but weak on labels), D3 (everything by hand),
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
| `lead` / `worker` | `tasks`: a lead task, or a plain task | `<project>-<slug>` | one node per task, holding its newest session; coloured by the task's state (`queued`, `working`, `in_review`, `merged`, ...); every session it went through (park, resume, restart) is listed in the details panel (`session`, `earlier_sessions`); lead is drawn larger |
| `part` | `tasks`: a task with a lead | `<project>-<slug>` | the same one node, hanging off its lead by a `part-of` line, not nested |
| `subagent` | the `agents` table (section 5) | agent type | hangs off its caller by a `called` line; description, model and depth in details; `seen`, `running` or `done` now by its meta file's `finished`, `running`/`done` at a past instant; the live view keeps only those still running; D2 collapses them into a count badge |
| `helper` | `helper-session` events | `headwork:helper` | hangs off its task by an `opened` line |
| `review` | `reviews` | flow and level | the reviewer lead part gets a review badge |
| `review-node` | `review_nodes` | node name (correctness, tests, ...) | under the reviewer part; findings count and verdicts in details |
| `job` | `jobs` | label | satellite of its task; colour by state |
| `pr` | `prs` | `#<number>` | satellite of its task; gate and merge state |
| `ask` | `asks` open or answered | header, else the start of its question | edge-node between the session and `user` while open; folds into a line on the edge once answered |
| `item` / `epic` | `backlog_items` (section 4a) | `#<id> <start of its text>` | the item's status (`backlog`, `in_progress`, `blocked`, `done`, `cancelled`) is its state; the project and full text are in details; an epic is drawn larger |
| `external` | a message address headwork does not know | the address | drawn apart, its id a hash of the address; a name headwork knows but this view does not draw gets no edge |

Projects are not nodes: tasks are coloured or grouped by project (a filter, section 7).

## 4. Edges

| Edge | From | Meaning |
|---|---|---|
| `spawned` | orchestrator or user → task (`task-created` event, actor) | who made a top-level task (a part has `part-of`); the orchestrator's other lines (messages, reports) end on the task too, never on a session |
| `part-of` | part's task → lead's task (`tasks.parent_id`) | a dotted line; nothing is nested |
| `depends-on` | task → task (`tasks.depends_on`), part → part (`after`) | dashed |
| `called` | task → subagent, or parent subagent → nested one (`agents.parent_agent_id`) | thin |
| `opened` | task → helper, task → job | thin |
| `message` | task → task, external or subagent (`SendMessage`) | counted from the receiver's `prompt` event when it is a session headwork runs, labelled with the sender's last summary; else from the sender's event; animated pulse when it happens |
| `report` | task → orchestrator (`report` events) | carries the last status |
| `asked` | task → ask → user while open, task → user once answered or superseded | red while open; weight is the ask count |
| `reviews` | review → the task under review | the review loop; findings per node come in D4 |
| `ran` | review → review-node (`review_nodes`) | the flow's nodes as they run |
| `delivered` | task → pr | |

**The one data gap: who a message went to.** The `PostToolUse` hook logged `tool-used` with only the tool name, and
the receiver's `prompt` event has `from` but no recipient for messages to something that is not a session headwork
runs. Change: the `tool-used` event for `SendMessage` also stores `to` (the address in the tool input `PostToolUse`
receives) and the sender's `summary`, each trimmed, stripped of control, bidirectional and zero-width characters and
cut to 120 characters, never the body. The receiver side already logs `cross_session` and `from` on `prompt`, also for
senders outside headwork and for history before `to`; a message to a session headwork runs is counted there, and one
to anything else from the sender's event. A message to a name headwork does not know becomes an edge to an `external`
node. No schema change, an event payload field only.

## 4a. Backlog items

Items are nodes `item:<id>` (`kind` `item` or `epic`), read straight from `backlog_items` and `backlog_deps` by `Store::graph_facts`. A view draws an item when:

- it is linked to a task the view draws (any status); or
- it is open (`backlog`, `in_progress`, `blocked`) and has a dependency or a dependant; or
- it is an epic with an open child (the last two rules apply to every view except one task's, `--task`).

Edges: `depends` (item to the item it waits for, dashed amber), `part-of` (item to its epic, dotted) and `tracks` (task to its linked item, dotted); an edge whose other end is not drawn is left out. A past instant (`--at`, the replay slider) draws the items and the dependencies added by then (`created_at`); a replay draws only the dependencies still stored, so a dependency removed before now is not drawn even at an instant when it existed, and an item's status is not replayed from the `backlog-moved` events, so each shows the status it has now. The page has a `backlog items` filter and a legend entry; the project filter applies through the item's project.

## 5. Store: one new migration

Everything else is already in the store. One table, at the next free migration number:

```sql
CREATE TABLE agents (
    agent_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_type TEXT,
    description TEXT,
    model TEXT,
    spawn_depth INTEGER,
    parent_agent_id TEXT,
    first_at TEXT,
    last_at TEXT
);
CREATE INDEX agents_session ON agents(session_id);
```

Filled by the same reader that fills `usage` (`ops/usage.rs`), in the same transaction as the usage rows: when it reads
new lines of a subagent transcript it also reads its `agent-<id>.meta.json` (description, type, model, depth) and
widens `first_at` and `last_at` to the lines it read. The migration seeds the table from the `usage` rows already
stored (type and times); `headwork usage --backfill` rereads every transcript and adds the descriptions. The task is
not stored: it is the calling session's, so it follows a session `headwork import` adopts later. Descriptions are the
short labels the caller gave the `Agent` call, never prompts or output. A nested subagent's meta names its parent
(`parentAgentId`, kept in `parent_agent_id`), and it hangs off that agent; `spawn_depth` is shown in the details.

## 6. Endpoints

All `GET`, JSON, read only:

| Path | Returns |
|---|---|
| `/` | the page |
| `/api/graph?at=<time>&scope=<live\|all\|task:<name>>` | nodes and edges as Cytoscape elements, the state at that time (default now); the scope takes a task name like `headwork graph --task` |
| `/api/events?since=<event id>` | new events since an id, each mapped to a graph change (node added, state changed, edge pulse) |
| `/api/node/<id>` | details for the side panel: state history, last report, recap, tokens by type, prs, asks, findings |
| `/api/range` | first and last event times, for the slider |
| `/api/stream` | server-sent events: the same changes as `/api/events`, pushed as they land |
| `/api/gates` | the cpu and gpu slots, holders and waiters, read from the slot folders |
| `/api/backlog?status=&project=&owner=` | the backlog items (status `open` by default: backlog, in progress and blocked; or one status, or `all`; the old `state` with `dropped` for cancelled is read for one release), each with `status`, `blocked_reason`, `blocked_note` and `status_before_block`, `waiting`: the ids of the open items that wait on the user by the "Needs you" digest's own rule (`core::digest::waiting_items`), and `today`; the page's read-only backlog drawer polls it every 30 s while open |

**Live:** the page opens `/api/stream`; the server polls the `events` table every second for ids above the last one it
sent (cheap: the primary key) and pushes the mapped changes. A dropped stream falls back to polling `/api/events`.

**Replay:** `/api/graph?at=T` builds the graph from rows whose start is at or before `T` (tasks
while they exist then) and their states from the logs: task `transition`, `pr-state`, `job-state`, `job-started`,
`job-forgotten` and `review-<trigger>` events, and review nodes' start and finish times (`core/src/graph`,
`change_at`). The slider asks for
snapshots while dragged and plays forward from a point by applying `/api/events` in order at 1x, 10x or 100x, so
messages pulse and nodes appear in the order they happened.

## 7. The page

- **Canvas:** the graph, force layout (the user node and the orchestrator start pinned top and in the middle, but the crossing reducer below may move them); each task one node, its parts joined
  to it by lines. Toggle a hierarchical layout (orchestrator → tasks → parts → subagents).
- **Colour by state:** working green (a working task or live orchestrator also has a ring moving outward, still under `prefers-reduced-motion`), needs_user red, in_review amber, queued grey, closed faded (hidden by default
  in live view). The header checkbox "hide done" hides every faded node (closed or merged tasks, done jobs and subagents,
  merged or closed pull requests, finished reviews, answered asks); it is on at first load, and a choice stored in
  localStorage wins. Node size by tokens (log scale) when "size by cost" is on.
- **Side panel on click:** the node's details from `/api/node/<id>`; a "focus" button keeps only its neighbourhood (two
  hops), like Neo4j Browser's expand.
- **Filters:** project, task state, kinds (hide subagents, jobs, prs), time window, "only live".
- **Slider:** bottom bar from the first event to now, play and speed buttons, "live" snaps back to now.
- **Badges:** `silent` on a task that closes without asking (drawn as "· silent" after its state); open asks to the user counted on the user node; tick findings (`findings`) as warning badges on the node
  they name.
- **Search:** type a task or session name to jump to it.
- Light and dark theme from the system setting. No external requests.
  After each force layout (not the hierarchy one), `dash/untangle.js` treats edges as straight lines and moves nodes: a node swaps
  places with another of its own connected component, or moves beside a neighbour, only while that lowers the crossings of its
  edges. It stops after 15 rounds or 150 ms, and does nothing above 120 nodes or on a bad position. A failure keeps the force
  result. Relayouts animate from the old positions. `node dash/untangle.test.js` tests it.

## 8. Security

- Binds `127.0.0.1` only; a `--bind` other than loopback is refused.
- Read only: `query_only` connection, no endpoint changes anything (section 10 has the later actions).
- No message bodies, prompts or transcripts are ever served, only what the store already holds (reports, recaps,
  ask questions, finding summaries) plus the new message summaries.
- `Host` header checked against `127.0.0.1:<port>` and `localhost:<port>` (blocks DNS rebinding).

## 9. Milestones (worker-days, one worker each)

| | What | Done when |
|---|---|---|
| D1 (built) | `core/src/graph.rs` (facts → nodes and edges, `state_at`), the `agents` migration and its fill in the usage reader, `to` and `summary` on `SendMessage` events, `headwork graph [--json] [--at T] [--all] [--task <name>]` printing the graph as JSON or an indented text tree | table tests for every node and edge kind; `headwork graph --json` on this machine's store shows the review-flows lead with its parts and subagents (checked 2026-11-23 on a copy of this machine's store: `headwork graph --task headwork-agent-graph-dashboard`, a lead whose level-3 review ran as a part, showed the lead (built then with a box around its session; since 2026-11-23 one node), the part's node, the part's 206 subagents, the review with its flow nodes, and messages both ways between lead and part) |
| D2 (built) | `headwork dash`: `tiny_http` server, `/api/graph`, `/api/node`, the page with Cytoscape.js vendored, layout, colours, side panel, filters, search | the page shows the live board as a graph and matches `headwork board` |
| D3 (built) | live and replay: `/api/events`, `/api/stream`, `/api/range`, the slider, play speeds, message pulses | a task's whole life replays from its first event; a new spawn appears within two seconds |
| D4 (built) | cost and health: size by tokens, tokens by type in the panel, finding and ask badges, review nodes with findings, the hierarchical layout; a trial with a few live tasks, results in `docs/trials/` | the user has watched a real lead, its parts and a review on the page |
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
