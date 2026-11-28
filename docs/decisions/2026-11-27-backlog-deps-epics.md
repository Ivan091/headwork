# 2026-11-27: backlog dependencies and epics, phase P3

Items depend on other items through the table `backlog_deps`, and epics group items one level deep (`docs/backlog.md`,
section 11; migration 0032).

Choices taken: epics are one level (a parent is an epic, an epic has no parent), checked by the pure `check_parent` at add
time, and `kind` and `parent` are not editable afterwards. The cascade runs inside the transaction of the move that causes it
(`write_move`), so every path (CLI, MCP, a task's end, close) frees or blocks dependants and a refusal of a cascaded move is
logged, never fatal. A cancelled dependency blocks its dependants as `dep_cancelled` (an item in `backlog`, or one blocked
`depends`, where a headwork-only guard row swaps the reason and keeps `status_before_block`); started and finished
dependants are untouched. Reopening a done dependency does not block its dependants again: they were freed on its done, and
re-blocking would stop work that may already run. Removing a dependency also frees an item blocked `dep_cancelled`, so the
way out of a cancelled dependency is `backlog undepend`. Only the user and the orchestrator change dependencies. The columns
`brief`, `task_kind`, `model`, `review` and `respawns` are stored for the epic explorer (P4) and runner (P5) and unused now.
`spawn --backlog N --after <task>` is refused when item N has item dependencies, so the item graph stays the one order.
