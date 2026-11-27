# The video flow, headwork side

me-studio decides where a scene stands between "on master" and "public" (`S:/Code/me-studio/me-studio/CLAUDE.md`, "The
pipeline report"). headwork owns two things on top of it: the phone push of a failing hourly release, and the done check of
video tasks. Backlog item #216.

## 1. The phone push

`[machine] push_outbox` names me-studio's outbox (`%LOCALAPPDATA%/me-studio/studio-push-outbox.jsonl`, `<database stem>-push-outbox.jsonl`
beside the database). Absent means the tick reads nothing; `push = "none"` or a host that is down sends nothing.

The tick stage `push_outbox` reads the file and takes every parseable line after the last id sent, found by position
(`core::video_flow::new_entries`):

- A last line without its newline may still be written: it is ignored until it ends.
- A line that does not parse is skipped.
- With no cursor, or one the file no longer holds (a replaced file), every parseable line is new. The file holds one line
  per failing streak, so that is a few lines, never a flood.
- A missing file is quiet; an unreadable one is a `tick-error` finding.

Each new line goes out as the host notification and as a notice the orchestrator's next prompt carries (the `push =
"claude"` path: the orchestrator's `PushNotification` reaches the phone). The cursor is the event `push-outbox-sent`,
whose `id` field is the last line sent; it moves after each line, so no migration is needed and a failure resends only
the lines not yet sent.

## 2. The done check

`[project.<name>] done_check = { kinds = ["video"], command = ["<exe>", "pipeline-ack", "--project", "me-sim", "--json"] }`.
For a task of a listed kind, `done` must carry `scenes` (the scene ids it made); headwork runs `command` with the scenes
appended, for at most 60 seconds, and reads the exit code (`core::video_flow::done_verdict`):

| Exit | `done` |
|---|---|
| 0, all acknowledged | passes |
| 1, a scene waits | refused, `done-check-waiting`, with one `scene: stage` line per waiting scene; the lead puts them in its `Close` ask |
| 2, any other code, a timeout, a program that does not start | passes; the result says the check could not tell |
| no `scenes` for a task of a listed kind | refused, `done-check-scenes` |

The me-sim video tasks:

```toml
[project.me-sim.done_check]
kinds = ["video"]
command = ["S:/Code/me-studio/me-studio/target/release/me-studio.exe", "pipeline-ack", "--project", "me-sim", "--json"]
```
