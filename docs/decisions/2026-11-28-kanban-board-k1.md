# 2026-11-28: the kanban board K1 is a second view, built from a new endpoint

Backlog #203, phase K1 of `S:/Code/notes/kanban-board-proposal-2026-11-28.md`. Columns are the item statuses and the
tasks' rolled-up statuses are groups inside In progress, so a task is never shown twice; the pure build lives in
`core::kanban` and `/api/kanban` is a new endpoint rather than an extension of `/api/backlog`, which stays the drawer's
contract. No store migration and no replay (`?at=` is not taken).

`StatusLine` gains `task_id`, `model`, `effort`, `parts`, `state` and `updated_at` so the board joins status lines to items
without a second read; `status --json` carries them too. The page keeps one `EventSource`: `app.js` dispatches a
`dash-change` event on each `change` and `kanban.js` reloads on it, at most once a second. The drawer stays until K3.
