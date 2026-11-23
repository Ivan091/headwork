CREATE TABLE agents (
    agent_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    task_id INTEGER REFERENCES tasks(id),
    agent_type TEXT,
    description TEXT,
    model TEXT,
    spawn_depth INTEGER,
    first_at TEXT,
    last_at TEXT
);

CREATE INDEX agents_session ON agents(session_id);
