# Trial 7 (L5): one real lead with two parts

The trial milestone of `docs/leads.md` (section 8). One real lead, `headwork-leads-l5-trial` (task 22), ran on cmux with
two parts it started itself with `part_spawn`, and a third that had to wait at `max_parts`.

**Verdict on cmux (2026-11-21): passed.** Placement, the queue at `max_parts`, scoped `board` and `inbox`, `status`,
drop of a queued part, an ask from a background tab and a part's self-close all behaved as designed. The findings below
are small and none blocks leads.

**Verdict on wmux (2026-11-22): passed.** The same checks on Windows with wmux 2.64.3; one host difference (a new tab is
selected) and one adapter bug (a session's job refused breakaway, fixed in #26). See "wmux" below.

## Setup

- cmux 0.65.2 on macOS, headwork at schema 13 with L1 to L4 installed, `[leads] enabled = true`, `max_parts = 2`,
  `part_title = "{part}"` (default), `hooks.enforce = false`.
- The lead was spawned by the orchestrator as a top-level task (`shape: lead`).

| Part | Model | What it did |
|---|---|---|
| `doctor` (task 23) | sonnet | removed the stale `leads.enabled` line from doctor's `unenforced-keys` check, PR #59 |
| `probe` (task 24) | haiku | read-only: looked at its tab, `board` and `inbox`, asked the user one question from its background tab, reported, closed |
| `extra` (task 25) | haiku | spawned as a third part to hit `max_parts`, dropped while queued |

## What was seen

| Check | Expected (`docs/leads.md`) | Seen |
|---|---|---|
| lead's workspace | its own workspace, titled with the task name | `workspace:…061` `headwork-leads-l5-trial`; it did not take focus (the orchestrator's stayed selected) |
| parts as tabs | each part a tab of the lead's workspace, titled `{part}` | tabs `headwork-leads-l5-trial`, `probe`, `doctor`; store `host_place = tab`, same `host_workspace` as the lead |
| selected tab | opening a part leaves the lead's tab selected | the lead's tab stayed `[selected]` after both parts opened |
| `max_parts` | a third part waits right behind its lead | `extra` came back `waiting`: "its lead already runs 2 parts, the most `[leads] max_parts` allows"; state `queued` |
| `part_drop` of a queued part | it closes without a session | `queued -> dropped -> closing -> closed` within 1 ms, drop reason logged with the lead's name |
| scoped `board` (lead) | only the lead and its parts | the lead's and the probe's `board` listed tasks 22 to 24 only |
| scoped `inbox` | only the lead and its parts | empty for the probe, as nothing else of the lead waited |
| tree `board` (CLI) | parts indented under their lead | parts printed with `└` under task 22, each "as a tab" |
| `status` | one line per top-level task | `headwork-leads-l5-trial: working` |
| ask from a background tab | the user notices it through the notification and the cmux sidebar status | the user answered "Yes, I saw it" in the probe's tab; the store went `working -> needs_user -> working` |
| part's report | `SendMessage` to `{parent}`, the lead | the probe's report reached the lead by name |
| part's close | `place::release` closes only its tab | the probe's tab went; the lead's and `doctor`'s stayed; worktree and branch removed |
| part with a pull request | report `review` moves it to `in_review` | `doctor` reported PR #59, `working -> in_review` |

## Findings

1. **The lead's session reads "in its own pane".** The lead got a workspace of its own, but the store records
   `host_place = pane` and the CLI `board` prints "in its own pane <id>". A lead's placement could print "in its own
   workspace". Cosmetic.
2. **A lead's `board` lists every project.** Tasks are scoped to the lead and its parts, but the `projects` array still
   holds all 16 configured projects. A lead can only spawn parts in its own project, so the list could be cut to it.
   Cosmetic, costs tokens.
3. **The tick repeats `review-opened` after a report.** Right after `doctor`'s `report` (status `review`) moved it to
   `in_review`, the tick logged `refused: review-opened does not apply to a task in state in_review`. Noise in the log only.
4. **A queued part's drop logs its recap as the `tick`.** The `recap` event of the dropped `extra` has actor `tick`,
   though the lead dropped it. The same holds for the probe's close recap. Only the actor label is off.
5. **The blocked-part line in the lead's prompt was not exercised.** The lead got no user prompt while the probe waited
   on the user (its ask lasted about two minutes), so the `UserPromptSubmit` line "part `<name>` waits for the user" was
   not seen. The L4 tests cover it; to see it live, type into the lead's tab while a part's ask is open.
6. **Parts have no `PushNotification`.** The probe's brief asked for one before its ask; the tool was not in its tool
   set. The ask was noticed anyway through cmux's own notification and sidebar status.
