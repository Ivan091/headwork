# 2026-11-27: a planned review starts by itself on the committed HEAD, and late commits get one more round

A planned review (`--review N` at spawn) used to be only requested at the lead's first pull request report or at `done`; the
lead then had to call `review_start`, about ten minutes lost per review. headwork now starts it right away in
`StartMode::Auto`, which pins the committed `HEAD` (what the pull request holds) and does not refuse a dirty tree, since the
lead is usually still fixing CI. The lead's own `review_start` keeps `review-dirty`. When the start fails, the report's note
falls back to asking for `review_start` with the reason; `done` is refused either way (`review-planned`), and says the
review runs when it started.

Commits that land while a round runs are flagged at `review_deliver` (`reviews.late_commits`, migration 0030: the lead's
`HEAD` differs from `head_sha`) and checked by one extra fix-scoped round (`late && round <= rounds` in `round_due`), reusing
`round_base_sha..head_sha` and the scope of a later round. The flag clears when a round starts, so a second wave of commits
during the extra round does not start another.

Re-pinning mid-round was rejected: nodes already running review the old head, so moving it would make a round review a
mixture of two heads and break `reviewed_sha`'s meaning; a short fix-scoped round afterwards costs less than the confusion.
