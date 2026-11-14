ALTER TABLE machine ADD COLUMN last_tick_at TEXT;

CREATE TABLE findings (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL,
    rule TEXT NOT NULL,
    task_id INTEGER REFERENCES tasks(id),
    detail TEXT NOT NULL,
    first_at TEXT NOT NULL,
    resolved_at TEXT
);

CREATE UNIQUE INDEX findings_open_key ON findings(key) WHERE resolved_at IS NULL;

CREATE TABLE screen_samples (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id),
    hash TEXT NOT NULL,
    at TEXT NOT NULL
);

CREATE TABLE file_samples (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL
);
