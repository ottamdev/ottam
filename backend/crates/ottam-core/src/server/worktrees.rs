//! Managed worktrees. Core cuts a branch off a project's primary checkout,
//! checks it out under `$OTTAM_HOME/worktrees`, records it as a non-primary
//! ProjectSource, and removes it when the last conversation working in it
//! goes or the user removes it.
//!
//! Creation is filesystem then database, undoing the checkout when the
//! transaction fails. Removal is database then filesystem: the archived row
//! is what owes the removal, so a failure is retried on the next launch.

use super::chat::NewWorktreeSource;
use super::request_params::{decode_params, InvalidParamsLog};
use super::responses::{core_error_response, error_response, success_response};
use super::{AgentServer, ConnectionId};
use crate::core_error::CoreError;
use crate::git;
use crate::storage::database::recorded_transaction;
use crate::storage::models::{ManagedWorktree, ProjectId, ProjectSourceId, WorkspaceId};
use crate::storage::repositories::{ProjectRepository, ReleasedWorktree, WorkspaceRepository};
use crate::storage::{worktrees_root, Database};
use ottam_protocol::{
    ConversationCreateParams, ConversationID, ConversationListChangedParams, ProjectSourceID,
    ProtocolNotification, ProtocolRequest, RequestID, WorkingRootSelection, WorkspaceID,
    WorktreeRemoval, WorktreeRemoveResult, WorktreeSourceParams, WorktreeStatusResult,
    METHOD_CONVERSATION_LIST_CHANGED,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Worktree checkouts and removals run on their own lane: each can hold its
/// slot for as long as git takes to write or delete a tree.
pub(super) const WORKTREE_SLOTS: usize = 4;

/// Words of the first message a branch name keeps.
const BRANCH_WORDS: usize = 4;
const BRANCH_WORDS_MAX_CHARS: usize = 32;
/// Tries at a free branch and directory name before giving up.
const NAME_ATTEMPTS: usize = 8;

/// A checkout Core made and has not recorded yet.
pub(super) struct CreatedCheckout {
    pub canonical_path: String,
    pub primary_path: String,
    pub worktree: ManagedWorktree,
}

/// `git worktree add` a new `ottam/<words>-<hex>` branch off the primary
/// checkout's HEAD into `$OTTAM_HOME/worktrees/<project>/<words>-<hex>`.
pub(super) fn create_checkout(
    home: &Path,
    project_name: &str,
    primary_path: &str,
    message: Option<&str>,
) -> Result<CreatedCheckout, CoreError> {
    // Before anything touches the disk, so a repository with no commits
    // leaves nothing behind.
    let base_commit = git::head_commit(primary_path)?.ok_or_else(|| {
        CoreError::new(
            "worktreeNoCommits",
            "This repository has no commits yet. Make a first commit, then start a worktree chat.",
        )
        .with_path(primary_path)
        .recoverable(true)
    })?;
    let words = slug(message.unwrap_or_default(), "chat");
    let project_dir = worktrees_root(home).join(slug(project_name, "project"));
    std::fs::create_dir_all(&project_dir).map_err(|error| {
        CoreError::new(
            "worktreeCreateFailed",
            format!("could not create the worktrees folder: {error}"),
        )
        .with_path(project_dir.to_string_lossy())
        .recoverable(true)
    })?;
    // Git records the path it is given; a canonical one is what the source
    // row stores and what removal later names.
    let project_dir = std::fs::canonicalize(&project_dir).map_err(|error| {
        CoreError::new(
            "worktreeCreateFailed",
            format!("could not resolve the worktrees folder: {error}"),
        )
        .recoverable(true)
    })?;
    let mut outcome = Err(CoreError::new(
        "worktreeNameTaken",
        "Ottam could not find a free branch name for the worktree",
    )
    .recoverable(true));
    for _ in 0..NAME_ATTEMPTS {
        let name = format!("{words}-{}", random_suffix());
        let branch = format!("ottam/{name}");
        let path = project_dir.join(&name);
        if path.exists() || git::branch_commit(primary_path, &branch)?.is_some() {
            continue;
        }
        outcome = git::worktree_add(primary_path, &path, &branch, &base_commit).map(|()| {
            CreatedCheckout {
                canonical_path: path.to_string_lossy().into_owned(),
                primary_path: primary_path.to_string(),
                worktree: ManagedWorktree {
                    branch,
                    base_commit: base_commit.clone(),
                },
            }
        });
        break;
    }
    if outcome.is_err() {
        // Only empty, so another worktree of the project is never touched.
        let _ = std::fs::remove_dir(&project_dir);
    }
    outcome
}

/// Undo a checkout whose conversation was never recorded.
pub(super) fn discard_checkout(created: &CreatedCheckout) {
    let path = PathBuf::from(&created.canonical_path);
    if let Err(error) = git::worktree_remove(&created.primary_path, &path) {
        tracing::warn!(kind = error.kind, "an unrecorded worktree was not removed");
    }
    if let Err(error) = git::delete_branch_if_unchanged(
        &created.primary_path,
        &created.worktree.branch,
        &created.worktree.base_commit,
    ) {
        tracing::warn!(kind = error.kind, "an unrecorded worktree's branch was not deleted");
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Remove an archived worktree's directory, then its branch when it has no
/// commits of its own. Never touches a path outside `$OTTAM_HOME/worktrees`.
pub(super) fn remove_checkout(home: &Path, released: &ReleasedWorktree) -> WorktreeRemoval {
    let path = PathBuf::from(&released.source.canonical_path);
    let primary = released
        .primary_path
        .as_deref()
        .filter(|primary| Path::new(primary).is_dir());
    let mut removal = WorktreeRemoval {
        project_source_id: ProjectSourceID(released.source.id.to_string()),
        branch: released.worktree.branch.clone(),
        path: released.source.canonical_path.clone(),
        removed: false,
        branch_kept: true,
        message: None,
    };
    if path.exists() {
        let inside_home = worktrees_root(home)
            .canonicalize()
            .ok()
            .zip(path.canonicalize().ok())
            .is_some_and(|(root, path)| path.starts_with(&root) && path != root);
        if !inside_home {
            tracing::warn!("a worktree path outside Ottam's home was not removed");
            removal.message = Some("Ottam removes only worktrees it made in its own folder.".into());
            return removal;
        }
        let removed_by_git = primary.is_some_and(|primary| {
            git::worktree_remove(primary, &path)
                .inspect_err(|error| {
                    tracing::warn!(kind = error.kind, "git worktree remove failed; deleting the folder")
                })
                .is_ok()
        });
        if !removed_by_git {
            if let Err(error) = std::fs::remove_dir_all(&path) {
                tracing::warn!(%error, "a worktree folder was not removed");
                // The app quotes this after the path; the retry promise is
                // Core's to make, since only Core knows the row owes it.
                removal.message = Some(format!("{error}. Ottam tries again on its next launch."));
                return removal;
            }
        }
    }
    removal.removed = true;
    if let Some(parent) = path.parent() {
        // Only empty: the project's other worktrees stay.
        let _ = std::fs::remove_dir(parent);
    }
    let Some(primary) = primary else {
        removal.message = Some("The project's checkout is gone, so its branch was left alone.".into());
        return removal;
    };
    if let Err(error) = git::worktree_prune(primary) {
        tracing::warn!(kind = error.kind, "git worktree prune failed");
    }
    match git::delete_branch_if_unchanged(
        primary,
        &released.worktree.branch,
        &released.worktree.base_commit,
    ) {
        Ok(kept) => removal.branch_kept = kept,
        Err(error) => {
            tracing::warn!(kind = error.kind, "a worktree branch was not checked");
            removal.message = Some(format!("The branch was kept: {}", error.message));
        }
    }
    removal
}

/// Retry the removals archived worktrees still owe: their directories
/// survived a failed removal or a quit mid-removal.
pub(super) async fn retry_owed_removals(database: Database) {
    let owed = database
        .run("worktree.owedRemovals", |connection| {
            ProjectRepository::archived_worktree_sources(connection)
        })
        .await;
    let owed = match owed {
        Ok(owed) => owed,
        Err(error) => {
            tracing::warn!(kind = error.kind, "owed worktree removals were not listed");
            return;
        }
    };
    let home = database.home().to_path_buf();
    for released in owed {
        if !Path::new(&released.source.canonical_path).exists() {
            continue;
        }
        let home = home.clone();
        let removal = git::run_blocking("worktreeRemove", move || {
            Ok(remove_checkout(&home, &released))
        })
        .await;
        match removal {
            Ok(removal) => tracing::info!(removed = removal.removed, "retried an owed worktree removal"),
            Err(error) => tracing::warn!(kind = error.kind, "an owed worktree removal failed"),
        }
    }
}

/// Lowercase ASCII words joined by `-`, for a branch or folder name.
fn slug(text: &str, fallback: &str) -> String {
    let mut slug = String::new();
    for word in text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(BRANCH_WORDS)
    {
        let word = word.to_ascii_lowercase();
        if !slug.is_empty() && slug.len() + 1 + word.len() > BRANCH_WORDS_MAX_CHARS {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.extend(word.chars().take(BRANCH_WORDS_MAX_CHARS));
    }
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

/// Four hex digits from a v7 UUID's random tail.
fn random_suffix() -> String {
    let simple = uuid::Uuid::now_v7().simple().to_string();
    simple[simple.len() - 4..].to_string()
}

fn invalid_id(kind: &'static str) -> CoreError {
    CoreError::new(kind, "the request names an unknown id").recoverable(true)
}

impl AgentServer {
    /// `conversation.create` with `createWorktree`: resolve the primary
    /// checkout inline, then check out and record off the dispatcher.
    pub(super) async fn handle_worktree_conversation_create(
        &mut self,
        origin: ConnectionId,
        request_id: RequestID,
        params: ConversationCreateParams,
    ) -> Option<Value> {
        let WorkingRootSelection::Project { project_id, .. } = &params.working_root else {
            return Some(core_error_response(
                request_id,
                -32000,
                CoreError::new(
                    "worktreeNeedsProject",
                    "a worktree conversation needs a project working root",
                )
                .recoverable(true),
            ));
        };
        let (Ok(workspace_id), Ok(project_id)) = (
            WorkspaceId::try_from(params.workspace_id.0.as_str()),
            ProjectId::try_from(project_id.0.as_str()),
        ) else {
            return Some(core_error_response(request_id, -32000, invalid_id("invalidID")));
        };
        let base = self
            .database
            .run("worktree.base", move |connection| {
                let membership = WorkspaceRepository::membership(connection, &workspace_id, &project_id)?;
                if membership.removed_at.is_some() {
                    return Err(crate::storage::StorageError::new(
                        "storageConstraint",
                        "the selected Project is not an active member of the Workspace",
                    )
                    .recoverable(true));
                }
                let project = ProjectRepository::get_project(connection, &project_id)?;
                let primary = ProjectRepository::primary_source(connection, &project_id)?;
                Ok((project.display_name, primary.canonical_path))
            })
            .await;
        let (project_name, primary_path) = match base {
            Ok(base) => base,
            Err(error) => return Some(core_error_response(request_id, -32000, error.into())),
        };
        let chat = self.chat_service(&params.workspace_id);
        let home = self.database.home().to_path_buf();
        let Some(lane) = self.worktree_lane.as_mut() else {
            return Some(error_response(
                request_id,
                -32000,
                "deferred request runtime is unavailable",
            ));
        };
        let id = request_id.clone();
        let spawned = lane.spawn(origin, async move {
            let message = params.message.as_ref().map(|message| message.body.clone());
            let created = git::run_blocking("worktreeAdd", move || {
                create_checkout(&home, &project_name, &primary_path, message.as_deref())
            })
            .await;
            let created = match created {
                Ok(created) => created,
                Err(error) => {
                    tracing::warn!(kind = error.kind, "worktree checkout failed");
                    return core_error_response(id, -32000, error);
                }
            };
            let recorded = chat
                .create_conversation(
                    params.working_root,
                    params.context_project_ids,
                    params.title,
                    params.message,
                    Some(NewWorktreeSource {
                        canonical_path: created.canonical_path.clone(),
                        worktree: created.worktree.clone(),
                    }),
                )
                .await;
            match recorded {
                Ok(result) => {
                    tracing::info!(
                        workspace_id = %params.workspace_id.0,
                        conversation_id = %result.conversation.conversation.conversation_id.0,
                        "worktree conversation created"
                    );
                    success_response(id, result)
                }
                Err(error) => {
                    tracing::warn!(kind = error.kind, "worktree conversation was not recorded");
                    let _ = git::run_blocking("worktreeDiscard", move || {
                        discard_checkout(&created);
                        Ok(())
                    })
                    .await;
                    core_error_response(id, -32000, error)
                }
            }
        });
        if spawned.is_err() {
            return Some(error_response(
                request_id,
                -32002,
                "worktree concurrency limit reached",
            ));
        }
        None
    }

    /// Close what works in released worktrees, then remove their folders off
    /// the dispatcher and answer with the first removal.
    pub(super) fn finish_worktree_release<F>(
        &mut self,
        origin: ConnectionId,
        request_id: RequestID,
        workspace_id: &WorkspaceID,
        released: Vec<ReleasedWorktree>,
        respond: F,
    ) -> Option<Value>
    where
        F: FnOnce(RequestID, Option<WorktreeRemoval>) -> Value + Clone + Send + 'static,
    {
        for worktree in &released {
            let source_id = ProjectSourceID(worktree.source.id.to_string());
            if let Some(terminals) = self.terminal_manager.as_mut() {
                terminals.close_source_terminals(workspace_id, &source_id);
            }
            self.source_roots.remove(&source_id);
        }
        if released.is_empty() {
            return Some(respond(request_id, None));
        }
        let home = self.database.home().to_path_buf();
        let Some(lane) = self.worktree_lane.as_mut() else {
            return Some(respond(request_id, released.first().map(owed_removal)));
        };
        let pending = released.first().map(owed_removal);
        let id = request_id.clone();
        let respond_later = respond.clone();
        let spawned = lane.spawn(origin, async move {
            let respond = respond_later;
            let removals = git::run_blocking("worktreeRemove", move || {
                Ok(released
                    .iter()
                    .map(|released| remove_checkout(&home, released))
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap_or_default();
            respond(id, removals.into_iter().next())
        });
        if spawned.is_err() {
            // The archived row still owes the removal; the next launch
            // retries it rather than this holding the dispatcher.
            return Some(respond(request_id, pending));
        }
        None
    }

    pub(super) async fn handle_worktree_status(
        &mut self,
        origin: ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<WorktreeSourceParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let Ok(source_id) = ProjectSourceId::try_from(params.project_source_id.0.as_str()) else {
            return Some(core_error_response(request.id, -32000, invalid_id("invalidID")));
        };
        let loaded = self
            .database
            .run("worktree.status", move |connection| {
                let source = ProjectRepository::get_source(connection, &source_id)?;
                let conversations = ProjectRepository::conversations_on_source(connection, &source_id)?;
                Ok((source, conversations.len()))
            })
            .await;
        let (source, conversation_count) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => return Some(core_error_response(request.id, -32000, error.into())),
        };
        let Some(worktree) = source.worktree.clone().filter(|_| source.archived_at.is_none()) else {
            return Some(core_error_response(
                request.id,
                -32000,
                CoreError::new("worktreeUnavailable", "the worktree has been removed").recoverable(true),
            ));
        };
        let Some(deferred) = self.deferred.as_mut() else {
            return Some(error_response(
                request.id,
                -32000,
                "deferred request runtime is unavailable",
            ));
        };
        let id = request.id.clone();
        let path = source.canonical_path.clone();
        let spawned = deferred.spawn(origin, async move {
            let status_path = path.clone();
            let status_worktree = worktree.clone();
            match git::run_blocking("worktreeStatus", move || {
                git::worktree_changes(&status_path, &status_worktree.branch, &status_worktree.base_commit)
            })
            .await
            {
                Ok(changes) => success_response(
                    id,
                    WorktreeStatusResult {
                        project_source_id: ProjectSourceID(source.id.to_string()),
                        branch: worktree.branch,
                        path,
                        uncommitted_changes: changes.uncommitted,
                        commits: changes.branch_commits,
                        branch_kept: changes.branch_kept,
                        unbranched_commits: changes.unbranched_commits,
                        conversation_count: u32::try_from(conversation_count).unwrap_or(u32::MAX),
                    },
                ),
                Err(error) => core_error_response(id, -32000, error),
            }
        });
        if spawned.is_err() {
            return Some(error_response(
                request.id,
                -32002,
                "worktree.status concurrency limit reached",
            ));
        }
        None
    }

    /// `worktree.remove`: archive the source so every conversation on it
    /// reads as detached, stop their runs and terminals, then remove the
    /// folder.
    pub(super) async fn handle_worktree_remove(
        &mut self,
        origin: ConnectionId,
        request: ProtocolRequest<Value>,
    ) -> Option<Value> {
        let params = match decode_params::<WorktreeSourceParams>(&request, InvalidParamsLog::Warn) {
            Ok(params) => params,
            Err(response) => return Some(response),
        };
        let Ok(source_id) = ProjectSourceId::try_from(params.project_source_id.0.as_str()) else {
            return Some(core_error_response(request.id, -32000, invalid_id("invalidID")));
        };
        let archived = self
            .database
            .run("worktree.remove", move |connection| {
                recorded_transaction(connection, "worktree.remove", |connection| {
                    let conversations =
                        ProjectRepository::conversations_on_source(connection, &source_id)?;
                    let (released, _) = ProjectRepository::archive_worktree_source_in_transaction(
                        connection,
                        &source_id,
                        chrono::Utc::now(),
                    )?;
                    Ok((released, conversations))
                })
            })
            .await;
        let (released, conversations) = match archived {
            Ok(archived) => archived,
            Err(error) => return Some(core_error_response(request.id, -32000, error.into())),
        };
        let conversations = conversations
            .into_iter()
            .map(|id| ConversationID(id.to_string()))
            .collect::<Vec<_>>();
        if let Some(bridge) = self.agent_bridge.as_ref() {
            bridge
                .cancel_conversations_runs(&params.workspace_id, &conversations)
                .await;
        }
        tracing::info!(conversations = conversations.len(), "worktree archived");
        // The conversations now read as detached.
        if let Some(outbound) = self.transcript_outbound.as_ref() {
            let notification = ProtocolNotification::new(
                METHOD_CONVERSATION_LIST_CHANGED,
                ConversationListChangedParams {
                    workspace_id: params.workspace_id.clone(),
                },
            );
            if let Ok(value) = serde_json::to_value(notification) {
                let _ = outbound.notify(value);
            }
        }
        self.finish_worktree_release(
            origin,
            request.id,
            &params.workspace_id,
            vec![released],
            |id, removal| match removal {
                Some(removal) => success_response(id, WorktreeRemoveResult { removal }),
                None => error_response(id, -32000, "the worktree was not released"),
            },
        )
    }
}

/// A removal the lane had no room for: the row is archived, the folder is
/// still there, and the next launch removes it.
fn owed_removal(released: &ReleasedWorktree) -> WorktreeRemoval {
    WorktreeRemoval {
        project_source_id: ProjectSourceID(released.source.id.to_string()),
        branch: released.worktree.branch.clone(),
        path: released.source.canonical_path.clone(),
        removed: false,
        branch_kept: true,
        message: Some("Ottam was busy with other worktrees and removes this one on its next launch.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slug_keeps_the_first_ascii_words() {
        assert_eq!(slug("Fix the login page's redirect loop", "chat"), "fix-the-login-page");
        assert_eq!(slug("  ", "chat"), "chat");
        assert_eq!(slug("வணக்கம்", "chat"), "chat");
        assert_eq!(
            slug("supercalifragilisticexpialidocious and more", "chat"),
            "supercalifragilisticexpialidocio"
        );
    }
}
