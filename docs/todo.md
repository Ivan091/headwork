# The to-do list in the store

Status: Part A is built (the store, `headwork todo`, the orchestrator's tools, import, export, the spawn link). The
"Needs you" digest that reads the list is a separate part, not built yet.

The user's to-do list of deferred work used to be one Markdown file, `S:/Code/todo.md`, edited by hand by the
orchestrator. A session that read it had to re-parse prose; two sessions could overwrite each other; nothing tied an item
to the task that finished it. The list now lives in headwork's store and `todo.md` is generated from it.

## 1. The model

Migration 0025 adds two tables (`crates/store/src/todos.rs`, pure types in `crates/core/src/todo.rs`).

`todos` holds one row per item:

| Column | Meaning |
|---|---|
| `project` | The area heading: `me-sim`, `headwork`, `claude-config`, `me-studio`, `Setup`, `general` (items outside every heading, and everything that came from the Done and Dropped sections) |
| `section` | A `###` subheading under the project, or none |
| `text` | The item, without the checkbox, the groups `added`, `not_before` and `due` came from, and a trailing done or dropped group |
| `notes` | The indented sub-bullets, one per line |
| `state` | `open`, `done` or `dropped` |
| `owner` | `user` when the text starts with `For you` (any case), else `agent` |
| `added`, `not_before`, `due` | `YYYY-MM-DD` or none |
| `done_on`, `done_note` | When an item was done or dropped, and what was said |
| `task_id` | The task spawned for it |
| `position` | The export order: an import keeps file order, a new item goes last |

`todo_files` holds one row per `todo.md` path: `imported_at`, and the `export_hash` and `exported_at` of the last export.

Events: `todo-added`, `todo-done`, `todo-dropped`, `todo-edited` (also a reopen, a link and an unlink),
`todo-imported`, `todo-exported`, and `todo-export-overwrote` when an export replaced a file edited by hand.

## 2. The file format

`parse` and `render` in `core/src/todo.rs` are pure.

| In the file | Becomes |
|---|---|
| `# To-do` and prose right under it | Ignored, not reported |
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
`## Done (last 20)` with the 20 most recent done items. An item prints `For you: ` when its owner is the user and the
text does not already say so, and `(not before D) (due D) (added D)` unless the text already says that date. A linked open
item ends with `(task <name>, <state>)`. Dropped items are not written. The reader understands what the renderer
writes: a test renders then parses and compares text, state, owner and dates.

## 3. Import

`headwork todo import [--file P] [--dry-run]` reads the file (default `[todo] file`), matches each item to the store by
text, and adds the ones the store lacks.

- Identity is the text with whitespace collapsed and a leading `For you:` removed. An item whose text a row already has,
  in any state, counts as `already` and is skipped. A second line with the same text in the same file counts as
  `duplicate` and is skipped.
- It never deletes or changes a row and never writes the file. Importing the same file twice adds nothing.
- It prints the counts and every unparsed line with its number and reason, and records `todo_files.imported_at`.
  `--dry-run` prints the same and writes nothing, not even `imported_at`.

## 4. Export

`headwork todo export [--file P]` writes the store to the file. The same export runs after every change (CLI or tool),
after a close that finished an item, and in every tick (a safety net: a store changed while no export was possible is
written at the next tick).

1. Render. When the content hashes to `todo_files.export_hash` and the file exists, nothing is written. The hash ignores
   the difference between `\n` and `\r\n`, so a Windows checkout does not look edited.
2. A file that exists while `todo_files` has no `imported_at` for its path is never written: the command says "run
   `headwork todo import` first".
3. When the file on disk no longer hashes to `export_hash` (edited by hand since the last export, or never exported
   after an import), it is copied to `<home>/todo-backups/todo-<timestamp>.md` first (the newest 10 are kept),
   `todo-export-overwrote` is logged with the copy's path and the command's output names the copy.
4. The write is atomic: a temp file in the same folder, then a rename.

headwork never commits `todo.md`; the orchestrator commits and pushes it in code-home as before. With no `[todo] file`
and no `--file`, nothing is exported and the output says so. The key is machine-only (`machines/<name>/headwork.toml`,
`docs/config-sync.md`), since the path differs per machine.

A failed export never undoes a change: the change is in the store and the output carries `export failed: <why>`.

## 5. Commands and tools

```bash
headwork todo [list] [--project P] [--owner user|agent] [--state open|done|dropped|all] [--json]
headwork todo add <project> <text> [--section S] [--owner user|agent] [--not-before D] [--due D] [--note N]...
headwork todo done <id> [--note N] [--drop]
headwork todo reopen <id>
headwork todo edit <id> [--text T] [--project P] [--section S|none] [--owner O] [--not-before D|none] [--due D|none] [--append T]
headwork todo import [--file P] [--dry-run] [--json]
headwork todo export [--file P] [--json]
```

A list line is `#<id> [<project>/<section>] <text> (owner, dates, task <name>, <state>)`.

The orchestrator has `todo_list`, `todo_add`, `todo_done` and `todo_edit` with the same filters and fields; each returns
the item lines and the export outcome. Workers have none of them.

## 6. The spawn link

`headwork spawn ... --todo <id>` and the `todo` argument of the MCP `spawn` tool tie a task to an item.

- Refused, before the task exists, when the item is not open or a live task already holds it (a live task is one that is
  not closed, dropped or failed).
- `todos.task_id` is set once the task exists; `todo_list` shows the task and its state, and the export prints it.
- Where a task's state changes (`apply_in` in `crates/store/src/queries.rs`, the one place every path goes through:
  the finisher, the tick, `close --force`, a part's drop), a task that becomes `closed` finishes its item: `done` today
  with the note `task <name>`, and `merged <url>, <url>` after it when the task had merged pull requests. A task that
  becomes `dropped` or `failed` frees the item, which stays open. The export follows the close
  (`ops::close`, after `Cleaned`).
- A change of the item by hand (`todo done`, a drop) unlinks it.

## 7. One machine

The store is local to each machine. Two machines that both import the same `todo.md` and then change items would
diverge; the export is what git carries, and only one machine (today the PC that runs the orchestrator) writes it. The
other machine, if it ever needs the list, reads the file or imports it once; it does not export.
