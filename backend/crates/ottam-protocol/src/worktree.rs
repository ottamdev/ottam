use crate::ids::{ProjectSourceID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_WORKTREE_STATUS: &str = "worktree.status";
pub const METHOD_WORKTREE_REMOVE: &str = "worktree.remove";

/// A worktree Core made for a conversation: the branch it created and the
/// primary checkout's HEAD commit that branch started from.
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
