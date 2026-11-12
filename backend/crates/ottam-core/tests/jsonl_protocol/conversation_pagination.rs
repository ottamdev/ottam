use crate::support::{
    assert_error_data_kind, assert_success_response, CoreProcess, FakeAcpRuntime, GitRepoFixture,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[test]
fn conversation_history_methods_preserve_public_paging_and_wire_contracts() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "conversation paging\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);

    let conversation = core.create_conversation(
        &project_id,
        Some("Paged history"),
        Some("first user message"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let first_input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial run input")
        .to_string();
    let first_started =
        core.start_conversation_run(&project_id, &conversation_id, "codex", &first_input_id);
    let first_run_id = first_started["run"]["runID"]
        .as_str()
        .expect("first runID")
        .to_string();
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &first_run_id);

    let posted =
        core.post_conversation_message(&project_id, &conversation_id, "second user message");
    let second_input_id = posted["input"]["runInputID"]
        .as_str()
        .expect("follow-up run input")
        .to_string();
    // A second engine forces a second durable AgentSession while retaining the
    // same fake ACP behavior, so the public session cursor is exercised too.
    let second_started =
        core.start_conversation_run(&project_id, &conversation_id, "claude", &second_input_id);
    let second_run_id = second_started["run"]["runID"]
        .as_str()
        .expect("second runID")
        .to_string();
    wait_for_completed_run(&mut core, &workspace_id, &conversation_id, &second_run_id);

    let mut request_id = 1_200;
    let messages = collect_nested_pages(
        &mut core,
        &workspace_id,
        &conversation_id,
        NestedPageSpec {
            method: "conversation.listMessages",
            collection_key: "messages",
            item_id_key: "messageID",
            ordinal_cursor: true,
        },
        &mut request_id,
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["body"], "second user message");
    assert_eq!(messages[1]["body"], "first user message");
    assert!(messages[0]["position"].as_u64().unwrap() > messages[1]["position"].as_u64().unwrap());
    assert_eq!(messages[0]["role"], "user");
    let mut cursor: Option<String> = None;
    let mut transcript = Vec::new();
    loop {
        request_id += 1;
        let response = core.send(json!({"id":request_id,"method":"conversation.listTranscript","params":{
            "workspaceID":workspace_id,"conversationID":conversation_id,"limit":1,"cursor":cursor
        }}));
        assert_success_response(&response, &json!(request_id));
        let result = &response["result"];
        transcript.extend(result["items"].as_array().unwrap().clone());
        cursor = result["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(transcript.len(), 6);
    assert_eq!(transcript[0]["kind"], "runBoundary");
    assert_eq!(transcript[0]["runStatus"], "completed");
    assert_eq!(transcript[1]["kind"], "assistantText");
    assert_eq!(transcript[1]["bodyPreview"], "Retained ACP reply.");
    assert_eq!(transcript[0]["runID"], second_run_id);
    assert_eq!(transcript[2]["kind"], "message");
    assert!(transcript
        .windows(2)
        .all(|items| items[0]["position"].as_u64() > items[1]["position"].as_u64()));

    let runs = collect_nested_pages(
        &mut core,
        &workspace_id,
        &conversation_id,
        NestedPageSpec {
            method: "conversation.listRuns",
            collection_key: "runs",
            item_id_key: "runID",
            ordinal_cursor: false,
        },
        &mut request_id,
    );
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["runID"], second_run_id);
    assert_eq!(runs[1]["runID"], first_run_id);
    assert_eq!(runs[0]["conversationID"], conversation_id);
    assert_eq!(runs[0]["engineID"], "claude");
    assert_eq!(runs[1]["engineID"], "codex");
    for run in &runs {
        assert_eq!(run["status"], "completed");
        assert!(run["agentSessionID"].as_str().is_some());
        assert_eq!(run["sessionOrigin"], "new");
        assert_eq!(run["stopReason"], "endTurn");
        assert_eq!(run["cost"], json!({}));
        assert_eq!(run["error"], json!({}));
    }

    let inputs = collect_nested_pages(
        &mut core,
        &workspace_id,
        &conversation_id,
        NestedPageSpec {
            method: "conversation.listRunInputs",
            collection_key: "runInputs",
            item_id_key: "runInputID",
            ordinal_cursor: false,
        },
        &mut request_id,
    );
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["runInputID"], second_input_id);
    assert_eq!(inputs[1]["runInputID"], first_input_id);
    assert_eq!(inputs[0]["messageID"], messages[0]["messageID"]);
    assert_eq!(inputs[1]["messageID"], messages[1]["messageID"]);
    assert_eq!(inputs[0]["targetRunID"], second_run_id);
    assert_eq!(inputs[1]["targetRunID"], first_run_id);
    for input in &inputs {
        assert_eq!(input["conversationID"], conversation_id);
        assert_eq!(input["kind"], "initial");
        assert_eq!(input["status"], "delivered");
        assert_eq!(input["deliveryEvidence"], "adapterConfirmed");
        assert!(input["idempotencyKey"].as_str().is_some());
    }

    let sessions = collect_nested_pages(
        &mut core,
        &workspace_id,
        &conversation_id,
        NestedPageSpec {
            method: "conversation.listAgentSessions",
            collection_key: "sessions",
            item_id_key: "agentSessionID",
            ordinal_cursor: false,
        },
        &mut request_id,
    );
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["engineID"], "claude");
    assert_eq!(sessions[1]["engineID"], "codex");
    for session in &sessions {
        assert_eq!(session["conversationID"], conversation_id);
        assert_eq!(session["state"], "active");
        assert!(session["sourceScopeFingerprint"].as_str().is_some());
        assert!(session["createdAt"].as_str().is_some());
        assert!(session["lastUsedAt"].as_str().is_some());
        assert!(session.get("endedAt").is_none());
        assert!(session.get("endReason").is_none());
    }

    let events = collect_conversation_event_pages(
        &mut core,
        &workspace_id,
        &conversation_id,
        &mut request_id,
    );
    assert!(!events.is_empty());
    let event_run_ids = events
        .iter()
        .map(|event| event["runID"].as_str().expect("event runID"))
        .collect::<Vec<_>>();
    assert!(event_run_ids.contains(&first_run_id.as_str()));
    assert!(event_run_ids.contains(&second_run_id.as_str()));
    let first_second_run_event = event_run_ids
        .iter()
        .position(|run_id| *run_id == second_run_id)
        .expect("second run events");
    assert!(event_run_ids[..first_second_run_event]
        .iter()
        .all(|run_id| *run_id == first_run_id));
    assert!(event_run_ids[first_second_run_event..]
        .iter()
        .all(|run_id| *run_id == second_run_id));
    for adjacent in events.windows(2) {
        if adjacent[0]["runID"] == adjacent[1]["runID"] {
            assert!(
                adjacent[0]["sequence"].as_u64().expect("event sequence")
                    < adjacent[1]["sequence"].as_u64().expect("event sequence"),
                "events stay in per-run insertion order"
            );
        }
    }
    let representative = &events[0];
    assert_eq!(representative["workspaceID"], workspace_id);
    assert_eq!(representative["projectID"], project_id);
    assert_eq!(representative["conversationID"], conversation_id);
    assert!(representative["eventID"].as_str().is_some());
    assert!(representative["sequence"].as_u64().is_some());
    assert!(representative["eventKind"].as_str().is_some());
    assert!(representative.get("payload").is_some());

    request_id += 1;
    let filtered = core.send(json!({
        "id": request_id,
        "method": "conversation.listEvents",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "runID": first_run_id,
            "limit": 100
        }
    }));
    assert_success_response(&filtered, &json!(request_id));
    assert!(filtered["result"].get("page").is_none());
    let filtered_events = filtered["result"]["events"]
        .as_array()
        .expect("filtered events");
    assert!(!filtered_events.is_empty());
    assert!(filtered_events
        .iter()
        .all(|event| event["runID"] == first_run_id));

    let other = core.create_conversation(&project_id, Some("Other"), None);
    let other_conversation_id = other["conversation"]["conversationID"]
        .as_str()
        .expect("other conversationID");
    request_id += 1;
    let mismatched = core.send(json!({
        "id": request_id,
        "method": "conversation.listEvents",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": other_conversation_id,
            "runID": first_run_id
        }
    }));
    assert_error_data_kind(&mismatched, "unknownConversationRunID");

    core.shutdown();
}

