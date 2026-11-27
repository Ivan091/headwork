# 2026-11-27: park closes the orchestrator as its last step

`headwork park` used to leave the orchestrator running and let the restart end it. A killed `claude` leaves its Remote
Control session listed in the Claude app, so after every restart a dead orchestrator stayed in the list next to the new
one. Park now closes the orchestrator the way `close` closes a worker: `/exit` typed into its pane, a grace of
`[watch] exit_grace_s`, then its process tree ended and its pane closed.

Choices taken:

- It is the very last step, after `finish_park`, so the machine is already `parked` and every worker is closed; the
  orchestrator could still tell the user how the park went until then. The step never changes `safe_to_restart`: the
  restart would end the orchestrator anyway.
- `/exit` is typed twice at most: first plainly, so a turn still running (the orchestrator reporting the park) ends and
  then exits, then after Escape and Ctrl+U, so an open `AskUserQuestion` dialog does not take the Enter. A grace of 0
  asks nothing and ends the processes at once, as `close` does.
- A park that runs inside the orchestrator cannot end it: the `park` tool's detached run is still a descendant of its
  `claude` (Windows `taskkill /T` follows parent links), and a `headwork park` it runs through Bash lives in its pane.
  Inside means the park's own surface (`WMUX_SURFACE_ID`, `CMUX_SURFACE_ID`) is the orchestrator's, or the park's pid
  descends from one of the orchestrator's recorded pids (`core::reap::descends_from`, parent links with start times).
  Then the park starts `headwork park --close-orchestrator <id> --after <its pid>` detached and outside any surface, and
  exits; the closer waits up to two minutes for that pid to be gone, so its parent link is broken before the tree is
  ended, then closes the orchestrator. The report says `detached` with the closer's pid.
- An orchestrator running outside headwork (restored by wmux with a plain `claude --resume`) has no recorded pids: it
  gets the same `/exit`, counts as gone when the host no longer shows its Claude session, and its pane is closed.
- `park --keep-orchestrator` (the `park` tool's `keep_orchestrator`) keeps the old behaviour, logged `orchestrator-kept`.
- Resume needed no change to open exactly one orchestrator: after a park it opens the session the park recorded, the
  closed session is `ended`, and a pane the host restores for it after the reboot is closed as before. With
  `--keep-orchestrator` and no restart, the running orchestrator is left alone. The park tests check one orchestrator pane
  after each path (closed, kept then crashed at logon, outside headwork).
