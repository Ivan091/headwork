# 2026-11-28: the epic runner (P5)

Phase P5 of `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md` (section 2, lifecycle steps 3 to 5, and the churn
cap of section 10). The behaviour is described in `docs/backlog.md`, section 13. It builds on P3 (dependencies, epic
columns, migration 0032) and P4 (the epic explorer, `tasks.epic_id`, migration 0033, `docs/decisions/2026-11-28-epic-explorer.md`).
Migration 0034 adds `epic_replans` and `epic_churn_blocks`. Park and resume on statuses (P6) and the skills (P8) are not
part of it.

1. **Where it runs.** `spawn::pump` is split: `pump_queue` starts queued tasks as before, and `pump` is `pump_queue`
   followed by the runner (`ops::epic::run`), so every caller of `pump` (the tick's `queue` stage, a close's finish,
   `queue`, resume, the MCP tools) runs it. A spawn (`spawn_shaped`) calls only `pump_queue`, so the runner's own spawns
   never re-enter it. The runner is best effort: an error is logged as `epic-runner-error` (with the epic and the child
   when known) and never fails the pump. The same error of an epic is logged once until a pass of that epic goes
   through without one, which logs `epic-runner-ok`; the tick stage `epics` keeps the standing finding
   `epic-runner-error` for each epic in progress whose newest runner status is an error. It does nothing while the
   machine is not in `normal` mode (a park in progress), and its starts are returned as `Pumped` lines like any queued
   start.
