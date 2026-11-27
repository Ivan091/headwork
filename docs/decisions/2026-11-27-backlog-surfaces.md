# 2026-11-27: backlog surfaces for stored statuses, phase P2

Phase P2 of `S:/Code/notes/backlog-workflow-v2-proposal-2026-11-27.md` on top of
`2026-11-27-stored-backlog-statuses.md` (`docs/backlog.md`, section 10, "Surfaces (P2)"). No store migration.

- The `partial` outcome is applied when the worker calls `done`, not when the task ends: a task that merged has already
  finished its item (user decision D2, done at merged), and the guard has no `done` to `blocked` move. A worker that
  wants its item left open says `partial` before the pull request merges. The note keeps the summary as one line
  without brackets, since a block note sits in the status tag.
- The digest lists `partial` blocks with `decision`, `task_lost` and `dep_cancelled`: a half-done item is as much a
  call for the user as a lost task. `depends` and `event` blocks wait on something else and stay out. Respawning by
  itself once is P6 (user decision D3), not here.
- The automatic item of a spawn without `--backlog` is made after the spawn, so a refused spawn leaves no item, and
  its start is made by headwork whoever spawned. The text is the first line with words of the brief.
- Cancelling an item with a live task drops and closes the task first, through the same checked drop the orchestrator's
  `drop` uses. The guard itself still refuses a cancel while a live task works on the item, so any other caller of the
  store keeps the old rule.
