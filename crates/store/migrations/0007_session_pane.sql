ALTER TABLE sessions ADD COLUMN host_pane TEXT;
ALTER TABLE sessions ADD COLUMN host_place TEXT CHECK (host_place IN ('pane', 'tab'));
