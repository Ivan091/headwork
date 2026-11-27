# 2026-11-27: review-unfinished lets a reviewer wait on its own subagents

The shadow review of 2026-11-27 found all 170 would-denies of `review-unfinished` false: each came from a reviewer
lead that had started its node or re-check subagents in the background and waited for their notices. The stop rule
now skips a reviewer lead that started background work this turn (`waiting_background`, the fact that already excuses
`stop-unfinished`), and a running review counts as busy only when a node is ready and a `[reviews] parallel` slot is
free, so nodes queued behind a full set of slots no longer hold the lead. `Rechecking` stays busy: the store does not
know about the lead's re-check subagents, and the background skip covers that wait.
