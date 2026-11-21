use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    ConversationId, ManagedWorktree, NewProjectRow, NewProjectSourceRow, NewWorkspaceProjectRow,
    NewWorkspaceRow, Project, ProjectId, ProjectRow, ProjectSource, ProjectSourceId,
    ProjectSourceKind, ProjectSourceRow, Workspace,
};
use crate::storage::schema::{
    conversation_projects, project_sources, projects, workspace_projects, workspaces,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedProject {
    pub project: Project,
    pub source: ProjectSource,
    pub created_workspace: Option<Workspace>,
}

/// A managed worktree whose source is archived, with what removing its
/// directory needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedWorktree {
    pub source: ProjectSource,
    pub worktree: ManagedWorktree,
    /// The primary checkout it was cut from, when the project still has one.
    pub primary_path: Option<String>,
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
            // A managed worktree belongs to its conversation. Opening it as a
            // project would make it the workspace's default source.
            if source.worktree_branch.is_some() {
                return Err(StorageError::new(
                    "managedWorktreePath",
                    "this folder is a worktree Ottam made for a conversation; open that conversation instead",
                )
                .recoverable(true));
            }
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
                worktree_branch: None,
                worktree_base_commit: None,
                worktree_cleaned_at: None,
            })
            .execute(connection)?;
        Ok((Self::get_opened_by_source_id(connection, &source_id)?, true))
    }

    /// Record a worktree Core just made as a non-primary source of `project_id`.
    pub(crate) fn insert_worktree_source_in_transaction(
        connection: &mut SqliteConnection,
        project_id: &ProjectId,
        canonical_path: &str,
        worktree: &ManagedWorktree,
        now: DateTime<Utc>,
    ) -> Result<ProjectSource, StorageError> {
        let now = now.timestamp_millis();
        let source_id = ProjectSourceId::new_v7();
        let source_id_value = source_id.to_string();
        let project_id_value = project_id.to_string();
        diesel::insert_into(project_sources::table)
            .values(NewProjectSourceRow {
                id: &source_id_value,
                project_id: &project_id_value,
                kind: ProjectSourceKind::LocalGit.as_storage_str(),
                display_name: &worktree.branch,
                canonical_path,
                is_primary: 0,
                created_at: now,
                updated_at: now,
                last_seen_at: now,
                archived_at: None,
                worktree_branch: Some(&worktree.branch),
                worktree_base_commit: Some(&worktree.base_commit),
                worktree_cleaned_at: None,
            })
            .execute(connection)?;
        Self::get_source(connection, &source_id)
    }

    /// Archive a managed worktree source, so every conversation on it reads
    /// as detached. Returns what the caller needs to remove its directory;
    /// `archived` is false when the source was archived already.
    pub fn archive_worktree_source_in_transaction(
        connection: &mut SqliteConnection,
        source_id: &ProjectSourceId,
        now: DateTime<Utc>,
    ) -> Result<(ReleasedWorktree, bool), StorageError> {
        let source = Self::get_source(connection, source_id)?;
        let archived = source.archived_at.is_none();
        if archived {
            let now = now.timestamp_millis();
            diesel::update(project_sources::table.find(source_id.to_string()))
                .set((
                    project_sources::archived_at.eq(now),
                    project_sources::updated_at.eq(now),
                ))
                .execute(connection)?;
        }
        let source = Self::get_source(connection, source_id)?;
        Ok((Self::released(connection, source)?, archived))
    }

    fn released(
        connection: &mut SqliteConnection,
        source: ProjectSource,
    ) -> Result<ReleasedWorktree, StorageError> {
        let Some(worktree) = source.worktree.clone() else {
            return Err(StorageError::new(
                "notManagedWorktree",
                "the ProjectSource is not a worktree Ottam made",
            )
            .recoverable(true));
        };
        let primary_path = match Self::primary_source(connection, &source.project_id) {
            Ok(primary) => Some(primary.canonical_path),
            Err(error) if error.kind == "workingRootUnavailable" => None,
            Err(error) => return Err(error),
        };
        Ok(ReleasedWorktree {
            source,
            worktree,
            primary_path,
        })
    }

    /// The project's active primary checkout, which worktrees are cut from.
    pub fn primary_source(
        connection: &mut SqliteConnection,
        project_id: &ProjectId,
    ) -> Result<ProjectSource, StorageError> {
        project_sources::table
            .filter(project_sources::project_id.eq(project_id.to_string()))
            .filter(project_sources::is_primary.eq(1))
            .filter(project_sources::archived_at.is_null())
            .select(ProjectSourceRow::as_select())
            .first::<ProjectSourceRow>(connection)
            .optional()?
            .ok_or_else(|| {
                StorageError::new(
                    "workingRootUnavailable",
                    "the Project has no active checkout to make a worktree from",
                )
                .recoverable(true)
            })?
            .try_into()
    }

    /// Conversations whose project link uses `source_id`.
    pub fn conversations_on_source(
        connection: &mut SqliteConnection,
        source_id: &ProjectSourceId,
    ) -> Result<Vec<ConversationId>, StorageError> {
        conversation_projects::table
            .filter(conversation_projects::source_id.eq(source_id.to_string()))
            .filter(conversation_projects::removed_at.is_null())
            .select(conversation_projects::conversation_id)
            .distinct()
            .load::<String>(connection)?
            .into_iter()
            .map(ConversationId::try_from)
            .collect()
    }

    /// Archived worktree sources whose directories Core may still owe a
    /// removal. A settled one, or a path an active source has since claimed,
    /// is left out, so a retry never deletes a later checkout or branch.
    pub fn archived_worktree_sources(
        connection: &mut SqliteConnection,
    ) -> Result<Vec<ReleasedWorktree>, StorageError> {
        let archived = project_sources::table
            .filter(project_sources::worktree_branch.is_not_null())
            .filter(project_sources::archived_at.is_not_null())
            .filter(project_sources::worktree_cleaned_at.is_null())
            .select(ProjectSourceRow::as_select())
            .load::<ProjectSourceRow>(connection)?
            .into_iter()
            .map(ProjectSource::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let paths = archived
            .iter()
            .map(|source| source.canonical_path.clone())
            .collect::<Vec<_>>();
        let claimed = project_sources::table
            .filter(project_sources::canonical_path.eq_any(&paths))
            .filter(project_sources::archived_at.is_null())
            .select(project_sources::canonical_path)
            .load::<String>(connection)?;
        archived
            .into_iter()
            .filter(|source| !claimed.contains(&source.canonical_path))
            .map(|source| Self::released(connection, source))
            .collect()
    }

    /// An archived worktree's folder and branch are settled; no launch
    /// retries it again.
    pub fn mark_worktree_cleaned(
        connection: &mut SqliteConnection,
        source_id: &ProjectSourceId,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        diesel::update(project_sources::table.find(source_id.to_string()))
            .filter(project_sources::archived_at.is_not_null())
            .set(project_sources::worktree_cleaned_at.eq(now.timestamp_millis()))
            .execute(connection)?;
        Ok(())
    }

    /// Every worktree path any source ever held, and every branch one of the
    /// project's did, archived or not. Ottam never hands one out again, so
    /// an owed removal can only ever name its own checkout.
    pub fn worktree_names_in_use(
        connection: &mut SqliteConnection,
        project_id: &ProjectId,
    ) -> Result<(Vec<String>, Vec<String>), StorageError> {
        // Paths across every project: two projects with one display name
        // share a worktrees folder. Branches belong to the project's repository.
        let paths = project_sources::table
            .filter(project_sources::worktree_branch.is_not_null())
            .select(project_sources::canonical_path)
            .load::<String>(connection)?;
        let branches = project_sources::table
            .filter(project_sources::project_id.eq(project_id.to_string()))
            .filter(project_sources::worktree_branch.is_not_null())
            .select(project_sources::worktree_branch)
            .load::<Option<String>>(connection)?
            .into_iter()
            .flatten()
            .collect();
        Ok((paths, branches))
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
