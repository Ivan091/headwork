2026-11-25 After a rebase a worker updates its own pull request branch with `git push --force-with-lease` instead of
opening a replacement pull request (the user's choice in headwork-contention-plan, replacing the rule of 2026-11-22):
11 of 12 pull requests closed unmerged in headwork were replacements made that way. A plain `--force`, the default
branch and another task's branch stay refused.

The guard had no rule against force pushes except to the default branch (`ask-push` is the push-notification-before-ask
rule), so the new rule `worker-force-push` makes the policy explicit: a plain `--force`, `-f` or `+refspec` is refused,
and a lease (`--force-with-lease`, `--force-with-lease=<ref>`, `--force-if-includes`) is allowed only when every target
is the branch checked out in the task's folder. Granted notes-repository pushes keep refusing every force form.
