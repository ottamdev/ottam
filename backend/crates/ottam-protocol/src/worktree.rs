use crate::ids::{ProjectSourceID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_WORKTREE_STATUS: &str = "worktree.status";
pub const METHOD_WORKTREE_REMOVE: &str = "worktree.remove";
pub const METHOD_WORKTREE_BASE_BRANCHES: &str = "worktree.baseBranches";

/// A worktree Core made for a conversation: the branch it created and the
/// base commit on the primary checkout that branch started from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSummary {
    pub branch: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorktreeSourceParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "projectSourceID")]
    pub project_source_id: ProjectSourceID,
}

pub type WorktreeStatusParams = WorktreeSourceParams;
pub type WorktreeRemoveParams = WorktreeSourceParams;
pub type WorktreeBaseBranchesParams = WorktreeSourceParams;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeBaseBranchKind {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeBaseBranch {
    /// Short display name, e.g. `main`, `feat/auth`, or `origin/main`.
    pub name: String,
    /// Unambiguous git ref name, e.g. `refs/heads/main` or `refs/remotes/origin/main`.
    pub ref_name: String,
    pub kind: WorktreeBaseBranchKind,
}

/// Branches on a project's primary checkout that a new worktree can cut from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeBaseBranchesResult {
    /// Branch currently checked out on the primary checkout, or `None` when
    /// HEAD is detached or the repository has no commits yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
    /// Short commit hash of HEAD when the primary checkout has a detached HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detached_head: Option<String>,
    /// Detected default branch's `refName` (prefers the local branch that
    /// `refs/remotes/origin/HEAD` points to, else its remote target, else local
    /// `main`/`master`, else `origin/main`/`origin/master`), when present in
    /// `branches`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch_ref: Option<String>,
    /// Whether the primary checkout has staged, unstaged, or untracked changes.
    pub primary_has_uncommitted_changes: bool,
    /// Branches available to cut a worktree from, local branches first.
    pub branches: Vec<WorktreeBaseBranch>,
}

/// What removing a worktree would discard, for the confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatusResult {
    #[serde(rename = "projectSourceID")]
    pub project_source_id: ProjectSourceID,
    pub branch: String,
    pub path: String,
    /// Changed, staged and untracked paths in the worktree.
    pub uncommitted_changes: u32,
    /// Commits on the branch since the worktree was made.
    pub commits: u32,
    /// Whether removal keeps the branch: it has moved off the commit it
    /// started from. Removal decides by the same rule.
    pub branch_kept: bool,
    /// Commits at the worktree's HEAD that no branch, tag or remote holds;
    /// removal loses them.
    pub unbranched_commits: u32,
    /// Conversations working in this worktree.
    pub conversation_count: u32,
}

/// What became of a worktree Core removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRemoval {
    #[serde(rename = "projectSourceID")]
    pub project_source_id: ProjectSourceID,
    pub branch: String,
    pub path: String,
    /// Whether the directory is gone. When false, `message` says why and
    /// whether Core tries again.
    pub removed: bool,
    /// Whether the branch is still there: it has moved off the commit it
    /// started from, or Core could not tell.
    pub branch_kept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRemoveResult {
    pub removal: WorktreeRemoval,
}
