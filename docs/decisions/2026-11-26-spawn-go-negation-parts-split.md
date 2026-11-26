# 2026-11-26: A brief may forbid a user-only command; parts carry their lead's go; allow-asks split

Audit items 7 and 13 of the 2026-11-26 headwork audit. The spawn guard (`core/src/user_go.rs`)
counted every user-only command in backticks, also in "never run `x`", so a brief could not warn against one.

**Negation is per command, not per line.** A command in backticks names nothing when the text between it and the
previous command in backticks (or the line's start) ends with a negation (`never`, `not`, `don't`, `no`, `nor`,
`mustn't`, `shouldn't`), optionally followed by one command verb (`run`, `call`, `use`, `execute`, `invoke`, `start`,
`type`), or when that text is only `or`, `nor` or a comma after a forbidden command ("never run `x` or `y`"). Quotes,
bold and bullets around the words are ignored, curly apostrophes read as straight. Anything else still counts, so the
guard stays closed for "never" in another sentence or clause, "do not forget to run `x`", "never skip `x`", "never
run `a` and `x`" and every fenced code block. A per-line rule was rejected: "Never guess; run `x`" would have passed.
The worker's `PreToolUse` guard still refuses the command itself, so a forbidden mention never becomes a go.

**A part may name what its lead was allowed.** For a part, `spawn_shaped` reads the lead's `user-only-allowed`
patterns; the part's brief and its new `part_spawn` `allow_ask` may name only those (refusal `NotLeadAllowed`), and
the main-only kind check does not apply, since the lead already holds the user's go for it. The lead's go is not
copied: the part's first action is its own `Allow` ask, and the user taps again in the part's tab.

**More than four allow-asks become several asks.** `MAX_ALLOW_ASKS` and its refusal are gone; the first-action
section groups the questions by four (`QUESTIONS_PER_ASK`) and tells the worker to call `AskUserQuestion` once per
group, one after another. The `PostToolUse` hook already grants per answered question, so nothing else changed.

The rules files are unchanged: they sit near the size a hook may inject, so the guidance lives in the `spawn` and
`part_spawn` tool descriptions and in the refusal texts.
