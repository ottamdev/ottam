//! Cache operations run off the dispatcher and never load a harness to evict.
use crate::storage::{
    payload::{
        self,
        cache::{Cache, Recovery},
        RevisionKey,
    },
    Database, StorageError,
};
use diesel::{
    prelude::*,
    sql_types::{BigInt, Nullable, Text},
};

#[derive(QueryableByName)]
struct Tool {
    #[diesel(sql_type = Text)]
    workspace_id: String,
    #[diesel(sql_type = Text)]
    conversation_id: String,
    #[diesel(sql_type = Text)]
    item_id: String,
    #[diesel(sql_type = Text)]
    payload_id: String,
    #[diesel(sql_type = BigInt)]
    revision: i64,
    #[diesel(sql_type = Nullable<Text>)]
    agent_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    session_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    tool_call_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    cwd: Option<String>,
}

pub(super) async fn sweep(
    database: &Database,
    clear: bool,
    mut limit: Option<u64>,
    all: bool,
) -> Result<(ottam_protocol::StorageStatusResult, bool), StorageError> {
    let mut cursor = database.payload_cache_scan().lock().await;
    if all {
        *cursor = (String::new(), -1);
    }
    loop {
        let after = cursor.clone();
        let tools = database.run("storage.cacheReferences", move |c| {
        diesel::sql_query("SELECT c.workspace_id, i.conversation_id, i.id AS item_id, p.payload_id, p.revision, r.engine_id AS agent_id, a.external_session_id AS session_id, t.provider_tool_call_id AS tool_call_id, (SELECT canonical_path FROM conversation_run_sources s WHERE s.run_id = r.id AND s.role = 'primary' LIMIT 1) AS cwd FROM transcript_payloads p JOIN transcript_items i ON i.id = p.item_id JOIN transcript_tools t ON t.item_id = i.id JOIN conversations c ON c.id = i.conversation_id LEFT JOIN conversation_runs r ON r.id = i.run_id LEFT JOIN agent_sessions a ON a.id = r.agent_session_id WHERE json_extract(p.descriptor, '$.role') = 'output' AND (r.id IS NULL OR r.completed_at IS NOT NULL) AND (p.payload_id, p.revision) > (?, ?) ORDER BY p.payload_id, p.revision LIMIT 128")
            .bind::<Text, _>(after.0).bind::<BigInt, _>(after.1)
            .load::<Tool>(c).map_err(StorageError::from)
    }).await?;
        let more_references = tools.len() == 128;
        let next = if more_references {
            tools
                .last()
                .map(|t| (t.payload_id.clone(), t.revision))
                .unwrap()
        } else {
            (String::new(), -1)
        };
        let database = database.clone();
        let current_limit = limit.take();
        let result = tokio::task::spawn_blocking(move || -> Result<_, StorageError> {
            let _gate = database.payload_cache_gate().lock().unwrap();
            let mut cache = Cache::open(database.home()).map_err(error)?;
            let mut protected = database.payload_leases().lock().unwrap().clone();
            protected.extend(database.payload_worker().active_keys());
            protected.extend(database.payload_cache_read_keys());
            for tool in tools {
                let key = RevisionKey {
                    workspace_id: tool.workspace_id,
                    conversation_id: tool.conversation_id,
                    item_id: tool.item_id,
                    payload_id: tool.payload_id,
                    revision: tool.revision as u64,
                };
                cache
                    .classify(
                        &key,
                        &Recovery {
                            agent_id: tool.agent_id,
                            session_id: tool.session_id,
                            tool_call_id: tool.tool_call_id,
                            cwd: tool.cwd,
                        },
                    )
                    .map_err(error)?;
            }
            if let Some(limit) = current_limit {
                cache.set_limit_gb(limit).map_err(error)?;
            }
            let mut more_eviction = cache.evict(&protected, clear).map_err(error)?;
            if all {
                while more_eviction {
                    more_eviction = cache.evict(&protected, clear).map_err(error)?;
                }
            }
            Ok((
                ottam_protocol::StorageStatusResult {
                    cache_bytes: cache.usage().map_err(error)?,
                    limit_gb: cache.limit_gb().map_err(error)?,
                    database_bytes: std::fs::metadata(database.home().join("payloads.sqlite"))
                        .map_or(0, |m| m.len()),
                    wal_bytes: std::fs::metadata(database.home().join("payloads.sqlite-wal"))
                        .map_or(0, |m| m.len()),
                },
                more_eviction,
            ))
        })
        .await
        .map_err(|_| StorageError::new("storageFailed", "Storage task stopped"))??;
        *cursor = next;
        if !all || !more_references {
            return Ok((result.0, more_references || result.1));
        }
    }
}

fn error(e: payload::Error) -> StorageError {
    StorageError::new("storageFailed", e.to_string())
}

pub(super) async fn handle_storage_request(
    database: &Database,
    request: ottam_protocol::ProtocolRequest<serde_json::Value>,
) -> serde_json::Value {
    use super::responses::{core_error_response, error_response, success_response};
    let params: ottam_protocol::StorageUpdateParams =
        match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => return error_response(request.id, -32602, error.to_string()),
        };
    let clear = request.method == ottam_protocol::METHOD_STORAGE_CLEAR_CACHE;
    let update = request.method == ottam_protocol::METHOD_STORAGE_UPDATE;
    let limit = if update { params.limit_gb } else { None };
    if update
        && limit.is_none_or(|gb| gb == 0 || gb > i64::MAX as u64 / 1_000_000_000)
    {
        return error_response(
            request.id,
            -32602,
            "Cache limit must be a positive whole number of GB",
        );
    }
    match sweep(database, clear, limit, true).await {
        Ok((status, _)) => {
            database.payload_worker().notify_changed();
            success_response(request.id, status)
        }
        Err(error) => core_error_response(request.id, -32000, error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ottam_protocol::{ProtocolRequest, RequestID};
    use serde_json::json;

    #[tokio::test]
    async fn payload_storage_rpc_validates_limits_and_retains_settings() {
        let home = tempfile::tempdir().unwrap();
        let database = Database::open_test_home(home.path().to_owned()).unwrap();
        for params in [
            json!({}),
            json!({"limitGb":0}),
            json!({"limitGb":1.5}),
            json!({"limitGb":u64::MAX}),
        ] {
            let response = handle_storage_request(
                &database,
                ProtocolRequest::new(RequestID::Number(1), "storage.update", params),
            )
            .await;
            assert!(response.get("error").is_some(), "{response}");
        }
        let response = handle_storage_request(
            &database,
            ProtocolRequest::new(
                RequestID::Number(2),
                "storage.update",
                json!({"limitGb":12}),
            ),
        )
        .await;
        assert_eq!(response["result"]["limitGb"], 12);
        drop(database);
        let database = Database::open(home.path().to_owned()).unwrap();
        let response = handle_storage_request(
            &database,
            ProtocolRequest::new(RequestID::Number(3), "storage.status", json!({})),
        )
        .await;
        assert_eq!(response["result"]["limitGb"], 12);
        assert_eq!(response["result"]["cacheBytes"], 0);
    }
}
