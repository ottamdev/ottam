use super::output_queue::READ_BUFFER_BYTES;
use super::{
    TerminalEventSender, TerminalManager, TerminalOutputQueue, TerminalSpawnSpec,
    TerminalWorkspaceIdentity,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ottam_protocol::{
    TerminalWriteParams, WorkingRootAvailability, WorkingRootKind, WorkingRootSummary, WorkspaceID,
};
use std::path::Path;
use std::time::Duration;
use tokio::runtime::Handle;

#[cfg(unix)]
mod hidden_sessions {
    use super::*;

    const WORKSPACE: &str = "workspace_hidden_test";

    fn manager() -> TerminalManager {
        let (outbound, _outbound_rx) = crate::server::test_sink();
        let events =
            TerminalEventSender::new(outbound, TerminalOutputQueue::new(READ_BUFFER_BYTES));
        TerminalManager::new(events, Handle::current())
    }

    fn working_root(root: &Path) -> WorkingRootSummary {
        WorkingRootSummary {
            kind: WorkingRootKind::WorkspaceFiles,
            display_name: "Hidden test".to_string(),
            canonical_path: root.to_string_lossy().into_owned(),
            project_id: None,
            source_id: None,
            availability: WorkingRootAvailability::Available,
        }
    }

    fn hidden_spec(root: &Path, args: &[&str]) -> TerminalSpawnSpec {
        TerminalSpawnSpec {
            workspace: Some(TerminalWorkspaceIdentity {
                workspace_id: WorkspaceID(WORKSPACE.to_string()),
                working_root: working_root(root),
            }),
            cwd: root.to_path_buf(),
            program: "/bin/sh".to_string(),
            args: args.iter().map(|argument| argument.to_string()).collect(),
            extra_env: Vec::new(),
            rows: 24,
            cols: 80,
            hidden: true,
            exit_observer: None,
        }
    }

    /// The whole point of `hidden`: a login terminal core started on the
    /// user's behalf must not turn up as a tab they never opened.
    #[tokio::test]
    async fn hidden_sessions_are_withheld_from_the_list() {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut manager = manager();
        let visible = manager
            .spawn_session(TerminalSpawnSpec {
                hidden: false,
                ..hidden_spec(root.path(), &["-c", "sleep 30"])
            })
            .await
            .expect("visible session");
        manager
            .spawn_session(hidden_spec(root.path(), &["-c", "sleep 30"]))
            .await
            .expect("hidden session");

        let listed = manager
            .list(&WorkspaceID(WORKSPACE.to_string()))
            .expect("list");
        assert_eq!(listed.terminals.len(), 1);
        assert_eq!(listed.terminals[0].terminal_id, visible.terminal_id);

        manager.close_all().await;
    }

    /// Hidden hides it from the list and nothing else — reveal reuses the
    /// same session by id, so replay has to work on it.
    #[tokio::test]
    async fn hidden_sessions_stay_reachable_by_id() {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut manager = manager();
        let session = manager
            .spawn_session(hidden_spec(root.path(), &["-c", "sleep 30"]))
            .await
            .expect("hidden session");

        manager
            .replay(
                Some(&WorkspaceID(WORKSPACE.to_string())),
                &session.terminal_id,
                4096,
            )
            .expect("hidden sessions replay by id");
        manager
            .write(TerminalWriteParams {
                session_id: session.session_id.clone(),
                data_base64: STANDARD.encode("\n"),
            })
            .expect("hidden sessions accept writes by id");

        manager.close_all().await;
    }

    /// A sign-in started from Settings has no workspace to belong to. It
    /// still has to be replayable — that is how a revealed login gets its
    /// scrollback — while staying invisible to every workspace's list.
    #[tokio::test]
    async fn a_workspace_less_session_replays_by_id_and_lists_nowhere() {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut manager = manager();
        let session = manager
            .spawn_session(TerminalSpawnSpec {
                workspace: None,
                ..hidden_spec(root.path(), &["-c", "sleep 30"])
            })
            .await
            .expect("workspace-less session");

        manager
            .replay(None, &session.terminal_id, 4096)
            .expect("a workspace-less session replays by terminal id");
        assert!(
            manager
                .list(&WorkspaceID(WORKSPACE.to_string()))
                .expect("list")
                .terminals
                .is_empty(),
            "a session that belongs to no workspace is in no workspace's list"
        );
        // Scoping still means something: asserting the wrong owner fails.
        assert!(
            manager
                .replay(
                    Some(&WorkspaceID(WORKSPACE.to_string())),
                    &session.terminal_id,
                    4096,
                )
                .is_err(),
            "a workspace assertion must not match a session that has none"
        );

        manager.close_all().await;
    }

    /// `terminal.exited` goes to the client, which never heard of this
    /// session, so the observer is core's only way to learn how the login
    /// ended — and the exit code is exactly what decides whether to reveal.
    #[tokio::test]
    async fn hidden_sessions_report_their_exit_status_to_core() {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut manager = manager();
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        manager
            .spawn_session(TerminalSpawnSpec {
                exit_observer: Some(exit_tx),
                ..hidden_spec(root.path(), &["-c", "exit 3"])
            })
            .await
            .expect("hidden session");

        let status = tokio::time::timeout(Duration::from_secs(10), exit_rx)
            .await
            .expect("the observer must fire before the timeout")
            .expect("the observer must not be dropped");
        assert_eq!(status, Some(3));

        manager.close_all().await;
    }

    /// Withheld from the list is not withheld from shutdown: a hidden PTY
    /// that outlived the app would be a stray process nobody can see.
    #[tokio::test]
    async fn hidden_sessions_are_reaped_by_close_all() {
        let root = tempfile::TempDir::new().expect("temp root");
        let mut manager = manager();
        let session = manager
            .spawn_session(hidden_spec(root.path(), &["-c", "sleep 30"]))
            .await
            .expect("hidden session");
        let sessions = manager.shared_sessions();

        manager.close_all().await;

        assert!(
            !sessions
                .lock()
                .expect("sessions lock")
                .contains_key(&session.session_id),
            "the hidden session should have been torn down"
        );
    }
}
