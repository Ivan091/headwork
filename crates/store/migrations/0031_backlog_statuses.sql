CREATE TABLE backlog_items_new (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    section TEXT,
    text TEXT NOT NULL,
    notes TEXT,
    status TEXT NOT NULL CHECK (status IN ('backlog', 'in_progress', 'blocked', 'done', 'cancelled')),
    blocked_reason TEXT CHECK (blocked_reason IN ('depends', 'task_lost', 'partial', 'decision', 'event', 'dep_cancelled')),
    blocked_note TEXT,
    status_before_block TEXT CHECK (status_before_block IN ('backlog', 'in_progress')),
    owner TEXT NOT NULL CHECK (owner IN ('user', 'agent')),
    added TEXT,
    not_before TEXT,
    due TEXT,
    done_on TEXT,
    done_note TEXT,
    task_id INTEGER REFERENCES tasks(id),
    position INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO backlog_items_new (id, project, section, text, notes, status, owner, added, not_before, due, done_on, done_note,
                               task_id, position, created_at, updated_at)
SELECT b.id, b.project, b.section, b.text, b.notes,
       CASE
           WHEN b.state = 'done' THEN 'done'
           WHEN b.state = 'dropped' THEN 'cancelled'
           WHEN EXISTS (
               SELECT 1 FROM tasks t WHERE t.id = b.task_id AND t.state NOT IN ('closed', 'dropped', 'failed')
           ) THEN 'in_progress'
           ELSE 'backlog'
       END,
       b.owner, b.added, b.not_before, b.due, b.done_on, b.done_note,
       (SELECT t.id FROM tasks t WHERE t.id = b.task_id), b.position, b.created_at, b.updated_at
FROM backlog_items b;

DROP TABLE backlog_items;

ALTER TABLE backlog_items_new RENAME TO backlog_items;

CREATE INDEX backlog_items_by_task ON backlog_items (task_id);

UPDATE events SET type = 'backlog-file-moved' WHERE type = 'backlog-moved';
