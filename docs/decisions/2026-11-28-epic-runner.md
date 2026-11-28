# 2026-11-28: the epic runner (P5)

Phase P5 of `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md` (section 2, lifecycle steps 3 to 5, and the churn
cap of section 10). The behaviour is described in `docs/backlog.md`, section 13. It builds on P3 (dependencies, epic
columns, migration 0032) and P4 (the epic explorer, `tasks.epic_id`, migration 0033, `docs/decisions/2026-11-28-epic-explorer.md`).
Migration 0034 adds `epic_replans`. Park and resume on statuses (P6) and the skills (P8) are not part of it.

1. **Where it runs.** `spawn::pump` is split: `pump_queue` starts queued tasks as before, and `pump` is `pump_queue`
   followed by the runner (`ops::epic::run`), so every caller of `pump` (the tick's `queue` stage, a close's finish,
   `queue`, resume, the MCP tools) runs it. A spawn (`spawn_shaped`) calls only `pump_queue`, so the runner's own spawns
   never re-enter it. The runner is best effort: an error is logged as `epic-runner-error` and never fails the pump. It
   does nothing while the machine is not in `normal` mode (a park in progress), and its starts are returned as
   `Pumped` lines like any queued start.
2. **Double starts.** A child is started in three steps: a claim, the spawn, the link. The claim is one store call,
   `Store::claim_epic_child`, in an immediate transaction: it reads the child, refuses unless it is still `backlog`, its
   epic `in_progress` and (for a child that counts) the epic's running children under `max_parallel`, and moves it to
   `in_progress` through the ordinary guard (actor headwork, no task yet). A second runner (another tick, a close's
   finish, a resume) meets the child `in_progress` and skips it; the immediate transaction makes the two claims
   serial across processes. Then the ordinary `spawn` runs (gates, trust, layout, start guard and review flags unchanged)
   and the task is linked (`Relink`, allowed for headwork while no live task holds the item). A spawn that fails, or a
   task that is already `failed` when the spawn returns, blocks the child `task_lost` with the error as its note
   instead of linking, which is a re-plan trigger. A crash between the claim and the link leaves an `in_progress` child
   with no task; nothing restarts it by itself (it is not `backlog`), and it still counts against `max_parallel`, so the
   failure shows rather than doubling.
3. **What may start.** In one pass, for each epic `in_progress`, in id order, the pure `core::backlog::epic_step`
   decides from facts the store gathers (`Store::epic_facts`): a child starts only in `backlog` (so every dependency
   is done: an item with an open or cancelled one is blocked `depends` or `dep_cancelled`, and the guard refuses the
   start anyway), owned by an agent (a child the user owns is theirs to do, so the runner never spawns a task for it),
   with `not_before` passed (that day counts), and with its project one headwork runs (otherwise the child is skipped and
   the pass logs it once per child as `epic-child-skipped`).
4. **The plan settles first.** While a live explorer of the epic runs (a live task whose `epic_id` is the epic: the
   spun-up explorer or a re-plan child; or a live task linked to the epic itself, such as one the orchestrator spawned
   with `--backlog <epic>`), nothing of that epic starts and no automatic re-plan fires: an explorer adds
   children one call at a time, and a child started between its `plan_add` and its `plan_depend` would run out of
   order. Children already running keep running.
5. **`[epics] max_parallel`** (default 3, 0 for no limit, any layer) caps the children of one epic that are `in_progress`,
   counting children of every task kind except `explore`. Explore children (re-plans) do not count: at most one of them
   runs per epic, and when one is ready it starts alone in that pass (it then holds the rest by point 4).
6. **The session warning.** With `[watch] many_sessions` set (not 0), a pass starts at most `many_sessions` minus the live
   sessions, so the runner never pushes the machine over the warning by itself; when that leaves no room it starts
   nothing and logs `epic-held` (reason `many-sessions`) once per pass. Worker caps (`max_workers`) and gates work as
   for any spawn: a child whose task waits in `queued` is `in_progress` and counts against `max_parallel`.
7. **The spawn of a child.** Project the child's; kind its `task_kind`, else `code`; model its `model`, else the kind's;
   review its `review` (decided by `judged`, reason `planned by the epic explorer`); close mode and main-only from the
   kind as any spawn; slug `e<epic>-<child>-<first words>` (`core::backlog::child_slug`, then `-2`, `-3` as for an
   explorer). The brief is the stored `brief`; a child added without one (by the user with `--parent`) gets a brief made
   from its text and notes and the epic's text (`core::backlog::child_brief`). An `explore` child is spawned with
   `epic` set, so it is an epic explorer with the `plan_*` tools; its brief gets a `Writes:` line naming the epic's plan
   notes file when it has none, so `explore-no-report` passes.
8. **Automatic re-plans.** When no explorer of the epic is live and no explore child of it is open (`backlog`,
   `in_progress`, `blocked`), the step looks for a trigger, in this order:
   - a child blocked `task_lost`, `partial` or `dep_cancelled` whose block was not yet the cause of a re-plan (the cause
     is the event id of the `backlog-moved` into that block, recorded in `epic_replans.cause_event`, so a child blocked
     again later triggers again, and an unchanged block only once). P6 will add its respawn before the `task_lost`
     trigger; a task the orchestrator dropped after failed checks is `task_lost` by the task-end table and so covered;
   - no open child at all: "plan complete?".
   A trigger adds a child (actor headwork) of kind `explore`, text `Re-plan: <reason>`, with a generated brief
   (`core::backlog::replan_brief`: the epic, the reason, each cause with its note, the plan notes file on a `Writes:`
   line), records one `epic_replans` row per cause (or one with no cause), logs `epic-replan`, and starts it in the
   same pass. Never two at once per epic follows from the open explore child rule.
9. **The churn cap.** `epic_replans` rows count until they are cleared. When a trigger fires while the epic already
   has `[epics] max_replans` (default 5) counted re-plans, no child is added: the epic is blocked `decision`, note
   `epic-churn: <n> re-plans`, a new guard row (headwork may block an epic, and only an epic, for `decision`), its rows
   are cleared (`cleared_at` set: they stop counting but still record which blocks caused a re-plan), and `epic-churn` is
   logged. The trigger that hit the cap records no row, so after the user's unblock it fires a re-plan at the next pass. The digest lists the block (a `decision` block waits on the user) and the tick
   stage `epics` keeps the standing finding `epic-churn` with a notice while an epic is blocked with that note. When
   the user unblocks it, the epic is `in_progress` with a fresh count.
10. **Explicit re-plans** need nothing new: an explorer adds an `explore` child with `after` naming the children to look
    at, the graph sequences it, and the runner spawns it as an epic explorer (point 7).
