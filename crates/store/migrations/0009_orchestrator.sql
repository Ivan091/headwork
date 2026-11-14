PRAGMA defer_foreign_keys = ON;

CREATE TABLE sessions_before_rename AS SELECT * FROM sessions;

DROP TABLE sessions;

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL CHECK (role IN ('orchestrator', 'worker')),
    rc_name TEXT NOT NULL,
    runtime TEXT NOT NULL CHECK (runtime IN ('pane', 'bg')),
    host TEXT NOT NULL,
    host_workspace TEXT,
    host_surface TEXT,
    launcher_pid INTEGER,
    claude_pid INTEGER,
    state TEXT NOT NULL CHECK (state IN ('starting', 'live', 'ended', 'gone')),
    rules_seen INTEGER NOT NULL DEFAULT 0,
    tool_in_flight TEXT,
    last_event_at TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    exit_code INTEGER,
    host_pane TEXT,
    host_place TEXT CHECK (host_place IN ('pane', 'tab')),
    dir TEXT
);

INSERT INTO sessions (
    id, task_id, role, rc_name, runtime, host, host_workspace, host_surface, launcher_pid, claude_pid, state,
    rules_seen, tool_in_flight, last_event_at, started_at, ended_at, exit_code, host_pane, host_place, dir
)
SELECT
    id, task_id, CASE role WHEN 'root' THEN 'orchestrator' ELSE 'worker' END, rc_name, runtime, host, host_workspace,
    host_surface, launcher_pid, claude_pid, state, rules_seen, tool_in_flight, last_event_at, started_at, ended_at,
    exit_code, host_pane, host_place, dir
FROM sessions_before_rename;

DROP TABLE sessions_before_rename;

ALTER TABLE parks RENAME COLUMN root_session TO orchestrator_session;

UPDATE events SET actor = 'orchestrator' WHERE actor = 'root';
UPDATE events SET actor = 'worker' WHERE actor = 'child';

UPDATE events SET type = 'park-orchestrator' WHERE type = 'park-root';
UPDATE events SET type = 'orchestrator-resume-failed' WHERE type = 'root-resume-failed';

UPDATE events
SET data = json_set(data, '$.role', CASE json_extract(data, '$.role') WHEN 'root' THEN 'orchestrator' ELSE 'worker' END)
WHERE CASE WHEN json_valid(data) THEN json_extract(data, '$.role') IN ('root', 'child') ELSE 0 END;

UPDATE events
SET data = json_set(data, '$.rule', 'orchestrator-' || substr(json_extract(data, '$.rule'), 6))
WHERE CASE WHEN json_valid(data) THEN json_extract(data, '$.rule') LIKE 'root-%' ELSE 0 END;

UPDATE events
SET data = json_set(data, '$.rule', 'worker-' || substr(json_extract(data, '$.rule'), 7))
WHERE CASE WHEN json_valid(data) THEN json_extract(data, '$.rule') LIKE 'child-%' ELSE 0 END;

UPDATE events
SET data = json_set(data, '$.result.rule', 'orchestrator-' || substr(json_extract(data, '$.result.rule'), 6))
WHERE CASE WHEN json_valid(data) THEN json_extract(data, '$.result.rule') LIKE 'root-%' ELSE 0 END;

UPDATE events
SET data = json_set(data, '$.result.rule', 'worker-' || substr(json_extract(data, '$.result.rule'), 7))
WHERE CASE WHEN json_valid(data) THEN json_extract(data, '$.result.rule') LIKE 'child-%' ELSE 0 END;

UPDATE rules SET scope = 'orchestrator' WHERE scope = 'root';
UPDATE rules SET scope = 'worker' WHERE scope = 'child';
UPDATE rules SET source = 'orchestrator' WHERE source = 'root';
UPDATE rules SET source = 'worker' WHERE source = 'child';