2. **Double starts.** A child is started in three steps: a claim, the spawn, the link. The claim is one store call,
   `Store::claim_epic_child`, in an immediate transaction: it reads the child, refuses unless it is still `backlog`, its
   epic `in_progress`, no explore child of the epic `in_progress` (point 4) and (for a child that counts) the epic's
   running children under `max_parallel`, and moves it to `in_progress` through the ordinary guard (actor headwork, no
   task yet). A second runner (another tick, a close's finish, a resume) meets the child `in_progress` and skips it; the
   immediate transaction makes the two claims serial across processes. Then the ordinary `spawn` runs (gates, trust,
   layout, start guard and review flags unchanged) and the task is linked (`Relink`, allowed for headwork while no live
   task holds the item).
   - A spawn that returns an error after its task row exists (a later step of `spawn_shaped` failed) links the child to
     that task, found by the name the runner chose, so the task's lifecycle settles it and no unlinked task runs the
     child's work. With no row, or a task already over, the child is blocked `task_lost` with the error as its note,
     which is a re-plan trigger. The spawn error is logged with the child either way.
   - A link or block that fails is logged with the spawn error and left to the reconcile below.
   - **Reconcile.** Every pass looks, before the step, for children of the epic headwork claimed more than ten minutes
     ago (the newest move into `in_progress` was headwork's) that are still in progress with no task: a crash, a killed
     tick or a failed link between claim and link. Each is linked to the live task named for it (slug `e<epic>-<child>`
     or starting `e<epic>-<child>-`) or, with none, blocked `task_lost` with the note `claimed but never linked`, which
     a re-plan then answers. So a cut-off claim holds a `max_parallel` slot for ten minutes at most.
3. **What may start.** In one pass, for each epic `in_progress`, in id order, the pure `core::backlog::epic_step`
   decides from facts the store gathers (`Store::epic_facts`): a child starts only in `backlog` (so every dependency
   is done: an item with an open or cancelled one is blocked `depends` or `dep_cancelled`, and the guard refuses the
   start anyway), owned by an agent (a child the user owns is theirs to do, so the runner never spawns a task for it),
   with `not_before` passed (that day counts), in the epic's own project, which headwork runs, and of a task kind the
   runner starts: one of `[epics] kinds` (default `code` and `explore`), never `review` or `silent`, a kind main-only
   in that project or one of its `extra_writable` kinds, even when listed (`core::backlog::child_kind_refusal`). An
   explorer's plan alone is no go for widening what a worker may do. Any other ready child is skipped and the pass logs
   it once per child as `epic-child-skipped` with the reason. `plan_add` and `plan_edit` refuse a kind outside
   `[epics] kinds`, and `plan_add` a project other than the epic's.
4. **The plan settles first.** While a live explorer of the epic runs (a live task whose `epic_id` is the epic: the
   spun-up explorer or a re-plan child; or a live task linked to the epic itself, such as one the orchestrator spawned
   with `--backlog <epic>`), or an explore child of it is `in_progress` (claimed, its task perhaps not created yet),
   nothing of that epic starts and no automatic re-plan fires: an explorer adds children one call at a time, and a
   child started between its `plan_add` and its `plan_depend` would run out of order. Children already running keep
   running.
5. **`[epics] max_parallel`** (default 3, 0 for no limit, any layer) caps the children of one epic that are `in_progress`,
   counting children of every task kind except `explore`. Explore children (re-plans) do not count: at most one of them
   runs per epic, and when one is ready it starts alone in that pass (it then holds the rest by point 4).
6. **The session warning.** With `[watch] many_sessions` set (not 0), a pass starts at most `many_sessions` minus the
   sessions live at its start, the room shared by every epic of the pass and lowered by each start that leaves a task
   holding its child, so the runner never pushes the machine over the warning by itself; when that leaves no room for a
   ready child it logs `epic-held` (reason `many-sessions`) once per pass. Worker caps (`max_workers`) and gates work as
   for any spawn: a child whose task waits in `queued` is `in_progress` and counts against `max_parallel`.
7. **The spawn of a child.** Project the child's; kind its `task_kind`, else `code`; model its `model`, else the kind's;
   review its `review` (decided by `judged`, reason `planned by the epic explorer`); close mode and main-only from the
   kind as any spawn; slug `e<epic>-<child>-<first words>` (`core::backlog::child_slug`, then `-2`, `-3` as for an
   explorer). The brief is the stored `brief`; a child added without one (by the user with `--parent`) gets a brief made
   from its text and notes and the epic's text (`core::backlog::child_brief`). An `explore` child is spawned with
   `epic` set, so it is an epic explorer with the `plan_*` tools; its brief gets a `Writes:` line naming the epic's plan
   notes file when the `explore-no-report` guard would refuse it (`with_writes_line` asks the guard's own
   `explore_report_check` with the `[hooks] brief_writable` roots).
8. **Automatic re-plans.** When no explorer of the epic is live and no explore child of it holds the triggers (one in
   `backlog` or `in_progress`, `core::backlog::holds_triggers`, the same rule in the store's recheck), the step looks
   for a trigger, in this order:
   - a child blocked `task_lost`, `partial` or `dep_cancelled` whose block was not yet the cause of a re-plan (the cause
     is the event id of the `backlog-moved` into that block, recorded in `epic_replans.cause_event`, so a child blocked
     again later triggers again, and an unchanged block only once). A blocked explore child (a lost re-plan, or an
     explicit re-plan waiting on a lost child) holds nothing, and its own block is a cause like any child's. P6 will add
     its respawn before the `task_lost` trigger; a task the orchestrator dropped after failed checks is `task_lost` by
     the task-end table and so covered;
   - no open child at all: "plan complete?".
   A trigger adds a child (actor headwork) of kind `explore`, text `Re-plan: <reason>`, with a generated brief
   (`core::backlog::replan_brief`: the epic, the reason, each cause with its note, the plan notes file on a `Writes:`
   line), records one `epic_replans` row per cause (or one with no cause), logs `epic-replan`, and starts it in the
   same pass. Never two at once per epic follows from the trigger holders rule. A re-plan explorer may `plan_finish` the
   epic: the child linked to the acting explorer's own task is left out of the open children its finish counts, and the
   task's end finishes that child.
9. **The churn cap.** `epic_replans` rows count, one per re-plan child, until they are cleared; every explore child the
   runner starts that a trigger did not add (an explorer planned it) records a row with no cause at its claim, so a chain
   of explorers counts too. When a trigger fires while the epic already has `[epics] max_replans` (default 5) counted
   re-plans, no child is added: the epic is blocked `decision`, note `epic-churn: <n> re-plans`, a new guard row
   (headwork may block an epic, and only an epic, for `decision`), the `backlog-moved` event of that block is recorded
   in `epic_churn_blocks`, its rows are cleared (`cleared_at` set: they stop counting but still record which blocks
   caused a re-plan), and `epic-churn` is logged. The trigger that hit the cap records no row, so after the user's
   unblock it fires a re-plan at the next pass. The digest lists the block (a `decision` block waits on the user) and
   the tick stage `epics` keeps the standing finding `epic-churn` with a notice while an epic's current block is a
   recorded churn block (`Store::epic_churn_blocked`); the note is never parsed, so a block by hand with a similar note
   raises nothing. When the user unblocks it, the epic is `in_progress` with a fresh count.
10. **Explicit re-plans** need nothing new: an explorer adds an `explore` child with `after` naming the children to look
    at, the graph sequences it, and the runner spawns it as an epic explorer (point 7).
