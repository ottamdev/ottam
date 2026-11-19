-- A managed worktree is a non-primary localGit source with both set: the
-- branch Core created for it, and the primary checkout's HEAD commit it was
-- cut from. Core made the directory under $OTTAM_HOME/worktrees and removes it
-- when the source is archived. NULL on every primary source and on sources
-- recorded before this migration.
ALTER TABLE project_sources ADD COLUMN worktree_branch TEXT;
ALTER TABLE project_sources ADD COLUMN worktree_base_commit TEXT;
