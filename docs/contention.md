# Many workers in one project: where the errors and time go, and what to change

Evidence covers 2026-11-14 to 11-25: 14,452 headwork events and 265 tasks, the GitHub runs and pull requests (run history starts 11-19 to 11-22 depending on the repository), and the ship, worktree and check scripts. Nothing was changed.

Status (2026-11-25): the user approved all four follow-up tasks and chose force-with-lease for item 8. The analysis
changed nothing; the headwork part (items 2 for this repository, 3, 7, 8 in the guard, and `heavy_slots` deprecated
for item 4) landed in pull request #73; the me-sim, me-studio and claude-config tasks are the orchestrator's.

## What the numbers say

| Suspected cause | Measured | Verdict |
|---|---|---|
| Waiting for a worker slot | 14 of 263 tasks queued for more than 10 min. 26 slot refusals, 24 of them on one day (11-22) | **Small** |
| Git conflicts between tasks | About 4 real conflicts in 11 days. In headwork, 12 of 68 PRs were closed unmerged and 11 of those were replaced by a new PR after a rebase. Most overlap is in append-only files: `docs/decisions.md` (44 PRs, 104 pairs of PRs open at the same time), `CLAUDE.md` (54 pairs), `docs/HISTORY.md` | **Medium**, concentrated in a few files |
| Merging without testing the combined code | Neither `ship.sh` has a lock. Two ships both run the gate against the same master X; after A merges, B still merges onto X+A without its gate ever running on that tree. headwork's `strict` check is off. me-sim history shows the same commit pushed four times in 8 minutes (repeated rebase and force-push cycles) | **Real risk**, rare so far. Each time it happens, master breaks for everyone |
| Worktree removal churn | 209 `worktree-refused` events and 205 "worktree neither removed nor kept" refusals: 106 retries for one worktree (`transcript-animation`) and 15 Windows "Permission denied" errors. The tick and `worktree.sh remove-later` (180 tries, 10 s apart) both retry with no backoff | **Biggest error volume**, mostly noise, but it hides real findings |
| CPU and disk during gates | 16 cores. Gates are capped at 2 machine slots in me-sim only, and only for the cargo steps. me-studio has no cap: its gate takes 13.6 min median and 20 min p90 on CI. Each worktree gets its own cold `target/` of 7 to 38 GB. S: is 87% full | **Medium** on busy days (five or more live tasks) |
| Load and failures | Project-days with five or more live tasks: 25 stuck findings and 7 failed or interrupted tasks over 7 days. Days with one or two: 1 and 2 over 16 days. This is confounded by heavy render and training days | **Correlated**, not proven |
| CI | headwork: 14 of 142 runs failed. 7 were flaky tests on one OS, and 1 macOS job waited 931 s. Two earlier releases failed because no macOS runner was free. me-sim: about 13 job failures from `taiki-e/install-action` downloads during one burst on 11-19. Cancelled runs (31) were superseded pushes | **Medium**: flakes cost reruns |
| Time from review to merge | headwork p90 165 min, me-sim 53 min | **Large but not contention**: this is waiting for the user and for reviews |
| Rules that contradict each other | me-sim `CLAUDE.md:123` and `ship.sh` say rebase and `--force-with-lease`. The user's rule says push to a new branch and open a new PR | Causes the replacement PRs and some confusion |

Tokens: tasks with rework-like names used 2.7% of managed tokens. Tasks with more than one PR used 42%, but most of those PRs were planned stages, so 42% is a ceiling on rework, not a measurement of it. Not measurable from the store: push rejections, `index.lock`, cargo "Blocking waiting for file lock" (tool output is not logged), and wall time lost to rebases.

**In short:** the errors come mostly from housekeeping retries and CI flakes. The real correctness risk is the unlocked ship flow. Most conflicts sit in two or three append-only files. Slot waits are not the bottleneck.

## Decision table

