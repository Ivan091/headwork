# Sub-roots: should a root spawn other roots?

Status: exploration, 2026-11-14. No code changed. The idea under test: "a root could spawn other roots to split the task
even further". The cheaper alternative: a **lead** child that owns one large feature, does it with subagents, and asks
the root for sibling tasks chained with `--after` when it needs parallel sessions.

## Recommendation

**Use the lead variant. Do not build sub-roots now.**

The lead variant works today with no code change: it is a way of writing a brief. A real sub-root touches every layer
headwork has (schema, MCP role gating, guard scopes, rule templates, close, board, stop rule) for a gain the machine
cannot use yet: the 10-slot cap and one GPU are shared, so a second coordinator only reorders a queue the root already
orders. Revisit when one of the triggers at the end holds.

One finding stands on its own and should be fixed whatever is chosen: **a child can already spawn, close, drop and
queue tasks through the CLI**, unguarded and recorded as the user (see Q1).

## The questions, answered from the code

### Q1. What already works for a session that is not the root?

- **MCP: no.** `spawn`, `drop`, `close`, `queue`, `rule_add`, `rule_retire`, `park`, `resume` carry `roles: ROOT`
  (`crates/headwork/src/ops/mcp.rs:63-65`, `:86`, `:117`); `dispatch` refuses them for a child (`mcp.rs:431`). Roles are
  a closed pair, `Role::Root | Role::Child` (`crates/core/src/vocabulary.rs`, `text_enum!(Role …)`).
- **CLI: yes, and nothing stops it.** A child's Bash calls go through `check_child_command`
  (`crates/core/src/guard/mod.rs:243`), which only looks at user-only commands, background long jobs, and `git commit` /
  `git push`. `headwork spawn …` passes. The CLI runs every command as `Actor::User` (`crates/headwork/src/main.rs`,
  `Ctx::with_env`), so the event log says the user did it. The new task is a top-level task like any other; its rules
  tell it to report to `root`.
- **Hooks:** the guard knows two scopes. Root: writes only under the headwork home, temp and `root_writable`; no git
  delivery, builds or delivery scripts (`guard/mod.rs:205`). Child: writes only in its worktree. A sub-root fits neither:
  it should own no code like the root, but be scoped to its task like a child.
- **Stop rule:** a working child that ends a turn without asking, messaging or reporting is a would-deny
  (`crates/core/src/guard/stop.rs:55`). A coordinator that idles while its children work hits it on every idle turn
  unless it is a `keep` task or reports `progress` each time.

### Q2. Parent link and a tree view on `board`

There is no parent link. `tasks` has `depends_on` (one task, "start after it merged") and nothing else
(`crates/store/migrations/0001_initial.sql`). The board is flat. Adding `parent_id` is cheap: `ALTER TABLE tasks ADD
COLUMN parent_id INTEGER REFERENCES tasks(id)` in a new migration 0006, and an indented `board` render. A new session
role is not cheap: `sessions.role` has `CHECK (role IN ('root', 'child'))` (`0001_initial.sql:55`), and SQLite cannot
alter a CHECK, so a `subroot` role means rebuilding `sessions`. A flag on the task (`can_spawn`) with the session role
left `child` avoids that.

### Q3. Reports and asks routing

- **Asks never go through the root.** A child asks with `AskUserQuestion` in its own pane; the `PreToolUse` hook stores
  the ask and shows the numbered choices as that pane's blocked state (`crates/headwork/src/ops/hook.rs:566`,
  `open_ask`); the push notification comes from the asking session. The root only sees asks in `inbox`, which lists every
  task's asks machine-wide (`mcp.rs:582`). So "asks reach the user through two layers" is not true today and would not
  become true with a sub-root: grandchildren would ask the user directly, numbered, once. The only risk is a sub-root
  re-asking a grandchild's question, which a rule line forbids.
- **Reports are two channels.** The `report` MCP tool writes to the store (`last_report`, events), which anyone with
  `board` reads. The prose report goes by `SendMessage` to the session named `root`, hard-coded in `rules/child.md:25`. A
  sub-root's children would message the root, not their parent, unless the rule gets a `{parent}` placeholder filled
  from `parent_id`.

### Q4. Closing cascades

None. `close` ends one task's session, pane, process tree and worktree (`crates/headwork/src/ops/close.rs:49`). Closing
or dropping a sub-root leaves its children running, messaging a session that no longer exists. A cascade needs `close`
to walk `parent_id` (close descendants first, or refuse while any is live) and the tick to flag tasks whose parent is
closed.

**Park** needs nothing: it parks every task in `working`, `needs_user`, `in_review`, `parking` regardless of shape
(`ops/park.rs:172`). **Resume** brings back every parked and interrupted task oldest first (`ops/resume.rs:144`), so a
sub-root would come back before its children. Note that the root itself is not a task and `resume` does not restart it;
a sub-root, being a task, would resume better than the root does.

