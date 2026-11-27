# The backlog in the store

Status: built (the store, `headwork backlog`, the orchestrator's tools, import, export, the spawn link, and the "Needs you"
digest of section 8).

The user's backlog of deferred work used to be one Markdown file, `S:/Code/todo.md`, edited by hand by the
orchestrator. A session that read it had to re-parse prose; two sessions could overwrite each other; nothing tied an item
to the task that finished it. The list now lives in headwork's store and `backlog.md` is generated from it.

## 1. The model

Migration 0026 added two tables as `todos` and `todo_files`; migration 0027 renamed them (see "Renamed from todo") (`crates/store/src/backlog.rs`, pure types in `crates/core/src/backlog.rs`).

`backlog_items` holds one row per item:

| Column | Meaning |
|---|---|
| `project` | The area heading: `me-sim`, `headwork`, `claude-config`, `me-studio`, `Setup`, `general` (items outside every heading, and everything that came from the Done and Dropped sections) |
| `section` | A `###` subheading under the project, or none |
| `text` | The item, one line, without the checkbox, the groups `added`, `not_before` and `due` came from, a trailing done or dropped group and a `For you:` prefix |
| `notes` | The indented sub-bullets, one per line |
| `state` | `open`, `done` or `dropped` |
| `owner` | `user` or `agent`: the only place ownership lives. A leading `For you:` or `For you (...):` (any case) is removed from the text when an item is imported, added or edited, and sets `user` |
| `added`, `not_before`, `due` | `YYYY-MM-DD` or none |
| `done_on`, `done_note` | When an item was done or dropped, and what was said |
| `task_id` | The task spawned for it |
| `position` | The export order: an import keeps file order, a new item goes last |

`backlog_files` holds one row per `backlog.md` path: `imported_at`, and the `export_hash` and `exported_at` of the last export.

Events: `backlog-added`, `backlog-done`, `backlog-dropped`, `backlog-edited` (also a reopen, a link and an unlink),
`backlog-imported`, `backlog-exported`, and `backlog-export-overwrote` when an export replaced a file edited by hand.

## 2. The file format

`parse` and `render` in `core/src/backlog.rs` are pure.

| In the file | Becomes |
|---|---|
| `# Backlog` and prose right under it | Ignored, not reported |
| `## <heading>` | The project; `### <heading>` the section, cleared by the next `##` |
| `## Done`, `## Dropped` (also `## Done (last 20)`) | Every item below is done, or dropped; project `general` |
| `- [ ] body` | Open |
| `- [x] body` | Done, or dropped when its trailing group starts with `dropped` or `superseded` |
| `- body` | Dropped under `## Dropped`, done under `## Done`; anywhere else it is reported |
| An indented `  - note` right after an item | One of its notes |
| `(done D: note)`, `(dropped D: note)`, `(superseded ...)` last on a checked item | `done_on` (the first date in it, if any) and `done_note`; removed from the text. In an open item it stays in the text |
| `(added D)`, `(not before D)`, `(due D)`, `(by D)` | The date field; the group is removed from the text only when it is exactly that |
| `added D` inside a longer group, `due D` anywhere | The date field; the text is left as it is |
| Anything else non-blank | Reported as `Unparsed { line_no, line, reason }` |

Lines are split on `\n` only, so a stray `\r` inside a line stays and one at its end goes. A line that starts with
`#` and is not `#`, `##` or `###` is reported too.

