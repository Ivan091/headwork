# 2026-11-27: backlog dependencies and epics, phase P3

Items depend on other items through the table `backlog_deps`, and epics group items one level deep (`docs/backlog.md`,
section 11; migration 0032).

Choices taken: epics are one level (a parent is an epic, an epic has no parent), checked by the pure `check_parent` at add
time, and `kind` and `parent` are not editable afterwards. The gate holds however a block ends: one store function,
`settle_dependency_block`, makes the block of an item match its dependencies (a cancelled one gives `dep_cancelled`, else a
not-done one gives `depends`, else the dependency block is lifted), and runs in the transaction of every move of a
dependency (for its dependants), of the item itself after any move (so an unblock by hand or of a `decision` block with open
edges blocks it again), and after an edge is added or removed. A refusal of a cascaded move is logged, never fatal. The guard
adds headwork-only rows turning `depends` into `dep_cancelled` and back, keeping `status_before_block`, and refuses a start or
respawn by anyone while a dependency is open or cancelled. A dependency is added only to an item in `backlog` or blocked with
`backlog` as its status before the block. Reopening a done dependency now re-blocks its dependants that are in `backlog`: that
is what the rule gives, and an item already started is left alone. Removing a dependency settles the item again, so the way
out of a cancelled dependency is `backlog undepend`, and `depend` with several ids is one transaction, all or nothing. Only the user and the orchestrator change dependencies. The columns
`brief`, `task_kind`, `model`, `review` and `respawns` are stored for the epic explorer (P4) and runner (P5) and unused now.
`spawn --backlog N --after <task>` is refused when item N has item dependencies, so the item graph stays the one order.
