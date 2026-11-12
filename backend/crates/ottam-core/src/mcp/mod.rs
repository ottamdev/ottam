//! Core's MCP proxy: agents reach every MCP server through one loopback
//! listener, and core is the MCP client to the servers behind it. See
//! `docs/architecture/backend/mcp.md`.

mod calls;
mod config;
mod ottam;
mod entries;
mod import;
mod managed;
mod oauth;
mod proxy;
mod runtime;
mod secrets;
mod service;
mod tokens;
mod upstream;

#[cfg(test)]
mod tests;

pub(crate) use config::McpServers;
pub(crate) use ottam::{OttamTools, DelegateRequest, DelegationTargets};
pub(crate) use oauth::redirect_port;
pub(crate) use proxy::{McpProxy, McpSessions};
pub(crate) use runtime::{McpNotice, McpNoticeSink, McpRuntime};
pub(crate) use secrets::default_secret_store;
pub(crate) use service::{McpService, SessionGrant};
pub(crate) use tokens::McpCaller;
pub(crate) use tokens::{McpGrant, McpTokens};

use crate::agent::{HarnessAdapter, McpServerEntry};
use std::sync::Arc;
use tokens::GrantRequest;

impl McpSessions {
    /// A token for a session about to be opened or restored, and one entry
    /// per path in `names` carrying it. `None` for no names, so the session
    /// is sent none and no token is minted.
    pub(crate) fn grant(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        names: Vec<String>,
        request: GrantRequest,
    ) -> Option<(McpGrant, Vec<McpServerEntry>)> {
        if names.is_empty() {
            return None;
        }
        let grant = self.tokens().mint(adapter, request);
        let authorization = grant.authorization();
        let entries = names
            .into_iter()
            .map(|name| McpServerEntry {
                url: self.server_url(&name),
                headers: vec![("Authorization".to_string(), authorization.clone())],
                name,
            })
            .collect();
        Some((grant, entries))
    }
}
