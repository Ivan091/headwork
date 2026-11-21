# Leads: one workspace per task, the user talks to the lead

Status: plan of 2026-11-21, nothing built. It turns the "lead variant" that `docs/sub-roots.md` recommended (a brief
convention) into a role headwork knows, and changes who the user talks to. Research behind it: `docs/cmux.md`
("Workspace groups", "Tabs in one workspace"), and the wmux notes below.

## 1. The model

The user's words (2026-11-21): the user talks mostly to the lead workers, not to the main orchestrator. The orchestrator
accepts tasks and hands each to a lead. When asked, it gives status, one line per task ("task x finished
successfully"), without details, and tells the user unprompted only when a task finishes or fails. The main feedback
and prompt loop runs in the leads.

| Who | What it is | Where it runs | Talks to |
|---|---|---|---|
| orchestrator | the dispatcher: takes the user's requests, makes one task per request, answers "how are things" in one line per task | its `orchestrator` workspace | the user (briefly), leads (status only) |
| lead | one per task: the user's counterpart for that task; refines the brief with the user, splits the work, does or starts the parts, delivers the pull requests | the first tab of the task's workspace | the user (in full), its parts, the orchestrator (status) |
| part | a session the lead starts for one piece (a separate review, a long render, a second branch) | a tab in the lead's workspace | its lead; the user when it must ask |
| helper | a short-lived program a lead or part opens with `helper_tab` | a tab in the same workspace | nobody |
| subagent | an `Agent` call inside the lead or a part | the lead's process tree, no tab | its caller |
| job | a long process registered with `job_start` | no tab, under its task | nobody |

So a task is a tree of depth one: task → lead → parts. Parts do not start parts.

**What the orchestrator still owns:** making tasks (`spawn`, now always a lead), the queue and its order, the machine
caps, rules (`rule_add`, `rule_retire`), park and resume, `clean`, `reap`, forced close and drop of any task, tasks that
span projects, and the status answer. It no longer writes detailed briefs: the brief is the user's request as given,
plus the project. The lead turns that into a plan with the user.

**What the orchestrator stops doing:** relaying asks, reviews and reports in detail, and spawning siblings on a lead's
proposal (the lead starts its parts itself, section 5). Its rules say: when the user asks about a task, answer from
`board` in one line (`working`, `needs you in <task>`, `in review <PR>`, `finished`, `failed`) and send the user to the
lead's workspace for anything more.

**Every top-level task is a lead.** A lead that needs no parts is exactly today's worker, so small tasks cost nothing
extra. A task the orchestrator spawns gets `lead = 1`; a part gets `lead = 0` and `parent_id` = its lead.

### Where the user's loop runs

- Asks: the lead (and a part) asks with `AskUserQuestion` in its own tab, as today. The blocked state, the push
  notification and the answer stay with that tab. The orchestrator's `inbox` still lists every ask, but its rules say
  "lead `<task>` needs you", never the question.
- Reviews: the lead sends the file with `SendUserFile` and raises the `Review` ask itself.
- Reports: a part sends its report to its lead by `SendMessage` (the worker rules name `{parent}`, the lead's address,
  instead of `{orchestrator}`). The lead sends the orchestrator only state changes: started, needs the user, in review
  (with the pull request URLs), finished, failed, each as `report` plus one short `SendMessage` line at the end.
- New work for a running task goes to its lead: the user types in the lead's tab. A new task goes to the orchestrator.

## 2. Placement: one workspace per task, sessions as tabs

The user chose tabs in one workspace over workspace groups (2026-11-21, `docs/decisions.md`). Each lead task gets a
workspace of its own titled with the task name (`<project>-<slug>`). The lead opens in it full-screen, replacing the
fresh workspace's shell, as `mode = "workspaces"` does today. Every part and every helper opens as a new tab of that
workspace (`Target::Tab`), with no splits. The workspace and the lead's tab carry the task name; a part's tab shows only
the part (`review`), since the workspace title already names the task: `[leads] part_title`, default `{part}`, with
`{name}`, `{model}` and `{effort}` as in `[layout] title`. So the workspace `headwork-lead-groups-plan` holds the tabs
`headwork-lead-groups-plan`, `review`, `render`.

This placement holds for lead tasks whatever `[layout] mode` says, because a task's sessions must stay together. The
`panes` grid and `tabs` mode stay for tasks spawned with `lead = 0` (none, once leads are on) and for an install with
`[leads] enabled = false`.

### The hosts

| | cmux 0.65.2 (this Mac, tried live) | wmux 2.64.2 (Windows, from source, not tried) |
|---|---|---|
| a workspace per task | `workspace.create {title, cwd, focus:false}` **[V]** | `workspace.create {title, cwd}` via `new-workspace` **[V in use]**; whether it takes focus and which window it opens in is **unverified** |
| a tab in it | `surface.create {workspace_id, initial_command, focus:false}`: lands in the workspace's pane as a new tab; the selected tab stays the same **[V]** | `agent spawn --workspace W --label L --cmd C` without `--pane` **[V in use: `helper_tab`]** |
| tab title | `tab.action rename` **[V]** | `rename-surface` **[V in use]** |
| close one tab | `surface.close {force:true}`; the other tabs stay **[V]** | `close-surface` **[V in use]** |
| workspace closes | with its last surface, or `workspace.close` **[V]** | `close-workspace` **[V in use]** |
| groups of workspaces | yes: `workspace.group.*` (section in `docs/cmux.md`) **[V]** | none: no group type or method in the source **[source]** |

So the tab plan works the same on both hosts with the trait as it is. wmux has windows (`window.create`, `list`,
`focus`) but no way to put a workspace into one over the socket, and `set_indicator` gives a workspace a coloured pill;
neither is needed.

### Hazards found

- cmux `surface.create` with a `workspace_id` that does not exist does not fail: it opens the surface in the **caller's**
  workspace (tried by accident, then closed). The cmux adapter must compare the `workspace_id` it gets back with the one
  it asked for, close the surface and fail when they differ. This matters already today, not only for leads.
- One tab shows at a time. A part that asks in a background tab is seen through its notification and its cmux sidebar
  status (`set-status headwork-<surface>`, shown on the workspace row) or the wmux blocked state on its surface. The
  lead is also told (section 6), so it can say "the review part waits for you in its tab".
- A user who drags a part's tab elsewhere (cmux `move-surface`, wmux drag) moves it out of the workspace. headwork never
  moves it back (wmux cannot); the tick only reports it.

## 3. Store: migration 0013

```sql
ALTER TABLE tasks ADD COLUMN parent_id INTEGER REFERENCES tasks(id);
ALTER TABLE tasks ADD COLUMN lead INTEGER NOT NULL DEFAULT 0;
CREATE INDEX tasks_parent ON tasks(parent_id);
```

- The group is the lead task: its parts are `tasks WHERE parent_id = lead.id`; its workspace is the lead session's
  `sessions.host_workspace`. No group table, and `sessions` (whose role `CHECK` cannot be altered) stays as it is: parts
  are `worker` sessions.
- Helpers stay out of the store, as today; they live and die with the workspace.
- Jobs already carry `task_id`; a part's jobs belong to the part.
- Existing tasks get `parent_id NULL`, `lead 0` and behave exactly as before.
- `core` gets a `TaskShape` enum (`Lead`, `Part { parent }`, `Plain`) built from the two columns, so a part with
  `lead = 1` cannot be represented.

Config, `[leads]`:

```toml
[leads]
enabled = true      # top-level tasks are leads; placement of section 2
max_parts = 2       # live parts per lead at once
part_title = "{part}"   # a part's tab title
```

## 4. Changes by area

**TerminalHost.** No new methods. The cmux adapter checks the workspace a new surface landed in (section 2). Optional
and cmux-only, not planned: `move_surface` to put a stray tab back.

**spawn** (`ops/spawn.rs`, `ops/place.rs`). `place::workspace_title` returns the lead's name for a part, the task's own
name for a lead. A lead opens like `workspaces` mode (replace the fresh shell); a part opens with `Target::Tab` in the
lead's recorded workspace, found by id first, then by title, and refused when the lead's workspace is gone (a part
never opens a workspace of its own). The part's worktree is made by the project's `worktree_new` like any task's.

**close** (`ops/close.rs`). A lead's `done` or close is refused while any part is live (`lead-parts-live`); a part
closes like a worker, and `place::release` closes only its tab, since the lead's tab still holds a session. The lead's
close then closes the workspace with it, and its helpers with it, as today. `close --force` by the orchestrator on a lead
first drops or force-closes its parts.

**drop.** Dropping a lead drops its live parts first (cascade); dropping a part is the lead's or the orchestrator's.

**park and resume** (`ops/park.rs`, `ops/resume.rs`). Park needs nothing new: it parks every live task. Resume brings
back each lead before its parts (ordered by `parent_id`, then age), so the lead's `ensure_workspace` reopens the
workspace and the parts become tabs in it again, in the order they were made. A part whose lead is not resumed (it
crashed with `resume_after_crash` off) waits for it.

**tick** (`ops/tick.rs`, `core/src/watch.rs`). New findings: `orphan-part` (a live part whose lead is closed, dropped or
failed), `stray-tab` (a part's surface outside its lead's workspace). The tick still retitles every tab. A blocked part
also gets a line in the lead's next prompt (the `UserPromptSubmit` hook already injects rules; it adds "part `<name>`
waits for the user").

**board and status.** `board` prints each lead with its parts indented under it, each with its tab. The orchestrator's
MCP `board` stays machine-wide; a lead's `board` and `inbox` show only itself and its parts. A new `status` (CLI and
orchestrator MCP tool) prints one line per top-level task, rolled up from its parts: `<task>: working`, `needs you`,
`in review <PR URLs>`, `finished`, `failed`. That line is all the orchestrator tells the user unless asked for more.

