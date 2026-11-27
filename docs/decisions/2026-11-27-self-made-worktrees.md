# 2026-11-27: a worktree a worker makes itself counts as its own

`worker-edit-outside` and `worker-git-outside` only trusted the task's own worktree and the helper rows `worktree_add`
records. Workers often made a worktree with a plain `git worktree add` (or the project's `worktree_new`) and then
edited and ran git there, which the rules refused although it was the task's own work.

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
