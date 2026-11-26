# 2026-11-26: the to-do list lives in the store and `todo.md` is generated from it

The to-do list moves from a hand-edited `todo.md` into the store (migration 0025, `docs/todo.md`), because a file the
orchestrator edits as prose cannot be queried, linked to a task, or written by two sessions safely. The file stays, as an
export git can carry, and is overwritten from the store.

Choices taken: an import matches items by their text (whitespace collapsed, a leading `For you:` or `For you (...):` removed, the same reading `add` and `edit` give a text) so it is
idempotent and tolerates the export's own wording, and never deletes or changes a row; nothing is exported to a file that
exists until it was imported once, so a first run cannot replace a list the store has not read; a file edited by hand
since the last export is copied to `<home>/todo-backups` (newest 10) and then overwritten, because the store is the
source and the hand edit is still recoverable; dropped items stay in the store and are not exported, and only the 20 most
recent done items are, because the file is read by people who want the open list; a group `(superseded ...)` or
`(dropped ...)` closes an item as dropped, one that starts with `done` as done, whatever the rest says; `[todo] file` is a
machine-only config key, since its path differs per machine; a failed export never undoes a change; the owner column is the only source of ownership, so the `For you:` words are removed from the text and printed again by the export for the user's items; a link refused after a task started leaves the task running and says so, instead of rolling the task back. The tick stays quiet on a file that exists and was never imported: setting `[todo] file` is followed by the import step of the migration note, `headwork todo export` and every todo command report `NotImported` loudly, and a tick finding would nag a machine that set the key on purpose before importing. An export holds `<home>/todo-export.lock` (created exclusively, waited for up to 5 s, taken over when older than 10 minutes) so a tick export and a `todo add` export never overlap, and a hand edit is copied once per distinct content, so an export that keeps failing to commit does not push the real copy out of the ten kept.
