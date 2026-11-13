CREATE TABLE parks (
    id INTEGER PRIMARY KEY,
    started_at TEXT NOT NULL,
    deadline TEXT NOT NULL,
    finished_at TEXT,
    resumed_at TEXT,
    recap_path TEXT
);

CREATE TABLE machine (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    mode TEXT NOT NULL CHECK (mode IN ('normal', 'parking', 'parked', 'resuming')),
    park_id INTEGER REFERENCES parks(id),
    rules_version INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

INSERT INTO machine (id, mode, updated_at) VALUES (1, 'normal', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE projects (
    name TEXT PRIMARY KEY,
    main_path TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    project TEXT NOT NULL REFERENCES projects(name),
    slug TEXT NOT NULL CHECK (slug GLOB '[a-z0-9]*' AND slug NOT GLOB '*[^a-z0-9-]*'),
    kind TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'spawning', 'working', 'needs_user', 'in_review', 'merged', 'closing', 'closed',
        'dropped', 'failed', 'interrupted', 'parking', 'parked'
    )),
    state_before_park TEXT,
    model TEXT NOT NULL CHECK (model IN ('haiku', 'sonnet', 'opus', 'fable')),
    permission_mode TEXT NOT NULL DEFAULT 'auto',
    keep INTEGER NOT NULL DEFAULT 0,
    in_main INTEGER NOT NULL DEFAULT 0,
    depends_on INTEGER REFERENCES tasks(id),
    fable_ask INTEGER REFERENCES asks(id),
    brief_path TEXT NOT NULL,
    handoff_path TEXT,
    last_report TEXT,
    queued_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX tasks_live_name ON tasks(project, slug) WHERE state <> 'closed';

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    role TEXT NOT NULL CHECK (role IN ('root', 'child')),
    rc_name TEXT NOT NULL,
    runtime TEXT NOT NULL CHECK (runtime IN ('pane', 'bg')),
    host TEXT NOT NULL,
    host_workspace TEXT,
    host_surface TEXT,
    launcher_pid INTEGER,
    claude_pid INTEGER,
    state TEXT NOT NULL CHECK (state IN ('starting', 'live', 'ended', 'gone')),
    rules_seen INTEGER NOT NULL DEFAULT 0,
    tool_in_flight TEXT,
    last_event_at TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    exit_code INTEGER
);

CREATE TABLE worktrees (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    path TEXT NOT NULL UNIQUE,
    branch TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('present', 'removing', 'removed', 'kept')),
    head TEXT,
    dirty_files INTEGER,
    park_ref TEXT
);

CREATE TABLE prs (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    url TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'merged', 'closed')),
    checked_at TEXT,
    UNIQUE (repo, number)
);

CREATE TABLE asks (
    id INTEGER PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    session_id TEXT REFERENCES sessions(id),
    kind TEXT NOT NULL CHECK (kind IN ('question', 'review', 'permission', 'tool')),
    topic TEXT,
    question TEXT NOT NULL,
    options TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'open', 'answered', 'expired', 'superseded')),
    answer TEXT,
    created_at TEXT NOT NULL,
    notified_at TEXT,
    answered_at TEXT
);

CREATE TABLE rules (
    id INTEGER PRIMARY KEY,
    scope TEXT NOT NULL,
    body TEXT NOT NULL,
    version INTEGER NOT NULL,
    source TEXT NOT NULL,
    active INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    task_id INTEGER REFERENCES tasks(id),
    project TEXT NOT NULL REFERENCES projects(name),
    label TEXT NOT NULL,
    park_policy TEXT NOT NULL CHECK (park_policy IN ('checkpoint', 'rerun', 'finish')),
    argv TEXT NOT NULL,
    cwd TEXT NOT NULL,
    pid INTEGER,
    checkpoint_path TEXT,
    heavy INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('running', 'stopping', 'parked', 'done', 'failed')),
    log_path TEXT NOT NULL,
    started_at TEXT NOT NULL
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    actor TEXT NOT NULL,
    task_id INTEGER,
    session_id TEXT,
    type TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX events_task ON events(task_id, id);
