# 2026-11-26: reviews deliver by a severity floor and caps, drop repeats, and stop without an important fix

The store held 30 finished reviews. Level 1 averaged 0.27M fresh tokens and 1.9 findings (0.7 important); level 2
averaged 2.4M fresh tokens and 28.5 findings (7.9 important, about 18 nits). The one level-3 run spent 13.75M fresh
tokens on 176 findings (36 important, 88 nits, 52 doubts) over 2 rounds, with 176 per-finding verify nodes and 152
fixes. The cost of one important finding is about the same at every level; the waste is the nits, each verified by its
own subagent and fixed one by one.

On 2026-11-26 the user kept the level table (`docs/review-flows.md`, section 4) and chose mechanics instead
(section 13 there; the pure parts in `core/src/review/policy.rs`):

- `[reviews] floor` (default `doubt`): findings below it get no verify subagent and are listed for information, never
  delivered for an answer. Their resolution stays empty, as a verify-rejected finding's does, so no migration was needed.
  `floor = "nit"` keeps the old behaviour.
- `[reviews] max_round_findings` (20) and `max_task_findings` (40, over every review of the task): the overflow is
  listed, picked by severity, then verdict, then number. When both caps bind, the tighter one names the reason; equal
  caps name the round's.
- From round 2 on, a finding that repeats an earlier round's finding (the same normalized summary, or the same file,
  line and node) is not recorded; the tool's answer and the `review-node` event name the earlier findings. Both file
  and line must be given for a place to match, and a summary with no letters or digits matches nothing.
- Another round is due only when the round had an important finding fixed, and the level allows more rounds.
- `board` shows each open review's fresh tokens (input, cache writes and output of the reviewer lead's part and its
  subagents, cache reads left out); `status` adds the cost of all the task's reviews, with `review_tokens` in its JSON.

Not built: dropping repeats within one round by place. Different nodes at one line often raise different issues; only
10 to 30% of such pairs overlapped.
