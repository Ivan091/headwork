# 2026-11-26: CPU and GPU gates in headwork, sessions uncapped

User decisions on the slots rework (`S:/Code/notes/slots-rework-2026-11-26.md`): agent sessions are not capped (a notice past
about 20 live sessions); CPU-heavy work takes one of `[gates] cpu = 3` slots and GPU work one of `gpu = 2`; the lock is the
machine-wide slot folders of the `heavy.sh` protocol plus events in the store; training takes `gpu` only; the guard rule
`heavy-ungated` runs in shadow first, then denies ungated cargo build/test/clippy/bench and `check.sh` with a hint, never
wrapping a command itself; token limits are warning only. This entry records how headwork does it.

## Classes and config

- `GateClass` is `cpu` or `gpu` (`core/src/gate.rs`, pure). `cpu` is cargo build/test/clippy/bench, `check.sh`, renders,
  scans and `git gc`; `gpu` is training.
- `[gates]` (machine layer): `cpu` (3), `gpu` (2), `dir` (the slot root, default `%LOCALAPPDATA%/heavy-slots`, else
  `~/.heavy-slots`), `held_long_min` (45, CPU holders only), `starved_min` (30). Counts are `NonZeroU32`. The environment
  `HEADWORK_GATE_DIR` overrides `dir` (tests).
- Every key has a default, so a machine needs no config change for the gates.

## The lock

- One folder per class under the root: `<dir>/cpu/slot-N` and `<dir>/gpu/slot-N`, N from 1 to the count. A slot is taken by
  creating its folder (atomic `create_dir`) and then writing `owner` inside it, line by line `key=value`: `pid` (the holder's
  OS-native process id), `native=1`, `class`, `task` (`HEADWORK_TASK`, may be empty), `worktree` (the git top level, else the
  current folder), `command`, `started` (local `%Y-%m-%d %H:%M:%S`, as `heavy.sh` writes it) and `epoch` (Unix seconds).
- A slot is stale when its holder is gone: for `native=1` the pid is checked with `proc::is_alive`; an owner without
  `native=1` was written by a Git Bash script whose `pid` is an MSYS pid, so on Windows it is looked up in Git Bash's process
  table (`ps -el`, the reader in `adapters/src/proc.rs`) and on unix it is native anyway. A folder with no owner file older
  than 10 s is stale. Taking over a stale slot holds `<dir>/<class>/reap.lock` (a folder, itself stale after 30 s), checks
  again, and removes the slot.
- A waiter writes `<dir>/<class>/wait-<pid>` (the same fields) while it waits and removes it when it gets a slot or exits.
  Waiting is first come first served: a waiter takes a free slot only when fewer live waiters started before it than there
  are free slots. Dead waiters' files are removed by anyone who sees them.
- The legacy root `<dir>/slot-N` (me-sim's and headwork's old `heavy.sh`) counts as `cpu`: after taking `cpu/slot-N`, a
  holder counts live legacy slots too, and when holders exceed the count it gives its slot back and waits again. Old scripts
  never look in `cpu/`, so until me-sim moves to the wrapper a legacy holder may still start past a full `cpu` class.
- Nesting is allowed per class: `HEADWORK_GATE_HELD` holds a comma list of classes the process tree already holds;
  `HEADWORK_HEAVY_HELD=1` (old `heavy.sh`) counts as `cpu`. A gated command whose class is already held runs directly.
- In CI (`CI` set) or with `HEADWORK_GATE_OFF=1` a gated command runs directly.

## Taking a slot

- `headwork gate run <class> -- <cmd...>` blocks until it holds a slot and never refuses. While it waits it prints to stderr
  `headwork: waiting for cpu (3/3: me-sim-x cargo test 4m, headwork-y check.sh 1m)` at once and every 30 s. It then runs the
  command with `HEADWORK_GATE_HELD` extended, passes stdin, stdout and stderr through, removes its slot when the command ends,
  and exits with the command's code. A killed gate leaves a slot whose pid is dead, which the next waiter or the tick reaps.
- `headwork gate status [--json]` lists each class: count, holders (task, command, age, stale or not), waiters.
- `job_start` and `job_register` take `gate: "cpu" | "gpu"`; the old boolean `heavy: true` still means `cpu`. A gated job is
  started as `headwork gate run <class> -- <argv>`, so a job waiting for its slot is a running job whose process waits: its
  status says `waiting for cpu` while a `wait-<pid>` file of its pid exists. Restart (resume) wraps the stored argv again.
  `job_register` records the class; when the registered pid holds no slot it takes a free one for that pid, else it warns that
  the job runs over the gate.
