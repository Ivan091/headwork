CREATE TABLE prs_by_task (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(id),
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    url TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'merged', 'closed')),
    checked_at TEXT,
    UNIQUE (task_id, url)
);

INSERT INTO prs_by_task (id, task_id, repo, number, url, state, checked_at)
SELECT id, task_id, repo, number, url, state, checked_at FROM prs;

DROP TABLE prs;

ALTER TABLE prs_by_task RENAME TO prs;

CREATE TEMP TABLE archive_prs AS
SELECT p.id FROM prs p
WHERE lower(p.repo) = 'ivan091/headwork'
  AND lower(p.url) = lower('https://github.com/Ivan091/headwork/pull/' || p.number)
  AND (
      EXISTS (
          SELECT 1 FROM events e
          WHERE e.task_id = p.task_id
            AND e.at < '2026-11-22T08:50:40Z'
            AND instr(e.data, '"' || p.url || '"') > 0
      )
      OR (
          NOT EXISTS (
              SELECT 1 FROM events e
              WHERE e.task_id = p.task_id AND instr(e.data, '"' || p.url || '"') > 0
          )
          AND EXISTS (
              SELECT 1 FROM tasks t WHERE t.id = p.task_id AND t.queued_at < '2026-11-22T08:50:40Z'
          )
      )
  );

INSERT INTO events (at, actor, task_id, session_id, type, data)
SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'user', p.task_id, NULL, 'pr-moved',
       json_object(
           'from', p.url,
           'to', 'https://github.com/Ivan091/headwork-archive/pull/' || p.number,
           'reason', 'Ivan091/headwork was re-created on 2026-11-22T08:50:40Z; the old repository is Ivan091/headwork-archive'
       )
FROM prs p
WHERE p.id IN (SELECT id FROM archive_prs);

UPDATE prs
SET repo = 'Ivan091/headwork-archive',
    url = 'https://github.com/Ivan091/headwork-archive/pull/' || number
WHERE id IN (SELECT id FROM archive_prs);

DROP TABLE archive_prs;
