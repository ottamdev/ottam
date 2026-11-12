use super::handlers::current_branch;
use super::request_params::{decode_params, InvalidParamsLog};
use super::responses::{core_error_response, error_response, success_response};
use super::AgentServer;
use crate::git::canonical_git_root;
use crate::storage::ensure_workspace_paths;
use crate::storage::models::{ProjectSourceKind as DomainProjectSourceKind, Workspace};
use crate::storage::repositories::{WorkspaceDetailRecord, WorkspaceRepository};
use crate::storage::StorageError;
use diesel::Connection;
use ottam_protocol::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

impl AgentServer {
    pub(super) async fn handle_workspace_list(&self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<WorkspaceListParams>(&request, InvalidParamsLog::Silent)
        {
            Ok(params) => params,
            Err(response) => return response,
        };
        let database = self.database.clone();
        match database
            .run("workspace.list", move |connection| {
                // One read snapshot, so a project added between the two
                // queries can't pair an old summary with new paths.
                connection.transaction::<_, StorageError, _>(|connection| {
                    let workspaces =
                        WorkspaceRepository::list(connection, params.include_archived)?;
                    let project_paths = WorkspaceRepository::project_paths(connection)?;
                    Ok((workspaces, project_paths))
                })
            })
            .await
        {
            Ok((workspaces, mut project_paths)) => success_response(
                request.id,
                WorkspaceListResult {
                    workspaces: workspaces
                        .into_iter()
                        .map(|workspace| WorkspaceListItem {
                            project_paths: project_paths.remove(&workspace.id).unwrap_or_default(),
                            workspace: workspace_summary(workspace),
                        })
                        .collect(),
                },
            ),
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_create(&self, request: ProtocolRequest<Value>) -> Value {
        let params =
            match decode_params::<WorkspaceCreateParams>(&request, InvalidParamsLog::Silent) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let display_name = match normalized_name(&params.display_name) {
            Ok(name) => name,
            Err(message) => return error_response(request.id, -32602, message),
        };
        let database = self.database.clone();
        match database
            .run("workspace.create", move |connection| {
                let workspace =
                    WorkspaceRepository::create(connection, &display_name, chrono::Utc::now())?;
                WorkspaceRepository::detail(connection, &workspace.id)
            })
            .await
        {
            Ok(detail) => {
                workspace_detail_response(request.id, detail, self.database.home().to_path_buf())
                    .await
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_open(&self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<WorkspaceOpenParams>(&request, InvalidParamsLog::Silent)
        {
            Ok(params) => params,
            Err(response) => return response,
        };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let database = self.database.clone();
        match database
            .run("workspace.open", move |connection| {
                WorkspaceRepository::open(connection, &workspace_id, chrono::Utc::now())
            })
            .await
        {
            Ok(detail) => {
                // Opening a Workspace is the natural moment to collect image
                // attachments from drafts that were never sent. Cleanup is
                // best-effort and must not hold the serial request dispatcher.
                // Repeated opens share one in-flight sweep per Workspace.
                if let Some(sweep_permit) = self.attachment_sweeps.try_start(workspace_id) {
                    let sweep_database = self.database.clone();
                    tokio::spawn(async move {
                        let _sweep_permit = sweep_permit;
                        super::chat::sweep_attachments(&sweep_database, &workspace_id).await;
                    });
                }
                workspace_detail_response(request.id, detail, self.database.home().to_path_buf())
                    .await
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_get(&self, request: ProtocolRequest<Value>) -> Value {
        let params = match decode_params::<WorkspaceGetParams>(&request, InvalidParamsLog::Silent) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let database = self.database.clone();
        match database
            .run("workspace.get", move |connection| {
                WorkspaceRepository::detail(connection, &workspace_id)
            })
            .await
        {
            Ok(detail) => {
                workspace_detail_response(request.id, detail, self.database.home().to_path_buf())
                    .await
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_update(&self, request: ProtocolRequest<Value>) -> Value {
        let params =
            match decode_params::<WorkspaceUpdateParams>(&request, InvalidParamsLog::Silent) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let display_name = match normalized_name(&params.display_name) {
            Ok(name) => name,
            Err(message) => return error_response(request.id, -32602, message),
        };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let database = self.database.clone();
        match database
            .run("workspace.update", move |connection| {
                WorkspaceRepository::rename(
                    connection,
                    &workspace_id,
                    &display_name,
                    chrono::Utc::now(),
                )?;
                WorkspaceRepository::detail(connection, &workspace_id)
            })
            .await
        {
            Ok(detail) => {
                workspace_detail_response(request.id, detail, self.database.home().to_path_buf())
                    .await
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_archive(&self, request: ProtocolRequest<Value>) -> Value {
        self.set_workspace_archived(request, true).await
    }

    pub(super) async fn handle_workspace_restore(&self, request: ProtocolRequest<Value>) -> Value {
        self.set_workspace_archived(request, false).await
    }

    async fn set_workspace_archived(
        &self,
        request: ProtocolRequest<Value>,
        archived: bool,
    ) -> Value {
        let params = match decode_params::<WorkspaceIDParams>(&request, InvalidParamsLog::Silent) {
            Ok(params) => params,
            Err(response) => return response,
        };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let database = self.database.clone();
        let operation = if archived {
            "workspace.archive"
        } else {
            "workspace.restore"
        };
        match database
            .run(operation, move |connection| {
                let now = chrono::Utc::now();
                WorkspaceRepository::set_archived(
                    connection,
                    &workspace_id,
                    archived.then_some(now),
                    now,
                )
            })
            .await
        {
            Ok(workspace) => {
                // An archived workspace's local MCP processes stop; they
                // start again with its next session.
                if let (true, Some(mcp)) = (archived, self.mcp.as_ref()) {
                    let runtime = mcp.runtime().clone();
                    let workspace_id = params.workspace_id.clone();
                    tokio::spawn(async move { runtime.stop_workspace(&workspace_id).await });
                }
                success_response(request.id, workspace_summary(workspace))
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_add_project(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<WorkspaceAddProjectParams>(&request, InvalidParamsLog::Silent) {
                Ok(params) => params,
                Err(response) => return response,
            };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let requested_path = params.path.clone();
        let canonical_path = match crate::git::run_blocking("canonicalGitRoot", move || {
            canonical_git_root(&requested_path)
        })
        .await
        {
            Ok(path) => path,
            Err(error) => return core_error_response(request.id, -32000, error),
        };
        let display_name = Path::new(&canonical_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Project")
            .to_string();
        let database = self.database.clone();
        let canonical_for_store = canonical_path.clone();
        match database
            .run("workspace.addProject", move |connection| {
                WorkspaceRepository::add_local_git_project(
                    connection,
                    &workspace_id,
                    &canonical_for_store,
                    &display_name,
                    chrono::Utc::now(),
                )
            })
            .await
        {
            Ok((detail, project_id)) => {
                let project_id = ProjectID(project_id.to_string());
                if let Some(member) = detail
                    .projects
                    .iter()
                    .find(|member| member.project.id.to_string() == project_id.0)
                {
                    let source_id = ProjectSourceID(member.source.id.to_string());
                    self.project_roots
                        .insert(project_id.clone(), member.source.canonical_path.clone());
                    self.source_roots
                        .insert(source_id, member.source.canonical_path.clone());
                }
                match workspace_detail_blocking(detail, self.database.home().to_path_buf()).await {
                    Ok(workspace) => success_response(
                        request.id,
                        WorkspaceAddProjectResult {
                            workspace,
                            project_id,
                        },
                    ),
                    Err(error) => core_error_response(request.id, -32000, error),
                }
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }

    pub(super) async fn handle_workspace_remove_project(
        &mut self,
        request: ProtocolRequest<Value>,
    ) -> Value {
        let params =
            match decode_params::<WorkspaceRemoveProjectParams>(&request, InvalidParamsLog::Silent)
            {
                Ok(params) => params,
                Err(response) => return response,
            };
        let workspace_id =
            match crate::storage::models::WorkspaceId::try_from(params.workspace_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        let project_id =
            match crate::storage::models::ProjectId::try_from(params.project_id.0.as_str()) {
                Ok(id) => id,
                Err(error) => return core_error_response(request.id, -32000, error.into()),
            };
        if let Some(agent_bridge) = self.agent_bridge.as_ref() {
            agent_bridge
                .cancel_project_runs(&params.workspace_id, &params.project_id)
                .await;
        }
        if let Some(terminal_manager) = self.terminal_manager.as_mut() {
            terminal_manager.close_project_terminals(&params.workspace_id, &params.project_id);
        }
        let database = self.database.clone();
        match database
            .run("workspace.removeProject", move |connection| {
                WorkspaceRepository::remove_project(
                    connection,
                    &workspace_id,
                    &project_id,
                    chrono::Utc::now(),
                )?;
                WorkspaceRepository::detail(connection, &workspace_id)
            })
            .await
        {
            Ok(detail) => {
                workspace_detail_response(request.id, detail, self.database.home().to_path_buf())
                    .await
            }
            Err(error) => core_error_response(request.id, -32000, error.into()),
        }
    }
}

fn normalized_name(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        Err("Workspace name must not be empty")
    } else {
        Ok(value.to_string())
    }
}

pub(super) fn workspace_summary(workspace: Workspace) -> WorkspaceSummary {
    WorkspaceSummary {
        workspace_id: WorkspaceID(workspace.id.to_string()),
        display_name: workspace.display_name,
        created_at: workspace.created_at.timestamp_millis(),
        updated_at: workspace.updated_at.timestamp_millis(),
        last_opened_at: workspace.last_opened_at.timestamp_millis(),
        archived_at: workspace.archived_at.map(|value| value.timestamp_millis()),
    }
}

async fn workspace_detail_response(
    request_id: RequestID,
    detail: WorkspaceDetailRecord,
    home: PathBuf,
) -> Value {
    match workspace_detail_blocking(detail, home).await {
        Ok(detail) => success_response(request_id, detail),
        Err(error) => core_error_response(request_id, -32000, error),
    }
}

/// Build a workspace detail off the dispatcher.
///
/// `workspace_detail` provisions the managed directories (~9 filesystem calls)
/// and runs `git symbolic-ref` for every member project, so resolving a
/// workspace with several projects would otherwise hold the serial dispatch
/// loop for the duration of all of them.
pub(super) async fn workspace_detail_blocking(
    detail: WorkspaceDetailRecord,
    home: PathBuf,
) -> Result<WorkspaceDetail, crate::core_error::CoreError> {
    crate::git::run_blocking("workspaceDetail", move || workspace_detail(detail, &home)).await
}

pub(super) fn workspace_detail(
    detail: WorkspaceDetailRecord,
    home: &Path,
) -> Result<WorkspaceDetail, crate::core_error::CoreError> {
    let paths = ensure_workspace_paths(home, &detail.workspace.id)?;
    Ok(WorkspaceDetail {
        workspace: workspace_summary(detail.workspace),
        workspace_files: WorkingRootSummary {
            kind: WorkingRootKind::WorkspaceFiles,
            display_name: "Workspace files".to_string(),
            canonical_path: paths.files.to_string_lossy().into_owned(),
            project_id: None,
            source_id: None,
            availability: WorkingRootAvailability::Available,
        },
        projects: detail
            .projects
            .into_iter()
            .map(|member| WorkspaceProjectSummary {
                project: ProjectSummary {
                    project_id: ProjectID(member.project.id.to_string()),
                    display_name: member.project.display_name,
                    created_at: member.project.created_at.timestamp_millis(),
                    updated_at: member.project.updated_at.timestamp_millis(),
                    last_opened_at: member.project.last_opened_at.timestamp_millis(),
                    archived_at: member
                        .project
                        .archived_at
                        .map(|value| value.timestamp_millis()),
                },
                source: ProjectSourceSummary {
                    project_source_id: ProjectSourceID(member.source.id.to_string()),
                    project_id: ProjectID(member.source.project_id.to_string()),
                    kind: match member.source.kind {
                        DomainProjectSourceKind::LocalGit => ProjectSourceKind::LocalGit,
                    },
                    display_name: member.source.display_name,
                    current_branch: current_branch(&member.source.canonical_path),
                    canonical_path: member.source.canonical_path,
                    created_at: member.source.created_at.timestamp_millis(),
                    updated_at: member.source.updated_at.timestamp_millis(),
                    archived_at: member
                        .source
                        .archived_at
                        .map(|value| value.timestamp_millis()),
                },
                sort_order: member.membership.sort_order,
                added_at: member.membership.added_at.timestamp_millis(),
            })
            .collect(),
    })
}
