# The video flow, headwork side: choices

Backlog #216; the behaviour is in `docs/video-flow.md`.

- **The cursor is an event, not a table.** `push-outbox-sent` carries the last id; no migration (0031 belongs to another
  task). It moves only after the notification and the orchestrator notice went out: at least once, never lost.
- **An unknown cursor sends the newest line only.** A first run or a rotated file would otherwise push every old streak at
  once; the current streak is the one that matters.
- **The push goes out in every mode but `push = "none"`.** The host notification and the orchestrator's notice (which
  pushes with `PushNotification`) are both used; that is what `push = "claude"` has for a tick.
- **The done check fails open.** Exit 2, a timeout, a program that does not start and any other exit code let `done`
  pass, because a broken checker must not trap a finished task; each logs `done-check-unsure`.
- **The check is in the `done` tool only.** `headwork close` and the tick's close are decisions of the user or the
  orchestrator, who see the scenes themselves; the worker's declaration is what is checked.
- **The check runs before review start.** It follows the store-only refusals (open ask, running job, state) and precedes
  `start_planned`, which has side effects: a review must not start for a `done` that is refused anyway.
- **Scenes are an argument of `done`.** headwork cannot derive them from a task, so a task of a listed kind that names none
  is refused.
