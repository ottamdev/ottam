use crate::support::{
    assert_error_data_kind, assert_error_response, assert_success_response, CoreProcess,
    FakeAcpRuntime, GitRepoFixture,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn persisted_surfaces_survive_restart_while_terminals_reset() {
    let repo = GitRepoFixture::init();
    repo.write("tracked.txt", "one\n");
    repo.commit_all("initial");
    repo.write("tracked.txt", "one\ntwo\n");
    let ottam_home = TempDir::new().expect("creates OTTAM_HOME");
    let fake_runtime = FakeAcpRuntime::new();
    let core_env = fake_runtime.environment();
    let (project_id, project_source_id, conversation_id, run_id, agent_session_id, terminal_id) = {
        let mut core = CoreProcess::new_in_home_with_env(ottam_home.path(), core_env.clone());
        core.initialize();
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        let project_source_id = opened["source"]["projectSourceID"]
            .as_str()
            .expect("projectSourceID")
            .to_string();

        let diff = core.diff_by_project_source_id(&project_source_id);
        assert_eq!(diff["files"][0]["path"].as_str(), Some("tracked.txt"));
        assert_eq!(diff["additions"].as_i64(), Some(1));

        let conversation = core.create_conversation(
            &project_id,
            Some("Phase zero retained behavior"),
            Some("Keep this Chat history through the persistence reset."),
        );
        let conversation_id = conversation["conversation"]["conversationID"]
            .as_str()
            .expect("conversationID")
            .to_string();
        let initial_input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
            .as_str()
            .unwrap_or("input-jsonl-test")
            .to_string();
        let started =
            core.start_conversation_run(&project_id, &conversation_id, "codex", &initial_input_id);
        let run_id = started["run"]["runID"].as_str().expect("runID").to_string();
        let conversation =
            wait_for_completed_conversation(&mut core, &project_id, &conversation_id, &run_id);
        let messages = conversation["latestMessagePage"]["messages"]
            .as_array()
            .expect("messages");
        assert_eq!(messages.len(), 1);
        // Message pages are newest-first.
        assert_eq!(messages[0]["role"].as_str(), Some("user"));
        assert_eq!(
            messages[0]["body"].as_str(),
            Some("Keep this Chat history through the persistence reset.")
        );
        let transcript = core.send(
            json!({"id":68,"method":"conversation.listTranscript","params":{
                "workspaceID":core.workspace_id(&project_id),"conversationID":conversation_id
            }}),
        );
        assert_eq!(
            transcript["result"]["items"][1]["bodyPreview"],
            "Retained ACP reply."
        );
        let run = &conversation["latestRunPage"]["runs"][0];
        assert_eq!(run["status"].as_str(), Some("completed"));
        assert_eq!(run["engineID"].as_str(), Some("codex"));
        assert_eq!(run["runID"].as_str(), Some(run_id.as_str()));
        let agent_session_id = run["agentSessionID"]
            .as_str()
            .expect("agentSessionID")
            .to_string();
        let inputs = core.send(json!({
            "id": 69,
            "method": "conversation.listRunInputs",
            "params": {
                "workspaceID": core.workspace_id(&project_id),
                "conversationID": conversation_id
            }
        }));
        assert_success_response(&inputs, &json!(69));
        assert_eq!(
            inputs["result"]["page"]["runInputs"][0]["runInputID"].as_str(),
            Some(initial_input_id.as_str())
        );
        assert_eq!(
            inputs["result"]["page"]["runInputs"][0]["status"].as_str(),
            Some("delivered")
        );
        assert_eq!(
            inputs["result"]["page"]["runInputs"][0]["deliveryEvidence"].as_str(),
            Some("adapterConfirmed")
        );

        let (open_response, _) = core.send_for_id(
            json!({
                "id": 70,
                "method": "terminal.open",
                "params": {
                    "workspaceID": core.workspace_id(&project_id),
                    "workingRoot": {
                        "kind": "project",
                        "projectID": project_id,
                        "sourceID": core.project_source_id(&project_id)
                    },
                    "rows": 24,
                    "cols": 80
                }
            }),
            json!(70),
        );
        assert_success_response(&open_response, &json!(70));
        let terminal_id = open_response["result"]["terminalID"]
            .as_str()
            .expect("terminalID")
            .to_string();
        let session_id = open_response["result"]["sessionID"]
            .as_str()
            .expect("sessionID")
            .to_string();

        let marker = "__ottam_phase_zero_terminal__";
        let (write_response, notifications) = core.send_for_id(
            json!({
                "id": 71,
                "method": "terminal.write",
                "params": {
                    "sessionID": session_id,
                    "dataBase64": STANDARD.encode(format!("printf {marker}\\n\n"))
                }
            }),
            json!(71),
        );
        assert_success_response(&write_response, &json!(71));
        let output = collect_terminal_output(&mut core, &session_id, notifications, marker);
        assert!(
            output.contains(marker),
            "terminal output did not contain marker: {output:?}"
        );

        let close_response = core.send(json!({
            "id": 72,
            "method": "terminal.close",
            "params": {
                "sessionID": session_id
            }
        }));
        assert_success_response(&close_response, &json!(72));
        core.shutdown();
        (
            project_id,
            project_source_id,
            conversation_id,
            run_id,
            agent_session_id,
            terminal_id,
        )
    };

    let mut restarted = CoreProcess::new_in_home_with_env(ottam_home.path(), core_env);
    restarted.initialize();
    let reopened = restarted.open_path(repo.path());
    assert_eq!(
        reopened["project"]["projectID"].as_str(),
        Some(project_id.as_str())
    );

    assert_eq!(
        reopened["source"]["projectSourceID"].as_str(),
        Some(project_source_id.as_str())
    );
    let diff = restarted.diff_by_project_source_id(&project_source_id);
    assert_eq!(diff["files"][0]["path"].as_str(), Some("tracked.txt"));
    assert_eq!(diff["additions"].as_i64(), Some(1));

    let conversation = restarted.get_conversation(&project_id, &conversation_id);
    assert_eq!(
        conversation["conversation"]["title"].as_str(),
        Some("Phase zero retained behavior")
    );
    let messages = conversation["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages");
    assert_eq!(messages.len(), 1);
    // Message pages are newest-first.
    assert_eq!(
        messages[0]["body"].as_str(),
        Some("Keep this Chat history through the persistence reset.")
    );
    let transcript = restarted.send(
        json!({"id":168,"method":"conversation.listTranscript","params":{
            "workspaceID":restarted.workspace_id(&project_id),"conversationID":conversation_id
        }}),
    );
    assert_eq!(
        transcript["result"]["items"][1]["bodyPreview"],
        "Retained ACP reply."
    );
    let runs = conversation["latestRunPage"]["runs"]
        .as_array()
        .expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["runID"].as_str(), Some(run_id.as_str()));
    assert_eq!(runs[0]["status"].as_str(), Some("completed"));
    assert_eq!(runs[0]["engineID"].as_str(), Some("codex"));
    assert_eq!(
        runs[0]["agentSessionID"].as_str(),
        Some(agent_session_id.as_str())
    );

    let list_response = restarted.send(json!({
        "id": 73,
        "method": "terminal.list",
        "params": {
            "workspaceID": restarted.workspace_id(&project_id)
        }
    }));
    assert_success_response(&list_response, &json!(73));
    let terminals = list_response["result"]["terminals"]
        .as_array()
        .expect("terminal list");
    assert!(terminals.is_empty());

    let replay_response = restarted.send(json!({
        "id": 74,
        "method": "terminal.replay",
        "params": {
            "workspaceID": restarted.workspace_id(&project_id),
            "terminalID": terminal_id,
            "maxBytes": 4096
        }
    }));
    assert_error_response(&replay_response, &json!(74), -32000, "terminal not found");
    assert_error_data_kind(&replay_response, "terminalNotFound");
    restarted.shutdown();
}