- No store migration: `jobs.heavy` is an integer, read as 0 none, 1 `cpu`, 2 `gpu`.
- Events in the store, written by the gate when `<home>/state.db` exists (never created by it): `gate-wait` (when it has to
  wait), `gate-acquired` (`waited_s`), `gate-released` (`held_s`, exit code), each with the class, command and task.

## Watching

- The tick reaps stale slots and dead waiters' files (event `gate-reaped`), and raises standing findings `gate-held-long` (a
  `cpu` holder older than `held_long_min`) and `gate-starved` (a waiter older than `starved_min`).
- `board` prints a line `Gates: cpu 2/3 (me-sim-x cargo test 4m, ...) · 1 waiting · gpu 0/2`; `status` ends a task's line with
  `waiting for cpu` when one of its waiters is live; the "Needs you" digest lists only starved waits and stale holders.
- The guard rule `heavy-ungated` (worker sessions) denies a Bash command that runs `cargo build|test|clippy|bench` or a
  `check.sh` outside `headwork gate run <class> --` or a `heavy.sh` wrapper, with the hint `headwork gate run cpu -- <cmd>`.
  It is shadow until the user lists it in `[hooks] enforce_rules` after a week of `shadow-report`.

## Sessions uncapped

- `[machine] max_workers`, `idle_overflow`, `[project.<name>] max_workers` and `[leads] max_parts` are optional; absent means
  no cap. `doctor` (check `config-keys`) warns when any is set. Idle and waiting detection stays, as information.
- The tick raises the standing finding `many-sessions`, with a notice, while more than `[watch] many_sessions` (20) sessions
  are live. It refuses nothing.

## Choices taken in the first pull request

- The owner file also carries `task_id` (`HEADWORK_TASK`), and `task` holds the task's name when the store resolves it,
  else the id: the wait message names tasks, and later readers join on the id.
- Waiters and holders are ordered by `(epoch, pid)`: seconds tie often, and every reader must order a class the same way.
- Owner and wait files are written to `<name>.tmp` and renamed into place, so a reader never takes half a `pid` for a dead
  holder; `.tmp` files older than 10 s are removed when a class is reaped.
- A `native=1` pid is checked through the system's process list on Windows (`sysinfo`), not `proc::is_alive`, whose
  `tasklist` takes a third of a second per pid, too slow for a poll every second; on unix `proc::is_alive` is used. When
  Git Bash's `ps -el` cannot be run, a non-native owner counts as alive.
- After a `cpu` holder takes its slot it gives it back only when the holders before it plus the live legacy slots fill the
  count, so of several new holders only the newest yield.
- The wait message adds `; N ahead` when live waiters came before this one.
- The gate's local `started` time comes from SQLite's `localtime` (the store crate's `local_time_now`), with UTC as the
  fallback; the workspace has no time zone crate.
- `check.sh` reruns itself as `headwork gate run cpu -- bash scripts/check.sh` when `headwork gate status` works and
  neither `CI`, `HEADWORK_GATE_OFF=1`, `HEADWORK_HEAVY_HELD=1` nor a held `cpu` says otherwise; `scripts/heavy.sh` and
  `scripts/test-heavy.sh` are gone.

## Second pull request

- `JobRow` keeps `heavy` (true when gated) and adds `gate`; `NewJob` takes `gate`. `job_status` and `headwork jobs` add
  `waiting_for`. `job_register` returns the row with a `warning` field when it runs over the gate.
- `Slots::claim_for` takes a slot for a pid without joining the queue and without the free-slot fairness check against
  waiters; a registered job is already running, so it cannot wait.
- The tick reaps stale slots even in shadow mode: it is the same cleanup every waiting gate does.
- Findings are keyed by slot, pid and epoch (holders) or pid and epoch (waiters), so each clears when that process ends.
- The digest shows gate findings at once instead of after `[digest] finding_hours`.
- `heavy-ungated` sees `headwork gate run ... -- <cmd>` and `heavy.sh <cmd>` as one command whose program is the wrapper, so
  the wrapped command is never inspected; reviewers keep their own rule `reviewer-write`.

## Review fixes

- A native owner whose pid runs a process that started more than 5 s (`PID_REUSE_SLACK_S`) after the owner's `epoch` is
  stale: the pid was taken over (`pid_taken_over`, start time from `proc::identify`). Owners without `epoch` keep the
  plain pid check.

## Not done here

- me-sim's `heavy.sh` becomes a wrapper around `headwork gate run cpu` with its `mkdir` fallback for CI; its renders and scans
  become `cpu` jobs and training a `gpu` job (a separate me-sim task).
- A free-disk wait inside the `cpu` gate, `long_jobs` default classes and a token window warning are later items.
