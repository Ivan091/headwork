CREATE TABLE todos (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    section TEXT,
    text TEXT NOT NULL,
    notes TEXT,
    state TEXT NOT NULL CHECK (state IN ('open', 'done', 'dropped')),
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

CREATE INDEX todos_by_task ON todos (task_id);

CREATE TABLE todo_files (
    path TEXT PRIMARY KEY,
    imported_at TEXT,
    export_hash TEXT,
    exported_at TEXT
);
