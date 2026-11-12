//! Chat persistence service. Every conversation-surface method runs through
//! the Diesel repositories on the shared `Database`, and all domain→wire
//! mapping lives here so repositories never see protocol types.

mod attachments;
mod conversations;
mod delegation;
mod history;
mod ids;
mod payloads;
mod projection;
mod runs;
mod transcript;
pub(super) use transcript::{image_call, TranscriptCoordinator};
pub(super) use transcript::{
    native_subagent_id, native_thread_id, NATIVE_ANCHOR_KEY, NATIVE_FOLLOW_UP_STAMP,
    NATIVE_THREAD_STAMP,
};
mod wire;

pub(super) use attachments::sweep_attachments;
pub(super) use delegation::{ChildLink, DelegatedChild, WaitingNote};
pub(super) use history::{HISTORY_DEFAULT_ENTRIES, HISTORY_MAX_ENTRIES};
pub(super) use runs::BeginRunRecord;

use crate::storage::error::StorageError;
use crate::storage::models::WorkspaceId;
use crate::storage::repositories::WorkspaceRepository;
use crate::storage::Database;
use ottam_protocol::WorkspaceID;
use diesel::sqlite::SqliteConnection;
use ids::{parse_workspace_id, unknown_workspace};
use std::path::PathBuf;

#[derive(Clone)]
pub(super) struct ChatService {
    registry: Option<std::sync::Arc<crate::agent::AgentRegistry>>,
    database: Database,
    home: PathBuf,
    workspace_id: WorkspaceID,
    transcript_outbound: Option<super::OutboundSink>,
    transcript_instance: Option<String>,
    transcript_coordinator: std::sync::Arc<TranscriptCoordinator>,
}

impl ChatService {
    pub(super) fn with_registry(
        mut self,
        registry: Option<std::sync::Arc<crate::agent::AgentRegistry>>,
    ) -> Self {
        self.registry = registry;
        self
    }
    pub(super) fn new(database: Database, workspace_id: WorkspaceID) -> Self {
        let home = database.home().to_path_buf();
        Self {
            registry: None,
            transcript_coordinator: TranscriptCoordinator::new(database.clone()),
            database,
            home,
            workspace_id,
            transcript_outbound: None,
            transcript_instance: None,
        }
    }

    pub(super) fn with_transcript_coordinator(
        mut self,
        coordinator: std::sync::Arc<TranscriptCoordinator>,
    ) -> Self {
        self.transcript_coordinator = coordinator;
        self
    }

    pub(super) fn with_transcript_delivery(
        mut self,
        outbound: Option<super::OutboundSink>,
        instance: String,
    ) -> Self {
        self.transcript_outbound = outbound;
        self.transcript_instance = Some(instance);
        self
    }

    fn workspace(&self) -> Result<WorkspaceId, StorageError> {
        parse_workspace_id(&self.workspace_id)
    }
}

fn ensure_workspace_active(
    connection: &mut SqliteConnection,
    workspace_id: &WorkspaceId,
) -> Result<(), StorageError> {
    let workspace = WorkspaceRepository::get(connection, workspace_id).map_err(|error| {
        if error.kind == "storageNotFound" {
            unknown_workspace()
        } else {
            error
        }
    })?;
    if workspace.archived_at.is_some() {
        return Err(unknown_workspace());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::workspaces;
    use chrono::Utc;
    use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
    use tempfile::TempDir;

    #[tokio::test]
    async fn workspace_activity_check_only_translates_not_found_errors() {
        let home = TempDir::new().expect("temporary home");
        let database = Database::open_test_home(home.path().to_path_buf()).expect("opens database");
        let (missing_kind, corrupt_kind) = database
            .run("test.workspaceActivityErrors", |connection| {
                let missing = ensure_workspace_active(connection, &WorkspaceId::new_v7())
                    .expect_err("missing workspace is rejected");
                let workspace = WorkspaceRepository::create(connection, "Corrupt", Utc::now())?;
                diesel::update(workspaces::table.find(workspace.id.to_string()))
                    .set(workspaces::created_at.eq(i64::MAX))
                    .execute(connection)?;
                let corrupt = ensure_workspace_active(connection, &workspace.id)
                    .expect_err("corrupt workspace is rejected");
                Ok((missing.kind, corrupt.kind))
            })
            .await
            .expect("checks workspace errors");

        assert_eq!(missing_kind, "unknownWorkspaceID");
        assert_eq!(corrupt_kind, "storageCorrupt");
    }
}
