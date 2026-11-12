use crate::support::{
    assert_error_response, assert_success_response, CoreProcess, FakeAcpRuntime, GitRepoFixture,
};
use serde_json::json;
use std::time::{Duration, Instant};

/// Runs one message through `engine_id` in a new conversation and waits for
/// the run to complete: the adapter is started and has opened a session.
fn run_to_completion(core: &mut CoreProcess, project_id: &str, engine_id: &str, message: &str) {
    let created = core.create_conversation(project_id, None, Some(message));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("inputID")
        .to_string();
    let started = core.start_conversation_run(project_id, &conversation_id, engine_id, &input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    let workspace_id = core.workspace_id(project_id);
    core.wait_for_run_status(&workspace_id, &conversation_id, run_id, &["completed"]);
}

#[test]
fn managed_profiles_create_rename_delete_and_restart_preserve_siblings() {
    let home = tempfile::tempdir().unwrap();
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    for provider in ["codex", "claude", "opencode", "omp"] {
        let installation = home
            .path()
            .join("agents")
            .join(provider)
            .join("adapter")
            .join("sentinel");
        std::fs::create_dir_all(&installation).unwrap();
        std::fs::write(installation.join("keep"), b"installation").unwrap();
        let seed = home.path().join("agents").join(provider).join("home");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("sentinel"), b"original").unwrap();
        let created = core.send(json!({"id":900,"method":"agent.create","params":{"harnessID":provider,"layout":"managed","label":" Work "}}));
        assert_success_response(&created, &json!(900));
        let id = created["result"]["agentID"].as_str().unwrap().to_owned();
        let profile = home.path().join("agents").join(&id).join("home");
        // Nothing is copied from the seed home; OpenCode's profile holds only
        // its config directory with Ottam's service setting and its empty
        // data and state homes.
        let mut entries: Vec<_> = std::fs::read_dir(&profile)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        if provider == "opencode" {
            assert_eq!(
                entries,
                [
                    profile.join("config"),
                    profile.join("data"),
                    profile.join("state")
                ]
            );
            let listing = |path: &std::path::Path| -> Vec<_> {
                std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect()
            };
            assert_eq!(listing(&entries[0]), ["service.json"]);
            assert!(listing(&entries[1]).is_empty());
            assert!(listing(&entries[2]).is_empty());
            let service: serde_json::Value =
                serde_json::from_slice(&std::fs::read(entries[0].join("service.json")).unwrap())
                    .unwrap();
            assert_eq!(service, json!({"disabled": true}));
        } else {
            assert!(entries.is_empty(), "{entries:?}");
        }
        assert_eq!(std::fs::read(seed.join("sentinel")).unwrap(), b"original");
        let duplicate = core.send(json!({"id":901,"method":"agent.create","params":{"harnessID":provider,"layout":"managed","label":"work"}}));
        assert_error_response(&duplicate, &json!(901), -32000, "name");
        let rename = core.send(
            json!({"id":902,"method":"agent.rename","params":{"agentID":id,"label":"Office"}}),
        );
        assert_eq!(rename["result"]["agentID"], id);
        std::fs::write(profile.join("sentinel"), b"second").unwrap();
        let deleted = core.send(json!({"id":903,"method":"agent.delete","params":{"agentID":id,"purge":false,"operationKey":format!("delete-{provider}")}}));
        assert_success_response(&deleted, &json!(903));
        assert!(profile.join("sentinel").exists());
        assert!(installation.join("keep").exists());
        let removed =
            core.send(json!({"id":904,"method":"agent.authState","params":{"agentID":id}}));
        assert_error_response(&removed, &json!(904), -32000, "removed");
        let seed_deleted = core.send(json!({"id":905,"method":"agent.delete","params":{"agentID":provider,"purge":true,"operationKey":format!("seed-{provider}")}}));
        assert_success_response(&seed_deleted, &json!(905));
        assert!(!seed.exists());
        assert!(installation.join("keep").exists());
    }
    core.shutdown();
    let mut restarted = CoreProcess::new_in_home(home.path());
    restarted.initialize();
    let agents = restarted.send(json!({"id":906,"method":"agent.list","params":{}}));
    assert_eq!(agents["result"]["agents"], json!([]));
    let recreated = restarted.send(json!({"id":907,"method":"agent.create","params":{"harnessID":"codex","layout":"managed","label":"Office"}}));
    assert_success_response(&recreated, &json!(907));
    assert_ne!(recreated["result"]["agentID"], "codex");
    restarted.shutdown();
}

