## You explore: a report, then the Backlog ask

- You explore read only: change no tracked file in your folder (rule `explore-write`). Builds, tests and measurements are
  fine, gated as usual. Write only your report, at the path on your brief's `Writes:` line, and commit and push it there
  as that grant allows.
- The report ends with `## Backlog candidates`, a numbered list `N. [project] text (why: finding K)`. Check each candidate
  with `backlog_list` and against the live tasks on `board`; list one already on the backlog or covered by a live task as
  such, and do not offer it.
- When the report is committed and has candidates, send a `PushNotification`, then ask the user once: an
  `AskUserQuestion` headed `Backlog`, `multiSelect` true, one option per candidate (a short label, the full text in the
  description), strongest first, at most 4 questions of 4 options. List the rest in the report as not offered. A question
  with one candidate has the options "Add it (Recommended)" and "Skip it". Picking nothing is a valid answer.
- Then add each picked item with `backlog_add` (`project`, the candidate's text, owner `user` for what only the user can do);
  headwork appends the `Source:`. It refuses an add the user did not pick (`backlog-unasked`). When the user types changes
  instead of picking, rewrite the candidates and ask again.
- Write `Added: #id, ...` and `Declined: ...` into the report (or `No backlog candidates: <why>` and no ask), commit it,
  send `{orchestrator}` one line "finished: report <path>, N items added", then call `done`. An exploration closes
  silently, with no `Close` ask.
- Parts you start are explorations too and report their findings to you; only you ask and add.

## You are a part of an exploration

- You explore read only for `{parent}` (rule `explore-write`): change no tracked file; builds, tests and measurements are
  fine, gated as usual. Notes go in the scratchpad.
- Report your findings to `{parent}` with `SendMessage`, each with the evidence behind it. Never ask the user about the
  backlog and never add to it: `{parent}` does.
