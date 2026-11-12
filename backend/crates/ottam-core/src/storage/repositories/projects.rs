use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    NewProjectRow, NewProjectSourceRow, NewWorkspaceProjectRow, NewWorkspaceRow, Project,
    ProjectId, ProjectRow, ProjectSource, ProjectSourceId, ProjectSourceKind, ProjectSourceRow,
    Workspace,
};
use crate::storage::schema::{project_sources, projects, workspace_projects, workspaces};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedProject {
    pub project: Project,
    pub source: ProjectSource,
    pub created_workspace: Option<Workspace>,
}

pub struct ProjectRepository;

impl ProjectRepository {
    pub(crate) fn resolve_local_git_in_transaction(
        connection: &mut SqliteConnection,
        canonical_path: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<(OpenedProject, bool), StorageError> {
        let now = now.timestamp_millis();
        let existing = project_sources::table
            .filter(project_sources::canonical_path.eq(canonical_path))
            .filter(project_sources::kind.eq(ProjectSourceKind::LocalGit.as_storage_str()))
            .filter(project_sources::archived_at.is_null())
            .select(ProjectSourceRow::as_select())
            .first::<ProjectSourceRow>(connection)
            .optional()?;
        if let Some(source) = existing {
            diesel::update(projects::table.find(&source.project_id))
                .set(projects::last_opened_at.eq(now))
                .execute(connection)?;
            diesel::update(project_sources::table.find(&source.id))
                .set(project_sources::last_seen_at.eq(now))
                .execute(connection)?;
            let source_id = ProjectSourceId::try_from(source.id)?;
            return Ok((
                Self::get_opened_by_source_id(connection, &source_id)?,
                false,
            ));
        }

        let project_id = ProjectId::new_v7();
        let source_id = ProjectSourceId::new_v7();
        let project_id_value = project_id.to_string();
        let source_id_value = source_id.to_string();
        diesel::insert_into(projects::table)
            .values(NewProjectRow {
                id: &project_id_value,
                display_name,
                created_at: now,
                updated_at: now,
                last_opened_at: now,
                archived_at: None,
            })
            .execute(connection)?;
        diesel::insert_into(project_sources::table)
            .values(NewProjectSourceRow {
                id: &source_id_value,
                project_id: &project_id_value,
                kind: ProjectSourceKind::LocalGit.as_storage_str(),
                display_name,
                canonical_path,
                is_primary: 1,
                created_at: now,
                updated_at: now,
                last_seen_at: now,
                archived_at: None,
            })
            .execute(connection)?;
        Ok((Self::get_opened_by_source_id(connection, &source_id)?, true))
    }

    pub fn get_project(
        connection: &mut SqliteConnection,
        project_id: &ProjectId,
    ) -> Result<Project, StorageError> {
        let project_id = project_id.to_string();
        projects::table
            .find(&project_id)
            .select(ProjectRow::as_select())
            .first::<ProjectRow>(connection)
            .map_err(StorageError::from)?
            .try_into()
    }

    pub fn open_local_git(
        connection: &mut SqliteConnection,
        canonical_path: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<OpenedProject, StorageError> {
        recorded_transaction(connection, "project.open_local_git", |connection| {
            let (mut opened, created) = Self::resolve_local_git_in_transaction(
                connection,
                canonical_path,
                display_name,
                now,
            )?;
            // Reopening a known source returns the existing Project untouched; only a
            // brand-new Project seeds a default Workspace named from it.
            if !created {
                return Ok(opened);
            }
            let now = now.timestamp_millis();
            let workspace_id = crate::storage::models::WorkspaceId::new_v7();
            let workspace_id_value = workspace_id.to_string();
            let project_id_value = opened.project.id.to_string();
            let source_id_value = opened.source.id.to_string();
            diesel::insert_into(workspaces::table)
                .values(NewWorkspaceRow {
                    id: &workspace_id_value,
                    display_name,
                    created_at: now,
                    updated_at: now,
                    last_opened_at: now,
                    archived_at: None,
                })
                .execute(connection)?;
            diesel::insert_into(workspace_projects::table)
                .values(NewWorkspaceProjectRow {
                    workspace_id: &workspace_id_value,
                    project_id: &project_id_value,
                    default_source_id: &source_id_value,
                    sort_order: 0,
                    added_at: now,
                    removed_at: None,
                })
                .execute(connection)?;
            opened.created_workspace = Some(
                workspaces::table
                    .find(&workspace_id_value)
                    .select(crate::storage::models::WorkspaceRow::as_select())
                    .first::<crate::storage::models::WorkspaceRow>(connection)?
                    .try_into()?,
            );
            Ok(opened)
        })
    }

    pub fn get_opened_by_source_id(
        connection: &mut SqliteConnection,
        source_id: &ProjectSourceId,
    ) -> Result<OpenedProject, StorageError> {
        let source_id = source_id.to_string();
        let source = project_sources::table
            .find(&source_id)
            .select(ProjectSourceRow::as_select())
            .first::<ProjectSourceRow>(connection)?;
        let project = projects::table
            .find(&source.project_id)
            .select(ProjectRow::as_select())
            .first::<ProjectRow>(connection)?;
        Ok(OpenedProject {
            project: project.try_into()?,
            source: source.try_into()?,
            created_workspace: None,
        })
    }

    pub fn get_source(
        connection: &mut SqliteConnection,
        source_id: &ProjectSourceId,
    ) -> Result<ProjectSource, StorageError> {
        let source_id = source_id.to_string();
        project_sources::table
            .find(&source_id)
            .select(ProjectSourceRow::as_select())
            .first::<ProjectSourceRow>(connection)
            .map_err(StorageError::from)?
            .try_into()
    }

    pub fn all_active_primary_sources(
        connection: &mut SqliteConnection,
    ) -> Result<Vec<ProjectSource>, StorageError> {
        project_sources::table
            .filter(project_sources::is_primary.eq(1))
            .filter(project_sources::archived_at.is_null())
            .select(ProjectSourceRow::as_select())
            .load::<ProjectSourceRow>(connection)
            .map_err(StorageError::from)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }
}
