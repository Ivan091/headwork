# Decisions, one file per entry

Choices the design leaves open, taken without the user and open to revision. Each choice is a file of its own,
`docs/decisions/<YYYY-MM-DD>-<slug>.md`, holding one paragraph that names the choice and the reason. One file per entry
keeps concurrent tasks from conflicting at the tail of a shared log (`docs/contention.md`, item 2).

`docs/decisions.md` holds every entry up to 2026-11-25 and takes no new lines; references to it stay valid.