#[derive(Clone, Copy)]
struct NestedPageSpec<'a> {
    method: &'a str,
    collection_key: &'a str,
    item_id_key: &'a str,
    ordinal_cursor: bool,
}

fn collect_nested_pages(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    spec: NestedPageSpec<'_>,
    request_id: &mut i64,
) -> Vec<Value> {
    let mut cursor = None;
    let mut collected = Vec::new();
    let mut seen = HashSet::new();
    loop {
        *request_id += 1;
        let mut params = Map::from_iter([
            ("workspaceID".to_string(), json!(workspace_id)),
            ("conversationID".to_string(), json!(conversation_id)),
            ("limit".to_string(), json!(1)),
        ]);
        if let Some(cursor) = &cursor {
            params.insert("cursor".to_string(), json!(cursor));
        }
        let response = core.send(json!({
            "id": *request_id,
            "method": spec.method,
            "params": params
        }));
        assert_success_response(&response, &json!(*request_id));
        let result = response["result"].as_object().expect("result object");
        assert!(
            result.get(spec.collection_key).is_none(),
            "{} keeps its collection inside the page wrapper",
            spec.method
        );
        let page = result["page"].as_object().expect("page object");
        let items = page[spec.collection_key]
            .as_array()
            .unwrap_or_else(|| panic!("{} {} array", spec.method, spec.collection_key));
        assert_eq!(items.len(), 1, "test fixture expects one item per page");
        let item = items[0].clone();
        let item_id = item[spec.item_id_key]
            .as_str()
            .unwrap_or_else(|| panic!("{} {}", spec.method, spec.item_id_key))
            .to_string();
        assert!(seen.insert(item_id.clone()), "cursor must be exclusive");
        collected.push(item);
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(next_cursor) => {
                let expected_cursor = if spec.ordinal_cursor {
                    collected
                        .last()
                        .and_then(|item| item["position"].as_u64())
                        .expect("message ordinal")
                        .to_string()
                } else {
                    item_id
                };
                assert_eq!(
                    next_cursor, expected_cursor,
                    "cursor is the last returned ordering key"
                );
                cursor = Some(next_cursor.to_string());
            }
            None => {
                assert!(
                    !page.contains_key("nextCursor"),
                    "the terminal page omits nextCursor"
                );
                break;
            }
        }
    }
    collected
}

