UPDATE sessions
SET state = 'ended',
    ended_at = COALESCE(ended_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
WHERE state IN ('starting', 'live')
  AND task_id IN (SELECT id FROM tasks WHERE state = 'closed');

UPDATE jobs
SET state = 'done',
    note = 'the task closed',
    ended_at = COALESCE(ended_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
WHERE state IN ('running', 'stopping', 'parked')
  AND task_id IN (SELECT id FROM tasks WHERE state = 'closed');
