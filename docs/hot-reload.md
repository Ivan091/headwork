# Hot reload: installing a new headwork while sessions run

Goal: after `headwork install`, the machine uses the new build without restarting every session.

## What runs where (exploration, 2026-11-14)

| Part | Lifetime | Program it runs | Picks up a new build |
|---|---|---|---|
| Hooks (`headwork hook <event>`) | one process per hook call, about 30 ms | `$HEADWORK_EXE`, the path the launcher was started from (`~/.local/bin/headwork.exe` for wrapper and MCP spawns) | yes, at the next call |
| CLI (`board`, `spawn`, `close`, `tick`, `park`, `resume`, ...) | per call | whatever path is called | yes |
| Finisher (`headwork finish`), detached `park`/`resume` from MCP | per close / per call | the caller's `current_exe()` path | yes, when started after the install |
| Tick | per scheduler call (nothing schedules it yet) | the path in the schedule | yes |
| Launcher (`headwork launch`, `headwork root`) | the whole session, days | its own image | **no**: the image is loaded once |
| MCP server (`headwork mcp`, plugin `.mcp.json`) | the whole session, one per session | `${HEADWORK_EXE:-headwork}` | before this change **no**; now a relay, see below |
| Tool schemas Claude loaded | the whole session | Claude's memory | only when Claude lists tools again |
| Plugin files (`hooks.json`, `.mcp.json`) | read by Claude Code at session start | Claude Code | **no**: a new session is needed |
| Store schema (`state.db`) | shared by all | migrations run by any process that opens it | an old process that opens a newer schema is refused (`SchemaTooNew`) |

Facts checked on this machine (Rust 1.99, Windows 11):

- A running `.exe` can be renamed, but not overwritten (`rename` onto it: access denied) and not deleted (access
  denied).
- A process whose `.exe` was renamed still gets the original path from `current_exe()`, so `HEADWORK_EXE` and every
  finisher path keep pointing at `~/.local/bin/headwork.exe`, which holds the new build after an install.
- An old binary answers an unknown command with exit code 2 (clap's usage error).

The real hazards were the two long-lived processes: the MCP server kept serving the old code for the whole session,
and an old launcher, when `claude` exits, reopens the store with its own code; after a migration it is refused and the
session's end is never recorded (the tick only cleans it up later).

## What was built

- **`headwork install [--dir ~/.local/bin]`**, run from a build (`target/release/headwork.exe install`). It copies the
  build next to the target as `headwork.new.exe`, renames the installed copy aside to `headwork.old-<millis>.exe`,
  renames the new one into place, runs `headwork --version` on it (a failure puts the old copy back), then deletes
  every `headwork.old*.exe` no process runs any more, the hand-made `headwork.old.exe` too. Copies still running stay
  and are deleted by a later install. The window without a `headwork.exe` lasts two renames; a hook that falls into it
  fails with exit 127, which never blocks a call. It then prints which sessions run what (below).
- **The MCP relay.** `headwork mcp` is now a relay that starts the real server as `headwork mcp --serve` from
  `$HEADWORK_EXE` and passes lines through. Before each request it compares the program file's stamp (length and
  modification time, which a copy keeps) with the one the inner server started from; when it changed, it starts a new
  inner server, replays the session's `initialize` handshake, retires the old one (stdin closed, 10 s grace, then
  killed), and after the response sends `notifications/tools/list_changed` so Claude lists the tools again. If the new
  server does not start, the old one keeps serving. The server now advertises `tools.listChanged`. Every later build
  must keep accepting `mcp --serve`.
- **The launcher hands its exit to the installed build.** When `claude` exits, the launcher runs
  `headwork exited --session ... --task ... --code ... --launcher-pid ...` from its own path (the installed build) and
  waits. Only when that program cannot run the command (it does not start, or exits 2 because it predates `exited`)
  does the launcher do the work itself. So an old launcher never meets a schema it does not know, and fixes to the
  exit logic apply to running sessions.
- **Build records and doctor.** Each launcher records a `build` event at start: the program path and its stamp.
  `headwork doctor` (check `builds`) and `headwork install` sort the live sessions into: on the installed build;
  switching at their next tool call (launched by a hot-reload build, so their MCP relay and exit hand-off follow the new
  build); pinned (launched before hot reload, without the event). Pinned sessions are a warning with the fix.

## What cannot be reloaded, and the least-effort refresh

| What | Why | Refresh |
|---|---|---|
| Sessions launched before this change ("pinned") | their MCP server is the old, non-relay code | in that session: `/mcp`, choose `headwork`, Reconnect; or park and resume. Their launcher records the exit with its own code, which a newer schema refuses (see the migration row) |
| The relay process itself | Claude holds it for the session | nothing needed: it only passes lines; the server behind it is replaced |
| A running launcher's start-up logic (how it started `claude`) | already done | nothing; its exit is handed to the installed build |
| Plugin files (`hooks.json`, `.mcp.json`, `plugin.json`) | Claude Code reads them at session start | a new session (park and resume restarts every session) |
| Tool descriptions and schemas | Claude caches them | the relay's `list_changed` makes Claude list them again; if a client ignored it, `/mcp` Reconnect |
| A store migration (0006, task effort, arrived with this install) | a pinned launcher reopens the store with old code when `claude` exits, is refused, and records nothing | a close from outside (root, `headwork close`) waits for the launcher, then finishes the close itself; a self-close of a pinned session also starts a backup finisher from the new build, and every finisher records the session end its launcher missed. A session that simply exits stays `working` until `headwork close <name>` or the tick |

## Using it

```bash
cargo build --release -p headwork
target/release/headwork.exe install          # into ~/.local/bin, safe while the old one runs
headwork doctor                              # check `builds`: who still runs what
```
