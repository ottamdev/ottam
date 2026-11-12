use crate::support::{
    assert_error_data_kind, assert_success_response, CoreProcess, FakeAcpRuntime,
    FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn wait_for_input_status(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    input_id: &str,
    expected: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_status = Value::Null;
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 721,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(721),
        );
        assert_success_response(&response, &json!(721));
        let detail = response["result"]["conversation"].clone();
        if let Some(input) = detail["latestRunInputPage"]["runInputs"]
            .as_array()
            .and_then(|inputs| inputs.iter().find(|input| input["runInputID"] == input_id))
        {
            last_status = input["status"].clone();
            if input["status"] == expected {
                return input.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "input {input_id} did not reach {expected}; last status was {last_status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_no_active_run(core: &mut CoreProcess, workspace_id: &str, conversation_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 727,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(727),
        );
        assert_success_response(&response, &json!(727));
        let runs = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if runs
            .iter()
            .all(|run| run["status"] != "queued" && run["status"] != "running")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the conversation still has an active run: {runs:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_prompt_call(runtime: &FakeAcpRuntime, expected_count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = runtime
            .request_log()
            .matches("\"method\":\"session/prompt\"")
            .count();
        if count >= expected_count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fake ACP prompt was not observed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn pending_intent_survives_restart_without_duplication() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "pending recovery\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let (project_id, conversation_id, input_id) = {
        let mut core = CoreProcess::new_in_home(home.path());
        core.initialize();
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        let created = core.create_conversation(
            &project_id,
            Some("Pending recovery"),
            Some("Preserve this pending intent"),
        );
        let conversation_id = created["conversation"]["conversationID"]
            .as_str()
            .expect("conversationID")
            .to_string();
        let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
            .as_str()
            .expect("inputID")
            .to_string();
        core.shutdown();
        (project_id, conversation_id, input_id)
    };

    let mut restarted = CoreProcess::new_in_home(home.path());
    restarted.initialize();
    restarted.open_path(repo.path());
    let detail = restarted.get_conversation(&project_id, &conversation_id);
    let matching = detail["latestRunInputPage"]["runInputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .filter(|input| input["runInputID"] == input_id)
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0]["status"], "pending");
    restarted.shutdown();
}

#[cfg(unix)]
#[test]
fn crash_after_sending_requires_explicit_retry_or_mark_delivered() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "delivery recovery\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::NeverAcknowledge);
    let env = runtime.environment();

    let (project_id, workspace_id, conversation_id, original_input_id) = {
        let mut core = CoreProcess::new_in_home_with_env(home.path(), env.clone());
        core.initialize();
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        let workspace_id = core.workspace_id(&project_id);
        let created = core.create_conversation(
            &project_id,
            Some("Delivery recovery"),
            Some("Exercise ambiguous ACP delivery"),
        );
        let conversation_id = created["conversation"]["conversationID"]
            .as_str()
            .expect("conversationID")
            .to_string();
        let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
            .as_str()
            .expect("inputID")
            .to_string();
        core.start_conversation_run(&project_id, &conversation_id, "codex", &input_id);
        wait_for_input_status(
            &mut core,
            &workspace_id,
            &conversation_id,
            &input_id,
            "sending",
        );
        wait_for_prompt_call(&runtime, 1);
        core.kill_now();
        (project_id, workspace_id, conversation_id, input_id)
    };

    let calls_before_restart = runtime
        .request_log()
        .matches("\"method\":\"session/prompt\"")
        .count();
    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), env.clone());
    restarted.initialize();
    restarted.open_path(repo.path());
    assert_eq!(
        runtime
            .request_log()
            .matches("\"method\":\"session/prompt\"")
            .count(),
        calls_before_restart,
        "startup must not replay an ambiguous prompt: {}",
        runtime.request_log()
    );
    let recovered = wait_for_input_status(
        &mut restarted,
        &workspace_id,
        &conversation_id,
        &original_input_id,
        "deliveryUnknown",
    );
    assert_eq!(
        runtime
            .request_log()
            .matches("\"method\":\"session/prompt\"")
            .count(),
        calls_before_restart
    );
    let (blocked, _) = restarted.send_for_id(
        json!({
            "id": 722,
            "method": "conversation.startRun",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "engineID": "codex",
                "runInputID": recovered["runInputID"]
            }
        }),
        json!(722),
    );
    assert_error_data_kind(&blocked, "deliveryUnresolved");

    let (retried, _) = restarted.send_for_id(
        json!({
            "id": 723,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runInputID": original_input_id,
                "resolution": {"action": "retry", "idempotencyKey": "delivery-retry-key"}
            }
        }),
        json!(723),
    );
    assert_success_response(&retried, &json!(723));
    assert_eq!(retried["result"]["resolvedInput"]["status"], "superseded");
    let retry_input_id = retried["result"]["retryInput"]["runInputID"]
        .as_str()
        .expect("retry inputID")
        .to_string();
    runtime.set_next_prompt(FakePromptBehavior::NeverAcknowledge);
    restarted.start_conversation_run(&project_id, &conversation_id, "codex", &retry_input_id);
    wait_for_input_status(
        &mut restarted,
        &workspace_id,
        &conversation_id,
        &retry_input_id,
        "sending",
    );
    wait_for_prompt_call(&runtime, calls_before_restart + 1);
    restarted.kill_now();

    let mut recovered_again = CoreProcess::new_in_home_with_env(home.path(), env);
    recovered_again.initialize();
    recovered_again.open_path(repo.path());
    wait_for_input_status(
        &mut recovered_again,
        &workspace_id,
        &conversation_id,
        &retry_input_id,
        "deliveryUnknown",
    );
    let other_workspace = recovered_again.send(json!({
        "id": 725,
        "method": "workspace.create",
        "params": {"displayName": "Unrelated Workspace"}
    }));
    assert_success_response(&other_workspace, &json!(725));
    let other_workspace_id = other_workspace["result"]["workspace"]["workspaceID"]
        .as_str()
        .expect("other workspaceID")
        .to_string();
    let (wrong_workspace_resolution, _) = recovered_again.send_for_id(
        json!({
            "id": 726,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": other_workspace_id,
                "conversationID": conversation_id,
                "runInputID": retry_input_id,
                "resolution": {"action": "markDelivered"}
            }
        }),
        json!(726),
    );
    assert_error_data_kind(&wrong_workspace_resolution, "unknownConversationID");
    wait_for_input_status(
        &mut recovered_again,
        &workspace_id,
        &conversation_id,
        &retry_input_id,
        "deliveryUnknown",
    );
    let (marked, _) = recovered_again.send_for_id(
        json!({
            "id": 724,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runInputID": retry_input_id,
                "resolution": {"action": "markDelivered"}
            }
        }),
        json!(724),
    );
    assert_success_response(&marked, &json!(724));
    assert_eq!(marked["result"]["resolvedInput"]["status"], "delivered");
    assert_eq!(
        marked["result"]["resolvedInput"]["deliveryEvidence"],
        "userMarkedDelivered"
    );
    assert!(marked["result"]["retryInput"].is_null());
    recovered_again.shutdown();
}

