use crate::ids::{CoreInstanceID, ProjectID, ProjectSourceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_INITIALIZED: &str = "initialized";
pub const METHOD_PROJECT_OPEN_PATH: &str = "project.openPath";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    #[serde(rename = "projectID")]
    pub project_id: ProjectID,
    pub display_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ProjectSourceKind {
    LocalGit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSourceSummary {
    #[serde(rename = "projectSourceID")]
    pub project_source_id: ProjectSourceID,
    #[serde(rename = "projectID")]
    pub project_id: ProjectID,
    pub kind: ProjectSourceKind,
    pub display_name: String,
    pub canonical_path: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// What the client can answer. Core sends a core→client request only when its
/// method is listed here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub experimental_methods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    #[serde(rename = "coreInstanceID")]
    pub core_instance_id: CoreInstanceID,
    pub core_version: String,
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializedParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenPathParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenPathResult {
    pub project: ProjectSummary,
    pub source: ProjectSourceSummary,
    pub workspaces: Vec<crate::WorkspaceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_workspace: Option<crate::WorkspaceSummary>,
}
