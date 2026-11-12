use crate::core_error::CoreError;
use crate::storage::models::{ProjectId, ProjectSourceId, WorkspaceId};
use crate::storage::repositories::WorkspaceRepository;
use crate::storage::{ensure_workspace_paths, Database};
use ottam_protocol::{
    ProjectID, ProjectSourceID, WorkingRootAvailability, WorkingRootKind, WorkingRootSelection,
    WorkingRootSummary, WorkspaceID,
};
use std::path::PathBuf;

#[derive(Clone)]
pub struct WorkspaceRuntimeStore {
    database: Database,
    workspace_id: WorkspaceID,
}

#[derive(Debug, Clone)]
pub struct ResolvedWorkingRoot {
    pub summary: WorkingRootSummary,
}

impl ResolvedWorkingRoot {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(&self.summary.canonical_path)
    }

    pub fn project_id(&self) -> Option<ProjectID> {
        self.summary.project_id.clone()
    }
}

impl WorkspaceRuntimeStore {
    pub fn new(database: Database, workspace_id: WorkspaceID) -> Self {
        Self {
            database,
            workspace_id,
        }
    }

    pub fn workspace_id(&self) -> &WorkspaceID {
        &self.workspace_id
    }

    pub async fn resolve_working_root(
        &self,
        selection: &WorkingRootSelection,
    ) -> Result<ResolvedWorkingRoot, CoreError> {
        let workspace_id = WorkspaceId::try_from(self.workspace_id.0.as_str())?;
        match selection {
            WorkingRootSelection::WorkspaceFiles => {
                let workspace_for_query = workspace_id;
                self.database
                    .run("working_root.workspace", move |connection| {
                        let workspace = WorkspaceRepository::get(connection, &workspace_for_query)?;
                        if workspace.archived_at.is_some() {
                            return Err(crate::storage::StorageError::new(
                                "unknownWorkspaceID",
                                "Workspace is unavailable",
                            )
                            .recoverable(true));
                        }
                        Ok(())
                    })
                    .await?;
                let paths = ensure_workspace_paths(self.database.home(), &workspace_id)?;
                Ok(ResolvedWorkingRoot {
                    summary: WorkingRootSummary {
                        kind: WorkingRootKind::WorkspaceFiles,
                        display_name: "Workspace files".to_string(),
                        canonical_path: paths.files.to_string_lossy().into_owned(),
                        project_id: None,
                        source_id: None,
                        availability: WorkingRootAvailability::Available,
                    },
                })
            }
            WorkingRootSelection::Project {
                project_id,
                source_id,
            } => {
                let project = ProjectId::try_from(project_id.0.as_str())?;
                let source = ProjectSourceId::try_from(source_id.0.as_str())?;
                let root = self
                    .database
                    .run("working_root.project", move |connection| {
                        WorkspaceRepository::resolve_project_root(
                            connection,
                            &workspace_id,
                            &project,
                            &source,
                        )
                    })
                    .await?;
                Ok(ResolvedWorkingRoot {
                    summary: WorkingRootSummary {
                        kind: WorkingRootKind::Project,
                        display_name: root.project.display_name,
                        canonical_path: root.source.canonical_path,
                        project_id: Some(ProjectID(root.project.id.to_string())),
                        source_id: Some(ProjectSourceID(root.source.id.to_string())),
                        availability: WorkingRootAvailability::Available,
                    },
                })
            }
        }
    }
}
