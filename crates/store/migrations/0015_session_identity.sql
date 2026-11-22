ALTER TABLE sessions ADD COLUMN launcher_image TEXT;
ALTER TABLE sessions ADD COLUMN launcher_started INTEGER;
ALTER TABLE sessions ADD COLUMN claude_image TEXT;
ALTER TABLE sessions ADD COLUMN claude_started INTEGER;
ALTER TABLE sessions ADD COLUMN pids_at INTEGER;

CREATE TEMP TABLE session_marks (
    session_id TEXT PRIMARY KEY,
    last_at TEXT,
    launched_at TEXT
);

INSERT INTO session_marks (session_id, last_at, launched_at)
SELECT session_id, MAX(at), MAX(CASE WHEN type = 'launched' THEN at END)
FROM events WHERE session_id IS NOT NULL GROUP BY session_id;

UPDATE sessions SET pids_at = CAST(strftime('%s', COALESCE(
    (SELECT launched_at FROM session_marks WHERE session_marks.session_id = sessions.id),
    started_at)) AS INTEGER)
WHERE launcher_pid IS NOT NULL OR claude_pid IS NOT NULL;

UPDATE sessions SET ended_at = COALESCE(
    (SELECT last_at FROM session_marks WHERE session_marks.session_id = sessions.id),
    last_event_at,
    started_at)
WHERE state IN ('ended', 'gone') AND ended_at IS NULL;

DROP TABLE session_marks;
