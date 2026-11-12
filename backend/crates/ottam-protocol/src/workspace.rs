use crate::{ProjectID, ProjectSourceID, ProjectSourceSummary, ProjectSummary, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_WORKSPACE_LIST: &str = "workspace.list";
pub const METHOD_WORKSPACE_CREATE: &str = "workspace.create";
pub const METHOD_WORKSPACE_OPEN: &str = "workspace.open";
pub const METHOD_WORKSPACE_GET: &str = "workspace.get";
pub const METHOD_WORKSPACE_UPDATE: &str = "workspace.update";
pub const METHOD_WORKSPACE_ARCHIVE: &str = "workspace.archive";
pub const METHOD_WORKSPACE_RESTORE: &str = "workspace.restore";
pub const METHOD_WORKSPACE_ADD_PROJECT: &str = "workspace.addProject";
pub const METHOD_WORKSPACE_REMOVE_PROJECT: &str = "workspace.removeProject";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkingRootKind {
    WorkspaceFiles,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkingRootAvailability {
    Available,
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkingRootSelection {
    WorkspaceFiles,
    Project {
        #[serde(rename = "projectID")]
        project_id: ProjectID,
        #[serde(rename = "sourceID")]
        source_id: ProjectSourceID,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkingRootSummary {
    pub kind: WorkingRootKind,
    pub display_name: String,
    pub canonical_path: String,
    #[serde(rename = "projectID", skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectID>,
    #[serde(rename = "sourceID", skip_serializing_if = "Option::is_none")]
    pub source_id: Option<ProjectSourceID>,
    pub availability: WorkingRootAvailability,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub display_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceProjectSummary {
    pub project: ProjectSummary,
    pub source: ProjectSourceSummary,
    pub sort_order: i64,
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDetail {
    pub workspace: WorkspaceSummary,
    pub workspace_files: WorkingRootSummary,
    pub projects: Vec<WorkspaceProjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListParams {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListResult {
    pub workspaces: Vec<WorkspaceListItem>,
}

/// One row of the Workspace list: the summary plus what a list shows about
/// its projects without opening it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListItem {
    pub workspace: WorkspaceSummary,
    /// The canonical path of each member project's default source, in the
    /// Workspace's project order.
    pub project_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCreateParams {
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIDParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
}

pub type WorkspaceOpenParams = WorkspaceIDParams;
pub type WorkspaceGetParams = WorkspaceIDParams;
pub type WorkspaceArchiveParams = WorkspaceIDParams;
pub type WorkspaceRestoreParams = WorkspaceIDParams;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUpdateParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAddProjectParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAddProjectResult {
    pub workspace: WorkspaceDetail,
    #[serde(rename = "projectID")]
    pub project_id: ProjectID,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRemoveProjectParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    #[serde(rename = "projectID")]
    pub project_id: ProjectID,
}
