# 2026-11-27: list the Remote Control leftovers instead of cleaning them

Every session runs `claude --remote-control <name>`. Claude Code archives that session in the Claude app only when
`claude` exits on `/exit`. A killed one stays listed as unreachable, and Claude Code has no command or API to archive it
(its docs say it does not archive them). Only the user can, by hand in the app.

Choices taken:

- Listing, not cleaning. `headwork leftovers` shows which sessions to archive, `headwork leftovers clear` records that the
  user did, and `doctor` and `board` count them. Reattaching a leftover with `claude remote-control --session-id <id>` or
  `claude --resume <id> --remote-control <name>` and then typing `/exit` might archive it within about four hours, but
  that is untested and the user chose to leave it out.
- The classification (`core/src/leftover.rs`). A session left gracefully when it has a `session-end` event with reason
  `prompt_input_exit` (the `SessionEnd` hook on `/exit`), or its exit code is 0 and it has no `exit-ignored` event.
  Anything else is a leftover, with a reason: `exit-ignored` present is "killed after /exit was ignored"; a non-zero
  exit code with no `exit-asked` is "ended without /exit"; the rest (no exit code, as after a crash, a restart or a
  session found gone) is "end unknown".
- Clearing is an `rc-archived` event on the session, actor `user`, so no schema change is needed. `clear --all` marks every
  leftover of all time, not only those in the window.
- The window is `[watch] leftover_days` (default 7; 0 is all time). Of 342 ended sessions on the first machine, about 165
  had exit code 1, nearly all before 2026-11-22, when closing began to type `/exit`; listing them all by default would
  bury the recent ones. `--all` lists everything.
- The key is shared, with a default, so no machine-only migration note is needed.
