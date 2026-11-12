//! The terminal capability behind a `clientTerminal` sign-in.
//!
//! Sign-in terminals are ordinary terminals started hidden: the same registry,
//! the same write/resize/replay/close, the same reaping at shutdown. Only
//! `terminal.list` treats them differently, which is what keeps a login the
//! user never asked to see out of the sidebar while leaving it revealable in
//! place if it turns out they need it.

use std::sync::Arc;

use ottam_protocol::{TerminalSessionID, WorkingRootSelection};

use crate::agent::{
    AuthTerminalFuture, AuthTerminalHost, AuthTerminalRequest, AuthTerminalRoot,
    AuthTerminalSession,
};
use crate::core_error::CoreError;
use crate::storage::{Database, WorkspaceRuntimeStore};
use crate::terminal::{TerminalSpawnSpec, TerminalSpawner, TerminalWorkspaceIdentity};

/// A login terminal nobody has looked at yet has no size worth honouring. This
/// is the conventional default; a client that reveals one resizes it to the
/// sheet on attach.
const AUTH_TERMINAL_ROWS: u16 = 24;
const AUTH_TERMINAL_COLS: u16 = 80;

pub(super) struct ServerAuthTerminalHost {
    spawner: TerminalSpawner,
    database: Database,
}

impl ServerAuthTerminalHost {
    pub(super) fn new(spawner: TerminalSpawner, database: Database) -> Arc<Self> {
        Arc::new(Self { spawner, database })
    }
}

impl AuthTerminalHost for ServerAuthTerminalHost {
    fn spawn(&self, request: AuthTerminalRequest) -> AuthTerminalFuture<'_> {
        Box::pin(async move {
            // A login writes to the CLI's own config directory, not to the
            // project, so the root only decides a working directory. With a
            // workspace, that is its own files — the one root that always
            // resolves, whatever state the projects are in. Without one, the
            // agent's home serves, and the session belongs to no workspace.
            let (workspace, cwd) = match request.root {
                AuthTerminalRoot::Workspace(workspace_id) => {
                    let store =
                        WorkspaceRuntimeStore::new(self.database.clone(), workspace_id.clone());
                    let resolved = store
                        .resolve_working_root(&WorkingRootSelection::WorkspaceFiles)
                        .await?;
                    let cwd = resolved.path();
                    (
                        Some(TerminalWorkspaceIdentity {
                            workspace_id,
                            working_root: resolved.summary,
                        }),
                        cwd,
                    )
                }
                AuthTerminalRoot::AgentHome(path) => {
                    std::fs::create_dir_all(&path).map_err(|error| {
                        CoreError::new(
                            "terminalCwdInvalid",
                            format!("failed to create the sign-in working directory: {error}"),
                        )
                        .with_path(path.to_string_lossy())
                        .recoverable(true)
                    })?;
                    (None, path)
                }
            };
            let identity = self
                .spawner
                .spawn_session(TerminalSpawnSpec {
                    workspace,
                    cwd,
                    program: request.program,
                    args: request.args,
                    extra_env: request.env,
                    rows: AUTH_TERMINAL_ROWS,
                    cols: AUTH_TERMINAL_COLS,
                    hidden: true,
                    exit_observer: Some(request.exit_observer),
                })
                .await?;
            Ok(AuthTerminalSession {
                session_id: identity.session_id,
                terminal_id: identity.terminal_id,
                rows: identity.rows,
                cols: identity.cols,
            })
        })
    }

    fn output_bytes(&self, session_id: &TerminalSessionID) -> Option<u64> {
        self.spawner.output_bytes(session_id)
    }

    fn close(&self, session_id: &TerminalSessionID) {
        self.spawner.close_session(session_id);
    }
}
