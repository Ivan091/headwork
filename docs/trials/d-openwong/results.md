# Trial 6: openwong2kim/wmux 4.15.0 as one host for Windows and Mac

**Verdict: dropped by the user (2026-11-13).** Nothing was installed or run.

The user wants to keep amirlehmam/wmux v2.60.0 on Windows and use cmux on the Mac, so a single cross-platform host is not a goal. The orchestrator should get two thin adapters, one for wmux and one for cmux.

## What was done before the drop

- Shallow-cloned the v4.15.0 source (`git clone --depth 1 --branch v4.15.0 git@github.com:openwong2kim/wmux.git`) to read it. The clone has since been deleted.
- One finding worth keeping if openwong is ever looked at again:
  - Both apps have the Electron productName `wmux`, so a normal install of openwong uses the same `%APPDATA%\wmux` userData as amirlehmam (single-instance lock, `session.json`). `%APPDATA%\wmux\install-profile` reading `uninstalled` shows the 2026-11-12 install did write there. Source: `package.json`, `src/main/index.ts` (around line 580).
  - It can be isolated by starting `wmux.exe` with `WMUX_DATA_SUFFIX=<suffix>`. That moves userData, the pipes (`\.\pipe\wmux<suffix>-<user>`) and `~/.wmux<suffix>`, and skips writing to outside configs such as `~/.claude.json` (issue #1151). Source: `src/shared/constants.ts` `dataSuffix()`, `src/main/firstRun/FirstRunOrchestrator.ts`.
- The CLI matches the earlier notes in `S:/Code/notes/wmux-migration.md`. `new-workspace` takes only `--name`; `split` and `new-surface` take no command; `send` and `read-screen` use `--to`. Source: `src/cli/commands/*.ts`.

## Not verified

None of the runtime checks were run: side-by-side install, `--remote-control` showing on the phone, sessions surviving an app close, the four CLI operations, the hang bug.
