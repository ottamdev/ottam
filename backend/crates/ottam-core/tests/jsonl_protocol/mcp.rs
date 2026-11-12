use crate::support::{assert_error_data_kind, assert_success_response, CoreProcess};
use serde_json::{json, Value};
use tempfile::TempDir;

fn stdio_server(name: &str, token: Option<&str>) -> Value {
    let mut token_value = json!({"name": "API_TOKEN", "secret": true});
    if let Some(token) = token {
        token_value["value"] = json!(token);
    }
    json!({
        "name": name,
        "transport": {
            "kind": "stdio",
            "command": "fixture-server",
            "args": ["--stdio"],
            "env": [{"name": "MODE", "value": "test"}, token_value],
        }
    })
}

fn list(core: &mut CoreProcess, id: u64) -> Vec<Value> {
    let listed = core.send(json!({"id": id, "method": "mcp.list", "params": {}}));
    assert_success_response(&listed, &json!(id));
    listed["result"]["servers"]
        .as_array()
        .expect("servers")
        .clone()
}

#[test]
fn mcp_servers_round_trip_and_survive_a_restart_without_their_secrets() {
    let home = TempDir::new().expect("creates OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let workspace = core.send(json!({
        "id": 2,
        "method": "workspace.create",
        "params": {"displayName": "MCP"}
    }));
    assert_success_response(&workspace, &json!(2));
    let workspace_id = workspace["result"]["workspace"]["workspaceID"].clone();

    let added = core.send(json!({
        "id": 3,
        "method": "mcp.add",
        "params": {"config": stdio_server("fixture", Some("s3cret"))}
    }));
    assert_success_response(&added, &json!(3));
    let server = &added["result"]["server"];
    let server_id = server["serverID"].clone();
    assert_eq!(server["state"]["status"], "notStarted");
    let env = &server["config"]["transport"]["env"];
    assert_eq!(
        env[0],
        json!({"name": "MODE", "value": "test", "secret": false})
    );
    assert_eq!(
        env[1],
        json!({"name": "API_TOKEN", "secret": true}),
        "a secret's value never comes back"
    );

    let taken = core.send(json!({
        "id": 4,
        "method": "mcp.add",
        "params": {"config": stdio_server("fixture", Some("other"))}
    }));
    assert_error_data_kind(&taken, "mcpServerNameTaken");
    let unnamed = core.send(json!({
        "id": 5,
        "method": "mcp.add",
        "params": {"config": stdio_server("has space", Some("x"))}
    }));
    assert_error_data_kind(&unnamed, "invalidMcpServer");
    let no_secret = core.send(json!({
        "id": 6,
        "method": "mcp.add",
        "params": {"config": stdio_server("second", None)}
    }));
    assert_error_data_kind(&no_secret, "invalidMcpServer");

    // Leaving the secret's value out keeps it; renaming is allowed.
    let updated = core.send(json!({
        "id": 7,
        "method": "mcp.update",
        "params": {"serverID": server_id, "config": stdio_server("renamed", None)}
    }));
    assert_success_response(&updated, &json!(7));
    assert_eq!(updated["result"]["server"]["config"]["name"], "renamed");

    let disabled = core.send(json!({
        "id": 8,
        "method": "mcp.setWorkspaceEnabled",
        "params": {"serverID": server_id, "workspaceID": workspace_id, "enabled": false}
    }));
    assert_success_response(&disabled, &json!(8));
    assert_eq!(
        disabled["result"]["server"]["disabledWorkspaceIDs"],
        json!([workspace_id])
    );
    let unknown_workspace = core.send(json!({
        "id": 9,
        "method": "mcp.setWorkspaceEnabled",
        "params": {"serverID": server_id, "workspaceID": "missing", "enabled": false}
    }));
    assert_error_data_kind(&unknown_workspace, "workspaceNotFound");
    core.shutdown();

    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let servers = list(&mut core, 10);
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["serverID"], server_id);
    assert_eq!(servers[0]["config"]["name"], "renamed");
    assert_eq!(servers[0]["disabledWorkspaceIDs"], json!([workspace_id]));
    let sqlite = std::fs::read(home.path().join("ottam.sqlite")).expect("reads database");
    assert!(
        !String::from_utf8_lossy(&sqlite).contains("s3cret"),
        "secret values stay out of SQLite"
    );

    let enabled = core.send(json!({
        "id": 11,
        "method": "mcp.setWorkspaceEnabled",
        "params": {"serverID": server_id, "workspaceID": workspace_id, "enabled": true}
    }));
    assert_success_response(&enabled, &json!(11));
    assert_eq!(
        enabled["result"]["server"]["disabledWorkspaceIDs"],
        json!([])
    );

    let removed = core.send(json!({
        "id": 12,
        "method": "mcp.remove",
        "params": {"serverID": server_id}
    }));
    assert_success_response(&removed, &json!(12));
    assert!(list(&mut core, 13).is_empty());
    let gone = core.send(json!({
        "id": 14,
        "method": "mcp.remove",
        "params": {"serverID": server_id}
    }));
    assert_error_data_kind(&gone, "mcpServerNotFound");
    core.shutdown();
}

