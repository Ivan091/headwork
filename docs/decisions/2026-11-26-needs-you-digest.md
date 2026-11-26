# 2026-11-26: the "Needs you" digest reads only what the store already holds

The digest (`docs/todo.md`, section 8) is built from store reads alone, so the hook, `board`, `status` and the recaps
stay fast. Pull requests that wait for the user are the open `prs` rows of a task in `in_review` whose newest session
ended, never existed or is marked waiting on the user: only then is nobody else going to act on them. Pending migration
notes are the tick's standing `migration-pending` findings; `doctor` and `migrate list` fetch, the digest does not, and
a machine whose tick never ran lists no notes (the doctor check and `migrate list` still say so). Items linked to a task
are left out of the to-do lines, since a task already works on them; an item due today counts as due.

The orchestrator's rules leave almost no room under the hook's 10,000 characters, so the digest at session start is cut
to what fits and the whole digest follows with the session's first prompt, once. A worker's recap does not carry the
digest (it is about the user, not the worker); the orchestrator's park recap and the resume summary do. The daily push
uses the host notification the tick uses for findings and waits for a host that answers.
