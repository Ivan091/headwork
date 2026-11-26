---
name: grooming
description: The orchestrator's backlog grooming, as in scrum. It sorts the open backlog items by who can act, proposes drops, orders the work in waves with project, model, review level and dependencies, puts the user's decisions in one batched ask, and hands the approved waves to headwork:planning. Use it in the orchestrator session when the user asks for a plan or what to do next, or when the "Needs you" digest shows backlog items no task owns.
---

# Grooming the backlog

Grooming decides what is done and in which order; `headwork:planning` then gets each approved wave implemented.
headwork's orchestrator rules own every tool call here (flags, the model ladder, the review table, asks, `depends_on`);
this skill only orders the steps. Paths come from the machine profile: its notes folder (a repository in `[hooks]
orchestrator_repos`) holds the audits and plan notes, and its newest `plan-*.md` is the worked example.

## 1. Read

- `backlog_list` (open items, all projects), `board`, `status` and the "Needs you" digest.
- The newest audits and notes in the notes folder, the newest plan note, and any results the machine profile names.
  Note which items they confirm, settle or make stale.

## 2. Sort

Put every open item in one class:

| Class | Meaning |
|---|---|
| A | agents can do it alone |
| B | agents can do it after one decision of the user's |
| C | only the user can do it (a login, an upload, a release, a physical step) |
| D | waits for a date or an event (`not_before`, a merge, a release, a long run) |

Propose a drop for each stale item: done by other work, superseded, or no longer wanted per the notes.

## 3. Order in waves

Take A and the B items whose decision is in, and order them:

- Value first: what loses published work or approvals, then what leaves the user uninformed, then the rest.
- Dependencies before dependents; `depends_on` is one task, merged, no fan-in: a step that needs two waits a wave.
- At most one store migration in flight per project (migration numbers collide).
- Fit the slot caps (`[machine] max_workers`, each project's own cap) and the heavy gate: builds, renders and training
  that take heavy slots share a wave only as far as the slots go.
- A deploy only in the window its project allows (for example between scheduled runs), as its notes say.

## 4. Each step

One line per step: project, slug, scope (one sentence, the user's words where they exist), model by the ladder,
review level (default 1; 3 only for ledger, security, concurrency or schema work), close flag, `depends_on`, and the
backlog id it comes from. Planning checks these against the code before it spawns.

## 5. Ask once

One `AskUserQuestion` for all B decisions and proposed drops (up to four questions per ask; several asks only when
there are more), each question naming its item and options. C items go into the plan as the user's list, not into the
ask. Send the plan with `SendUserFile` first, so the user decides with it in front of them.

## 6. After the go

- Keep the plan in the backlog: `backlog_edit` items with their class and wave, `backlog_done` the dropped ones
  (`drop`), `backlog_add` what the reading surfaced.
- Write the plan to `plan-<date>.md` in the notes folder (classes, waves, step lines, the user's decisions) and commit
  and push it there.
- Hand wave 1 to `headwork:planning`. Groom again when the user asks, when new items pile up, or when a wave's results
  change the order.
