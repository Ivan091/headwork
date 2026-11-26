# 2026-11-26: the to-do list lives in the store and `todo.md` is generated from it

The to-do list moves from a hand-edited `todo.md` into the store (migration 0025, `docs/todo.md`), because a file the
orchestrator edits as prose cannot be queried, linked to a task, or written by two sessions safely. The file stays, as an
export git can carry, and is overwritten from the store.

Choices taken: an import matches items by their text (whitespace collapsed, a leading `For you:` removed) so it is
idempotent and tolerates the export's own wording, and never deletes or changes a row; nothing is exported to a file that
exists until it was imported once, so a first run cannot replace a list the store has not read; a file edited by hand
since the last export is copied to `<home>/todo-backups` (newest 10) and then overwritten, because the store is the
source and the hand edit is still recoverable; dropped items stay in the store and are not exported, and only the 20 most
recent done items are, because the file is read by people who want the open list; a group `(superseded ...)` or
`(dropped ...)` closes an item as dropped, one that starts with `done` as done, whatever the rest says; `[todo] file` is a
machine-only config key, since its path differs per machine; a failed export never undoes a change.
