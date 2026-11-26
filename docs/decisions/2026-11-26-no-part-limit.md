# No per-lead part limit (2026-11-26)

The user: "I think there should not be any limit on how many subworkers a lead can have", and chose "No limit at all".

`[leads] max_parts` is now optional with no default; absent or 0 means no limit. A part then waits only for the machine's
`max_workers` and its project's cap, as any task does, and parts of different leads share the project slots by queue
rank. Its heavy commands wait for a `[gates]` slot like everyone else's (`docs/decisions/2026-11-26-gates-in-headwork.md`).
When the key is set it limits live parts per lead as before. Negative or non-numeric values are still refused by the
config check. `headwork doctor` does not warn about a missing key.

Migration: `migrations/2026-11-26-max-parts-optional.toml`; the shared config may drop `max_parts`.
