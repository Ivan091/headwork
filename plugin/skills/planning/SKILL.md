---
name: planning
description: The orchestrator's sprint planning and follow-through, as in scrum. It turns a groomed wave the user said go to into running tasks that get implemented, writing each task's brief from the code, picking its flags, checking conflicts with running tasks, spawning in wave order linked to the backlog item, watching, and sending one recap per wave. Use it in the orchestrator session after the user's go on a wave that headwork:grooming produced, and when a wave's tasks merge and the next wave is due.
---

# Planning a groomed wave

`headwork:grooming` decides what is done and in which order. This skill gets that work implemented. It starts only
after the user's go on a groomed wave, from the step lines of the plan note grooming wrote. headwork's orchestrator
rules own every tool call, flag table and the model ladder; this skill only orders the steps. The worked example is
`S:/Code/notes/todo-plan-2026-11-26.md` (its wave 1 was spawned by hand).

## 1. Per step: the brief

Read the code the step touches first: a subagent on Sonnet for anything more than a few files. Then write the brief:

- Goal: the user's words from the backlog item, quoted, and the outcome that ends the task.
- Scope: what changes, with the files and the risks the reading found (shared files, a store migration, hooks, the MCP
  surface, a deploy).
- Tests and checks that must pass, and the delivery (pull request, `Migration:` line, deploy timing where it applies).
- What not to touch: files other running or queued tasks own, user-only steps.
- A `Writes: <absolute path>` line per notes file outside the worker's folder.

## 2. Per step: the flags

Confirm the step line against the code: project, slug, `model` by the ladder, `effort` (`medium` unless the step is
delicate), `review`, `close`, `depends_on`, and the item's id as `backlog`. A step that covers part of one item gets an
item of its own first (`backlog_add`, then `backlog_edit` the old one), so closing each task finishes its own item.

## 3. Check before spawning

Grooming's ordering rules (its section 3), now against what actually runs on `board` and `status`: free slots, store
migrations and the heavy gate, plus any live task editing the same files. A conflict gets `depends_on` on that task, or
the step moves a wave.

## 4. Spawn and follow through

- Spawn the wave's steps in plan order; a step held by `depends_on` queues and starts by itself.
- Watch through the orchestrator rules (`status`, findings, the digest); a stuck or failed task gets the rules' retry
  or goes back to grooming.
- When the wave's last task finishes or fails, send the user one recap: per step, the pull request or why it failed,
  and what the next wave starts. Update the plan note, then plan the next wave from section 1.
