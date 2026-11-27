# Reports reach the user as files they can open

Over Remote Control the phone app and the Claude desktop app cannot open a path on this machine. Worker, lead and
explorer rules now require `SendUserFile` for every report, notes file or file the user should read, and plain
inline markdown for a short result. A shadow guard rule that detects a handed-over path without a send is left for
later.
