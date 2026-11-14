CREATE TABLE recaps (
    id INTEGER PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    session_id TEXT,
    park_id INTEGER REFERENCES parks(id),
    kind TEXT NOT NULL CHECK (kind IN ('session', 'mechanical', 'close', 'interrupted')),
    body TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX recaps_task ON recaps(task_id, id);

ALTER TABLE jobs ADD COLUMN restart INTEGER NOT NULL DEFAULT 1;
ALTER TABLE jobs ADD COLUMN note TEXT;
ALTER TABLE jobs ADD COLUMN ended_at TEXT;
ALTER TABLE jobs ADD COLUMN session_id TEXT;
