ALTER TABLE tasks ADD COLUMN effort TEXT CHECK (effort IN ('low', 'medium', 'high', 'xhigh', 'max'));
