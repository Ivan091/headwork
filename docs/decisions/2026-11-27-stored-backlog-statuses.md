# 2026-11-27: stored backlog statuses, phase P1

Backlog items carry a stored status (`backlog`, `in_progress`, `blocked`, `done`, `cancelled`) instead of the old `open`,
`done` and `dropped`. Moves go only through the pure guard `core::backlog::transition` and `Store::move_backlog_item`,
adds through `transition(None, backlog, ...)`, and import is the one way in that skips the guard (it restores the
statuses the file names) (`docs/backlog.md`, section 10; design `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md`). P1 builds the
statuses, the guard, the task-end table and the user's and orchestrator's commands; epics, dependencies, the worker's
item tools, `done` outcomes and headwork's automatic respawns come in later phases; a respawn by hand, `spawn
--backlog` of an item blocked as `task_lost` or `partial`, exists now.

Choices taken for P1:

- `done` by the user or the orchestrator (`backlog done`, `backlog_done`) is allowed from `backlog`, `in_progress` or
  `blocked` only while no live task works on the item; headwork alone finishes an item a live task holds, when the task
  merges or closes.
- `cancelled` on an item a live task works on is refused with "drop the task first" (P2 makes the cancel drop the task).
  `cancelled` on `done` is refused. The orchestrator's cancel needs a non-empty reason, kept as the item's `done_note`.
- `backlog start` moves `backlog` to `in_progress` and spawns nothing. It is refused for an item whose project is not a
  configured headwork project (the design's "no spawnable path"). `spawn --backlog` links and moves the item from
  `backlog`, from `in_progress` with no live task, or from `blocked` for `task_lost` or `partial`; the spawn names the
  project, so it is never refused for that. The user may respawn too, not only the orchestrator and headwork.
- `block` by the user or the orchestrator takes `decision` or `event` with a note, from `backlog` or `in_progress`, and
  keeps the status to return to; `unblock` returns there. `reopen` takes `done` and `cancelled` back to `backlog`,
  clearing `done_on` and `done_note`. A start, relink or respawn sets the link to the new task (none for `backlog
  start`), a finish, a cancel and a `task_lost` block end it, every other move keeps it.
- The task-end table runs on every in-progress item linked to the task (it read one row before, #207). A merged task
  finishes the item at once with `task <name> merged <urls>`; a close whose item is still in progress finishes it with
  the merged URLs or `task <name>, no pull request`; a dropped or failed task blocks it as `task_lost`. `TaskOutcome`
  has only `Done` for now: the seam where P2's `partial` goes. A move the guard refuses there is logged
  (`backlog-move-refused`) and never fails the task's transition, since it runs inside it.
- Done at `merged` is the user's decision (D2). The merge keeps the link, so a merged task that opens another pull
  request (back to `in_review`) takes its done items back to `in_progress` (headwork's guard row done to in_progress,
  only with a live linked task). At `closed` (and `dropped`, `failed`) the done items still linked lose the link and
  their note is rewritten with every merged pull request. Blocked items linked to an ending task lose the link and keep
  the block. The digest counts an item as taken only while its linked task lives.
- Adds pass the guard (`transition(None, backlog, ...)`): a plain worker cannot add, an exploration adds as the
  explorer. Import is the exception: it restores the statuses the file names, without the guard.
- The item's move logs `backlog-moved` as the design says. That name already named the move of the generated file, so
  that event is now `backlog-file-moved`; old events keep their names and nothing reads them by type.
- The CLI's actor comes from `HEADWORK_ROLE` as the MCP server reads it: none is the user, `orchestrator` the
  orchestrator, `worker` a worker (refused for every item it is not linked to).
- Migration 0031 rebuilds the table and keeps ids. An `open` item linked to a live task becomes `in_progress`, any
  other `open` one `backlog`; `dropped` becomes `cancelled`. A link to a task the store does not have is dropped, since
  the rebuilt table checks its foreign key on copy.
- The guard knows epics and explorers only as a fact (`in_actor_epic`) that is always false in P1, so the explorer's
  rows are tested but no command makes them.
- The CLI trusts `HEADWORK_ROLE` for its actor. A worker could leave the variable out, so the guard rule
  `worker-backlog-cli` (`core/src/guard/backlog_cli.rs`; shadow, so it only logs, unless `worker-backlog-cli` is
  listed in `[hooks] enforce_rules`) denies a
  worker or explorer session every writing `headwork backlog` subcommand (add, done, start, cancel, block, unblock,
  reopen, edit, import, export); `list` stays allowed.
