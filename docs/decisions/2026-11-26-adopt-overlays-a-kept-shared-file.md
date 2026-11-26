# 2026-11-26: adopt overlays a kept shared file in the machine part

`config adopt` used a shared file already in the repository as it is, but split only the machine-only keys into the
machine part. A joining machine whose projects the shared file lacks then merged to projects holding only `main`, and
the config failed with `missing field worktree_new`.

Chosen: with a kept shared file, every live value the shared file lacks or sets differently goes into the machine
part, and every key only the shared file sets goes into the machine part's `unset` list. The merge then reproduces
the live config; a kept machine file that does not still stops the adoption.
