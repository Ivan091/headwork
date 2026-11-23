CREATE TABLE agents (
    agent_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_type TEXT,
    description TEXT,
    model TEXT,
    spawn_depth INTEGER,
    parent_agent_id TEXT,
    first_at TEXT,
    last_at TEXT
);

CREATE INDEX agents_session ON agents(session_id);

CREATE INDEX events_type ON events(type, id);

INSERT INTO agents (agent_id, session_id, agent_type, first_at, last_at)
SELECT agent_id, MIN(session_id), MAX(agent_type), MIN(at), MAX(at)
FROM usage
WHERE agent_id IS NOT NULL AND length(agent_id) BETWEEN 1 AND 64 AND agent_id NOT GLOB '*[^A-Za-z0-9_-]*'
GROUP BY agent_id;
