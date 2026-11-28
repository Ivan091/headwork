# 2026-11-28: the epic explorer (P4)

Phase P4 of `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md` (section 2, lifecycle steps 1, 2 and 6). The
behaviour is described in `docs/backlog.md`, section 12. Migration 0032 already holds `brief`, `task_kind`, `model` and
`review`, which this phase starts to write and read; migration 0033 adds `tasks.epic_id` (decision 1).

1. **Who is an epic explorer.** A task of kind `explore`, not a part, whose row records the epic it was spun up for
   (`tasks.epic_id`, migration 0033), written once by the spawn (`SpawnRequest::epic`). A first cut read the role from
   the item link, which made it vanish at `plan_finish` and at the release of a merged explorer, and gave the tools to any
   explore task linked to an epic's child. A role recorded at spawn is fixed for the session, so the rules the
   `SessionStart` hook renders never go stale. P5's re-plan children will set `epic` through the same spawn path,
   explicitly. The changing `plan_*` tools need the epic `in_progress`; `plan_list` always works.
2. **The spin-up spawns the explorer** through `spawn_linked`, so the epic moves to `in_progress` by the same guard and
   the same "check before, move after" order as `spawn --backlog`. Each spin-up gets a task name of its own
(`epic-<id>-<words>`, then `-2`, `-3`, ...), so a respawn after a lost explorer never meets the old brief file. A link
refused after the task started leaves the task running (as `spawn_linked` documents) and the output says the epic was not
moved to in progress. The brief is generated in `core` (pure) and the model
   and close mode are the kind's defaults (Opus, silent), not chosen per epic. The plan notes file goes under the first
   `[hooks] brief_writable` root, in `epics/`, because that root is what the brief grant and `explore-no-report` already
   check; with no root the spin-up is refused before anything moves rather than spawning an explorer that could not
   write its plan.
3. **Guard scoping.** `BacklogMove::actor_epic` gives the store the explorer's epic, and the guard learns two facts:
   `in_actor_epic` (a child) and `is_actor_epic` (the epic itself). The explorer's one move on its epic is `plan_finish`,
   `in_progress` to `done`, refused while a child is open (`ChildrenOpen`). It clears the link though the live task is
   the explorer's own: headwork's merge-finish keeps the link so a task can take its item back, but an explorer that
   finished its epic has nothing to take back. A child's dependency may point outside the epic (an epic child may wait
   for a plain item, as the design allows), so `plan_depend` checks only that the dependant is a child.
4. **The task's end leaves the epic in progress.** Only `plan_finish` or the user finishes an epic: an explorer's merge
   or close says the plan was written, not that the children are done. `on_task_end` takes the item's kind (passed in, so
   `core` reads nothing), releases the epic (`Unlink` while `in_progress`, allowed to headwork only for the ending
   task's own item, and only for an epic: `MoveFacts::is_epic`) so a later explorer or the runner can relink it, and a lost task blocks the epic as `task_lost` like
   any item. `record_partial_outcome` skips epics for the same reason.
5. **`EPIC_TOOLS`** are worker tools gated like `EXPLORER_TOOLS`: listed and callable only when `epic_of` finds an
   epic. The logic is in `ops/epic.rs`; the MCP layer parses. `plan_add` reuses the shaped add (parent, `after`, plan),
   so a child with an open dependency is added and settled to `depends` by the existing cascade, and the `Backlog` pick
   cap does not apply. Dependencies use new explorer-scoped calls instead of loosening `ops::backlog::depend`, which
   still refuses workers. Each call logs its events with the explorer's task; a move of an unlinked child carries the
   acting task in the event's task column. The epic itself is refused as a dependency of its child.
6. **Rules.** `rules/explorer.md` gains "You explore an epic", given instead of the report-and-`Backlog`-ask section to
   an epic explorer. There is no `Plan` ask: decision D4 of the design (the spin-up is the user's go for the whole epic;
   the plan file is readable and every child cancellable at any time). The role is on the task before the brief is
   written, so rules appended to a brief at spawn (no plugin) get the epic section too. A rules-folder `explorer.md`
   older than this section gets the built-in epic section appended.

A child's `kind` is checked as a kebab-case word only: there is no list of task kinds to check it against.
