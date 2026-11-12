//! The AIR `_session/async_task/stop` request: stop a background task the
//! agent started in a session.
//!
//! Both pinned adapters register it once the client advertises AIR
//! `asyncTasks`. claude-agent-acp calls the SDK's `stopTask` and codex-acp
//! terminates the background terminal; each answers `{stopped}` and reports
//! the task's end as an `async_task_state_update`, so the answer only says
//! whether the agent found the task live.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[serde(rename_all = "camelCase")]
#[request(method = "_session/async_task/stop", response = StopAsyncTaskResponse)]
pub(super) struct StopAsyncTaskRequest {
    pub(super) session_id: String,
    pub(super) async_task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub(super) struct StopAsyncTaskResponse {
    /// Absent from an adapter that answers `{}`: the task's own state update
    /// is what reports the stop, so treat silence as not stopped.
    #[serde(default)]
    pub(super) stopped: bool,
}
