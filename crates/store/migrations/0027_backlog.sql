ALTER TABLE todos RENAME TO backlog_items;
DROP INDEX todos_by_task;
CREATE INDEX backlog_items_by_task ON backlog_items (task_id);
ALTER TABLE todo_files RENAME TO backlog_files;
UPDATE events SET type = 'backlog-' || substr(type, 6) WHERE type LIKE 'todo-%';
ALTER TABLE backlog_files ADD COLUMN configured_at TEXT;
UPDATE backlog_files SET configured_at = exported_at WHERE exported_at IS NOT NULL;