#[cfg(unix)]
fn wait_for_completed_conversation(
    core: &mut CoreProcess,
    project_id: &str,
    conversation_id: &str,
    run_id: &str,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 75,
                "method": "conversation.get",
                "params": {
                    "workspaceID": core.workspace_id(project_id),
                    "conversationID": conversation_id
                }
            }),
            json!(75),
        );
        assert_success_response(&response, &json!(75));
        let conversation = response["result"]["conversation"].clone();
        let completed = conversation["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| {
                runs.iter()
                    .find(|run| run["runID"].as_str() == Some(run_id))
            })
            .and_then(|run| run["status"].as_str())
            == Some("completed");
        if completed {
            return conversation;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for adapter-backed Chat run {run_id}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn collect_terminal_output(
    core: &mut CoreProcess,
    session_id: &str,
    initial_notifications: Vec<Value>,
    marker: &str,
) -> String {
    let mut output = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut pending = initial_notifications.into_iter();

    loop {
        let value = if let Some(value) = pending.next() {
            value
        } else {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for terminal marker {marker:?}; output={output:?}"
            );
            core.read_json_timeout(remaining)
                .expect("terminal output notification")
        };

        if value["method"].as_str() != Some("terminal.outputDelta")
            || value["params"]["sessionID"].as_str() != Some(session_id)
        {
            continue;
        }
        let bytes = STANDARD
            .decode(
                value["params"]["dataBase64"]
                    .as_str()
                    .expect("terminal output data"),
            )
            .expect("decodes terminal output");
        output.push_str(&String::from_utf8_lossy(&bytes));
        if output.contains(marker) {
            return output;
        }
    }
}
