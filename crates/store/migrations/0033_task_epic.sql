ALTER TABLE tasks ADD COLUMN epic_id INTEGER REFERENCES backlog_items(id);
