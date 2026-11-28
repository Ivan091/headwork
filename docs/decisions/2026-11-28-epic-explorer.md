# 2026-11-28: the epic explorer (P4)

Phase P4 of `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md` (section 2, lifecycle steps 1, 2 and 6). The
behaviour is described in `docs/backlog.md`, section 12. No store migration: migration 0032 already holds `brief`,
`task_kind`, `model` and `review`, which this phase starts to write and read.

1. **Who is an epic explorer.** A task of kind `explore`, not a part, whose linked open item is an epic, or a child of an
   epic (then the parent is its epic). The link is the one fact both the spin-up and P5's runner write, so no new column
   marks an explorer; the child case is there now so a re-plan child (P5) gets the same tools without a change here.
2. **The spin-up spawns the explorer** through `spawn_linked`, so the epic moves to `in_progress` by the same guard and
   the same "check before, move after" order as `spawn --backlog`. The brief is generated in `core` (pure) and the model
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
   task's own item) so a later explorer or the runner can relink it, and a lost task blocks the epic as `task_lost` like
   any item. `record_partial_outcome` skips epics for the same reason.
5. **`EPIC_TOOLS`** are worker tools gated like `EXPLORER_TOOLS`: listed and callable only when `epic_of` finds an
   epic. The logic is in `ops/epic.rs`; the MCP layer parses. `plan_add` reuses the shaped add (parent, `after`, plan),
   so a child with an open dependency is added and settled to `depends` by the existing cascade, and the `Backlog` pick
   cap does not apply. Dependencies use new explorer-scoped calls instead of loosening `ops::backlog::depend`, which
   still refuses workers. Each call logs its events with the explorer's task; a move of an unlinked child carries the
   acting task in the event's task column.
6. **Rules.** `rules/explorer.md` gains "You explore an epic", given instead of the report-and-`Backlog`-ask section to
   an epic explorer. There is no `Plan` ask: decision D4 of the design (the spin-up is the user's go for the whole epic;
   the plan file is readable and every child cancellable at any time). Rules appended to a brief at spawn, when the
   plugin is not loaded, still get the report section, because the epic is linked only after the spawn; sessions with
   the plugin get the epic section from the `SessionStart` hook.

A child's `kind` is checked as a kebab-case word only: there is no list of task kinds to check it against.
