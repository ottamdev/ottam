-- A managed worktree is a non-primary localGit source with both set: the
-- branch Core created for it, and the primary checkout's HEAD commit it was
-- cut from. Core made the directory under $OTTAM_HOME/worktrees and removes it
-- when the source is archived. NULL on every primary source and on sources
-- recorded before this migration.
ALTER TABLE project_sources ADD COLUMN worktree_branch TEXT;
ALTER TABLE project_sources ADD COLUMN worktree_base_commit TEXT;
-- When an archived worktree's folder and branch were both settled, so no
-- later launch retries them. NULL while a removal is still owed.
ALTER TABLE project_sources ADD COLUMN worktree_cleaned_at BIGINT
    CHECK (worktree_cleaned_at >= 0);
