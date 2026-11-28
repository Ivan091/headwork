ALTER TABLE backlog_items ADD COLUMN kind TEXT NOT NULL DEFAULT 'item' CHECK (kind IN ('item', 'epic'));

ALTER TABLE backlog_items ADD COLUMN parent_id INTEGER REFERENCES backlog_items(id);

ALTER TABLE backlog_items ADD COLUMN brief TEXT;

ALTER TABLE backlog_items ADD COLUMN task_kind TEXT;

ALTER TABLE backlog_items ADD COLUMN model TEXT;

ALTER TABLE backlog_items ADD COLUMN review INTEGER CHECK (review BETWEEN 0 AND 3);

ALTER TABLE backlog_items ADD COLUMN respawns INTEGER NOT NULL DEFAULT 0;

CREATE INDEX backlog_items_by_parent ON backlog_items (parent_id);

CREATE TABLE backlog_deps (
    item_id INTEGER NOT NULL REFERENCES backlog_items(id),
    depends_on_id INTEGER NOT NULL REFERENCES backlog_items(id),
    added_by_task INTEGER REFERENCES tasks(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (item_id, depends_on_id),
    CHECK (item_id <> depends_on_id)
);

CREATE INDEX backlog_deps_by_dependency ON backlog_deps (depends_on_id);
