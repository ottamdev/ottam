mod support;

#[cfg(unix)]
#[path = "jsonl_protocol/acp_handler_chain.rs"]
mod acp_handler_chain;
#[path = "jsonl_protocol/agent.rs"]
mod agent;
#[cfg(unix)]
#[path = "jsonl_protocol/agent_auth.rs"]
mod agent_auth;
#[cfg(unix)]
#[path = "jsonl_protocol/agent_install.rs"]
mod agent_install;
#[cfg(unix)]
#[path = "jsonl_protocol/attach_socket.rs"]
mod attach_socket;
#[path = "jsonl_protocol/attachments.rs"]
mod attachments;
#[cfg(unix)]
#[path = "jsonl_protocol/binary_lane.rs"]
mod binary_lane;
#[path = "jsonl_protocol/client_roundtrip.rs"]
mod client_roundtrip;
#[cfg(unix)]
#[path = "jsonl_protocol/conversation_pagination.rs"]
mod conversation_pagination;
#[path = "jsonl_protocol/delivery_recovery.rs"]
mod delivery_recovery;
#[path = "jsonl_protocol/engine_conversation.rs"]
mod engine_conversation;
#[cfg(unix)]
#[path = "jsonl_protocol/managed_runtime.rs"]
mod managed_runtime;
#[path = "jsonl_protocol/mcp.rs"]
mod mcp;
#[path = "jsonl_protocol/multi_project_conversation.rs"]
mod multi_project_conversation;
#[cfg(unix)]
#[path = "jsonl_protocol/native_subagents.rs"]
mod native_subagents;
#[path = "jsonl_protocol/project_git.rs"]
mod project_git;
#[path = "jsonl_protocol/protocol_errors.rs"]
mod protocol_errors;
#[cfg(unix)]
#[path = "jsonl_protocol/questions.rs"]
mod questions;
#[path = "jsonl_protocol/retained_surfaces.rs"]
mod retained_surfaces;
#[path = "jsonl_protocol/steering.rs"]
mod steering;
#[path = "jsonl_protocol/terminal.rs"]
mod terminal;
#[path = "jsonl_protocol/workspace_files.rs"]
mod workspace_files;