7. **The doctor warning was stale.** `unenforced-keys` warned that `leads.enabled` was recorded only. Fixed in the
   trial by the `doctor` part (PR #59).

## Still to do

- Findings 1 to 4 (cmux) and the wmux findings below are small follow-ups; none blocks leads. cmux findings 1 and 2
  were fixed by the wmux trial's parts (#25, #27).
- The wmux restart check (last section): the user chose to run it later; it is on the to-do list.

## wmux (2026-11-22)

**Verdict on wmux: passed.** One real lead, `headwork-l5-wmux-trial` (task 201), ran on wmux with two real parts and a
third that waited at `max_parts`. Placement, titles, the queue, scoped `board`, tree `board`, `status`, a part's ask
from a background tab, the blocked-part line in the lead's prompt, a part's close and the slot release all behaved as
designed. One wmux behaviour differs from section 2 (a new tab is selected), and the trial found one adapter bug,
fixed in PR #26.

### Setup

- wmux 2.64.3 on Windows 11 (`wmux identify`), one wmux window, headwork 0.1.17 at schema 17, `[leads] enabled =
  true`, `max_parts = 2` (default), `part_title = "{part}"` (default), `hooks.enforce = false`.
- The lead was spawned by the orchestrator and started by the scheduled tick once the task it depended on merged.

| Part | Model | What it did |
|---|---|---|
| `own-workspace` (task 202) | sonnet | cmux finding 1: a lead's `board` line reads "in its own workspace", PR #25; asked the user `Review` before merging, from its background tab |
| `lead-board` (task 203) | sonnet | cmux finding 2: a lead's and a part's MCP `board` lists only its own project, PR #27 |
| `extra` (task 204) | haiku | spawned as a third part to hit `max_parts`, dropped while queued |

### What was seen

| Check | Expected (`docs/leads.md`) | Seen on wmux |
|---|---|---|
| lead's workspace | its own workspace, titled with the task name | `ws-9ed81ca2…` `headwork-l5-wmux-trial`, one pane, the lead's surface its only tab |
| does `new-workspace` take focus | unverified | no: `wmux new-workspace --title headwork-test-l5 --cwd … --panes 1 --layout single` left the active workspace as it was (`isActive` unchanged in `list-workspaces`, before and after) |
| which window it opens in | unverified | the only window (`list-windows` shows one); with several windows not tried |
| parts as tabs | `agent spawn --workspace W` without `--pane` lands as a tab of the lead's workspace | `wmux list-panes`: one pane, `tabCount: 3`; store `host_place = tab`, `host_pane = null`, same `host_workspace` as the lead |
| tab titles | the lead's tab `<name>`, parts `{part}` | `wmux tree`: `customTitle` `headwork-l5-wmux-trial`, `own-workspace`, `lead-board` |
| selected tab | opening a part leaves the lead's tab selected (cmux) | **differs:** each new part's tab became the selected one (`activeSurfaceIndex: 2` after the second part) |
| `max_parts` | a third part waits right behind its lead | `extra` came back `waiting`: "its lead already runs 2 parts, the most `[leads] max_parts` allows"; state `queued` |
| `part_drop` of a queued part | it closes without a session | `queued -> dropped -> closing -> closed` within 150 ms, `drop-reason` names the lead |
| scoped `board` (lead) | only the lead and its parts | tasks 201 to 203 only (204 after its drop with `all`) |
| tree `board` (CLI) | parts indented under their lead | `└ #202 … as a tab`, `└ #203 … as a tab` under #201; an open ask printed under its part |
| `status` | one line per top-level task | `headwork-l5-wmux-trial: working`, later `in review` |
| ask from a background tab | wmux blocked state on its surface, a notification | `wmux agent-state --surface <part>`: `state: blocked`, choices `1 Merge it (Recommended)`, `2 Leave it open`; `wmux list-notifications`: the part's `PushNotification` text and "Claude needs your permission", both on the part's surface in the lead's workspace |
| blocked-part line to the lead | "part `<name>` waits for the user" in the lead's next prompt | seen on every prompt of the lead while the ask was open: `headwork: part \`headwork-l5-wmux-trial-own-workspace\` waits for the user in its tab.` (cmux finding 5, now seen live) |
| slot release while a part waits | a part waiting in its own ask frees its slot | board header `3 of 8 slots (+2 waiting on the user, slot freed)` |
| part's report | `SendMessage` to `{parent}`, the lead | both parts' reports reached the lead by name |
| part's close | `place::release` closes only its tab | `lead-board`'s tab went (`wmux tree`: two tabs left); the selection moved to the next tab (`activeSurfaceIndex: 1`); worktree and both branches removed; then `own-workspace` closed the same way and the lead's tab was the only one left |
| part with a pull request | report `review` moves it to `in_review` | both parts `working -> in_review` on their report |
| lead's own close | held by `ask_before_close` until the user's `Close` go | the lead asks `Close` after this PR merges and closes on the go |

### Differences from section 2

1. **A new tab is selected.** wmux's `agent spawn` without `--pane` selects the tab it opens, so after each part
   starts, the newest part's tab shows instead of the lead's. cmux's `surface.create {focus:false}` keeps the lead's
   tab. wmux's `agent spawn` has no focus flag (`wmux help agent`), so headwork cannot keep the lead's tab in front
   without a select call after the spawn (`focus-surface`), which would steal focus whenever the user is in the
   workspace. Left as is: the user sees the part that just started, and the lead's tab is one click away.
2. **The blocked state's reason is wmux's, not the question.** The part's surface read `blockedReason: "Claude needs
   your permission"` (from Claude Code's `Notification` hook) with the ask's options as choices; the question text
   itself shows only in the tab and in the `PushNotification`.
3. **Parts do have `PushNotification` here.** The part's notification reached `wmux list-notifications` (cmux finding
   6 does not hold on this machine).

