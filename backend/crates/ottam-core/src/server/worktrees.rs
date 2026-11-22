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
    ProtocolNotification, ProtocolRequest, RequestID, WorkingRootSelection,
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
/// `taken` is every path and branch a worktree row of the project holds,
/// archived or not: a name is never handed out twice.
pub(super) fn create_checkout(
    home: &Path,
    project_name: &str,
    primary_path: &str,
    taken: &(Vec<String>, Vec<String>),
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
    // A symlinked project folder would put the checkout where removal
    // refuses to go.
    if !is_managed_path(home, &project_dir) {
        return Err(CoreError::new(
            "worktreeCreateFailed",
            "the project's worktrees folder resolves outside Ottam's worktrees folder",
        )
        .with_path(project_dir.to_string_lossy())
        .recoverable(true));
    }
    let mut outcome = Err(CoreError::new(
        "worktreeNameTaken",
        "Ottam could not find a free branch name for the worktree",
    )
    .recoverable(true));
    for _ in 0..NAME_ATTEMPTS {
        let name = format!("{words}-{}", random_suffix());
        let branch = format!("ottam/{name}");
        let path = project_dir.join(&name);
        if taken.0.iter().any(|held| Path::new(held) == path)
            || taken.1.contains(&branch)
            || git::branch_commit(primary_path, &branch)?.is_some()
        {
            continue;
        }
        // Claims the name: a concurrent create that drew it too fails here
        // and draws again, so what this attempt later discards is its own.
        // Git checks out into an empty folder. The project folder is never
        // removed, so no concurrent removal takes it out from under this.
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                outcome = Err(CoreError::new(
                    "worktreeCreateFailed",
                    format!("could not create the worktree folder: {error}"),
                )
                .with_path(path.to_string_lossy())
                .recoverable(true));
                break;
            }
        }
        let created = CreatedCheckout {
            canonical_path: path.to_string_lossy().into_owned(),
            primary_path: primary_path.to_string(),
            worktree: ManagedWorktree {
                branch: branch.clone(),
                base_commit: base_commit.clone(),
            },
        };
        // Claims the branch the same way: another writer that made it since
        // the check keeps it, and this attempt draws again.
        match git::create_branch(primary_path, &branch, &base_commit) {
            Ok(true) => {}
            Ok(false) => {
                let _ = std::fs::remove_dir(&path);
                continue;
            }
            Err(error) => {
                let _ = std::fs::remove_dir(&path);
                outcome = Err(error);
                break;
            }
        }
        outcome = match git::worktree_add(primary_path, &path, &branch) {
            Ok(()) => Ok(created),
            Err(error) => {
                // This attempt claimed both the folder and the branch, so
                // what exists now is its own: a failing post-checkout hook or
                // the timeout leaves the checkout registered.
                discard_checkout(&created);
                Err(error)
            }
        };
        break;
    }
    outcome
}

/// Undo a checkout whose conversation was never recorded, or that
/// `git worktree add` left half made.
pub(super) fn discard_checkout(created: &CreatedCheckout) {
    let path = PathBuf::from(&created.canonical_path);
    if path.exists() && git::worktree_remove(&created.primary_path, &path).is_err() {
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::warn!(%error, "an unrecorded worktree was not removed");
        }
    }
    if let Err(error) = git::worktree_prune(&created.primary_path) {
        tracing::warn!(kind = error.kind, "git worktree prune failed");
    }
    if let Err(error) = git::delete_branch_if_unchanged(
        &created.primary_path,
        &created.worktree.branch,
        &created.worktree.base_commit,
    ) {
        tracing::warn!(kind = error.kind, "an unrecorded worktree's branch was not deleted");
    }
}

