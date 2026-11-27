# 2026-11-27: a task kind `explore` for audits and read-only research

Audits, explorations and shadow reviews end in a report, and what the user wants from it reaches the backlog only if
someone asks at the moment the report is fresh. Kind `explore` (`EXPLORE_KIND`) makes that part of the task: the explorer
writes its report, asks the user one `AskUserQuestion` headed `Backlog` (multi-select, one option per candidate) and
adds the picks itself (option B of `S:/Code/notes/explore-kind-proposal-2026-11-27.md`, section 3).

- Tools: `EXPLORER_TOOLS` (`backlog_list`, `backlog_add`) are listed and callable for a task of kind `explore` that is not
  a part, besides the orchestrator. `backlog_add` is refused as `backlog-unasked` once the picks are used up: the
  `PostToolUse` hook records each answered `Backlog` ask as the event `backlog-picked` (`{"count", "picks"}`, labels that
  equal an option, never "Skip it" or "Add none"), and the explorer may add that many items after the newest one. The quota caps the count only (`adds_left`): headwork cannot
  match an add's text to the short option labels, so adding exactly the picked candidates is the explorer's rule. The
  `backlog-added` event carries the task. headwork appends ` Source: <report path>, task <name>` when the text has no
  `Source:` (not `(task <name>)`: a trailing parenthesis is the backlog file's own link to a task and would be cut off).
- Model: Opus is the built-in default of the kind when `[models] allowed` has it, else `default`
  (`ModelsConfig::for_kind`); `[models.by_kind] explore` or a spawn's
  `model` still wins, so no shared config change is needed. A part of an explore lead is kind `explore` too and runs on
  Sonnet when allowed, unless the lead names another model (`ModelsConfig::for_part`).
- Refusal `explore-no-report`: spawn (the CLI and the orchestrator's tool alike) refuses a top-level `explore` task whose
  brief has no `Writes:` path strictly under `[hooks] brief_writable`, the same paths the guard later grants. Parts are
  exempt.
- Close: the default close mode of the kind is `silent`; the `Backlog` ask is the user's checkpoint, so the lead rules
  drop "Ask before you close".
- Guard: rule `explore-write` (modeled on `reviewer-write`): Edit, Write and NotebookEdit only in the scratchpad and the
  brief's `Writes:` paths, no git write outside those grants, no `gh pr` write; builds and tests stay allowed (gated). It is
  a normal rule id: shadow until listed in `[hooks] enforce_rules`. Known limit: a Bash write inside the folder (a
  redirect, `sed -i`, `tee`, `cp`, `mv`) is not caught; `rules/explorer.md` tells the explorer not to, and a test records it.
- Rules: `rules/explorer.md` is added to the worker and lead rules of a task of kind `explore`; a part gets only its own
  section, which sends findings to the lead and never asks about the backlog.

Deferred: the stop rule `explore-unasked` and a `done` refusal while candidates went unasked (the stop and close code are
being changed by other tasks), a `source` column on backlog items, dedupe of candidates against the backlog (the explorer
checks `backlog_list` by rule only), appending a note to an existing item, and a line in the "Needs you" digest.
