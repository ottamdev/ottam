use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    NewWorkspaceProjectRow, NewWorkspaceRow, Project, ProjectId, ProjectRow, ProjectSource,
    ProjectSourceId, ProjectSourceRow, Workspace, WorkspaceId, WorkspaceProject,
    WorkspaceProjectRow, WorkspaceRow,
};
use crate::storage::repositories::ProjectRepository;
use crate::storage::schema::{
    conversation_projects, project_sources, projects, workspace_projects, workspaces,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

pub struct WorkspaceRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProjectDetail {
    pub membership: WorkspaceProject,
    pub project: Project,
    pub source: ProjectSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDetailRecord {
    pub workspace: Workspace,
    pub projects: Vec<WorkspaceProjectDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProjectRoot {
    pub project: Project,
    pub source: ProjectSource,
}

impl WorkspaceRepository {
    pub fn resolve_project_root(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        project_id: &ProjectId,
        source_id: &ProjectSourceId,
    ) -> Result<WorkspaceProjectRoot, StorageError> {
        let workspace = Self::get(connection, workspace_id)?;
        if workspace.archived_at.is_some() {
            return Err(
                StorageError::new("unknownWorkspaceID", "Workspace is unavailable")
                    .recoverable(true),
            );
        }
        let project_id_value = project_id.to_string();
        let source_id_value = source_id.to_string();
        workspace_projects::table
            .inner_join(projects::table.on(projects::id.eq(workspace_projects::project_id)))
            .inner_join(
                project_sources::table
                    .on(project_sources::id.eq(workspace_projects::default_source_id)),
            )
            .filter(workspace_projects::workspace_id.eq(workspace_id.to_string()))
            .filter(workspace_projects::project_id.eq(&project_id_value))
            .filter(workspace_projects::default_source_id.eq(&source_id_value))
            .filter(workspace_projects::removed_at.is_null())
            .filter(projects::archived_at.is_null())
            .filter(project_sources::archived_at.is_null())
            .select((ProjectRow::as_select(), ProjectSourceRow::as_select()))
            .first::<(ProjectRow, ProjectSourceRow)>(connection)
            .optional()?
            .ok_or_else(|| {
                StorageError::new(
                    "workingRootUnavailable",
                    "the selected Project source is not attached to this Workspace",
                )
                .recoverable(true)
            })
            .and_then(|(project, source)| {
                Ok(WorkspaceProjectRoot {
                    project: project.try_into()?,
                    source: source.try_into()?,
                })
            })
    }

    pub fn create(
        connection: &mut SqliteConnection,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Workspace, StorageError> {
        let id = WorkspaceId::new_v7();
        let id_value = id.to_string();
        let now = now.timestamp_millis();
        diesel::insert_into(workspaces::table)
            .values(NewWorkspaceRow {
                id: &id_value,
                display_name,
                created_at: now,
                updated_at: now,
                last_opened_at: now,
                archived_at: None,
            })
            .execute(connection)?;
        Self::get(connection, &id)
    }

    pub fn get(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<Workspace, StorageError> {
        let workspace_id = workspace_id.to_string();
        workspaces::table
            .find(&workspace_id)
            .select(WorkspaceRow::as_select())
            .first::<WorkspaceRow>(connection)
            .map_err(StorageError::from)?
            .try_into()
    }

    pub fn list(
        connection: &mut SqliteConnection,
        include_archived: bool,
    ) -> Result<Vec<Workspace>, StorageError> {
        let mut query = workspaces::table.into_boxed();
        if !include_archived {
            query = query.filter(workspaces::archived_at.is_null());
        }
        query
            .order((
                workspaces::archived_at.asc(),
                workspaces::last_opened_at.desc(),
                workspaces::id.asc(),
            ))
            .select(WorkspaceRow::as_select())
            .load::<WorkspaceRow>(connection)
            .map_err(StorageError::from)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    /// Each Workspace's project paths in project order, for the list. The same
    /// membership and source rule as `detail`. A Workspace with no projects
    /// has no entry.
    pub fn project_paths(
        connection: &mut SqliteConnection,
    ) -> Result<HashMap<WorkspaceId, Vec<String>>, StorageError> {
        let rows = workspace_projects::table
            .inner_join(
                project_sources::table
                    .on(project_sources::id.eq(workspace_projects::default_source_id)),
            )
            .filter(workspace_projects::removed_at.is_null())
            .order((
                workspace_projects::workspace_id.asc(),
                workspace_projects::sort_order.asc(),
                workspace_projects::project_id.asc(),
            ))
            .select((
                workspace_projects::workspace_id,
                project_sources::canonical_path,
            ))
            .load::<(String, String)>(connection)?;
        let mut paths: HashMap<WorkspaceId, Vec<String>> = HashMap::new();
        for (workspace_id, path) in rows {
            paths
                .entry(WorkspaceId::try_from(workspace_id)?)
                .or_default()
                .push(path);
        }
        Ok(paths)
    }

    pub fn open(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        now: DateTime<Utc>,
    ) -> Result<WorkspaceDetailRecord, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        recorded_transaction(connection, "workspace.open", |connection| {
            let changed = diesel::update(
                workspaces::table
                    .find(&workspace_id_value)
                    .filter(workspaces::archived_at.is_null()),
            )
            .set(workspaces::last_opened_at.eq(now.timestamp_millis()))
            .execute(connection)?;
            if changed == 0 {
                let workspace = Self::get(connection, workspace_id)?;
                if workspace.archived_at.is_some() {
                    return Err(
                        StorageError::new("storageConstraint", "Workspace is archived")
                            .recoverable(true),
                    );
                }
                return Err(StorageError::not_found("Workspace"));
            }
            Self::detail(connection, workspace_id)
        })
    }

    pub fn detail(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceDetailRecord, StorageError> {
        let workspace = Self::get(connection, workspace_id)?;
        let workspace_id_value = workspace_id.to_string();
        // One join instead of 1 + 2N point lookups (get_project + get_source per member).
        let projects = workspace_projects::table
            .inner_join(projects::table.on(projects::id.eq(workspace_projects::project_id)))
            .inner_join(
                project_sources::table
                    .on(project_sources::id.eq(workspace_projects::default_source_id)),
            )
            .filter(workspace_projects::workspace_id.eq(&workspace_id_value))
            .filter(workspace_projects::removed_at.is_null())
            .order((
                workspace_projects::sort_order.asc(),
                workspace_projects::project_id.asc(),
            ))
            .select((
                WorkspaceProjectRow::as_select(),
                ProjectRow::as_select(),
                ProjectSourceRow::as_select(),
            ))
            .load::<(WorkspaceProjectRow, ProjectRow, ProjectSourceRow)>(connection)?
            .into_iter()
            .map(|(membership, project, source)| {
                Ok(WorkspaceProjectDetail {
                    membership: membership.try_into()?,
                    project: project.try_into()?,
                    source: source.try_into()?,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        Ok(WorkspaceDetailRecord {
            workspace,
            projects,
        })
    }

    pub fn workspaces_for_project(
        connection: &mut SqliteConnection,
        project_id: &ProjectId,
    ) -> Result<Vec<Workspace>, StorageError> {
        let project_id = project_id.to_string();
        workspaces::table
            .inner_join(
                workspace_projects::table.on(workspace_projects::workspace_id.eq(workspaces::id)),
            )
            .filter(workspace_projects::project_id.eq(project_id))
            .filter(workspace_projects::removed_at.is_null())
            .order((
                workspaces::archived_at.asc(),
                workspaces::last_opened_at.desc(),
                workspaces::id.asc(),
            ))
            .select(WorkspaceRow::as_select())
            .load::<WorkspaceRow>(connection)
            .map_err(StorageError::from)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    pub fn rename(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<Workspace, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        let changed = diesel::update(workspaces::table.find(&workspace_id_value))
            .set((
                workspaces::display_name.eq(display_name),
                workspaces::updated_at.eq(now.timestamp_millis()),
            ))
            .execute(connection)?;
        if changed == 0 {
            return Err(StorageError::not_found("Workspace"));
        }
        Self::get(connection, workspace_id)
    }

    pub fn set_archived(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        archived_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Workspace, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        let changed = diesel::update(workspaces::table.find(&workspace_id_value))
            .set((
                workspaces::archived_at.eq(archived_at.map(|value| value.timestamp_millis())),
                workspaces::updated_at.eq(now.timestamp_millis()),
            ))
            .execute(connection)?;
        if changed == 0 {
            return Err(StorageError::not_found("Workspace"));
        }
        Self::get(connection, workspace_id)
    }

    pub fn add_project(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        project_id: &ProjectId,
        source_id: &ProjectSourceId,
        sort_order: i64,
        now: DateTime<Utc>,
    ) -> Result<WorkspaceProject, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        let project_id_value = project_id.to_string();
        let source_id_value = source_id.to_string();
        let now = now.timestamp_millis();
        recorded_transaction(connection, "workspace.add_project", |connection| {
            let (source_project, source_archived_at) = project_sources::table
                .find(&source_id_value)
                .select((project_sources::project_id, project_sources::archived_at))
                .first::<(String, Option<i64>)>(connection)?;
            if source_project != project_id_value {
                return Err(StorageError::new(
                    "storageConstraint",
                    "ProjectSource belongs to another Project",
                )
                .recoverable(true));
            }
            if source_archived_at.is_some() {
                return Err(
                    StorageError::new("storageConstraint", "ProjectSource is archived")
                        .recoverable(true),
                );
            }
            diesel::insert_into(workspace_projects::table)
                .values(NewWorkspaceProjectRow {
                    workspace_id: &workspace_id_value,
                    project_id: &project_id_value,
                    default_source_id: &source_id_value,
                    sort_order,
                    added_at: now,
                    removed_at: None,
                })
                .on_conflict((
                    workspace_projects::workspace_id,
                    workspace_projects::project_id,
                ))
                .do_update()
                .set((
                    workspace_projects::default_source_id.eq(&source_id_value),
                    workspace_projects::sort_order.eq(sort_order),
                    workspace_projects::added_at.eq(now),
                    workspace_projects::removed_at.eq::<Option<i64>>(None),
                ))
                .execute(connection)?;
            diesel::update(workspaces::table.find(&workspace_id_value))
                .set(workspaces::updated_at.eq(now))
                .execute(connection)?;
            Self::membership(connection, workspace_id, project_id)
        })
    }

    pub fn add_local_git_project(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        canonical_path: &str,
        display_name: &str,
        now: DateTime<Utc>,
    ) -> Result<(WorkspaceDetailRecord, ProjectId), StorageError> {
        let workspace_id_value = workspace_id.to_string();
        recorded_transaction(
            connection,
            "workspace.add_local_git_project",
            |connection| {
                let workspace = Self::get(connection, workspace_id)?;
                if workspace.archived_at.is_some() {
                    return Err(
                        StorageError::new("storageConstraint", "Workspace is archived")
                            .recoverable(true),
                    );
                }
                let (opened, _) = ProjectRepository::resolve_local_git_in_transaction(
                    connection,
                    canonical_path,
                    display_name,
                    now,
                )?;
                let project_id = opened.project.id;
                let project_id_value = project_id.to_string();
                let source_id_value = opened.source.id.to_string();
                let existing = workspace_projects::table
                    .find((&workspace_id_value, &project_id_value))
                    .select(WorkspaceProjectRow::as_select())
                    .first::<WorkspaceProjectRow>(connection)
                    .optional()?;
                if existing
                    .as_ref()
                    .is_some_and(|row| row.removed_at.is_none())
                {
                    diesel::update(
                        workspace_projects::table.find((&workspace_id_value, &project_id_value)),
                    )
                    .set(workspace_projects::default_source_id.eq(&source_id_value))
                    .execute(connection)?;
                    diesel::update(workspaces::table.find(&workspace_id_value))
                        .set(workspaces::updated_at.eq(now.timestamp_millis()))
                        .execute(connection)?;
                    return Ok((Self::detail(connection, workspace_id)?, project_id));
                }
                let sort_order = workspace_projects::table
                    .filter(workspace_projects::workspace_id.eq(&workspace_id_value))
                    .filter(workspace_projects::removed_at.is_null())
                    .select(diesel::dsl::max(workspace_projects::sort_order))
                    .first::<Option<i64>>(connection)?
                    .map_or(0, |value| value + 1);
                let now_millis = now.timestamp_millis();
                diesel::insert_into(workspace_projects::table)
                    .values(NewWorkspaceProjectRow {
                        workspace_id: &workspace_id_value,
                        project_id: &project_id_value,
                        default_source_id: &source_id_value,
                        sort_order,
                        added_at: now_millis,
                        removed_at: None,
                    })
                    .on_conflict((
                        workspace_projects::workspace_id,
                        workspace_projects::project_id,
                    ))
                    .do_update()
                    .set((
                        workspace_projects::default_source_id.eq(&source_id_value),
                        workspace_projects::sort_order.eq(sort_order),
                        workspace_projects::added_at.eq(now_millis),
                        workspace_projects::removed_at.eq::<Option<i64>>(None),
                    ))
                    .execute(connection)?;
                diesel::update(workspaces::table.find(&workspace_id_value))
                    .set(workspaces::updated_at.eq(now_millis))
                    .execute(connection)?;
                Ok((Self::detail(connection, workspace_id)?, project_id))
            },
        )
    }

    pub fn remove_project(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        project_id: &ProjectId,
        now: DateTime<Utc>,
    ) -> Result<WorkspaceProject, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        let project_id_value = project_id.to_string();
        let now = now.timestamp_millis();
        recorded_transaction(connection, "workspace.remove_project", |connection| {
            let changed = diesel::update(
                workspace_projects::table
                    .find((&workspace_id_value, &project_id_value))
                    .filter(workspace_projects::removed_at.is_null()),
            )
            .set(workspace_projects::removed_at.eq(now))
            .execute(connection)?;
            if changed == 0 {
                return Err(StorageError::not_found("WorkspaceProject"));
            }
            diesel::update(
                conversation_projects::table
                    .filter(conversation_projects::workspace_id.eq(&workspace_id_value))
                    .filter(conversation_projects::project_id.eq(&project_id_value))
                    .filter(conversation_projects::removed_at.is_null()),
            )
            .set(conversation_projects::removed_at.eq(now))
            .execute(connection)?;
            diesel::update(workspaces::table.find(&workspace_id_value))
                .set(workspaces::updated_at.eq(now))
                .execute(connection)?;
            Self::membership(connection, workspace_id, project_id)
        })
    }

    pub fn membership(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        project_id: &ProjectId,
    ) -> Result<WorkspaceProject, StorageError> {
        let workspace_id = workspace_id.to_string();
        let project_id = project_id.to_string();
        workspace_projects::table
            .find((&workspace_id, &project_id))
            .select(WorkspaceProjectRow::as_select())
            .first::<WorkspaceProjectRow>(connection)
            .map_err(StorageError::from)?
            .try_into()
    }

    pub fn list_memberships(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<WorkspaceProject>, StorageError> {
        let workspace_id = workspace_id.to_string();
        workspace_projects::table
            .filter(workspace_projects::workspace_id.eq(workspace_id))
            .filter(workspace_projects::removed_at.is_null())
            .order((
                workspace_projects::sort_order.asc(),
                workspace_projects::project_id.asc(),
            ))
            .select(WorkspaceProjectRow::as_select())
            .load::<WorkspaceProjectRow>(connection)
            .map_err(StorageError::from)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }
}