/// Remove an archived worktree's directory, then its branch when it has no
/// commits of its own. Never touches a path outside `$OTTAM_HOME/worktrees`.
/// The flag says the removal is settled: its folder went and its branch was
/// decided, so no later launch owes it anything.
pub(super) fn remove_checkout(home: &Path, released: &ReleasedWorktree) -> (WorktreeRemoval, bool) {
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
    let mut kept_ref = None;
    // A folder Core cannot look up (an untraversable parent) is not gone.
    let present = match path.try_exists() {
        Ok(present) => present,
        Err(error) => {
            tracing::warn!(%error, "a worktree folder could not be looked up");
            removal.message = Some(format!("{error}. Ottam tries again on its next launch."));
            return (removal, false);
        }
    };
    if present {
        if !is_managed_path(home, &path) {
            tracing::warn!("a worktree path outside Ottam's home was not removed");
            removal.message = Some("Ottam removes only worktrees it made in its own folder.".into());
            return (removal, false);
        }
        // Right before the folder goes, so a commit made after the user
        // confirmed is kept too; the HEAD lock, held until this block ends,
        // fails any commit a straggler (a subagent outliving its run) tries
        // after the read. With the project's checkout gone there is no
        // repository left to keep it in. A folder a failed removal already
        // emptied of its `.git` has no HEAD left to keep.
        let _head_lock = if primary.is_some() && path.join(".git").exists() {
            match git::keep_unbranched_head(&released.source.canonical_path, &released.worktree.branch) {
                Ok((lock, kept)) => {
                    kept_ref = kept;
                    Some(lock)
                }
                Err(error) => {
                    tracing::warn!(kind = error.kind, "a worktree's unbranched commits were not checked");
                    removal.message = Some(
                        "Ottam could not check the worktree for commits no branch holds, so it kept the folder. \
                         Ottam tries again on its next launch."
                            .into(),
                    );
                    return (removal, false);
                }
            }
        } else {
            None
        };
        let removed_by_git = primary.is_some_and(|primary| {
            git::worktree_remove(primary, &path)
                .inspect_err(|error| {
                    tracing::warn!(kind = error.kind, "git worktree remove failed; deleting the folder")
                })
                .is_ok()
        });
        if !removed_by_git {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                // Git deleted the folder, then failed to unregister it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(%error, "a worktree folder was not removed");
                    // The app quotes this after the path; the retry promise is
                    // Core's to make, since only Core knows the row owes it.
                    removal.message = Some(format!("{error}. Ottam tries again on its next launch."));
                    return (removal, false);
                }
            }
        }
    }
    removal.removed = true;
    let Some(primary) = primary else {
        removal.message = Some("The project's checkout is gone, so its branch was left alone.".into());
        return (removal, false);
    };
    if let Err(error) = git::worktree_prune(primary) {
        // Its stale registration still holds the branch checked out, so the
        // removal stays owed rather than settling with the branch kept.
        tracing::warn!(kind = error.kind, "git worktree prune failed");
        removal.message = Some(
            "Git could not forget the worktree yet, so its branch is kept for now. \
             Ottam tries again on its next launch."
                .into(),
        );
        return (removal, false);
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
            return (removal, false);
        }
    }
    if let Some(reference) = kept_ref {
        removal.message = Some(format!("Commits no branch held are kept under {reference}."));
    }
    (removal, true)
}

/// Whether `path` resolves to a folder inside `$OTTAM_HOME/worktrees`.
/// Create, remove and project opening all ask, so Core never records a
/// checkout it would later refuse to remove.
pub(super) fn is_managed_path(home: &Path, path: &Path) -> bool {
    worktrees_root(home)
        .canonicalize()
        .ok()
        .zip(path.canonicalize().ok())
        .is_some_and(|(root, path)| path.starts_with(&root) && path != root)
}

/// A folder under `$OTTAM_HOME/worktrees` belongs to a conversation, recorded
/// or not (a removal still owed, a crash mid-create), so it never opens as a
/// project.
pub(super) fn refuse_managed_project_path(home: &Path, canonical_path: &str) -> Result<(), CoreError> {
    if is_managed_path(home, Path::new(canonical_path)) {
        return Err(CoreError::new(
            "managedWorktreePath",
            "this folder is a worktree Ottam made for a conversation; open that conversation instead",
        )
        .recoverable(true));
    }
    Ok(())
}

/// Retry the removals archived worktrees still owe: a directory that
/// survived a failed removal or a quit mid-removal, or a branch left after
/// its folder went.
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
        // A folder that cannot be looked up counts as left.
        let folder_left = Path::new(&released.source.canonical_path)
            .try_exists()
            .unwrap_or(true);
        // With the folder gone only the branch can be owed, and that needs
        // the project's checkout. Deleting it is a no-op once done.
        if !folder_left && released.primary_path.is_none() {
            continue;
        }
        let home = home.clone();
        let removal = git::run_blocking("worktreeRemove", move || {
            Ok(remove_checkout(&home, &released))
        })
        .await;
        match removal {
            Ok((removal, settled)) => {
                if folder_left {
                    tracing::info!(removed = removal.removed, "retried an owed worktree removal");
                }
                record_settled(&database, &removal, settled).await;
            }
            Err(error) => tracing::warn!(kind = error.kind, "an owed worktree removal failed"),
        }
    }
}