### Q5. Slot cap and per-parent budgets

The start guard counts every task in a slot-taking state against `machine.max_children` and the project cap
(`crates/core/src/lifecycle.rs:481`, `vocabulary.rs:109`). Grandchildren would count exactly like children, and the
sub-root itself holds a slot while only coordinating. A sub-root that spawns five tasks takes six of ten slots, and the
queue is first-in first-out by `queue_rank`, so it starves the root's other work. Per-parent budgets would be a new fact
in `StartFacts` and a new refusal. The GPU is not gated at all: jobs record `heavy` but nothing enforces `heavy_slots`
(only `jobs.rs` and `park.rs` touch it), so a second coordinator gains no GPU control either.

### Q6. Naming

Task names are `<project>-<slug>` (`TaskName`), unique among live tasks, and the Remote Control name equals the task
name. A tree needs no new naming: children keep flat names (`me-sim-crash-sim-physics`), the tree lives in `parent_id`.
Panes go to the project's workspace, so a sub-root's children sit beside every other task of the project. A sub-root
spanning two projects is possible in the store but its own worktree belongs to one project.

### Q7. Role rules for a sub-root

It would need its own rule file: owns no code (so no worktree: today every task gets a worktree or the main checkout,
`ops/spawn.rs`, `open_pane`), spawns only in its own project and only children of itself, closes only its descendants,
never touches `queue` order or rules, reports to its parent. In the guard it needs a third scope (root-like writes,
child-like task binding) and in the MCP server a third tool set with ownership checks on `close`/`drop`/`spawn`. The
`board` and `inbox` tools would need scoping or the sub-root sees the whole machine.

### Q8. Resume after a wmux restart

Covered by Q4: as a task, a sub-root resumes with its recap. Its children resume independently. Nothing new needed
beyond `parent_id` surviving, which it does in the store.

### Q9. The lead-child alternative

A lead is an ordinary child (Opus) whose brief says: you own feature X end to end; split it; do the parts in your own
worktree with subagents (`Agent`, with `isolation: "worktree"` where parts touch the same files); deliver in one or a few
pull requests. When a part truly needs its own session (a long render, a separate review, a different project), the lead
sends the root a split by `report` (`progress`, with the proposed slugs and briefs) or `SendMessage`, and the root
spawns them, chaining with `--after` where order matters.

What already works for it: `report` (store-visible progress), `SendMessage` to `root`, `spawn --after` / MCP
`depends_on` (`mcp.rs`, `spawn`), the slot cap and queue unchanged, park/resume unchanged, close unchanged, asks
unchanged. Its limits:

- Subagents live inside the lead's process tree: a park ends them and they do not resume; the lead's recap must say
  where each part stood. Long work belongs in `job_start` jobs, which do resume.
- `depends_on` is one task and means "merged". Fan-in (C after A and B) is not possible, and a dependency that is
  dropped instead of merged leaves its dependent queued forever (`crates/store/src/queries.rs:432`). The root works
  around both by spawning C only when A and B are done.
- Every extra session still costs the root one spawn decision. That is the point: one coordinator, one queue.

### Smallest change set, if sub-roots are built later

1. Migration 0006: `tasks.parent_id`, `tasks.can_spawn` (no session role change).
2. MCP: `spawn`, `close`, `drop` open to a child whose task has `can_spawn`, limited to its own project and its own
   descendants; `spawn` sets `parent_id`. `board`/`inbox` scoped to descendants for such a caller.
3. Guard: a coordinator scope (writes only in the home's task folder and temp, no git delivery or builds) and a task
   that needs no worktree.
4. Rules: `{parent}` in `child.md`, a `subroot.md` file.
5. `close` cascade or refusal; a tick finding for a live task whose parent is closed.
6. Per-parent slot budget in `StartFacts`.
7. `board` tree render; tests per transition and guard as the code rules require.

Roughly a milestone of its own, not a prototype.

## Triggers to revisit

- The cap rises well above 10, or a second machine joins, so one root cannot follow every task.
- A recurring kind of work needs more than three sessions coordinated over days (a video series, a multi-crate
  refactor) and the lead-plus-root split has been tried on it and failed.
- `depends_on` grows fan-in and drop handling, which a sub-root would need anyway.

## Follow-up regardless of the choice

Guard the CLI hole from Q1: a would-deny rule (`child-admin`, shadow like the rest) for a child running `headwork`
`spawn`, `queue`, `drop`, `park`, `resume`, `import`, or `close` of any name but its own, since children close
themselves with `headwork close <own-name>`. It belongs in its own small task, after the shadow week's report.
