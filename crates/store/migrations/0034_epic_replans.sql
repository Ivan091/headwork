CREATE TABLE epic_replans (
    id INTEGER PRIMARY KEY,
    epic_id INTEGER NOT NULL REFERENCES backlog_items(id),
    child_id INTEGER NOT NULL REFERENCES backlog_items(id),
    cause_item INTEGER REFERENCES backlog_items(id),
    cause_event INTEGER,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    cleared_at TEXT
);

CREATE INDEX epic_replans_by_epic ON epic_replans (epic_id);

CREATE INDEX epic_replans_by_cause ON epic_replans (cause_item);

CREATE TABLE epic_churn_blocks (
    epic_id INTEGER NOT NULL REFERENCES backlog_items(id),
    event_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (epic_id, event_id)
);
