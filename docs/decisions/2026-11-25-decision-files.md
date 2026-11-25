# 2026-11-25: decisions one file per entry

Decisions are recorded one file per entry in `docs/decisions/` instead of new lines in `docs/decisions.md`:
the log was the file most often changed by pull requests open at the same time (44 PRs, 104 overlapping pairs), and
appends at its tail were the main cause of rebases and replacement pull requests. The old log stays as it is so that
existing references keep working.
