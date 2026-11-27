# 2026-11-27: a worktree a worker makes itself counts as its own

`worker-edit-outside` and `worker-git-outside` only trusted the task's own worktree and the helper rows `worktree_add`
records. Workers often made a worktree with a plain `git worktree add` (or the project's `worktree_new`) and then
edited and ran git there, which the rules refused although it was the task's own work.

This reverses the line in `rules/worker.md` that told workers never to run `git worktree add`: they may now, and the
hooks keep the result under the task's rules.

Choices taken:

- The `PostToolUse` hook reads a worker's successful Bash line (`core/src/guard/worktree_add.rs`, pure: follows `cd`,
  `pushd` and `-C`, reads `-b`, `-B`, `--detach` and relative or absolute folders) and records each folder as a helper
  row (`ops/helpers.rs`, `record_self_made`), the shape `worktree_add` writes, so close removes it the way it removes
  helpers. Only a folder holding a `.git` file (a linked worktree) that no row records is taken: another task's
  worktree, the task's own and a folder git refused are never recorded. A main-only task records them too.
- Git writes already counted every helper row as the task's folder, so no second list was added; the deny reason of
  `worker-edit-outside` now names the `worktree_add` tool.
- `git worktree add` and `git worktree prune` are allowed in the project's main checkout, beside `git pull --ff-only`;
  `remove`, `reset`, `rebase`, `tag`, `merge` and a wide `add` stay refused there.
- A worktree made in the same command that edits it (an edit before the call returns) is still refused once: it is
  recorded only after the command succeeds.
- Only a folder that did not exist before the line ran is taken. The `PreToolUse` hook logs `worktree-add-pending` with
  the folders the line's `git worktree add` commands aim at that are absent, and the `PostToolUse` hook records only
  those that the session's latest pending event names and that now hold a `.git` file. A failed add aimed at an
  existing, unrecorded linked worktree therefore never takes it over.
- The row is marked `made_by = 'worker'` (migration 0028). Its branch is the real one: after the add, git reports the
  branch checked out in the folder, and it is stored only when the add created it with `-b` or `-B` and git reports it
  now; a detached worktree, or one on an existing branch, stores none. A failure to record one folder is logged as a
  `note` event and neither skips the other folders nor the rest of `PostToolUse`.
- At close a worker-made row is never removed with the project's `worktree_remove`: it goes through plain
  `git worktree remove` (no force), so one holding work stays and is logged as `helper-worktree-refused`. Its branch is
  deleted, while empty, only when the row stores one.
- `git worktree add` in the main checkout is refused (`worker-git-outside`) when it carries `-f`, `--force` or `-B`,
  which can force-reset an existing branch or take over a folder git holds.
