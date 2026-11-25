# 2026-11-25: how concurrent tasks stop contending

Contention between concurrent tasks is handled by a delivery lock in each ship script, one-file-per-entry
logs, one owner with backoff for worktree removal, a machine-wide heavy slot for gates, per-project `max_workers`
caps and a `Touches:` line in briefs; a GitHub merge queue, strict up-to-date checks, a shared cargo `target/` and a
lower machine-wide `max_workers` were rejected (`docs/contention.md`).
