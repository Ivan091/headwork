ALTER TABLE tasks ADD COLUMN close_mode TEXT NOT NULL DEFAULT 'confirm';
ALTER TABLE tasks ADD COLUMN review_plan INTEGER;
UPDATE tasks SET close_mode = 'silent' WHERE kind = 'silent';
