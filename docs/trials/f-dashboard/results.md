# Trial 8 (D4): the agent graph on a real lead, its review part and the live machine

The trial milestone of `docs/dashboard.md` (section 9, D4). The page ran against this machine's own store, read only,
while the dashboard lead itself (`headwork-agent-graph-dashboard`, task 216) and four other tasks worked.

**Verdict (2026-11-23): passed as far as an agent can watch it.** Every check below behaved as designed on real data.
What is left is the user's own look at the page (`headwork dash --open`), which the D4 done-when names.

## Setup

- Windows 11, wmux, headwork 0.1.33 (D1 and D2) installed, D3 and D4 run from release builds of their branches on
  ports 8743 and 8744 as registered jobs, stopped after each check.
- The store: 11,000 events, 38,500 usage rows, 370 subagents, five live tasks (the dashboard lead with its level-3
  review part, the review-flows lead, a me-sim car-training task with 34 jobs, a me-sim lead, a me-studio lead).
- The page was driven in the wmux browser panel; screenshots timed out (the window was minimised), so the checks read
  the page's own state through its DOM.

## Checks

| Check | Result |
|---|---|
| Live view of now | 302 to 312 nodes, matching `headwork board`'s five live tasks with their sessions, parts and satellites |
| Project filter | lists headwork, me-sim, me-studio; filters boxes and their contents |
| Search and side panel | "agent-graph" finds task 216; the panel shows its details, six state changes and its newest recap |
| A lead with its part and a review | `--task headwork-agent-graph-dashboard`: the part's box inside the lead's, the review with 13 flow nodes and their verify runs, 206 subagents of the part, messages both ways between lead and part |
| Replay a task from its first event | "replay this task" at 1000×: from the task's start, 94 nodes growing to 225 as the review part and its subagents appear |
| Live stream | the first `change` arrives at once; the page refetches on each change, so a new event shows within about two seconds |
| Back to live | "live" snaps to now, play stops |
| Size by cost | 710 nodes carry tokens (largest 489M, an orchestrator session); sizes and labels follow |
| Tokens by type | task 216: 165M in all, 164M of them cache reads, 433k output (61k thinking), 716k one-hour cache writes |
| Finding badges | the finished review: 176 findings, 0 open; each flow node its own count and severities |
| Security | a foreign `Host` gets 403, a `POST` 405; the page loads nothing from elsewhere |

## Findings

1. **Rejected findings counted as open.** The first D4 build counted findings the verify stage rejected (they never
   reach the leads and keep no resolution) as open, so a finished review showed 1 or 2 open per node. Fixed before the
   pull request: a rejected finding is never open.
2. **The stream was silent at first.** A `tiny_http` response of unknown length holds its body back, so the first D3
   build sent nothing. Fixed in D3 by writing the stream to the socket directly (`docs/decisions.md`, D3).
3. **The live view is large on this machine.** 300 nodes, mostly the car-training task's finished jobs and pull
   requests. The kind toggles hide them; a later change could hide finished satellites in the live view by default.
