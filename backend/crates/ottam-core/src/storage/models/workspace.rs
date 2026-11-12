use super::types::{optional_timestamp, timestamp, ProjectId, ProjectSourceId, WorkspaceId};
use crate::storage::error::StorageError;
use crate::storage::schema::{workspace_projects, workspaces};
use chrono::{DateTime, Utc};
use diesel::{Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = workspaces)]
pub struct WorkspaceRow {
    pub id: String,
    pub display_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = workspaces)]
pub struct NewWorkspaceRow<'a> {
    pub id: &'a str,
    pub display_name: &'a str,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = workspace_projects)]
#[diesel(primary_key(workspace_id, project_id))]
pub struct WorkspaceProjectRow {
    pub workspace_id: String,
    pub project_id: String,
    pub default_source_id: String,
    pub sort_order: i64,
    pub added_at: i64,
    pub removed_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = workspace_projects)]
pub struct NewWorkspaceProjectRow<'a> {
    pub workspace_id: &'a str,
    pub project_id: &'a str,
    pub default_source_id: &'a str,
    pub sort_order: i64,
    pub added_at: i64,
    pub removed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl TryFrom<WorkspaceRow> for Workspace {
    type Error = StorageError;

    fn try_from(row: WorkspaceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: WorkspaceId::try_from(row.id)?,
            display_name: row.display_name,
            created_at: timestamp(row.created_at, "Workspace.created_at")?,
            updated_at: timestamp(row.updated_at, "Workspace.updated_at")?,
            last_opened_at: timestamp(row.last_opened_at, "Workspace.last_opened_at")?,
            archived_at: optional_timestamp(row.archived_at, "Workspace.archived_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProject {
    pub workspace_id: WorkspaceId,
    pub project_id: ProjectId,
    pub default_source_id: ProjectSourceId,
    pub sort_order: i64,
    pub added_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

impl TryFrom<WorkspaceProjectRow> for WorkspaceProject {
    type Error = StorageError;

    fn try_from(row: WorkspaceProjectRow) -> Result<Self, Self::Error> {
        Ok(Self {
            workspace_id: WorkspaceId::try_from(row.workspace_id)?,
            project_id: ProjectId::try_from(row.project_id)?,
            default_source_id: ProjectSourceId::try_from(row.default_source_id)?,
            sort_order: row.sort_order,
            added_at: timestamp(row.added_at, "WorkspaceProject.added_at")?,
            removed_at: optional_timestamp(row.removed_at, "WorkspaceProject.removed_at")?,
        })
    }
}