/// The `mcpServers` each `method` request the fake agent was sent carried.
#[cfg(unix)]
fn sent_entries(runtime: &crate::support::FakeAcpRuntime, method: &str) -> Vec<Value> {
    runtime
        .request_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|request| request["method"] == method)
        .map(|request| request["params"]["mcpServers"].clone())
        .collect()
}

/// The entry named `name`, after checking the session was sent exactly
/// Ottam's own server and `name`.
#[cfg(unix)]
fn only_entry<'a>(entries: &'a Value, name: &str) -> &'a Value {
    let entries = entries.as_array().expect("entries");
    let mut names: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    names.sort_unstable();
    let mut expected = vec!["ottam", name];
    expected.sort_unstable();
    assert_eq!(names, expected, "Ottam's own server and {name}");
    entries
        .iter()
        .find(|entry| entry["name"] == name)
        .expect("the named entry")
}

#[cfg(unix)]
#[test]
fn sessions_are_sent_their_workspaces_servers_through_the_proxy() {
    use crate::support::{FakeAcpRuntime, GitRepoFixture};
    use std::time::{Duration, Instant};

    let repo = GitRepoFixture::init();
    repo.write("README.md", "mcp\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let remote = |name: &str| json!({"name": name, "transport": {"kind": "http", "url": "http://127.0.0.1:9/mcp", "headers": []}});
    for (id, name) in [(2, "docs"), (3, "off")] {
        let added =
            core.send(json!({"id": id, "method": "mcp.add", "params": {"config": remote(name)}}));
        assert_success_response(&added, &json!(id));
        if name == "off" {
            let server_id = added["result"]["server"]["serverID"].clone();
            let disabled = core.send(json!({
                "id": 4,
                "method": "mcp.setWorkspaceEnabled",
                "params": {"serverID": server_id, "workspaceID": workspace_id, "enabled": false}
            }));
            assert_success_response(&disabled, &json!(4));
        }
    }

    for (sessions, engine) in [(1, "codex"), (2, "claude")] {
        let created = core.create_conversation(&project_id, None, Some("Use the docs"));
        let conversation_id = created["conversation"]["conversationID"].as_str().unwrap();
        let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
            .as_str()
            .unwrap();
        core.start_conversation_run(&project_id, conversation_id, engine, input_id);
        let deadline = Instant::now() + Duration::from_secs(10);
        while sent_entries(&runtime, "session/new").len() < sessions {
            assert!(Instant::now() < deadline, "{engine} never opened a session");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let sent = sent_entries(&runtime, "session/new");
    let mut tokens = Vec::new();
    for entries in &sent {
        let entry = only_entry(entries, "docs");
        assert_eq!(entry["type"], "http");
        let url = entry["url"].as_str().unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp/docs"),
            "the proxy's path, not the server's own URL: {url}"
        );
        assert_eq!(entry["headers"][0]["name"], "Authorization");
        let token = entry["headers"][0]["value"].as_str().unwrap();
        assert!(token.starts_with("Bearer "));
        tokens.push(token.to_owned());
    }
    assert_ne!(tokens[0], tokens[1], "one token per session");
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_session_loaded_after_a_restart_is_sent_its_servers_again() {
    use crate::support::{FakeAcpRuntime, GitRepoFixture};
    use std::time::{Duration, Instant};

    let repo = GitRepoFixture::init();
    repo.write("README.md", "mcp\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    let run_status = |core: &mut CoreProcess, workspace: &str, conversation: &str, run: &str| {
        let response = core.send(json!({
            "id": 6,
            "method": "conversation.get",
            "params": {"workspaceID": workspace, "conversationID": conversation}
        }));
        assert_success_response(&response, &json!(6));
        response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|entry| entry["runID"] == run))
            .and_then(|entry| entry["status"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let until_completed =
        |core: &mut CoreProcess, workspace: &str, conversation: &str, run: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while run_status(core, workspace, conversation, run) != "completed" {
                assert!(Instant::now() < deadline, "run {run} never completed");
                std::thread::sleep(Duration::from_millis(20));
            }
        };

    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let added = core.send(json!({"id": 2, "method": "mcp.add", "params": {"config": {
        "name": "docs",
        "transport": {"kind": "http", "url": "http://127.0.0.1:9/mcp", "headers": []}
    }}}));
    assert_success_response(&added, &json!(2));
    let created = core.create_conversation(&project_id, None, Some("Use the docs"));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .unwrap();
    let started = core.start_conversation_run(&project_id, &conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().unwrap().to_string();
    until_completed(&mut core, &workspace_id, &conversation_id, &run_id);
    core.shutdown();

    let mut restarted = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    restarted.initialize();
    restarted.open_path(repo.path());
    let posted = restarted.post_conversation_message(&project_id, &conversation_id, "And now?");
    let input_id = posted["input"]["runInputID"].as_str().expect("input");
    let started =
        restarted.start_conversation_run(&project_id, &conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().unwrap().to_string();
    until_completed(&mut restarted, &workspace_id, &conversation_id, &run_id);

    let first = sent_entries(&runtime, "session/new");
    let loaded = sent_entries(&runtime, "session/load");
    assert_eq!(first.len(), 1, "the restarted core loaded the session");
    let [entries] = loaded.as_slice() else {
        panic!("one load after the restart: {loaded:?}");
    };
    let entry = only_entry(entries, "docs");
    assert_ne!(
        entry["headers"][0]["value"],
        only_entry(&first[0], "docs")["headers"][0]["value"],
        "the restarted core's own token"
    );
    restarted.shutdown();
}

#[test]
fn a_repositorys_mcp_json_is_previewed_for_import() {
    let home = TempDir::new().expect("creates OTTAM_HOME");
    let repo = TempDir::new().expect("creates repository");
    std::fs::write(
        repo.path().join(".mcp.json"),
        json!({"mcpServers": {
            "docs": {"command": "npx", "args": ["-y", "docs-mcp"], "env": {"API_TOKEN": "t0ken"}},
            "search": {
                "type": "http",
                "url": "https://search.example.com/mcp",
                "headers": {"Authorization": "Bearer ${SEARCH_TOKEN}"}
            },
            "legacy": {"type": "sse", "url": "https://legacy.example.com/sse"},
        }})
        .to_string(),
    )
    .expect("writes .mcp.json");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let added = core.send(json!({
        "id": 2,
        "method": "mcp.add",
        "params": {"config": stdio_server("docs", Some("s3cret"))}
    }));
    assert_success_response(&added, &json!(2));
    let docs_id = added["result"]["server"]["serverID"].clone();

    let previewed = core.send(json!({
        "id": 3,
        "method": "mcp.importCandidates",
        "params": {"source": {"kind": "file", "path": repo.path()}}
    }));
    assert_success_response(&previewed, &json!(3));
    let result = &previewed["result"];
    assert!(result["path"]
        .as_str()
        .expect("path")
        .ends_with(".mcp.json"));
    let candidates = result["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["config"]["name"], "docs");
    assert_eq!(candidates[0]["existingServerID"], docs_id);
    assert_eq!(
        candidates[0]["config"]["transport"]["env"],
        json!([{"name": "API_TOKEN", "value": "t0ken", "secret": true}]),
        "the value travels with the preview, so adding it needs no second read"
    );
    assert_eq!(candidates[1]["config"]["name"], "search");
    assert!(candidates[1].get("existingServerID").is_none());
    assert_eq!(
        candidates[1]["config"]["transport"]["headers"][0]["secret"],
        true
    );
    assert_eq!(candidates[1]["notes"].as_array().expect("notes").len(), 1);
    assert_eq!(result["skipped"][0]["name"], "legacy");
    assert_eq!(list(&mut core, 4).len(), 1, "a preview adds nothing");

    let missing = core.send(json!({
        "id": 5,
        "method": "mcp.importCandidates",
        "params": {"source": {"kind": "file", "path": home.path().join("missing")}}
    }));
    assert_error_data_kind(&missing, "mcpImportUnreadable");
    core.shutdown();
}

#[test]
fn only_remote_servers_sign_in_and_every_change_is_announced() {
    let home = TempDir::new().expect("creates OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let (added, notices) = core.send_for_id(
        json!({
            "id": 2,
            "method": "mcp.add",
            "params": {"config": stdio_server("fixture", Some("s3cret"))}
        }),
        json!(2),
    );
    assert_success_response(&added, &json!(2));
    let server_id = added["result"]["server"]["serverID"].clone();
    assert_eq!(added["result"]["server"]["signedIn"], false);
    assert!(
        notices
            .iter()
            .any(|notice| notice["method"] == "mcp.serversChanged"),
        "{notices:?}"
    );

    let listed = core.send(json!({"id": 3, "method": "mcp.list", "params": {}}));
    assert_success_response(&listed, &json!(3));
    let redirect = listed["result"]["redirectURI"]
        .as_str()
        .expect("a redirect URI");
    assert!(
        redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/oauth/callback"),
        "{redirect}"
    );

    let local = core.send(json!({
        "id": 4,
        "method": "mcp.signIn",
        "params": {"serverID": server_id}
    }));
    assert_error_data_kind(&local, "mcpServerNotRemote");
    let missing = core.send(json!({
        "id": 5,
        "method": "mcp.signOut",
        "params": {"serverID": "missing"}
    }));
    assert_error_data_kind(&missing, "mcpServerNotFound");
    let cancelled = core.send(json!({
        "id": 6,
        "method": "mcp.cancelSignIn",
        "params": {"serverID": server_id}
    }));
    assert_success_response(&cancelled, &json!(6));
    core.shutdown();
}

/// The OAuth fixture, approving every sign-in at once, and its `/mcp` URL.
#[cfg(unix)]
struct OAuthFixture {
    child: std::process::Child,
    url: String,
}

#[cfg(unix)]
impl OAuthFixture {
    fn start() -> Self {
        use std::io::BufRead;
        let binary = std::env::var_os("OTTAM_TEST_MCP_BINARY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/debug/ottam-test-mcp")
            });
        assert!(binary.is_file(), "build ottam-test-mcp first");
        let mut child = std::process::Command::new(binary)
            .args(["--http", "--oauth", "--auto-approve"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("starts the OAuth fixture");
        let mut url = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut url)
            .expect("prints its URL");
        Self {
            child,
            url: url.trim().to_owned(),
        }
    }

    /// What revoking the app at the provider does: every token stops working.
    fn revoke(&self) {
        let base = self.url.trim_end_matches("/mcp").to_owned();
        block_on(async move {
            reqwest::Client::new()
                .post(format!("{base}/admin/revoke"))
                .send()
                .await
                .expect("revokes")
        });
    }
}

#[cfg(unix)]
impl Drop for OAuthFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
fn block_on<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(work)
}

/// Sign in as the app does: start it, then do what the browser does with
/// the authorization URL, and wait until core reads the server as signed in.
#[cfg(unix)]
fn sign_in(core: &mut CoreProcess, server_id: &Value, id: u64) {
    use std::time::{Duration, Instant};
    let started =
        core.send(json!({"id": id, "method": "mcp.signIn", "params": {"serverID": server_id}}));
    assert_success_response(&started, &json!(id));
    let url = started["result"]["authorizationURL"]
        .as_str()
        .expect("an authorization URL")
        .to_owned();
    let page = block_on(async move {
        reqwest::get(url)
            .await
            .expect("the browser follows the sign-in back to core")
            .text()
            .await
            .expect("reads the page")
    });
    assert!(page.contains("Signed in"), "{page}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let server = list(core, id + 1)
            .into_iter()
            .find(|server| &server["serverID"] == server_id)
            .expect("the server");
        if server["signedIn"] == true && server["state"]["status"] != "signedOut" {
            return;
        }
        assert!(Instant::now() < deadline, "never signed in: {server}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Post `message` to the conversation under its own idempotency key; the
/// new input's ID.
#[cfg(unix)]
fn post(core: &mut CoreProcess, project_id: &str, conversation_id: &str, message: &str) -> String {
    let workspace_id = core.workspace_id(project_id);
    let posted = core.send(json!({
        "id": 903,
        "method": "conversation.postMessage",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "message": message,
            "idempotencyKey": message
        }
    }));
    assert_success_response(&posted, &json!(903));
    posted["result"]["input"]["runInputID"]
        .as_str()
        .expect("the new input")
        .to_owned()
}

/// Start a run for `input_id` and wait for it to end: the agent's reply,
/// and every notification read meanwhile.
#[cfg(unix)]
fn run_turn(
    core: &mut CoreProcess,
    project_id: &str,
    conversation_id: &str,
    input_id: &str,
) -> (String, Vec<Value>) {
    use std::time::{Duration, Instant};
    let workspace_id = core.workspace_id(project_id);
    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    loop {
        let (response, notifications) = core.send_for_id(
            json!({
                "id": 901,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(901),
        );
        assert_success_response(&response, &json!(901));
        seen.extend(notifications);
        let conversation = &response["result"]["conversation"];
        let status = conversation["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
            .map(|run| run["status"].clone());
        if status == Some(json!("completed")) {
            // Transcript pages are newest-first.
            let transcript = core.send(json!({
                "id": 902,
                "method": "conversation.listTranscript",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }));
            assert_success_response(&transcript, &json!(902));
            let reply = transcript["result"]["items"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["kind"] == "assistantText"))
                .and_then(|item| item["bodyPreview"].as_str())
                .unwrap_or_default()
                .to_owned();
            return (reply, seen);
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not complete; last status {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Walkthrough step 5 end to end, through a real adapter process: a chat
/// calls a signed-in remote server through the proxy; once the provider
/// revokes the app, the next call fails saying to sign in and core asks the
/// app for that chat's Sign in card; signing in again lets the same chat's
/// next call succeed.
#[cfg(unix)]
#[test]
fn a_chat_calls_a_signed_in_server_and_a_revoked_sign_in_asks_that_chat_to_sign_in() {
    use crate::support::{FakeAcpRuntime, GitRepoFixture};

    let fixture = OAuthFixture::start();
    let repo = GitRepoFixture::init();
    repo.write("README.md", "mcp\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();

    let added = core.send(json!({
        "id": 2,
        "method": "mcp.add",
        "params": {"config": {
            "name": "linear",
            "transport": {"kind": "http", "url": fixture.url, "headers": []}
        }}
    }));
    assert_success_response(&added, &json!(2));
    let server_id = added["result"]["server"]["serverID"].clone();
    assert_eq!(added["result"]["server"]["state"]["status"], "signedOut");
    sign_in(&mut core, &server_id, 10);

    runtime.set_next_mcp_call("linear", "echo", json!({"text": "hello"}));
    let created = core.create_conversation(&project_id, None, Some("Echo hello"));
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let input_id = created["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .unwrap()
        .to_string();
    let (reply, _) = run_turn(&mut core, &project_id, &conversation_id, &input_id);
    assert_eq!(reply, "MCP result: hello");

    fixture.revoke();
    runtime.set_next_mcp_call("linear", "echo", json!({"text": "again"}));
    let input_id = post(&mut core, &project_id, &conversation_id, "Echo again");
    let (reply, notifications) = run_turn(&mut core, &project_id, &conversation_id, &input_id);
    assert!(
        reply.starts_with("MCP error:") && reply.contains("needs the user to sign in"),
        "{reply}"
    );
    let needed = notifications
        .iter()
        .find(|notice| notice["method"] == "mcp.signInNeeded")
        .unwrap_or_else(|| panic!("no Sign in card asked for: {notifications:?}"));
    assert_eq!(needed["params"]["serverID"], server_id);
    assert_eq!(needed["params"]["serverName"], "linear");
    assert_eq!(needed["params"]["conversationID"], conversation_id);
    let server = list(&mut core, 20)
        .into_iter()
        .find(|server| server["serverID"] == server_id)
        .expect("the server");
    assert_eq!(server["state"]["status"], "signedOut");

    sign_in(&mut core, &server_id, 30);
    runtime.set_next_mcp_call("linear", "echo", json!({"text": "signed in"}));
    let input_id = post(&mut core, &project_id, &conversation_id, "Echo once more");
    let (reply, _) = run_turn(&mut core, &project_id, &conversation_id, &input_id);
    assert_eq!(reply, "MCP result: signed in");
    core.shutdown();
}