The export (`render`) writes the title and one intro paragraph, then every open item by project (in the order of the
projects' first items, `general` last) with the items that have no section first and each section after, then
`## Done (last 20)` with the 20 most recent done items. An item prints `For you: ` when its owner is the user (and only then), and `(not before D) (due D) (added D)` unless the
text already says that date. A linked open item ends with `(task <name>)`; the reader also takes the earlier
`(task <name>, <state>)`, and the state is shown by `backlog_list` and the CLI list only. Dropped items are not written. The reader understands what the renderer
writes: a test renders then parses and compares text, state, owner and dates.

## 3. Import

`headwork backlog import [--file P] [--dry-run]` reads the file (default `[backlog] file`), matches each item to the store by
text, and adds the ones the store lacks.

- Identity is the text with whitespace collapsed and a leading `For you:` or `For you (...):` removed. An item whose text a row already has,
  in any state, counts as `already` and is skipped. A second line with the same text in the same file counts as
  `duplicate` and is skipped.
- It never deletes or changes a row and never writes the file. Importing the same file twice adds nothing.
- It prints the counts and every unparsed line with its number and reason, and records `backlog_files.imported_at`.
  `--dry-run` prints the same and writes nothing, not even `imported_at`.

## 4. Export

`headwork backlog export [--file P]` writes the store to the file. The same export runs after every change (CLI or tool),
after a close that finished an item, and in every tick (a safety net: a store changed while no export was possible is
written at the next tick).

1. Render. When the content hashes to `backlog_files.export_hash` and the file exists, nothing is written (a file edited by
   hand stays until the store changes). The hash ignores the difference between `\n` and `\r\n`, so a Windows checkout
   does not look edited. A file that is missing is written again.
2. A file that exists while `backlog_files` has neither `imported_at` nor an `export_hash` for its path is never written: the
   command says "run `headwork backlog import` first". A file headwork created itself has an `export_hash`, so it counts
   as imported.
3. The new content is written to a temp file in the same folder (its name is unique per call). Then, when the file on
   disk no longer hashes to `export_hash` (edited by hand since the last export, or never exported after an import), it
   is copied to `<home>/backlog-backups/backlog-<timestamp>.md` (the newest 10 are kept), `backlog-export-overwrote` is logged with
   the copy's path and the command's output names the copy. A temp file that cannot be written makes no copy and no event.
4. The temp file is renamed over the file.

The path is made absolute (and resolved when the file exists) before it is used as the key of `backlog_files`, so `--file
backlog.md` and its absolute path are one record.

headwork never commits `backlog.md`; the orchestrator commits and pushes it in code-home as before. With no `[backlog] file`
and no `--file`, nothing is exported and the output says so. The key is machine-only (`machines/<name>/headwork.toml`,
`docs/config-sync.md`), since the path differs per machine.

A failed export never undoes a change: the change is in the store and the output carries `export failed: <why>`. After
a close a failed export logs `backlog-export-failed` with the error and the close stands. In the tick a failed export is
the stage's error (a `tick-error` finding) and the later stages run; a file that exists and was never imported stays
quiet there, since setting `[backlog] file` is followed by the import step of the migration note.

## 5. Commands and tools

```bash
headwork backlog [list] [--project P] [--owner user|agent] [--state open|done|dropped|all] [--json]
headwork backlog add <project> <text> [--section S] [--owner user|agent] [--not-before D] [--due D] [--note N]...
headwork backlog done <id> [--note N] [--drop]
headwork backlog reopen <id>
headwork backlog edit <id> [--text T] [--project P] [--section S|none] [--owner O] [--not-before D|none] [--due D|none] [--append T]
headwork backlog import [--file P] [--dry-run] [--json]
headwork backlog export [--file P] [--json]
```

A list line is `#<id> [<project>/<section>] <text> (owner, dates, task <name>, <state>)`.

An item must be writable to the file: the text is non-empty and one line, the project is non-empty, one line, does not
start with `#` and does not start with Done or Dropped (those headings hold finished items), the section is one line
and does not start with `#`, and each note is one line (a leading `- ` is dropped). `add` and `edit` trim, read the text
as an import reads a line (date groups become the fields, a `For you:` prefix the owner; an explicit option wins) and
refuse what is left with the reason; the store refuses it too.

The orchestrator has `backlog_list`, `backlog_add`, `backlog_done` and `backlog_edit` with the same filters and fields; each returns
the item lines and the export outcome. Workers have none of them, except an explorer (a task of kind `explore`, not a part):
it has `backlog_list` and a narrow `backlog_add` that adds only what the user picked in its `Backlog` ask (`backlog-unasked`
otherwise), logs `backlog-added` with its task and appends ` Source: <report>, task <name>` (`docs/decisions/2026-11-27-explore-kind.md`).

## 6. The spawn link

`headwork spawn ... --backlog <id>` and the `backlog` argument of the MCP `spawn` tool tie a task to an item.

- Refused, before the task exists, when the item is unknown, not open or a live task already holds it (a live task is
  one that is not closed, dropped or failed). A link refused after the task started leaves it running unlinked: the
  spawn output and the MCP result (`backlog_unlinked`) say so and why.
- `backlog_items.task_id` is set once the task exists; `backlog_list` shows the task and its state, and the export prints it.
- Where a task's state changes (`apply_in` in `crates/store/src/queries.rs`, the one place every path goes through:
  the finisher, the tick, `close --force`, a part's drop), a task that becomes `closed` finishes its item: `done` today
  with the note `task <name>` (the link ends), and `merged <url>, <url>` after it when the task had merged pull requests. A task that
  becomes `dropped` or `failed` frees the item, which stays open. The export follows every close
  (`ops::close`, after `Cleaned`; a no-op when nothing changed).
- A change of the item by hand (`backlog done`, a drop) unlinks it.

## 7. One machine

The store is local to each machine. Two machines that both import the same `backlog.md` and then change items would
diverge; the export is what git carries, and only one machine (today the PC that runs the orchestrator) writes it. The
other machine, if it ever needs the list, reads the file or imports it once; it does not export.

## 8. "Needs you": what waits on the user

`headwork digest [--json]` lists what waits on the user, at most ten lines with the header (`core/src/digest.rs`, pure;
`ops/digest.rs` gathers the facts; the reads are in `store/src/digest.rs` and touch no network and no process). Empty
when nothing waits. In order:

1. `ask <id> <task>: <first words> (waiting 3h)`: asks pending or open, oldest first; an ask of the orchestrator says
   `orchestrator`.
2. `backlog #<id>: <text, 80 characters> (due D)` or `(from D)`: open items whose due date is today or past, or whose
   `not_before` is today or past, by date.
3. `migration note <source>/<id>: <title>`: the notes the tick last found pending (its standing `migration-pending`
   findings; nothing is fetched, so a machine whose tick never ran lists none).
4. `pr <task>: <urls>`: open pull requests waiting for the user (definition below).
5. `finding <kind> <task> (open 5h)`: standing tick findings older than `[digest] finding_hours` (default 3), except
   pending notes.
6. `backlog #<id>: <text>`: the other open items of owner `user`.

Items linked to a task, done or dropped, and items with a future `not_before` are left out. When more than nine items
wait, eight show and the last line is `... and N more: headwork digest`.

A pull request waits for the user when it is open in the `prs` rows, its task is `in_review`, and the task's newest
session has ended or is marked as waiting on the user (`sessions.waiting_since`), or there is no session. The user is
the one who can review or merge it; a session that still works on it is not waiting for anyone.

Where it shows:

- At the orchestrator's session start, after its rules. The rules fill most of the hook's 10,000 characters, so a digest
  that does not fit whole is cut (fewer lines, then a one-line count), logged as `digest-deferred`, and handed over
  complete with the session's first prompt (`digest-delivered`).
- On top of `board` and `status` (the commands, not `--json`, and the orchestrator's tools of the same names), above
  `headwork recap <name>`, in the orchestrator's mechanical recap a park stores, and at the top of the summary
  `resume` writes (`recaps/park-<id>.md`).
- Once a day, as a host notification: the tick's `digest` stage, when `[digest] push` is on (default), the local hour
  is `[digest] push_hour` (default 9) or later, the digest is not empty and no `digest-pushed` event carries today's
  local date. A host that is down sends nothing and logs nothing, so the next tick tries again.

```toml
[digest]
finding_hours = 3
push = true
push_hour = 9
```

## 9. Renamed from todo

The feature was called "todo" until migration 0027. Each old name below is accepted for one release wherever its new
name is (the old tools, like the new ones, by the orchestrator only), and is then removed. None of them appears in
`--help` or `tools/list`.

| Was | Is | Until removed |
|---|---|---|
| Tables `todos`, `todo_files`, index `todos_by_task` | `backlog_items`, `backlog_files` (with the new `configured_at`), `backlog_items_by_task` | nothing to keep: migration 0027 moves the rows and rewrites the event types `todo-*` to `backlog-*` |
| `headwork todo ...` | `headwork backlog ...` | a hidden alias that prints `headwork todo is deprecated; use headwork backlog` on stderr |
| MCP `todo_list`, `todo_add`, `todo_done`, `todo_edit` | `backlog_list`, `backlog_add`, `backlog_done`, `backlog_edit` | accepted by `tools/call` |
| `headwork spawn --todo <id>`, the `todo` argument of the MCP `spawn` | `--backlog <id>`, `backlog` | accepted; the first of `backlog` and `todo` that is not null counts |
| Config `[todo]` | `[backlog]` | folded into `[backlog]` before the config is read, so one layer may still say `[todo]` while another says `[backlog]`; a key the two give different values is refused (`todo.<key> and backlog.<key> differ`). `headwork doctor` (check `config-keys`) names each `todo.<key>`; `todo.file` and `todo.leave_pointer` stay machine-only |
| The folder `<home>/todo-backups` | `<home>/backlog-backups` | left in place: never listed, pruned, moved or deleted; new copies go to the new folder |

The spawn result carries `backlog_unlinked` and, for the same release, the same value as `todo_unlinked`; events are
`backlog-*` with no alias. The earlier migration note `2026-11-26-todo-in-store` now uses the backlog names and points
to `2026-11-26-todo-to-backlog`.

### Moving the file

The generated file keeps the name the machine's `[backlog] file` gives it. To move it (for example from `todo.md` to
`backlog.md`, migration note `2026-11-26-todo-to-backlog`), point `[backlog] file` at the new name. `backlog_files`
records, per path, when it was last exported as the configured file (`configured_at`; migration 0027 fills it from
`exported_at` for the files exported before). An export of the configured file whose path never was the configured one,
while another path was, is a move from the path configured most recently (`move_source` in `core/src/backlog.rs`). An
export to `--file` never moves anything and never marks its path configured.

A move first deals with the old file, then writes and records the new one, then logs `backlog-moved` with both names;
the output says "moved from ...". The old file is never deleted. It stays as it was, or, with `[backlog] leave_pointer =
true` (machine-only, default off), its text is copied to `<home>/backlog-backups` and replaced by one line naming the new
file. A failure in that step writes nothing new, so the next export tries the move again; a file already holding the
pointer is not copied a second time. The pointer's hash becomes the old file's last export, so pointing `[backlog] file`
back at it later writes the list there without taking the pointer for a hand edit. A new file that already exists and
was never imported (a copy) is refused as in section 4 until `headwork backlog import`; the next export then moves.