| # | Change | Recommend | Expected gain | Cost |
|---|---|---|---|---|
| 1 | **A delivery lock per project in `ship.sh`** (me-sim, me-studio): a `mkdir` lock in the git common dir, like `heavy.sh`, held from fetch through rebase, gate, merge and fast-forwarding main. Re-gate only if master moved | **Yes** | No more merges onto an untested tree. No force-push loops. No two ships fighting over the main folder's index. A rebase conflict appears while the lock is held, not after a merge | ~60 lines per script. Ships wait for each other: at 2 to 10 min per gate and about 10 to 25 ships a day, worst-case waits are tens of minutes |
| 2 | **Split append-only files into one file per entry**: `docs/decisions/` in headwork, `docs/history/` in me-sim and me-studio, with an index generated or read by glob | **Yes** | Removes the largest source of overlapping PRs (decisions.md alone is about 40% of headwork's overlapping pairs) and most of the replacement PRs | One migration PR per repository, plus updating the rules that say "add a line to decisions.md" |
| 3 | **One owner for worktree removal, with backoff**: headwork retries with backoff for transient failures (1, 2, 4, 8, 16 min), then raises one ask; a lasting refusal asks at once. `remove-later` goes, or headwork leaves removal to it | **Yes** | Cuts about 400 of the 540 refusals and 209 worktree events, so real findings show | Small change in `ops/clean.rs` / the tick, plus `worktree.sh` |
| 4 | **One machine-wide heavy slot shared by all gates**: move `heavy-slots` from the per-repository git dir to `%LOCALAPPDATA%/heavy-slots`, use it in me-studio `check.sh` and headwork `check.sh`, and cover the whole gate | **Yes** | Stops 16 cores being split between 3 to 6 parallel gates. Steadier gate times and fewer stuck findings on busy days | Small. A gate may queue for a slot (visible in its output every 30 s) |
| 5 | **A cap per project**: `project.<p>.max_workers` already exists (`lifecycle.rs:598`) and is unset. Proposed: me-sim 5, me-studio 3, headwork 4 | **Yes, config only** | Limits the five-or-more days that line up with stuck and failed tasks | No code. A little more queueing on peak days, which today peak at 7, 6 and 5 |
| 6 | **Dispatch rules for the orchestrator**: each brief gets a `Touches:` line (areas or files). A task whose `Touches:` overlaps a live task in the same project goes into that task's lead or waits. An engine or shared-crate change ships first | **Yes, rules first** | Prevents the semantic conflicts that file overlap misses, at almost no cost | Edits to rules and the agent file. Enforced by judgement, not code |
| 6b | Code-enforced overlap check at spawn (headwork reads `Touches:` and queues on overlap) | Later, if 6 proves useful | Makes rule 6 automatic | Medium: parser, queue reason, tests |
| 7 | **CI hygiene**: `timeout-minutes` on every job, retry the release publish, fix the one live test race (update notices), cache or retry `taiki-e/install-action` in me-sim | **Yes** | A closer look at the 15 failed headwork runs: 5 were macOS jobs that never got a runner, 2 were HTTP 500s on publish, and the "flaky" tests had already been fixed on master except one race | A few PRs |
| 8 | **Make the rebase rule consistent**: allow `--force-with-lease` on the task's own branch (ship.sh already does this) and stop opening replacement PRs, or keep new PRs and fix me-sim `CLAUDE.md` and `ship.sh` | **Your call.** I lean towards allowing force-with-lease on own task branches | Removes the contradiction. Option A also ends the 11 replacement PRs | Edits to rules and memory, and possibly the `ask-push` guard |
| 9 | GitHub merge queue / `strict` up-to-date checks in headwork | **No** | Would test the combined tree, but every merge would wait for an extra 7 to 10 min CI round and need rebases. The release workflow already re-gates master after each merge | Slower delivery. The private repos cannot use it anyway (free plan, 403) |
| 10 | Shared cargo `target/` across worktrees | **No** | Saves disk and cold builds | `.cargo/config.toml` documents why it was avoided: worktrees overwrite each other's builds. sccache is already shared. Removing `target/` with the worktree (already done) keeps disk in check |
| 11 | Lower `max_workers` overall (8 to 6) | **No** | Slot waits are already rare | Throughput loss everywhere for a problem that is per project |

## Follow-up tasks per repository

| Repository | Task | Items | Review |
|---|---|---|---|
| me-sim | `ship-lock` | 1, 4 (move the slots machine-wide), `docs/history/` fragments (2), CI install retry (7) | 2 |
| me-studio | `ship-lock` | 1, heavy slot in `check.sh` (4), release-lock check before fast-forwarding main (like me-sim), `docs/history/` fragments (2) | 2 |
| headwork | `contention-fixes` (lead, about 3 PRs) | `docs/decisions/` fragments (2). Removal backoff and single owner (3). Either enforce `heavy_slots` or delete it from the config, since nothing enforces it today. Flaky tests, `timeout-minutes` and a macOS retry (7). Check why 155 asks were refused in `in_review` | 2 |
| claude-config | `dispatch-rules` | Orchestrator rules (6). Rebase rule (8). Per-project `max_workers` in the shared headwork config layer (5) | 1 |

Order: claude-config and the per-project caps first (rules only, immediate), then me-sim and me-studio, then headwork.
