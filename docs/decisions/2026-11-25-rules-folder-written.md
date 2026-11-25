# 2026-11-25: the rules folder is written

Nothing ever wrote `machine.rules_dir`, yet the over-limit pointer named files there. Every rules load now writes the missing built-in role files and `added.md` (the `rule_add` rules, kept current) into it, never overwriting an override; `doctor` check `rules-folder` reports what is missing or stale. Config sync passes `rules_dir` through unchanged.
