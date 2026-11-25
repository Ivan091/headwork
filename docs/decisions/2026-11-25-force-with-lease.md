2026-11-25 After a rebase a worker updates its own pull request branch with `git push --force-with-lease` instead of
opening a replacement pull request (the user's choice in headwork-contention-plan, replacing the rule of 2026-11-22):
11 of 12 pull requests closed unmerged in headwork were replacements made that way. A plain `--force`, the default
branch and another task's branch stay refused.
