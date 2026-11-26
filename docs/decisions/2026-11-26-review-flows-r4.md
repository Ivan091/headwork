# 2026-11-26: review flows R4, the plan flow and the surface

R4 adds the `plan` flow for plan documents (target `files`): nodes `feasibility` (from level 1; sonnet at level 1, opus
at 2 and 3), `consistency` (from 1, sonnet), `open-points` and `effort` (from 2, sonnet), `challenge-scope` (from 3,
opus) and `verify` (from 2, after every other node, one per finding, opus). It was written in the code flow's shape so
the loop needs no change.

`doctor` check `flows` fails while `[reviews] enabled` is on and only warns while it is off, for a broken flow and for an
unreadable `<home>/flows`, because a broken flow blocks a review the user can ask for only when reviews are on. The fix
hint names the folder only when the broken flow comes from it.

`board` prints the review summary on a line under the lead's task and not under its parts, and `status` ends the task's
line with `in review (level N)`, or `review requested (level N)` before the review starts; both the line and the JSON
`review` come from the same open review. The summary counts findings over every round up to the current one, like the
counts the state machine reads, and nodes that failed count as over with those that finished.