**clean, reap.** No change: they work per task and per worktree.

## 5. A lead starts its own parts

The lead starts parts itself, through MCP tools only (the `worker-admin` rule keeps refusing `headwork spawn` and the
rest through Bash):

| Tool | For | Limits |
|---|---|---|
| `part_spawn {slug, brief, model?, after?}` | a lead | its own project; `max_parts` live parts; the model within `[models] allowed` (Fable still needs the user); `after` names another part of the same lead |
| `part_drop {name}` | a lead | its own parts |
| `board`, `inbox` | a lead | itself and its parts |

**Slots.** A part takes a slot under the machine's `max_workers` and the project cap like every task, so the machine
never runs more sessions than today. On top of that a lead may hold at most `max_parts` live parts. A part that does not
fit waits in the queue right behind its lead's `queue_rank`, ahead of tasks queued later, so a started task finishes
before new tasks start. The lead holds its own slot while it waits for its parts.

**Names.** A part's slug is `<lead-slug>-<part>` (`lead-groups-plan-review`), so its task name, worktree and branch show
whose it is, and the live-name uniqueness check stays as it is. Its tab shows only `<part>` (section 2); its address
(`-n` name, `SendMessage` target) stays the full `<project>-<lead-slug>-<part>`.

**Rules and grants.**

