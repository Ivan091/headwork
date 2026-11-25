# 2026-11-25: force-with-lease to the task's own branch after a rebase

After a rebase a worker updates its own pull request branch with `git push --force-with-lease` instead of opening a
replacement pull request (the user's choice in headwork-contention-plan, replacing the rule of 2026-11-22), since 11 of
12 pull requests closed unmerged in headwork were replacements made that way.

The guard rule `worker-force-push` refuses a plain `--force`, `-f` or `+refspec`, and allows a lease
(`--force-with-lease`, `--force-with-lease=<ref>`, `--force-if-includes`) only when every target is the branch the
store records for the task's worktree (`Scope::task_branch`, else the branch checked out in its folder) and nothing
wide is pushed (`--all`, `--branches`, `--mirror`, `--tags`, `--prune`, `--delete`); `grants.rs` and the guard share
one classifier (`classify_push`), and granted notes-repository pushes refuse every force form.
