# cmux, the macOS terminal host

`crates/adapters/src/host/cmux.rs` drives [cmux](https://github.com/manaflow-ai/cmux) (checked against 0.65.2, build
113, on macOS 26) through its command line. Choose it with `[machine] host = "cmux"`; `host_bin` overrides the
program, else `CMUX_BUNDLED_CLI_PATH` (set in cmux terminals), else
`/Applications/cmux.app/Contents/Resources/bin/cmux`, else `cmux` on `PATH`.

Almost every call is `cmux rpc <method> '<json>'`, the CLI's passthrough to the v2 socket API: JSON in, JSON out, UUID
handles that stay valid, and no draft guard (the `send` and `send-key` commands refuse while a Claude dialog is open,
which would block answering the trust dialog). The sidebar status commands and `sessions list` have no socket method
and run as plain commands. Every row below was tried on the machine with cmux running; `[V]` marks what a real
headwork worker exercised end to end.

| Trait method | cmux | Notes |
|---|---|---|
| `ping` | `rpc system.ping {}` → `{"pong": true}` **[V]** | a failure means not running |
| `workspaces` | `rpc system.tree {"all":true}` **[V]** | every window's workspaces |
| `ensure_workspace` | find by title in the tree, else `rpc workspace.create {title, cwd, focus:false}` **[V]** | `focus:false` everywhere, so headwork never takes the user's view |
| `spawn` | `rpc surface.create {workspace_id, pane_id?, working_directory, initial_command, focus:false}` **[V]** | see "Starting a program" |
| `spawn` with `replace_tab` | the pane's selected surface from the tree, closed after the program's surface opened **[V]** | |
| `layout` | the workspace's `layout` in `system.tree`: `{pane}` leaves, `{direction, split, children}` splits **[V]** | `vertical` stacks top and bottom, `horizontal` sits side by side, as in wmux |
| `split` | the first pane in layout order, its selected surface: `rpc surface.split {surface_id, direction: down/right, focus:false}` **[V]** | cmux splits any pane either way; the adapter keeps the trait's contract |
| `split_pane` | `rpc surface.split {direction:"right"}` on the pane's selected surface **[V]** | equal halves by default |
| `close_pane` | `rpc surface.close` for each of its surfaces | cmux has no pane close; a pane goes with its last surface **[V]** |
| `panes` | `rpc surface.list {workspace_id}` per workspace; the label is read back from `initial_command` **[V]** | cmux keeps no registry of started programs |
| `surfaces` | the panes' `surface_ids` in the tree **[V]** | |
| `surface_states` | `cmux sessions list --json --agent claude` **[V]** | records cmux's own Claude hooks keep; `running` → `working`, `needsInput` → `blocked`, `idle` stays; only `active_for_surface` rows of open surfaces |
| `read_screen` | `rpc surface.read_text {surface_id, lines, scrollback:true}` **[V]** | works for a workspace never shown |
| `send_text` | `rpc surface.send_text {surface_id, text}` | |
| `send_key` | `rpc surface.send_key {surface_id, key}` for `enter` and `escape`; a char and Ctrl+char as text (`\u{3}` for Ctrl+C) | cmux names only a few Ctrl keys |
| `rename_surface` | `rpc tab.action {action:"rename", surface_id, title}` **[V]** | an OSC title from the program does not replace it |
| `close_surface` | `rpc surface.close {surface_id, force:true}` **[V]** | without `force` cmux refuses a surface whose program runs (`confirmation_required`) |
| `close_workspace` | `rpc workspace.close {workspace_id, force:true}` **[V]** | same |
| `kill_agent` | `close_surface`; an already closed one is fine; a workspace's last surface, which cmux refuses to close (`invalid_state: Cannot close the last surface`), goes with `close_workspace` **[V]** | see "What a close ends" |
| `show_blocked` | `cmux set-status headwork-<surface> "<question> (1 … · 2 …)" --workspace W --icon bell.fill --color #FF9500 --priority 100`, then `rpc notification.create_for_surface` **[V]** | no buttons: see `docs/decisions.md` (2026-11-21) |
| `clear_blocked` | `cmux clear-status headwork-<surface> --workspace W` **[V]** | nothing to do once the surface is gone |
| `notify` | `rpc notification.create {title:"headwork", body}` | |

## Starting a program

`initial_command` on `surface.create` replaces the terminal's shell with a one-shot program: cmux runs
`/usr/bin/login -flp <user> /bin/bash --noprofile --norc -c "exec -l <initial_command>"`, the surface closes when the
program exits, and the workspace closes with its last surface. (`initial_input` instead types the text into a live
shell.) The program gets cmux's own environment, not the caller's: `PATH` is `<surface shims>:<cmux bin>:/usr/bin:/bin:
/usr/sbin:/sbin`, and `startup_environment` cannot change `PATH`. So the adapter starts

```
/usr/bin/env HEADWORK_HOST_LABEL=<label> "PATH=${PATH%%:*}:"'<caller PATH>' '<argv0>' '<argv1>' ...
```

`${PATH%%:*}` keeps the surface's shim folder first, so `claude` still goes through cmux's wrapper (which adds cmux's
hooks and keeps headwork's `--session-id`); the caller's `PATH` (without other surfaces' shim folders) finds `git`,
`gh` and the rest. `surface.list` gives `initial_command` back, which is where `panes` finds the label.

A program that exits within Ghostty's abnormal-exit window (about 250 ms) leaves the surface open with "Ghostty failed
to launch the requested command"; such a surface still counts as running in `panes`.

## What a close ends

Tried on 2026-11-21 (cmux 0.65.2, Claude Code 2.1.311, macOS 26) with a throwaway surface and with real headwork
workers whose `Bash` ran `sleep` with `run_in_background`, `nohup sleep … &` and `(sleep … &)`:

- `surface.close` or `workspace.close` with `force` sends `SIGHUP` to the surface's foreground process group, and
  `SIGKILL` to that group about 11 seconds later. A plain child ends; a `nohup` child or one that ignores `SIGHUP`
  outlives the hangup but not the later kill.
- A process in a session of its own (`setsid`) survives both, adopted by `launchd`.
- `claude` starts every `Bash` command as a session of its own. Its `run_in_background` commands stay its children
  and end with it (and with `kill_tree`). A `nohup cmd &` or `(cmd &)` from a `Bash` call is adopted by `launchd` in
  that command's session: neither the hangup, nor the launcher's kill of `claude`'s process group, nor `kill_tree` by
  parent links reaches it. Its environment cannot be read back either (macOS shows none for `/bin` programs).
- What it keeps is its working folder, the task's worktree. So `close` ends, once the tree is gone and before it removes
  the worktree, every process adopted by `launchd` that runs in one of the task's worktrees and started while its
  session ran; `reap` ends the same for every ended session after its settle time, which covers a surface the user
  closed (`core/src/reap.rs`). A main-only task has no folder of its own, so its detached processes stay; the worker
  rules send long-running work through `job_start`, which is never ended this way.

## Outside a cmux terminal

The socket's default mode is `cmuxOnly` ("only the bundled cmux CLI may connect"), and the adapter always goes through
that CLI. A scheduled tick under launchd was not tried: there is no launchd schedule yet (see the macOS tick in
`docs/decisions.md`, 2026-11-21: a LaunchAgent, planned in the follow-up `headwork-macos-launchd-tick`).
