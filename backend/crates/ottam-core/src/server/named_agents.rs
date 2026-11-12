//! `namedAgent.*` requests: the named agents conversations delegate to
//! through Ottam's `delegate` tool. See `docs/architecture/backend/mcp.md`
//! § Named Agents.

use super::request_params::{decode_params, InvalidParamsLog};
use super::responses::{core_error_response, error_response, success_response};
use crate::agent::{AgentId, AgentRegistry};
use crate::core_error::CoreError;
use crate::storage::repositories::{NamedAgentRecord, NamedAgentRepository};
use crate::storage::Database;
use chrono::{TimeZone, Utc};
use ottam_protocol::{
    EngineID, NamedAgent, NamedAgentConfig, NamedAgentCreateParams, NamedAgentDeleteParams,
    NamedAgentDeleteResult, NamedAgentID, NamedAgentListParams, NamedAgentListResult,
    NamedAgentResult, NamedAgentUpdateParams, ProtocolRequest, METHOD_NAMED_AGENT_CREATE,
    METHOD_NAMED_AGENT_DELETE, METHOD_NAMED_AGENT_LIST, METHOD_NAMED_AGENT_UPDATE,
};
use serde_json::Value;
use std::sync::Arc;

const MAX_NAME_LEN: usize = 40;

pub(super) fn is_named_agent_method(method: &str) -> bool {
    matches!(
        method,
        METHOD_NAMED_AGENT_LIST
            | METHOD_NAMED_AGENT_CREATE
            | METHOD_NAMED_AGENT_UPDATE
            | METHOD_NAMED_AGENT_DELETE
    )
}

fn iso(millis: i64) -> String {
    Utc.timestamp_millis_opt(millis)
        .single()
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn to_wire(record: NamedAgentRecord) -> NamedAgent {
    NamedAgent {
        named_agent_id: NamedAgentID(record.id),
        name: record.name,
        engine_id: EngineID(record.engine_id),
        model_id: record.model_id,
        mode_id: record.mode_id,
        instructions: record.instructions,
        avatar_seed: record.avatar_seed.and_then(|seed| u32::try_from(seed).ok()),
        created_at: iso(record.created_at),
        updated_at: iso(record.updated_at),
    }
}

fn invalid(message: &str) -> CoreError {
    CoreError::new("invalidNamedAgent", message).recoverable(true)
}

/// The config as stored: a trimmed name an agent can type, an agent that
/// exists, and the agent's read-only mode when none is chosen.
fn validated(
    registry: &AgentRegistry,
    config: NamedAgentConfig,
) -> Result<NamedAgentConfig, CoreError> {
    let name = config.name.trim().to_owned();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_LEN
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(invalid(
            "A named agent's name is 1 to 40 letters, digits, hyphens, or underscores",
        ));
    }
    let definition = registry
        .definition(&AgentId(config.engine_id.0.clone()))
        .map_err(|_| invalid("Choose an agent that is set up in Settings › Agents"))?;
    let blank = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    Ok(NamedAgentConfig {
        name,
        engine_id: config.engine_id,
        model_id: blank(config.model_id),
        mode_id: blank(config.mode_id).or_else(|| definition.read_only_mode().map(str::to_owned)),
        instructions: config.instructions.trim().to_owned(),
        avatar_seed: config.avatar_seed,
    })
}

pub(super) async fn handle_named_agent_request(
    database: &Database,
    registry: Option<Arc<AgentRegistry>>,
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
    let Some(registry) = registry else {
        return error_response(id, -32000, "agent runtime is unavailable");
    };
    let result: Result<Value, CoreError> = match request.method.as_str() {
        METHOD_NAMED_AGENT_LIST => {
            let _: NamedAgentListParams = params!(NamedAgentListParams);
            database
                .run("namedAgent.list", NamedAgentRepository::list)
                .await
                .map_err(CoreError::from)
                .map(|records| {
                    success_response(
                        id.clone(),
                        NamedAgentListResult {
                            named_agents: records.into_iter().map(to_wire).collect(),
                        },
                    )
                })
        }
        METHOD_NAMED_AGENT_CREATE => {
            let params = params!(NamedAgentCreateParams);
            match validated(&registry, params.config) {
                Ok(config) => {
                    let now = Utc::now().timestamp_millis();
                    let record = NamedAgentRecord {
                        id: uuid::Uuid::now_v7().to_string(),
                        name: config.name,
                        engine_id: config.engine_id.0,
                        model_id: config.model_id,
                        mode_id: config.mode_id,
                        instructions: config.instructions,
                        created_at: now,
                        updated_at: now,
                        avatar_seed: config.avatar_seed.map(i64::from),
                    };
                    let stored = record.clone();
                    database
                        .run("namedAgent.create", move |connection| {
                            NamedAgentRepository::insert(connection, &stored)
                        })
                        .await
                        .map_err(CoreError::from)
                        .map(|()| {
                            success_response(
                                id.clone(),
                                NamedAgentResult {
                                    named_agent: to_wire(record),
                                },
                            )
                        })
                }
                Err(error) => Err(error),
            }
        }
        METHOD_NAMED_AGENT_UPDATE => {
            let params = params!(NamedAgentUpdateParams);
            match validated(&registry, params.config) {
                Ok(config) => {
                    let named_agent_id = params.named_agent_id.0;
                    database
                        .run("namedAgent.update", move |connection| {
                            let existing = NamedAgentRepository::get(connection, &named_agent_id)?
                                .ok_or_else(|| {
                                    crate::storage::error::StorageError::not_found("NamedAgent")
                                })?;
                            let record = NamedAgentRecord {
                                name: config.name,
                                engine_id: config.engine_id.0,
                                model_id: config.model_id,
                                mode_id: config.mode_id,
                                instructions: config.instructions,
                                avatar_seed: config.avatar_seed.map(i64::from),
                                updated_at: Utc::now().timestamp_millis(),
                                ..existing
                            };
                            NamedAgentRepository::update(connection, &record)?;
                            Ok(record)
                        })
                        .await
                        .map_err(CoreError::from)
                        .map(|record| {
                            success_response(
                                id.clone(),
                                NamedAgentResult {
                                    named_agent: to_wire(record),
                                },
                            )
                        })
                }
                Err(error) => Err(error),
            }
        }
        METHOD_NAMED_AGENT_DELETE => {
            let params = params!(NamedAgentDeleteParams);
            let named_agent_id = params.named_agent_id.0;
            database
                .run("namedAgent.delete", move |connection| {
                    NamedAgentRepository::delete(connection, &named_agent_id)
                })
                .await
                .map_err(CoreError::from)
                .map(|_| success_response(id.clone(), NamedAgentDeleteResult {}))
        }
        other => return error_response(id, -32601, format!("unknown method {other}")),
    };
    result.unwrap_or_else(|error| core_error_response(id, -32000, error))
}
