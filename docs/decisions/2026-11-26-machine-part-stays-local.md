# 2026-11-26: the machine part of the synced config stays on the machine

**Choice:** `headwork config adopt` writes the machine part to `~/.headwork/config.machine.toml` by default. The
repository's `machines/<name>/headwork.toml` is used only when it already exists or `--machine-in-repo` is passed. The
adopt migration note no longer asks the user to ship a machine file through a pull request.

**Why:** the machine part holds paths, repository names and hostnames that can be employer-internal (the work Mac); the
repository it was meant for has a public-facing remote. Options left out: ignoring machine paths in the repository by
`.gitignore` (still copies them into a clone) and redacting values (breaks the round-trip check).
