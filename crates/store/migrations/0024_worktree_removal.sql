ALTER TABLE worktrees ADD COLUMN removal_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE worktrees ADD COLUMN removal_last_at INTEGER;
ALTER TABLE worktrees ADD COLUMN removal_last_error TEXT;