### Findings

1. **A session's Job Object refused breakaway (fixed, PR #26).** Every `update-triggered` from inside a session read
   `"note":"could not leave the job: Access is denied. (os error 5)"`, for example the one the lead's `part_drop` of
   `extra` started. The innermost job of a session process had `LimitFlags = 0x2000` (kill-on-close only, no
   `BREAKAWAY_OK`), and a `CREATE_BREAKAWAY_FROM_JOB` spawn from the lead's shell failed with error 5. So a detached
   `headwork update` started by a close inside a session stayed in the session's job and died with it. The job now
   sets `limit_breakaway_ok`. A close run by the finisher after the session's exit (the part's self-close) already
   had `"note":null` and its `update-started` followed. The scheduled tick's own `update-triggered` showed the same
   note (Task Scheduler's job); that one PR #26 does not touch.
2. **A dropped queued part triggers an update.** Dropping `extra`, which never ran, still started `headwork update
   --wait 30` (`by: close`). Harmless (it finds nothing new or the release of another merge), but a task that
   delivered nothing could skip it.
3. **The finisher's events read actor `user`.** After `lead-board`'s self-close, the finisher's `worktree-state`,
   `branch-deleted`, `cleaned` and `update-triggered` carry actor `user`, and the close recap carries `tick` (cmux
   finding 4 again).
4. **A part reported "merged" before its merge.** `lead-board` sent the lead "PR #27 merged" while the merge had failed
   on a `docs/decisions.md` conflict, then corrected itself after rebasing and merging. Every parallel part appending
   to `docs/decisions.md` conflicts with the others; the lead's own PR needed three rebases.
5. **The workspace's folder is the main checkout.** `wmux current-workspace` gives the lead's workspace `cwd
   S:/Code/headwork/headwork`, not the task's worktree; the sessions themselves run in their worktrees. Cosmetic.
6. **The `Review` ask lacked a file.** `own-workspace`'s ask logged `would-deny` by `ask-review-file` ("send the
   thing under review with SendUserFile first"): the lead's brief named only the PR URL. Under enforcement the ask
   would have been refused once; a lead's brief that asks for a review should say what to send.
7. **A part cannot see its own tab.** `own-workspace` found its workspace and surface with `wmux current-workspace`
   but not its title or whether it opened selected; only the lead's `wmux tree` shows those.
8. **A flaky Windows test.** #25's first Windows gate failed in `close_types_exit_and_kills_only_a_claude_that_stays`,
   which the change does not touch; a rerun passed.
9. **A workspace is found by title.** The wmux adapter's `ensure_workspace` reuses any workspace with the task's title; a stale one left
   from an earlier task of the same name would be taken over. Not seen; noted for the restart check.

## Planned: a worker survives a wmux restart

Not run in the trial: restarting wmux ends every pane, the orchestrator's too. The user starts it; the steps below are
what to run and what to check.

What should happen (from `ops/lost.rs` and `ops/resume.rs`): quitting wmux ends each pane's `headwork launch`, whose
Job Object ends its `claude`. The store sees the sessions end and marks their tasks `interrupted` (the `SessionEnd`
hook if it fires, else `board` or the tick through `lost.rs`). wmux restarts and reopens every Claude pane with a plain
`claude --resume <id>`: the same conversation, but outside headwork (no `HEADWORK_EXE`, so its hooks and MCP tools do
nothing). `headwork resume` then closes those restored panes (`restored-pane-closed`) and reopens each conversation
under headwork, the orchestrator too.

Steps:

1. Before: a lead with one part running (a harmless trial lead, so nothing real is cut off mid-edit), no job running,
   no ask open. Save `headwork board`, `headwork status`, `wmux list-workspaces`, `wmux tree --workspace <lead ws>` and
   `wmux agent-state` (it carries each surface's `sessionId`).
2. Quit wmux from its window (no `headwork park`: this is the unplanned case). Start wmux again.
3. Check, before anything else runs: which panes wmux restored and with what command (`wmux agent-state`: the same
   `sessionId`s on new surfaces); whether the lead's workspace came back with the part as a tab of it; that
   `headwork board` shows the lead and the part `interrupted` (and what it says of the restored sessions);
   `headwork tick --json` findings.
4. From a plain wmux terminal (not a restored Claude pane), run `headwork resume`.
5. Check: one `restored-pane-closed` per restored worker and the orchestrator (`headwork log`); every task back in
   `working` with the same session id; the lead in one workspace titled with its name (no second, duplicate workspace
   next to the restored one), the part as a tab of it titled `{part}`; the hooks live again (a `report` from the part
   works, `headwork board` shows `last_event_at` moving); the Remote Control name unchanged in the Claude app; the
   orchestrator back in its `orchestrator` workspace.
6. Record what differed in `docs/trials/e-leads/results.md` (wmux section) or a new trial folder.
