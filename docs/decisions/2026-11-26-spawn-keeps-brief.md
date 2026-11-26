# 2026-11-26 Spawn keeps an existing brief; `answer` defaults to the only waiting ask

A spawn (and a lead's `part_spawn`) wrote its brief to `<home>/briefs/<project>-<slug>.md` without looking, so a pointer
brief once replaced the real one. Now the spawn is refused, before the task exists, when that file already exists and
does not start with the new brief's text (the same text, with the worker rules a plugin-less spawn appended, is
accepted). The error names the file and offers a new slug, or the file's own path as the brief.

The orchestrator's `answer` tool takes `ask` as optional: omitted, it answers the one ask the tick raised that waits
(`Store::waiting_tick_asks`, `answer::only_waiting`); none or several is refused, listing the ids.

The allow-ask split into several asks (audit item 13) is left out: it is not tiny and needs its own change.
