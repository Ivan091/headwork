# The daily store backup

The store (`~/.headwork/state.db`) holds the backlog, tasks, asks and the event log, and the exported `backlog.md` is only
its mirror of the backlog. A tick stage copies the store once a day.

- The stage `store_backup` (`ops/tick.rs`, logic in `ops/store_backup.rs`) makes `<home>/backups/daily-<millis>.db` with
  `VACUUM INTO`, a consistent copy while other processes write the WAL database, never a raw file copy. The copy is written
  as `daily-<millis>.db.part`, checked (same schema version, `PRAGMA quick_check`) and only then renamed.
- It is made when the newest daily copy is 24 hours old or there is none. The newest seven are kept; older ones are deleted.
  The copies a migration takes (`state-<from>-to-<to>-*.db`, `docs/autoupdate.md`) share the folder and are never counted or
  deleted by this stage.
- The event log gets `store-backup` when a copy is made and `store-backup-failed` when it fails (the stage then also raises
  the `tick-error` finding). A copy that is not due logs nothing.
- `headwork doctor` check `store-backup` warns when the newest daily copy is older than two days; a store with no copy yet passes, since the `tick` check covers a tick that never ran.
- To restore: stop headwork sessions, copy a `daily-*.db` over `state.db` (delete `state.db-wal` and `state.db-shm`).

No store migration and no config key.
