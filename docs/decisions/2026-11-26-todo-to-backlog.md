# 2026-11-26 The to-do list becomes the backlog; grooming and planning skills work it

The user asked for the scrum words: the to-do list is the backlog, ordering it is grooming, and getting a wave done is planning.

- Store migration 0027 renames `todos` to `backlog_items`, `todo_files` to `backlog_files`, the index, and the
  `todo-*` event types to `backlog-*`, so `headwork log` reads one vocabulary. Event `data` keys are kept as written.
  The migration is the ordinary automatic one, backed up to `<home>/backups` before it runs.
- `headwork todo`, the `todo_*` orchestrator tools, `spawn --todo`, the spawn tool's `todo` argument and `[todo]`
  stay accepted for one release. The old tools are no longer listed, so a session that starts after the update
  sees only `backlog_*`, while one started before keeps working. `[todo]` loads as `[backlog]` and `doctor` warns
  (`config-keys`), like `max_children`.
- The export moves to `backlog.md` when the machine file names it. A move is decided from `backlog_files.configured_at`
  (added by 0027, filled from `exported_at`): an export of the configured file whose path never was the configured one
  moves from the last configured path; an export to `--file` never moves. The old file is never deleted and is only
  touched when `[backlog] leave_pointer = true`, which replaces its text with one pointer line after a copy goes to
  `<home>/backlog-backups`. The pointer is written before the new file is recorded, so a failed move is retried by the
  next export. `[todo]` is folded into `[backlog]` before the config is read rather than a serde alias, because a
  machine mid-migration can have both sections in different layers.
- Grooming and planning are two skills in headwork's plugin (`plugin/skills/grooming/SKILL.md` and
  `plugin/skills/planning/SKILL.md`, shown as `headwork:grooming` and `headwork:planning`), not orchestrator rules:
  they are procedures the orchestrator follows when asked, and the rules only say when to use them. Grooming decides
  what is done and in which order (sort, drops, waves, one batched ask); planning gets an approved wave implemented
  (briefs from the code, flags, conflict checks, spawn linked to the item, follow-through, one recap per wave). Each
  names the other once; the ordering rules live only in grooming. The plugin is a directory marketplace loaded from
  the main checkout, so `skills/` needs no manifest change; a session sees new skills only once it starts after the pull.
- The migration note `2026-11-26-todo-to-backlog` covers the machine file's `[todo]` → `[backlog]` rename and the
  first export; a machine without `[todo]` passes its check.
