CREATE TABLE migrations_applied (
    repo TEXT NOT NULL,
    note_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('applied', 'skipped')),
    actor TEXT NOT NULL,
    at TEXT NOT NULL,
    output TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (repo, note_id)
);
