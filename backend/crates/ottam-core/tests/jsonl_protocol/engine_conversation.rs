use crate::support::{assert_error_data_kind, CoreProcess, GitRepoFixture};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;

fn engine<'a>(result: &'a serde_json::Value, engine_id: &str) -> &'a serde_json::Value {
    result["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == engine_id)
        .expect("engine")
}

#[test]
fn engine_list_returns_registered_harness_engines_for_open_project() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let engines = core.engine_list(project_id);
    // Same registration order as agent.list; the picker's default breaks
    // readiness ties by position.
    let engine_ids = engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .filter_map(|engine| engine["engineID"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(engine_ids, ["claude", "codex", "opencode", "omp"]);
    assert!(engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .all(|engine| engine["steeringCapability"] == "interruptThenPrompt"));
    assert!(!engines.to_string().contains("mock.chat"));

    let agents = core.send(json!({"id": 46, "method": "agent.list", "params": {}}));
    assert!(agents["result"]["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .all(|agent| {
            agent["adapter"]["lifecycle"] == "stopped"
                && agent["adapter"]["steeringCapability"] == "interruptThenPrompt"
        }));

    core.shutdown();
}

/// The launchd-PATH case: the app is started by Launch Services, so it never
/// sees the PATH a terminal has. `OTTAM_NODE` has to be enough on its own,
/// because for most users nothing else on the impoverished PATH is.
#[test]
fn an_explicit_node_override_survives_an_impoverished_path() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let fake_bin = TempDir::new().expect("fake runtime directory");
    for (name, version) in [("node", "v22.12.0"), ("npx", "10.9.0")] {
        let binary = fake_bin.path().join(name);
        std::fs::write(&binary, format!("#!/bin/sh\necho {version}\n")).expect("writes probe");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .expect("makes probe executable");
    }

    let mut core = CoreProcess::new_with_env(vec![
        // Exactly what launchd hands the app: no Homebrew, no version manager.
        (
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
        ),
        (
            "OTTAM_NODE".to_string(),
            fake_bin.path().join("node").display().to_string(),
        ),
    ]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let engines = core.engine_list(project_id);
    let claude = engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == "claude")
        .expect("claude engine");
    assert_eq!(claude["installState"], "ready");
    assert_eq!(claude["runtime"]["availability"], "ready");
    assert_eq!(claude["runtime"]["nodeVersion"], "v22.12.0");

    core.shutdown();
}

#[test]
fn explicit_engine_refresh_reprobes_a_cached_ready_runtime() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let fake_bin = TempDir::new().expect("fake runtime directory");
    let node = fake_bin.path().join("node");
    let npx = fake_bin.path().join("npx");
    // Both versions clear every engine floor (Claude's 22 is the strictest):
    // this test is about the runtime cache being re-probed on an explicit
    // refresh, so neither reading may be rejected for being too old.
    std::fs::write(&node, "#!/bin/sh\necho v22.0.0\n").expect("writes old node");
    std::fs::write(&npx, "#!/bin/sh\necho 10.9.0\n").expect("writes npx");
    for binary in [&node, &npx] {
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755))
            .expect("makes probe executable");
    }

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_NODE".to_string(),
        node.display().to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let initial = core.engine_list(project_id);
    assert_eq!(
        engine(&initial, "claude")["runtime"]["nodeVersion"],
        "v22.0.0"
    );
    std::fs::write(&node, "#!/bin/sh\necho v24.0.0\n").expect("updates node");
    let cached = core.engine_list(project_id);
    assert_eq!(
        engine(&cached, "claude")["runtime"]["nodeVersion"],
        "v22.0.0"
    );

    let refreshed = core.engine_list_with_runtime_refresh(project_id);
    assert_eq!(
        engine(&refreshed, "claude")["runtime"]["nodeVersion"],
        "v24.0.0"
    );

    core.shutdown();
}

