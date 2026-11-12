use super::types::{optional_timestamp, timestamp, ProjectId, ProjectSourceId, ProjectSourceKind};
use crate::storage::error::StorageError;
use crate::storage::schema::{project_sources, projects};
use chrono::{DateTime, Utc};
use diesel::{AsChangeset, Identifiable, Insertable, Queryable, Selectable};

#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = projects)]
pub struct ProjectRow {
    pub id: String,
    pub display_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = projects)]
pub struct NewProjectRow<'a> {
    pub id: &'a str,
    pub display_name: &'a str,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, AsChangeset)]
#[diesel(table_name = project_sources)]
pub struct ProjectSourceRow {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub display_name: String,
    pub canonical_path: String,
    pub is_primary: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = project_sources)]
pub struct NewProjectSourceRow<'a> {
    pub id: &'a str,
    pub project_id: &'a str,
    pub kind: &'a str,
    pub display_name: &'a str,
    pub canonical_path: &'a str,
    pub is_primary: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_opened_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl TryFrom<ProjectRow> for Project {
    type Error = StorageError;

    fn try_from(row: ProjectRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ProjectId::try_from(row.id)?,
            display_name: row.display_name,
            created_at: timestamp(row.created_at, "Project.created_at")?,
            updated_at: timestamp(row.updated_at, "Project.updated_at")?,
            last_opened_at: timestamp(row.last_opened_at, "Project.last_opened_at")?,
            archived_at: optional_timestamp(row.archived_at, "Project.archived_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSource {
    pub id: ProjectSourceId,
    pub project_id: ProjectId,
    pub kind: ProjectSourceKind,
    pub display_name: String,
    pub canonical_path: String,
    pub is_primary: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
}

impl TryFrom<ProjectSourceRow> for ProjectSource {
    type Error = StorageError;

    fn try_from(row: ProjectSourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ProjectSourceId::try_from(row.id)?,
            project_id: ProjectId::try_from(row.project_id)?,
            kind: ProjectSourceKind::try_from(row.kind)?,
            display_name: row.display_name,
            canonical_path: row.canonical_path,
            is_primary: row.is_primary != 0,
            created_at: timestamp(row.created_at, "ProjectSource.created_at")?,
            updated_at: timestamp(row.updated_at, "ProjectSource.updated_at")?,
            last_seen_at: timestamp(row.last_seen_at, "ProjectSource.last_seen_at")?,
            archived_at: optional_timestamp(row.archived_at, "ProjectSource.archived_at")?,
        })
    }
}
