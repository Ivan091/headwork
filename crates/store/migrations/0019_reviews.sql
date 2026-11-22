CREATE TABLE reviews (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    part_id INTEGER REFERENCES tasks(id),
    flow TEXT NOT NULL,
    level INTEGER NOT NULL CHECK (level BETWEEN 1 AND 3),
    focus TEXT NOT NULL DEFAULT '',
    base_sha TEXT,
    head_sha TEXT,
    round INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL CHECK (state IN ('requested', 'running', 'delivered', 'rechecking', 'asking', 'done', 'cancelled')),
    requested_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX reviews_task ON reviews(task_id);
CREATE INDEX reviews_part ON reviews(part_id);
CREATE TABLE review_nodes (
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    round INTEGER NOT NULL,
    node TEXT NOT NULL,
    item INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('ready', 'running', 'done', 'failed')),
    started_at TEXT,
    finished_at TEXT,
    PRIMARY KEY (review_id, round, node, item)
);
CREATE TABLE review_findings (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id),
    round INTEGER NOT NULL,
    node TEXT NOT NULL,
    file TEXT,
    line INTEGER,
    severity TEXT NOT NULL CHECK (severity IN ('important', 'nit', 'doubt')),
    summary TEXT NOT NULL,
    scenario TEXT NOT NULL,
    verdict TEXT CHECK (verdict IN ('confirmed', 'plausible', 'rejected')),
    resolution TEXT CHECK (resolution IN (
        'open', 'fixed', 'fix-accepted', 'disputed', 'dispute-accepted', 'fix-rejected', 'asked', 'user-kept', 'user-dropped'
    )),
    note TEXT,
    argued INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX review_findings_review ON review_findings(review_id, round);