fn collect_conversation_event_pages(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    request_id: &mut i64,
) -> Vec<Value> {
    let mut cursor = None;
    let mut collected = Vec::new();
    let mut seen = HashSet::new();
    loop {
        *request_id += 1;
        let mut params = Map::from_iter([
            ("workspaceID".to_string(), json!(workspace_id)),
            ("conversationID".to_string(), json!(conversation_id)),
            ("limit".to_string(), json!(1)),
        ]);
        if let Some(cursor) = &cursor {
            params.insert("cursor".to_string(), json!(cursor));
        }
        let response = core.send(json!({
            "id": *request_id,
            "method": "conversation.listEvents",
            "params": params
        }));
        assert_success_response(&response, &json!(*request_id));
        let result = response["result"].as_object().expect("event result object");
        assert!(result.get("page").is_none(), "events have no page wrapper");
        let events = result["events"].as_array().expect("events array");
        assert_eq!(events.len(), 1, "test fixture expects one event per page");
        let event = events[0].clone();
        let event_id = event["eventID"].as_str().expect("eventID").to_string();
        assert!(
            seen.insert(event_id.clone()),
            "event cursor must be exclusive"
        );
        collected.push(event);
        match result.get("nextCursor").and_then(Value::as_str) {
            Some(next_cursor) => {
                assert_eq!(next_cursor, event_id);
                cursor = Some(next_cursor.to_string());
            }
            None => {
                assert!(
                    !result.contains_key("nextCursor"),
                    "the terminal event page omits nextCursor"
                );
                break;
            }
        }
    }
    collected
}

fn wait_for_completed_run(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = core.send(json!({
            "id": 1_199,
            "method": "conversation.get",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id
            }
        }));
        assert_success_response(&response, &json!(1_199));
        let completed = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .expect("latest runs")
            .iter()
            .find(|run| run["runID"].as_str() == Some(run_id))
            .and_then(|run| run["status"].as_str())
            == Some("completed");
        if completed {
            return;
        }
        assert!(Instant::now() < deadline, "run {run_id} did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
}