/// Marks a removal whose folder went and whose branch was decided, so no
/// later launch retries it against a branch someone made since.
async fn record_settled(database: &Database, removal: &WorktreeRemoval, settled: bool) {
    if !settled {
        return;
    }
    let Ok(source_id) = ProjectSourceId::try_from(removal.project_source_id.0.as_str()) else {
        return;
    };
    let marked = database
        .run("worktree.cleaned", move |connection| {
            ProjectRepository::mark_worktree_cleaned(connection, &source_id, chrono::Utc::now())
        })
        .await;
    if let Err(error) = marked {
        tracing::warn!(kind = error.kind, "a settled worktree removal was not recorded");
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
                let taken = ProjectRepository::worktree_names_in_use(connection, &project_id)?;
                Ok((project.display_name, primary.canonical_path, taken))
            })
            .await;
        let (project_name, primary_path, taken) = match base {
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
                create_checkout(&home, &project_name, &primary_path, &taken, message.as_deref())
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
        released: Vec<ReleasedWorktree>,
        respond: F,
    ) -> Option<Value>
    where
        F: FnOnce(RequestID, Option<WorktreeRemoval>) -> Value + Clone + Send + 'static,
    {
        for worktree in &released {
            let source_id = ProjectSourceID(worktree.source.id.to_string());
            // Every workspace's: the source is archived for all of them.
            if let Some(terminals) = self.terminal_manager.as_mut() {
                terminals.close_source_terminals(&source_id);
            }
            self.source_roots.remove(&source_id);
        }
        if released.is_empty() {
            return Some(respond(request_id, None));
        }
        let home = self.database.home().to_path_buf();
        let database = self.database.clone();
        let busy = "Ottam was busy with other worktrees and removes this one on its next launch.";
        let Some(lane) = self.worktree_lane.as_mut() else {
            return Some(respond(request_id, released.first().map(|first| owed_removal(first, busy))));
        };
        let pending = released.first().map(|first| owed_removal(first, busy));
        let first = released.first().cloned();
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
            .await;
            let removal = match removals {
                Ok(removals) => {
                    for (removal, settled) in &removals {
                        record_settled(&database, removal, *settled).await;
                    }
                    removals.into_iter().next().map(|(removal, _)| removal)
                }
                // The row is archived already, so the folder is owed.
                Err(error) => {
                    tracing::warn!(kind = error.kind, "the worktree removal task failed");
                    let retry = format!("{}. Ottam tries again on its next launch.", error.message);
                    first.as_ref().map(|first| owed_removal(first, &retry))
                }
            };
            respond(id, removal)
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
                    let (released, archived) = ProjectRepository::archive_worktree_source_in_transaction(
                        connection,
                        &source_id,
                        chrono::Utc::now(),
                    )?;
                    Ok((released, archived, conversations))
                })
            })
            .await;
        let (released, archived, conversations) = match archived {
            Ok(archived) => archived,
            Err(error) => return Some(core_error_response(request.id, -32000, error.into())),
        };
        // Removed before: what is still owed, the next launch retries. Doing
        // it again here could reach a checkout that reused the path.
        if !archived {
            return Some(core_error_response(
                request.id,
                -32000,
                CoreError::new("worktreeUnavailable", "the worktree has been removed").recoverable(true),
            ));
        }
        let conversations = conversations
            .into_iter()
            .map(|id| ConversationID(id.to_string()))
            .collect::<Vec<_>>();
        // In every workspace: conversation ids are global, and so is the
        // archived source.
        if let Some(bridge) = self.agent_bridge.as_ref() {
            bridge.cancel_conversations_runs(&conversations).await;
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
            vec![released],
            |id, removal| match removal {
                Some(removal) => success_response(id, WorktreeRemoveResult { removal }),
                None => error_response(id, -32000, "the worktree was not released"),
            },
        )
    }
}

/// A removal that did not run (no room on the lane, or its task failed): the
/// row is archived, the folder is still there, and the next launch removes it.
fn owed_removal(released: &ReleasedWorktree, message: &str) -> WorktreeRemoval {
    WorktreeRemoval {
        project_source_id: ProjectSourceID(released.source.id.to_string()),
        branch: released.worktree.branch.clone(),
        path: released.source.canonical_path.clone(),
        removed: false,
        branch_kept: true,
        message: Some(message.to_string()),
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
