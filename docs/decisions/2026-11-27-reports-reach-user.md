# Reports reach the user as files they can open

Over Remote Control the phone app and the Claude desktop app cannot open a path on this machine. Worker, lead and
explorer rules now require `SendUserFile` for every report, notes file or file the user should read, and plain
inline markdown for a short result. A shadow guard rule that detects a handed-over path without a send is left for
later.

The worker rules' delegation bullet was shortened in the same change (its rationale sentence and the "small edits
yourself" clause dropped from the text, the rest unchanged; small edits stay the worker's own by the "keep integration yourself" line) because
a lead's combined rules, worker plus lead, must stay under `CONTEXT_LIMIT` (10,000 characters).
