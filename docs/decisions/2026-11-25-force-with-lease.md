# 2026-11-25: force-with-lease to the task's own branch after a rebase

After a rebase a worker updates its own pull request branch with `git push --force-with-lease` instead of opening a
replacement pull request (the user's choice in headwork-contention-plan, replacing the rule of 2026-11-22), since 11 of
12 pull requests closed unmerged in headwork were replacements made that way.

The guard rule `worker-force-push` refuses a plain `--force`, `-f` or `+refspec`, and allows a lease
(`--force-with-lease`, `--force-with-lease=<ref>`, `--force-if-includes`) only when every target is the branch the
store records for the task's worktree (`Scope::task_branch`, else the branch checked out in its folder) and nothing
wide is pushed (`--all`, `--branches`, `--mirror`, `--tags`, `--prune`, `--delete`); `grants.rs` and the guard share
one classifier (`classify_push`), and granted notes-repository pushes refuse every force form.

A lease is accepted only in the shape `git push --force-with-lease[=...] [-u|-q|-v] [<remote>] [HEAD|<task branch>]`;
any other option or a `--` refuses it, and an unforced `--mirror` or `--prune` counts as a plain force. Before any git
rule, a push, a force word or an unknown subcommand (an alias) on a line that sets configuration (`-c`, `--config-env`,
a `GIT_CONFIG*` assignment in front of git) is refused, and so is a forced push whose words come from `$(...)`,
backticks or `${...}`. The guard reads command lines, so an alias or a setting already in the repository's config, or
a `GIT_CONFIG*` variable exported earlier, stays out of its reach.
