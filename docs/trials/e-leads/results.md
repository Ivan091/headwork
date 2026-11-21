# Trial 7 (L5): one real lead with two parts

The trial milestone of `docs/leads.md` (section 8). One real lead, `headwork-leads-l5-trial` (task 22), ran on cmux with
two parts it started itself with `part_spawn`, and a third that had to wait at `max_parts`.

**Verdict on cmux (2026-11-21): passed.** Placement, the queue at `max_parts`, scoped `board` and `inbox`, `status`,
drop of a queued part, an ask from a background tab and a part's self-close all behaved as designed. The findings below
are small and none blocks leads.

**Verdict on wmux: not run.** This Mac has cmux only; the wmux half runs on the Windows machine (see "Still to do").

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

- **wmux.** On the Windows machine, with `[leads] enabled = true`: spawn one lead, have it start two parts and a third
  at `max_parts`, and check the rows above, in particular the two cells `docs/leads.md` (section 2, "The hosts") marks
  unverified: whether `workspace.create` takes focus and which window it opens in, and that `agent spawn --workspace W`
  without `--pane` lands as a tab of the lead's workspace. Add the results as a "wmux" section here.
- Findings 1 to 4 are small follow-ups; none blocks leads.
