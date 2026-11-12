//! `mcp.*` requests: the configured MCP server list and remote servers'
//! sign-ins. They run off the dispatcher, since secrets go through the
//! Keychain. Each server's state comes from the upstream runtime, which
//! reports changes as `mcp.serverStateChanged`; sign-ins finish as
//! `mcp.signInFinished`, chats that need one hear `mcp.signInNeeded`, and
//! any change to the list is `mcp.serversChanged`.

use super::outbound::OutboundSink;
use super::request_params::{decode_params, InvalidParamsLog};
use super::responses::{core_error_response, error_response, success_response};
use crate::mcp::{McpNotice, McpNoticeSink, McpService};
use ottam_protocol::{
    McpAddParams, McpCancelSignInParams, McpCancelSignInResult, McpImportCandidatesParams,
    McpListParams, McpListResult, McpRemoveParams, McpRemoveResult, McpServerResult,
    McpServerStateChangedParams, McpServersChangedParams, McpSetWorkspaceEnabledParams,
    McpSignInParams, McpSignOutParams, McpUpdateParams, ProtocolNotification, ProtocolRequest,
    METHOD_MCP_ADD, METHOD_MCP_CANCEL_SIGN_IN, METHOD_MCP_IMPORT_CANDIDATES, METHOD_MCP_LIST,
    METHOD_MCP_REMOVE, METHOD_MCP_SERVERS_CHANGED, METHOD_MCP_SERVER_STATE_CHANGED,
    METHOD_MCP_SET_WORKSPACE_ENABLED, METHOD_MCP_SIGN_IN, METHOD_MCP_SIGN_IN_FINISHED,
    METHOD_MCP_SIGN_IN_NEEDED, METHOD_MCP_SIGN_OUT, METHOD_MCP_UPDATE,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

pub(super) fn notice_sink(outbound: OutboundSink) -> McpNoticeSink {
    fn send(outbound: &OutboundSink, method: &str, params: impl Serialize) {
        if let Ok(value) = serde_json::to_value(ProtocolNotification::new(method, params)) {
            let _ = outbound.notify(value);
        }
    }
    Arc::new(move |notice| match notice {
        McpNotice::State(server_id, state) => send(
            &outbound,
            METHOD_MCP_SERVER_STATE_CHANGED,
            McpServerStateChangedParams { server_id, state },
        ),
        McpNotice::SignInFinished(params) => send(&outbound, METHOD_MCP_SIGN_IN_FINISHED, params),
        McpNotice::SignInNeeded(params) => send(&outbound, METHOD_MCP_SIGN_IN_NEEDED, params),
        McpNotice::ServersChanged => send(
            &outbound,
            METHOD_MCP_SERVERS_CHANGED,
            McpServersChangedParams {},
        ),
    })
}

pub(super) fn is_mcp_method(method: &str) -> bool {
    matches!(
        method,
        METHOD_MCP_LIST
            | METHOD_MCP_ADD
            | METHOD_MCP_UPDATE
            | METHOD_MCP_REMOVE
            | METHOD_MCP_SET_WORKSPACE_ENABLED
            | METHOD_MCP_IMPORT_CANDIDATES
            | METHOD_MCP_SIGN_IN
            | METHOD_MCP_CANCEL_SIGN_IN
            | METHOD_MCP_SIGN_OUT
    )
}

pub(super) async fn handle_mcp_request(
    service: &McpService,
    request: ProtocolRequest<Value>,
) -> Value {
    let id = request.id.clone();
    macro_rules! params {
        ($type:ty) => {
            match decode_params::<$type>(&request, InvalidParamsLog::Warn) {
                Ok(params) => params,
                Err(response) => return response,
            }
        };
    }
    let server = |server| success_response(id.clone(), McpServerResult { server });
    let result = match request.method.as_str() {
        METHOD_MCP_LIST => {
            let _: McpListParams = params!(McpListParams);
            service.list().await.map(|servers| {
                let redirect_uri = service.redirect_uri();
                success_response(
                    id.clone(),
                    McpListResult {
                        servers,
                        redirect_uri,
                    },
                )
            })
        }
        METHOD_MCP_ADD => {
            let params = params!(McpAddParams);
            service.add(params.config).await.map(server)
        }
        METHOD_MCP_UPDATE => {
            let params = params!(McpUpdateParams);
            service
                .update(&params.server_id, params.config)
                .await
                .map(server)
        }
        METHOD_MCP_REMOVE => {
            let params = params!(McpRemoveParams);
            service
                .remove(&params.server_id)
                .await
                .map(|()| success_response(id.clone(), McpRemoveResult {}))
        }
        METHOD_MCP_SET_WORKSPACE_ENABLED => {
            let params = params!(McpSetWorkspaceEnabledParams);
            service
                .set_workspace_enabled(&params.server_id, &params.workspace_id, params.enabled)
                .await
                .map(server)
        }
        METHOD_MCP_IMPORT_CANDIDATES => {
            let params = params!(McpImportCandidatesParams);
            service
                .import_candidates(params.source)
                .await
                .map(|result| success_response(id.clone(), result))
        }
        METHOD_MCP_SIGN_IN => {
            let params = params!(McpSignInParams);
            service
                .sign_in(&params.server_id, params.client_id)
                .await
                .map(|result| success_response(id.clone(), result))
        }
        METHOD_MCP_CANCEL_SIGN_IN => {
            let params = params!(McpCancelSignInParams);
            service.cancel_sign_in(&params.server_id).await;
            Ok(success_response(id.clone(), McpCancelSignInResult {}))
        }
        METHOD_MCP_SIGN_OUT => {
            let params = params!(McpSignOutParams);
            service.sign_out(&params.server_id).await.map(server)
        }
        other => return error_response(id, -32601, format!("unknown method {other}")),
    };
    result.unwrap_or_else(|error| core_error_response(id, -32000, error))
}
