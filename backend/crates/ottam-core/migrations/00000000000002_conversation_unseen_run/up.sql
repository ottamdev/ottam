-- The finished run a conversation holds unseen: it ended while nobody was
-- looking and the user has not looked since. NULL when there is none.
-- Deleting the run clears it; the repository only sets a run of the same
-- conversation, since this key cannot also name the workspace.
ALTER TABLE conversations ADD COLUMN unseen_run_id TEXT
    REFERENCES conversation_runs(id) ON DELETE SET NULL;
