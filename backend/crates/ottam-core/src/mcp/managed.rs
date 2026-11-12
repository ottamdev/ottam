//! The proxy path of one configured server. Core answers `initialize`
//! itself, so an agent connects whatever state the server is in, and passes
//! `tools/list` and `tools/call` to the upstream connection for the
//! caller's workspace, waiting for it to start. Upstream failures reach the
//! agent as MCP errors; a call to a server that needs a sign-in also tells
//! the caller's chat, which offers one.

use super::calls::CallOrigin;
use super::proxy::McpEndpoint;
use super::runtime::{McpNotice, McpRuntime, SIGN_IN_CHANGED};
use super::tokens::McpCaller;
use super::upstream::{Upstream, UpstreamError};
use ottam_protocol::{McpServerID, McpSignInNeededParams};
use futures::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServiceError};
use std::sync::atomic::Ordering;

pub(crate) struct ManagedEndpoint {
    server_id: McpServerID,
    /// The path's name, as the chat's sign-in card shows it.
    name: String,
    runtime: McpRuntime,
}

impl ManagedEndpoint {
    pub(crate) fn new(server_id: McpServerID, name: String, runtime: McpRuntime) -> Self {
        Self {
            server_id,
            name,
            runtime,
        }
    }

    /// The upstream connection; `call` says the caller's chat should be
    /// offered a sign-in if the server needs one.
    async fn upstream(&self, caller: &McpCaller, call: bool) -> Result<Upstream, ErrorData> {
        match self
            .runtime
            .upstream(&self.server_id, &caller.workspace_id)
            .await
        {
            Ok(upstream) => Ok(upstream),
            Err(UpstreamError::SignedOut) => Err(self.sign_in_needed(caller, call)),
            Err(UpstreamError::Failed(message)) => Err(ErrorData::internal_error(message, None)),
        }
    }

    /// The error a call to a signed-out server gets. A tool call, not a
    /// listing, puts the Sign in card in its chat: the chat asked for this
    /// server just now.
    fn sign_in_needed(&self, caller: &McpCaller, call: bool) -> ErrorData {
        if call && (!caller.shared || caller.session_id.is_some()) {
            self.runtime
                .notify(McpNotice::SignInNeeded(McpSignInNeededParams {
                    server_id: self.server_id.clone(),
                    server_name: self.name.clone(),
                    conversation_id: caller.conversation_id.clone(),
                }));
        }
        ErrorData::internal_error(
            format!(
                "The {} MCP server needs the user to sign in. Ottam shows a Sign in \
                 button in this chat; ask the user to sign in there, then try again.",
                self.name
            ),
            None,
        )
    }

    /// A failed request's error, unless the server's answer was that it
    /// needs a sign-in: then the server is signed out and the caller told so,
    /// or, when a sign-in came after this connection started, asked to try
    /// again.
    async fn failed(
        &self,
        upstream: &Upstream,
        caller: &McpCaller,
        call: bool,
        error: ServiceError,
    ) -> ErrorData {
        if !upstream.signed_out.load(Ordering::Acquire) {
            return upstream_error(error);
        }
        if self
            .runtime
            .mark_signed_out(&self.server_id, Some(upstream.sign_in_epoch))
            .await
        {
            self.sign_in_needed(caller, call)
        } else {
            ErrorData::internal_error(SIGN_IN_CHANGED, None)
        }
    }
}

impl McpEndpoint for ManagedEndpoint {
    fn server_id(&self) -> Option<&McpServerID> {
        Some(&self.server_id)
    }

    fn info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
    }

    fn list_tools(
        &self,
        caller: McpCaller,
        request: Option<PaginatedRequestParams>,
    ) -> BoxFuture<'_, Result<ListToolsResult, ErrorData>> {
        Box::pin(async move {
            let upstream = self.upstream(&caller, false).await?;
            match upstream.peer.list_tools(request).await {
                Ok(tools) => Ok(tools),
                Err(error) => Err(self.failed(&upstream, &caller, false, error).await),
            }
        })
    }

    fn call_tool(
        &self,
        caller: McpCaller,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> BoxFuture<'_, Result<CallToolResponse, ErrorData>> {
        Box::pin(async move {
            let cancelled = || ErrorData::internal_error("The call was cancelled", None);
            // The agent's `_meta` describes its own session with core, not
            // core's with the server, so it stays here. It names a shared
            // token's session, and so the workspace whose server it reaches.
            let meta = request.meta.take();
            let meta = meta
                .as_ref()
                .map(|meta| &meta.0 .0)
                .or(Some(&context.meta.0 .0));
            let caller = caller.attributed(meta);
            // A call cancelled while the server starts is never sent.
            let upstream = tokio::select! {
                upstream = self.upstream(&caller, true) => upstream?,
                () = context.ct.cancelled() => return Err(cancelled()),
            };
            // A child, so ending the call withdraws its questions without
            // cancelling the agent's request.
            let origin = CallOrigin::new(caller.clone(), meta, context.ct.child_token());
            let _tracked = upstream.calls.track(origin);
            // Dropping the request on cancel sends the server its own
            // `notifications/cancelled`.
            let response = tokio::select! {
                response = upstream.peer.call_tool_once(request) => response,
                () = context.ct.cancelled() => return Err(cancelled()),
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => return Err(self.failed(&upstream, &caller, true, error).await),
            };
            match response {
                complete @ CallToolResponse::Complete(_) => Ok(complete),
                // Only a server on protocol 2026-07-28 answers this way, and
                // core's connections use the `initialize` handshake.
                _ => Err(ErrorData::internal_error(
                    "The server answered in a way Ottam does not support yet",
                    None,
                )),
            }
        })
    }
}

/// An upstream failure as the MCP error the agent sees: the server's own
/// error when it sent one, else what went wrong reaching it.
fn upstream_error(error: ServiceError) -> ErrorData {
    match error {
        ServiceError::McpError(error) => error,
        other => {
            ErrorData::internal_error(format!("The server could not be reached: {other}"), None)
        }
    }
}