/// `npx` is a `#!/usr/bin/env node` script, so running it by absolute path is
/// not enough — its shebang re-resolves `node` through PATH, which under
/// launchd finds nothing. Without the probe prepending the toolchain's bin
/// directory, a perfectly good npx reports as invalid.
#[test]
fn npx_resolves_node_through_its_shebang_under_an_impoverished_path() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let fake_bin = TempDir::new().expect("fake runtime directory");
    // Answers `node --version` for itself, and anything else as the npx script
    // it was handed — which is what really happens when npx's shebang runs.
    let node = fake_bin.path().join("node");
    std::fs::write(
        &node,
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo v22.12.0 ;;\n  *) echo 10.9.0 ;;\nesac\n",
    )
    .expect("writes node");
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755))
        .expect("makes node executable");
    // Exactly npx's real shape: it can only run if `node` is on PATH.
    let npx = fake_bin.path().join("npx");
    std::fs::write(&npx, "#!/usr/bin/env node\n").expect("writes npx");
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755))
        .expect("makes npx executable");

    let mut core = CoreProcess::new_with_env(vec![
        (
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
        ),
        ("OTTAM_NODE".to_string(), node.display().to_string()),
    ]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let engines = core.engine_list(project_id);
    let claude = engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == "claude")
        .expect("claude engine");
    // The fake node echoes the version for both probes, so npx answering at
    // all is the assertion: its shebang found node.
    assert_eq!(claude["runtime"]["availability"], "ready");
    assert_eq!(claude["installState"], "ready");

    core.shutdown();
}

/// Engines carry their own Node floor, taken from the `engines.node` of the
/// adapter each one launches. Node 20 is the version that proves they are read
/// separately: Claude's adapter demands 22, Codex's declares nothing, so one
/// runtime has to produce two verdicts. A single shared floor — whichever way
/// it was set — would get one of these two engines wrong.
#[test]
fn engine_node_floors_are_read_per_engine() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let fake_bin = TempDir::new().expect("fake runtime directory");
    let node = fake_bin.path().join("node");
    std::fs::write(
        &node,
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo v20.0.0 ;;\n  *) echo 10.9.0 ;;\nesac\n",
    )
    .expect("writes node");
    let npx = fake_bin.path().join("npx");
    std::fs::write(&npx, "#!/usr/bin/env node\n").expect("writes npx");
    for binary in [&node, &npx] {
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755))
            .expect("makes probe executable");
    }

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_NODE".to_string(),
        node.display().to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let engines = core.engine_list(project_id);

    let claude = engine(&engines, "claude");
    assert_eq!(claude["runtime"]["availability"], "unsupported");
    assert_eq!(claude["runtime"]["foundMajor"], 20);
    assert_eq!(claude["runtime"]["minimumMajor"], 22);
    assert_eq!(claude["installState"], "unsupported");

    // Same Node, same probe, different verdict.
    assert_eq!(
        engine(&engines, "codex")["runtime"]["availability"],
        "ready"
    );

    core.shutdown();
}

/// The inverse: an override that points at nothing must report honestly rather
/// than quietly searching past it and blaming the system Node.
#[test]
fn a_broken_node_override_reports_a_missing_runtime() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_NODE".to_string(),
        "/nonexistent/node".to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let engines = core.engine_list(project_id);
    let claude = engines["engines"]
        .as_array()
        .expect("engines")
        .iter()
        .find(|engine| engine["engineID"] == "claude")
        .expect("claude engine");
    assert_eq!(claude["installState"], "missing");
    assert_eq!(claude["runtime"]["availability"], "missing");
    assert_eq!(claude["runtime"]["component"], "node");

    core.shutdown();
}

#[test]
fn slow_engine_probe_does_not_block_unrelated_requests() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let fake_bin = TempDir::new().expect("fake runtime directory");
    let node = fake_bin.path().join("node");
    std::fs::write(&node, "#!/bin/sh\nsleep 30\n").expect("writes slow node probe");
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755))
        .expect("makes node probe executable");

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_NODE".to_string(),
        node.display().to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    core.write_message(json!({
        "id": 46,
        "method": "engine.list",
        "params": {"workspaceID": workspace_id}
    }));
    core.write_message(json!({
        "id": 47,
        "method": "conversation.list",
        "params": {"workspaceID": core.workspace_id(project_id)}
    }));

    let (response, _) = core.read_until_response_id(&json!(47), Duration::from_secs(1));
    assert_eq!(response["result"]["conversations"], json!([]));
    core.shutdown();
}

