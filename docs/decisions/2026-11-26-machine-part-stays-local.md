# 2026-11-26: the machine part of the synced config stays on the machine

**Choice:** `headwork config adopt` writes the machine part to `~/.headwork/config.machine.toml` by default. That file,
when it exists, replaces the machine layer; otherwise `machines/<name>/headwork.toml` in the repository is used, as
before (the Windows machine keeps its committed file). `--machine-in-repo` writes the repository file and refuses while
a local file would hide it; `doctor` warns (`config-machine-shadow`) when both exist. The adopt migration note
`2026-11-25-config-sync-adopt` no longer asks for a pull request with the machine file; it was edited in place because
it was still pending on every machine that matters, and a machine that already committed a machine file keeps working
with it unchanged.

**Why:** the machine part holds paths, repository names and hostnames that can be employer-internal (the work Mac), and
`Ivan091/claude_config` has a personal GitHub remote that employer-internal details must not reach. Left out:
`.gitignore` entries (the values would still sit in a clone) and redacting values (breaks the round-trip check).
