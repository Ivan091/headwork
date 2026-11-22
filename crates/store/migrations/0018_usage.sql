CREATE TABLE usage (
    message_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id TEXT,
    agent_type TEXT,
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL CHECK (role IN ('orchestrator', 'lead', 'part', 'worker', 'helper', 'unmanaged')),
    dir TEXT NOT NULL,
    at TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT,
    speed TEXT,
    service_tier TEXT,
    input INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    output INTEGER NOT NULL,
    thinking INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    web_fetches INTEGER NOT NULL
);

CREATE INDEX usage_task ON usage(task_id, at);
CREATE INDEX usage_at ON usage(at);
CREATE INDEX usage_session ON usage(session_id);

CREATE TABLE usage_daily (
    day TEXT NOT NULL,
    session_id TEXT NOT NULL,
    agent_type TEXT NOT NULL DEFAULT '',
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL CHECK (role IN ('orchestrator', 'lead', 'part', 'worker', 'helper', 'unmanaged')),
    dir TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL DEFAULT '',
    responses INTEGER NOT NULL,
    input INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    output INTEGER NOT NULL,
    thinking INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    web_fetches INTEGER NOT NULL,
    PRIMARY KEY (day, session_id, agent_type, model, effort)
);

CREATE TABLE usage_files (
    path TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id TEXT,
    read_to INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL,
    read_at TEXT NOT NULL
);