#[test]
fn conversation_create_list_and_get_persist_draft_message() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let conversation = core.create_conversation(
        project_id,
        Some("Chat engine routing"),
        Some("How should Chat routing work?"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    assert_eq!(
        conversation["latestMessagePage"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        conversation["latestRunPage"]["runs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let initial_input = &conversation["latestRunInputPage"]["runInputs"][0];
    assert!(initial_input["runInputID"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(initial_input["status"].as_str(), Some("pending"));
    assert_eq!(
        initial_input["idempotencyKey"].as_str(),
        Some("jsonl-test-create-message")
    );

    let listed = core.list_conversations(project_id);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(
        listed[0]["conversationID"].as_str(),
        Some(conversation_id.as_str())
    );

    let fetched = core.get_conversation(project_id, &conversation_id);
    assert_eq!(
        fetched["latestMessagePage"]["messages"][0]["body"].as_str(),
        Some("How should Chat routing work?")
    );

    core.shutdown();
}

#[test]
fn conversation_delete_removes_local_history() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let conversation = core.create_conversation(
        project_id,
        Some("Temporary conversation"),
        Some("Delete this later."),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");

    let deleted = core.send(json!({
        "id": 49,
        "method": "conversation.delete",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "conversationID": conversation_id
        }
    }));
    assert_eq!(
        deleted["result"]["conversationID"].as_str(),
        Some(conversation_id)
    );
    assert_eq!(deleted["result"]["harnessSessionCleanup"], json!([]));
    assert_eq!(core.list_conversations(project_id), json!([]));

    let missing = core.send(json!({
        "id": 50,
        "method": "conversation.get",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "conversationID": conversation_id
        }
    }));
    assert_error_data_kind(&missing, "unknownConversationID");
    core.shutdown();
}

#[test]
fn conversation_post_message_appends_user_message_and_metadata() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let conversation =
        core.create_conversation(project_id, Some("Chat UI"), Some("Initial prompt"));
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();

    let updated = core.post_conversation_message(project_id, &conversation_id, "Second prompt");

    assert_eq!(updated["message"]["body"].as_str(), Some("Second prompt"));
    assert_eq!(updated["message"]["role"].as_str(), Some("user"));
    assert_eq!(updated["input"]["status"].as_str(), Some("pending"));

    let fetched = core.get_conversation(project_id, &conversation_id);
    let messages = fetched["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages");
    assert_eq!(messages.len(), 2);
    // Message pages are newest-first.
    assert_eq!(messages[0]["body"].as_str(), Some("Second prompt"));

    core.shutdown();
}

#[test]
fn conversation_start_run_rejects_unsupported_engine() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let conversation = core.create_conversation(project_id, None, Some("Use bad engine."));
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");

    let response = core.send(json!({
        "id": 45,
        "method": "conversation.startRun",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "conversationID": conversation_id,
            "engineID": "codex.missing",
            "runInputID": "input-jsonl-test"
        }
    }));
    assert_error_data_kind(&response, "unsupportedConversationEngine");

    let missing_input = core.send(json!({
        "id": 451,
        "method": "conversation.startRun",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "conversationID": conversation_id,
            "engineID": "codex",
            "runInputID": "input-jsonl-test"
        }
    }));
    assert_error_data_kind(&missing_input, "unknownRunInputID");
    core.shutdown();
}

#[test]
fn conversation_records_survive_core_process_restart() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");
    let ottam_home = TempDir::new().expect("creates OTTAM_HOME");

    let (project_id, conversation_id) = {
        let mut core = CoreProcess::new_in_home(ottam_home.path());
        core.initialize();
        let opened = core.open_path(repo.path());
        let project_id = opened["project"]["projectID"]
            .as_str()
            .expect("projectID")
            .to_string();
        let conversation = core.create_conversation(
            &project_id,
            Some("Persistent conversation"),
            Some("Persist this conversation."),
        );
        let conversation_id = conversation["conversation"]["conversationID"]
            .as_str()
            .expect("conversationID")
            .to_string();
        core.shutdown();
        (project_id, conversation_id)
    };

    let mut core = CoreProcess::new_in_home(ottam_home.path());
    core.initialize();
    core.open_path(repo.path());
    let fetched = core.get_conversation(&project_id, &conversation_id);
    assert_eq!(
        fetched["conversation"]["title"].as_str(),
        Some("Persistent conversation")
    );
    assert_eq!(
        fetched["latestMessagePage"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    core.shutdown();
}

#[test]
fn conversation_get_unknown_id_returns_typed_error() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let response = core.send(json!({
        "id": 44,
        "method": "conversation.get",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "conversationID": "conversation_missing"
        }
    }));
    assert_error_data_kind(&response, "unknownConversationID");
    core.shutdown();
}
