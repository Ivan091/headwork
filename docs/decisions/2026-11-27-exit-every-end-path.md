# 2026-11-27: every end path climbs the exit ladder before it kills

A Claude Code Remote Control session leaves the Claude app's list only when `claude` exits on its own (`/exit`, or a
second Ctrl+C at an empty prompt); a killed one stays listed as "Can't reach your computer", and Claude Code offers no
archive command. Since `/exit` came in (2026-11-22) the store still showed misses: a self-close through `headwork close
<own name>` typed `/exit` while its own Bash call was the running turn, so `/exit` waited behind the call until the
grace ran out and the tree was killed (`exit-asked`, `exit-ignored`, `self-close`, exit code 1); outside closes were
ignored twice in five cases and killed; the MCP `done` path failed a few times when the model kept writing past the
15 s grace; and park's workers, the tick's spawn timeout, merged grace and leftover-process kills, and resume's closing
of panes wmux restored with `claude --resume` killed live sessions with no `/exit` at all.

Choices taken:

- One ladder for every path. The steps are pure (`core/src/exit.rs`, table-tested); `ops/exit.rs` types them and waits.
  `Own` (the session closing itself, after its tool call returned): `/exit`; then Escape, Ctrl+U, `/exit`; then
  Ctrl+C twice. `Outside`: Escape, Ctrl+U, `/exit`, twice; then Ctrl+C twice. Each step waits up to `[watch]
  exit_grace_s` (15) for `claude` to be gone, so a ladder takes at most 45 s plus about 2.5 s of pauses before the kill;
  0 still turns asking off. The two Ctrl+C are 300 ms apart, inside the window Claude Code gives the second one.
- Several sessions in one pass: each input goes into every pane still running before the next, the pauses are taken
  once, and the grace is waited for all together. Park asks every worker this way after the deadline and before it
  closes their panes, so its time grows by one ladder, not one per worker.
- Events: `exit-asked` keeps `attempt` and `grace_s` and adds `step` (`exit`, `clear-and-exit`, `ctrl-c`) and `mode`
  (`own`, `outside`); `exit-ignored` and `exit-unasked` (the host refused a key; that session gets no further steps and
  is ended) as before. The old `interrupt` field is gone: `step` says it.
- A self-close through the CLI does not wait on its own exit. `headwork close` moves the task to `closing` as before,
  then starts a detached `headwork self-exit --task <id> --session <id> --after <its pid>` (hidden; every build must
  keep accepting it), logs `closer` with its argv and pid, prints that a detached closer asks the session to exit, and
  exits 0. The closer waits up to two minutes for the command to be gone, so the Bash call has returned, checks that
  the task is still `closing` and the launcher still runs, climbs the `Own` ladder, and ends `claude` only when it
  stays. It takes no close claim: the launcher's hand-off still starts the finisher that holds the claim, so two
  finishers never run one close, and a closer holding the claim would only make that finisher give up. When the closer
  does not start, the command ends `claude` at once, as before. A pinned session still gets its backup finisher.
- The MCP `done` keeps running in-process after its answer is out, now with the `Own` ladder: a model that keeps
  writing past the first grace gets Escape, then Ctrl+C twice, before the kill.
- Park's orchestrator close (PR #98) had its own copy of the ask; it now uses the `Own` ladder, which adds Ctrl+C twice.
- The tick asks only a session whose recorded `claude` still runs in a known pane, with the host up, and acts only
  under `enforce` as before. A spawn timeout waits up to 10 s for the launcher after `claude` left; merged grace and a
  closing task's leftover processes leave the rest to the launcher's hand-off when `claude` left, and kill as before when
  it stayed.
- A `finish` that meets a live `claude` (by hand, `close --force`, the reaper after a launcher died) climbs the outside
  ladder before it closes the pane.
- Resume knows no pid of a `claude --resume` wmux restored. It climbs the outside ladder in all such panes in one pass,
  counts a session gone once the host no longer shows it in that pane (a host that stops answering counts as showing
  it), then closes the panes as before; `restored-pane-closed` records `exited`. A restored pane thus costs up to one
  ladder at resume.
- Not covered: a runaway file on a stopped volume still ends the owning session at once, since waiting up to 45 s while
  a disk fills is the worse harm; helper tabs (`helper_tab`) run whatever the worker asked for, usually no Remote
  Control session, and are closed as before.