#[cfg(unix)]
#[test]
fn shared_home_shutdown_cancel_and_claiming_remain_observable_over_jsonl() {
    use std::os::unix::fs::PermissionsExt;
    let runtime = FakeAcpRuntime::new();
    let shared = tempfile::tempdir().unwrap();
    let shell_dir = tempfile::tempdir().unwrap();
    let shell = shell_dir.path().join("zsh");
    std::fs::write(&shell, "#!/bin/sh\nprintf '\\036OTTAM_SETUP\\000set\\000%s\\000\\000' \"$CLAUDE_CONFIG_DIR\"\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut environment = runtime.environment();
    environment.extend([
        ("SHELL".into(), shell.to_string_lossy().into_owned()),
        (
            "CLAUDE_CONFIG_DIR".into(),
            shared.path().to_string_lossy().into_owned(),
        ),
    ]);
    let ottam_home = tempfile::tempdir().unwrap();
    let mut core = CoreProcess::new_in_home_with_env(ottam_home.path(), environment.clone());
    core.initialize();
    let changed = core.send(json!({"id": 800, "method": "agent.setLayout", "params": {"agentID": "claude", "layout": "existing"}}));
    assert_success_response(&changed, &json!(800));
    let purge = core.send(json!({"id": 890, "method": "agent.delete", "params": {"agentID": "claude", "purge": true, "operationKey": "never-purge-cli"}}));
    assert_error_response(&purge, &json!(890), -32602, "CLI home");
    assert!(shared.path().exists());
    let lock = shared.path().join(".oauth_refresh.lock");
    std::fs::create_dir(&lock).unwrap();
    let idle = core.send(json!({"id": 801, "method": "agent.shutdownReadiness", "params": {}}));
    assert_eq!(idle["result"]["ready"], true);
    assert_eq!(idle["result"]["hasSharedHomes"], true);
    assert_eq!(idle["result"]["refreshLockPaths"], json!([]));
    std::fs::remove_dir(&lock).unwrap();

    let repo = GitRepoFixture::init();
    repo.write("README.md", "test\n");
    repo.commit_all("initial");
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap();
    let claude = |value: &serde_json::Value| {
        value["result"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["agentID"] == "claude")
            .unwrap()["canSetLayout"]
            .clone()
    };
    let before = core.send(json!({"id": 810, "method": "agent.list", "params": {}}));
    assert_eq!(claude(&before), true);
    run_to_completion(&mut core, project_id, "claude", "start the adapter");
    let after = core.send(json!({"id": 811, "method": "agent.list", "params": {}}));
    assert_eq!(
        claude(&after),
        false,
        "claiming must be read from storage, not the stale registry definition"
    );
    std::fs::create_dir(&lock).unwrap();
    core.write_message(
        json!({"id": 803, "method": "agent.prepareShutdown", "params": {"operationKey": "held"}}),
    );
    let waiting = core.send(json!({"id": 804, "method": "agent.shutdownReadiness", "params": {}}));
    assert_eq!(waiting["result"]["ready"], false);
    let cancelled = core.send(
        json!({"id": 805, "method": "agent.cancelShutdown", "params": {"operationKey": "held"}}),
    );
    assert_success_response(&cancelled, &json!(805));
    let auth =
        core.send(json!({"id": 806, "method": "agent.authState", "params": {"agentID": "claude"}}));
    assert_eq!(auth["result"]["adapter"]["lifecycle"], "ready");
    assert!(lock.exists(), "cancellation must preserve the shared lock");
    core.write_message(json!({"id":891,"method":"agent.delete","params":{"agentID":"claude","purge":false,"operationKey":"cancel-profile-delete"}}));
    let cancelled = core.send(json!({"id":892,"method":"agent.cancelShutdown","params":{"operationKey":"cancel-profile-delete"}}));
    assert_success_response(&cancelled, &json!(892));
    let retained =
        core.send(json!({"id":893,"method":"agent.authState","params":{"agentID":"claude"}}));
    assert_eq!(retained["result"]["adapter"]["lifecycle"], "ready");
    assert!(lock.exists());
    std::fs::remove_dir(&lock).unwrap();
    let prepared = core.send(json!({"id": 807, "method": "agent.prepareShutdown", "params": {"operationKey": "finished"}}));
    assert_success_response(&prepared, &json!(807));
    let cancelled = core.send(json!({"id": 808, "method": "agent.cancelShutdown", "params": {"operationKey": "finished"}}));
    assert_success_response(&cancelled, &json!(808));
    run_to_completion(&mut core, project_id, "claude", "still opens sessions");
    core.shutdown();

    // Represent models learned during a prior real session. An initialization
    // recheck cannot replenish these for a shared home, so it must retain them.
    use ottam_core::agent::{AgentId, AgentRegistry, ModelCatalogService};
    let catalog_path = ottam_home.path().join("model-catalog.json");
    let id = AgentId::new("claude");
    let package = AgentRegistry::default()
        .definition(&id)
        .unwrap()
        .adapter
        .unwrap();
    ModelCatalogService::load(Some(catalog_path.clone())).record(
        &id,
        vec![ottam_core::agent::AgentModelOption {
            model_id: "prior-session-model".into(),
            display_name: "Prior session model".into(),
        }],
        Some("prior-session-model".into()),
        Some(false),
        Some(format!("{}@{}", package.name, package.version)),
        None,
    );
    let mut core = CoreProcess::new_in_home_with_env(ottam_home.path(), environment);
    core.initialize();
    let refreshed = core.send(json!({"id": 812, "method": "agent.authState", "params": {"agentID": "claude", "refresh": true}}));
    assert_success_response(&refreshed, &json!(812));
    // Initialize observation persists auth/capability changes, but never erases
    // session models. Inspect the cache via its public API after graceful exit.
    core.shutdown();
    assert_eq!(
        ModelCatalogService::load(Some(catalog_path)).models_for(&id)[0].model_id,
        "prior-session-model"
    );
}

#[test]
fn passive_agent_queries_return_without_starting_adapters() {
    // `agent.list` probes every agent's runtime, cold and again on
    // `refreshRuntime`. The fake toolchain answers that probe at once, where
    // the real one costs a login shell and real `node`/`npx` per agent.
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();

    let listed = core.send(json!({
        "id": 700,
        "method": "agent.list",
        "params": {}
    }));
    assert_success_response(&listed, &json!(700));
    let agents = listed["result"]["agents"].as_array().expect("agents");
    // Registration order, not id order: clients take position as the
    // default-pick tiebreak.
    let ids: Vec<_> = agents.iter().map(|agent| &agent["agentID"]).collect();
    assert_eq!(ids, ["claude", "codex", "opencode", "omp"]);
    assert!(agents
        .iter()
        .all(|agent| agent["adapter"]["lifecycle"] == "stopped"));
    // The pinned package is statically known, so it is present even though no
    // adapter has run — the one version a cold Settings pane can show.
    let claude = agents
        .iter()
        .find(|agent| agent["agentID"] == "claude")
        .expect("claude agent");
    assert_eq!(
        claude["adapterPackage"]["name"],
        "@agentclientprotocol/claude-agent-acp"
    );
    assert!(claude["adapterPackage"]["version"]
        .as_str()
        .is_some_and(|version| !version.is_empty()));
    // A prebuilt binary needs no Node, so its runtime is ready untouched.
    let opencode = agents
        .iter()
        .find(|agent| agent["agentID"] == "opencode")
        .expect("opencode agent");
    assert_eq!(opencode["adapterPackage"]["source"], "binary");
    assert_eq!(opencode["adapterPackage"]["name"], "@opencode/cli");
    assert_eq!(opencode["runtime"]["availability"], "ready");
    assert_eq!(opencode["brand"], "anomaly");
    // Same product name in both list RPCs; the app-global window and the
    // workspace picker must not disagree about what "claude" is called.
    assert_eq!(claude["displayName"], "Claude Code");
    // Presentation comes from the definition, so the app needs no id switch.
    assert_eq!(claude["vendorName"], "Anthropic");
    assert_eq!(claude["vendorHome"], "~/.claude");
    assert_eq!(claude["brand"], "anthropic");
    assert_eq!(claude["accountLabel"], "Claude account");
    assert_eq!(claude["accountKind"], "subscription");
    // The compatibility matrix is on the wire even before a probe: SDK and
    // client columns are static, and the agent column is simply absent.
    let compatibility = claude["adapter"]["compatibility"]
        .as_array()
        .expect("compatibility rows");
    assert!(!compatibility.is_empty());
    assert!(compatibility.iter().all(|row| {
        row["feature"].is_string()
            && row["group"].is_string()
            && row["sdkStatus"].is_string()
            && row["clientSupport"].is_string()
            && row["verdict"].is_string()
    }));
    let load_session = compatibility
        .iter()
        .find(|row| row["feature"] == "loadSession")
        .expect("loadSession row");
    assert_eq!(load_session["clientSupport"], "supported");
    assert_eq!(load_session["verdict"], "unknown");
    assert!(load_session.get("agentAdvertises").is_none());

    let refreshed = core.send(json!({
        "id": 703,
        "method": "agent.list",
        "params": {"refreshRuntime": true}
    }));
    assert_success_response(&refreshed, &json!(703));

    let auth = core.send(json!({
        "id": 701,
        "method": "agent.authState",
        "params": {"agentID": "codex"}
    }));
    assert_success_response(&auth, &json!(701));
    assert_eq!(auth["result"]["adapter"]["lifecycle"], "stopped");
    assert_eq!(auth["result"]["adapter"]["authState"], "unknown");

    core.shutdown();
}

/// `agent.homeInfo` answers cold — the path is known before the home exists,
/// with a zero size — and refuses agents that have no managed home.
#[test]
fn agent_home_info_reports_the_path_and_size_before_the_home_exists() {
    let mut core = CoreProcess::new();
    core.initialize();

    let info = core.send(json!({
        "id": 720,
        "method": "agent.homeInfo",
        "params": {"agentID": "claude"}
    }));
    assert_success_response(&info, &json!(720));
    let path = info["result"]["path"].as_str().expect("path");
    assert!(
        path.ends_with("agents/claude/home"),
        "unexpected home path: {path}"
    );
    assert_eq!(info["result"]["sizeBytes"], 0);

    let unknown = core.send(json!({
        "id": 721,
        "method": "agent.homeInfo",
        "params": {"agentID": "unknown"}
    }));
    assert_eq!(unknown["error"]["code"], -32000);

    core.shutdown();
}

#[test]
fn agent_methods_reject_unknown_fields_and_unknown_agents() {
    let mut core = CoreProcess::new();
    core.initialize();

    let malformed = core.send(json!({
        "id": 710,
        "method": "agent.list",
        "params": {"unexpected": true}
    }));
    assert_eq!(malformed["error"]["code"], -32602);

    let unknown = core.send(json!({
        "id": 711,
        "method": "agent.authState",
        "params": {"agentID": "unknown"}
    }));
    assert_eq!(unknown["error"]["code"], -32000);
    assert_eq!(unknown["error"]["data"]["kind"], "agentNotFound");

    core.shutdown();
}

/// A browser login is open-ended — the adapter answers `authenticate` only once
/// the user finishes in the browser, and may never answer at all. Everything
/// else the adapter can do has to stay available meanwhile, or one abandoned
/// tab wedges the agent until the app quits.
#[cfg(unix)]
#[test]
fn a_login_awaiting_the_browser_does_not_stall_the_rest_of_the_adapter() {
    let repo = GitRepoFixture::init();
    repo.write("readme.md", "hello\n");
    repo.commit_all("initial");
    let runtime = FakeAcpRuntime::new();
    let ottam_home = tempfile::TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_env(
        ottam_home.path(),
        runtime.environment_with_a_login_that_never_returns("test-login"),
    );
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();

    // The sign-in answers at once; the adapter's login it starts never does.
    let (signed_in, _) = core.send_for_id(
        json!({
            "id": 720,
            "method": "agent.signIn",
            "params": {"agentID": "codex", "methodID": "test-login"}
        }),
        json!(720),
    );
    assert_success_response(&signed_in, &json!(720));
    let auth_session_id = signed_in["result"]["authSessionID"].clone();

    // Wait for the adapter to have actually picked the login up. Without this the
    // test races: a run that finished before the adapter ever dequeued the
    // login would prove nothing about whether the login blocks the adapter.
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the adapter never began authenticating"
        );
        let state = core.send(json!({
            "id": 730,
            "method": "agent.authState",
            "params": {"agentID": "codex"}
        }));
        if state["result"]["adapter"]["authState"] == "authenticating" {
            break;
        }
    }

    // Pressing the button again while the first login is still open joins it,
    // rather than opening a second browser tab.
    let (duplicate, _) = core.send_for_id(
        json!({
            "id": 722,
            "method": "agent.signIn",
            "params": {"agentID": "codex", "methodID": "test-login"}
        }),
        json!(722),
    );
    assert_success_response(&duplicate, &json!(722));
    assert_eq!(duplicate["result"]["authSessionID"], auth_session_id);

    run_to_completion(
        &mut core,
        &project_id,
        "codex",
        "work while the login waits",
    );

    // And the login really is still outstanding — otherwise the test would pass
    // for the uninteresting reason that it completed before the run did. A
    // finished run may mark auth ready, so ask the sign-in itself: another
    // press joins only a login that is still live, and a finished one would
    // have been retired.
    let (rejoined, _) = core.send_for_id(
        json!({
            "id": 731,
            "method": "agent.signIn",
            "params": {"agentID": "codex", "methodID": "test-login"}
        }),
        json!(731),
    );
    assert_success_response(&rejoined, &json!(731));
    assert_eq!(rejoined["result"]["authSessionID"], auth_session_id);

    core.shutdown();
}