- `rules/lead.md` (new, added to a lead's worker rules): you are the user's counterpart for this task; confirm scope
  with the user, start parts with `part_spawn`, take your parts' reports, tell the orchestrator only state changes,
  close only after every part has closed.
- `rules/worker.md` gets `{parent}`: a part's report goes to its lead; a lead's and a plain worker's to the
  orchestrator.
- `rules/orchestrator.md`: "Briefing a lead" becomes "Dispatching": the brief is the user's request; status in one line;
  send the user to the lead; never relay an ask's text.
- Guard: no new scope. A lead keeps the worker scope (its worktree, its helper worktrees). A part has its own. The
  `worker-git-outside` and grant rules apply unchanged. The `stop-unfinished` rule treats a lead with a live part like a
  worker with background work: it may stop and wait for the part's message.

## 6. Messages between them

The lead's address is its task name (its `-n` name and Remote Control name). A part's `SendMessage` to `{parent}`
reaches it like any session. The orchestrator gets: `report` rows (store) for every state change, and one `SendMessage`
line when the task finishes or fails. This is the "task x is finished successfully" line the user hears.

## 7. The user's answers (2026-11-21)

1. Status: one line per task when asked, plus one unprompted line when a task finishes or fails.
2. The lead starts its parts itself, with `part_spawn`.
3. Parts count against the machine's `max_workers` and the project cap, with at most `max_parts = 2` live per lead.
4. Every top-level task is a lead.
5. A lead's close is refused while a part runs; only the orchestrator's forced close ends the parts first.
6. Parts in another project go through the orchestrator as a new task.
7. A part's tab shows only the part (`review`), not the full task name; its session address stays the full name.

## 8. Milestones (worker-days, one worker each)

| # | Milestone | Effort |
|---|---|---|
| L1 | Store and core: migration 0013, `TaskShape`, `[leads]` config, `max_parts` in `StartFacts`, queue rank behind the lead, close and drop guards with live parts, table and proptest cases | 1 |
| L2 | Placement: the lead's workspace, parts and helpers as tabs, `part_title`, `release`, resume order, retitle; fake-host tests and a live cmux test | 1 |
| L3 | MCP and rules: `part_spawn`, `part_drop`, scoped `board` and `inbox`, `lead.md`, `{parent}`, the dispatcher `orchestrator.md`, the stop rule for a waiting lead | 1.5 |
| L4 | Close, tick, board: drop cascade, `orphan-part` and `stray-tab` findings, blocked-part line to the lead, tree `board`, `status` command and tool | 1 |
| L5 | Trial: one real lead with two parts on cmux, then on wmux; results in `docs/trials/` | 0.5 |
| | Total | about 5 |

L1 first; L2 and L3 can run in parallel after it; L4 after both; L5 last. Once L3 is in, the orchestrator's
behaviour changes for every new task; the answers in section 7 settle what it does.

## Deferred

cmux workspace groups (a group per task, the lead's workspace as anchor and one workspace per part) remain possible on
the Mac: the socket API supports them fully (`docs/cmux.md`). They were set aside for tabs, which work the same on both
hosts. Nested parts, cross-project parts by a lead, and per-lead GPU budgets are out of scope.