/// An expired login is the one failure a user can fix without leaving the app,
/// so the message it rejected has to be re-sendable: sign in, press Send Again,
/// and the same message reaches the agent. Before this, a `failed` input was a
/// dead end and the only recourse was retyping it.
///
/// The order matters: the resend is refused until the sign-in. Allowed through,
/// each press would add a run, a failed input, and — since the adapter narrates
/// the failure as a message chunk — a "partial" assistant turn to the thread.
#[cfg(unix)]
#[test]
fn a_run_that_failed_for_want_of_credentials_can_be_sent_again() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "auth recovery\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AuthRequired);

    let mut core = CoreProcess::new_in_home_with_env(
        home.path(),
        runtime.environment_with_a_login("test-login"),
    );
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = core.create_conversation(
        &project_id,
        Some("Auth recovery"),
        Some("Exercise the expired-credential path"),
    );
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("inputID")
        .to_string();

    core.start_conversation_run(&project_id, &conversation_id, "codex", &input_id);
    let failed = wait_for_input_status(
        &mut core,
        &workspace_id,
        &conversation_id,
        &input_id,
        "failed",
    );
    wait_for_prompt_call(&runtime, 1);
    // The input is failed a moment before its run is finalized, and a retry is
    // refused while the conversation still has a run in flight.
    wait_for_no_active_run(&mut core, &workspace_id, &conversation_id);
    // The run carries why: the app reads the kind there to offer sign-in.
    let failed_run = core.wait_for_run_status(
        &workspace_id,
        &conversation_id,
        failed["targetRunID"].as_str().expect("the failed input's run"),
        &["failed"],
    );
    assert_eq!(failed_run["error"]["kind"], "authRequired");

    // Nothing was accepted, so there is no delivery for the user to attest to.
    let (refused, _) = core.send_for_id(
        json!({
            "id": 731,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runInputID": input_id,
                "resolution": {"action": "markDelivered"}
            }
        }),
        json!(731),
    );
    assert_error_data_kind(&refused, "inputTransitionInvalid");

    // The narrated failure is not a turn: the run's error carries the words,
    // and the thread holds nothing but the message that never went out.
    let detail = core.get_conversation(&project_id, &conversation_id);
    let roles: Vec<&str> = detail["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|message| message["role"].as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user"],
        "a failed login leaves no assistant turn"
    );

    // Sending again while still signed out would only fail the same way, so
    // the resend is refused outright and the failed input is left as it was.
    let (blocked, _) = core.send_for_id(
        json!({
            "id": 733,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runInputID": input_id,
                "resolution": {"action": "retry", "idempotencyKey": "auth-retry-too-early"}
            }
        }),
        json!(733),
    );
    assert_error_data_kind(&blocked, "authRequired");
    let still_failed = wait_for_input_status(
        &mut core,
        &workspace_id,
        &conversation_id,
        &input_id,
        "failed",
    );
    assert_eq!(still_failed["status"], "failed");

    let (signed_in, _) = core.send_for_id(
        json!({
            "id": 734,
            "method": "agent.signIn",
            "params": {"agentID": "codex", "methodID": "test-login"}
        }),
        json!(734),
    );
    assert_success_response(&signed_in, &json!(734));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = core.send(json!({
            "id": 735,
            "method": "agent.authState",
            "params": {"agentID": "codex"}
        }));
        if state["result"]["adapter"]["authState"] == "ready" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sign-in never completed: {state}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let (retried, _) = core.send_for_id(
        json!({
            "id": 732,
            "method": "conversation.resolveRunInputDelivery",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id,
                "runInputID": input_id,
                "resolution": {"action": "retry", "idempotencyKey": "auth-retry-key"}
            }
        }),
        json!(732),
    );
    assert_success_response(&retried, &json!(732));
    assert_eq!(retried["result"]["resolvedInput"]["status"], "superseded");
    let retry_input_id = retried["result"]["retryInput"]["runInputID"]
        .as_str()
        .expect("retry inputID")
        .to_string();
    assert_eq!(retried["result"]["retryInput"]["kind"], "retry");

    // The credentials are good this time, and the same message goes out again.
    core.start_conversation_run(&project_id, &conversation_id, "codex", &retry_input_id);
    wait_for_input_status(
        &mut core,
        &workspace_id,
        &conversation_id,
        &retry_input_id,
        "delivered",
    );
    wait_for_prompt_call(&runtime, 2);
    let detail = core.get_conversation(&project_id, &conversation_id);
    let messages = detail["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages");
    let user_messages = messages
        .iter()
        .filter(|message| message["role"] == "user")
        .count();
    assert_eq!(
        user_messages, 1,
        "a retry reuses the immutable Message rather than posting a second one"
    );
    core.shutdown();
}
