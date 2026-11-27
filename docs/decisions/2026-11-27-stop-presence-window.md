# 2026-11-27: stop-unfinished presumes the user for a longer window

The 11-27 shadow review found two `stop-unfinished` events (8947, 8998) where a worker answered a user's question in a live chat, but another session's message had started the turn 199 s and 204 s after the user typed, past `push_window_secs` (120 s).

`[hooks] stop_presence_secs` (default 300, `0` off) is a second window used by the stop gate only: the user counts as present when they typed within `max(push_window_secs, stop_presence_secs)`. `ask-push` keeps `push_window_secs`, so a push is still needed for an ask long after the user's last prompt. The 817 s borderline event of 11-25 stays a deny.

`stop-unfinished` is already in `[hooks] enforce_rules` of this machine's `~/.headwork/config.toml`. The `enforce_rules` list of the shared claude-config layer was not checked or edited from this task.
