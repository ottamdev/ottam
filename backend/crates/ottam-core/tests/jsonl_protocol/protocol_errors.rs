use crate::support::{assert_error_response, assert_success_response, read_log_text, CoreProcess};
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn malformed_json_returns_parse_error_and_connection_remains_usable() {
    let mut core = CoreProcess::new();
    core.write_raw_line("{not json");
    let response = core
        .read_response_timeout(Duration::from_secs(3))
        .expect("parse error response");
    assert_error_response(&response, &json!(null), -32700, "parse error");

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
}

#[test]
fn oversize_request_is_rejected_and_core_keeps_serving() {
    let mut core = CoreProcess::new();
    let oversize = format!(
        "{{\"id\":7,\"method\":\"terminal.write\",\"params\":{{\"data\":\"{}\"}}}}",
        "x".repeat(2 * 1024 * 1024)
    );
    core.write_raw_line(&oversize);
    let response = core
        .read_response_timeout(Duration::from_secs(3))
        .expect("oversize error response");
    assert_error_response(&response, &json!(null), -32001, "exceeds");

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
}

/// The app's close shuts the read end of core's stdout, then stdin. A response
/// still in flight then hits a broken pipe. That is the peer leaving, not a
/// server error: core shuts down gracefully and exits 0 without an error log.
#[cfg(unix)]
#[test]
fn client_closing_stdout_is_a_clean_shutdown() {
    use std::io::{BufRead, BufReader, Read, Write};

    let home = TempDir::new().expect("creates OTTAM_HOME");
    let log_path = home.path().join("core-close.log");
    let mut child = crate::support::core_command(home.path())
        .env("OTTAM_CORE_LOG_FILE", &log_path)
        .spawn()
        .expect("spawns ottam-core");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let mut stderr = child.stderr.take().expect("stderr is piped");

    writeln!(stdin, "{{not json").expect("writes request");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("reads parse error");
    assert!(line.contains("-32700"), "core answered: {line}");

    // Close the read end first, so the next response has nowhere to go.
    drop(stdout);
    writeln!(stdin, "{{not json").expect("writes request");
    drop(stdin);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("polls ottam-core") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!("core still running 5s after the client closed");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr_text = String::new();
    let _ = stderr.read_to_string(&mut stderr_text);
    let log_text = read_log_text(&log_path);
    assert!(
        status.success(),
        "core exited with {status}\nstderr:\n{stderr_text}\nlog:\n{log_text}"
    );
    for text in [&stderr_text, &log_text] {
        assert!(!text.contains("Broken pipe"), "broken pipe logged:\n{text}");
        assert!(!text.contains("ERROR"), "error logged:\n{text}");
    }
}

#[test]
fn backend_writes_trace_log_without_corrupting_protocol_stdout() {
    let temp_dir = TempDir::new().expect("creates log tempdir");
    let log_path = temp_dir.path().join("core-test.log");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_CORE_LOG_FILE".to_string(),
        log_path.to_string_lossy().into_owned(),
    )]);

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
    core.shutdown();

    let log_text = read_log_text(&log_path);
    assert!(log_text.contains("ottam-core logging initialized"));
    assert!(log_text.contains("initialize accepted"));
}

#[test]
fn malformed_json_is_logged_without_raw_payload() {
    let temp_dir = TempDir::new().expect("creates log tempdir");
    let log_path = temp_dir.path().join("core-malformed.log");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_CORE_LOG_FILE".to_string(),
        log_path.to_string_lossy().into_owned(),
    )]);

    core.write_raw_line("{not json with secret payload");
    let response = core
        .read_response_timeout(Duration::from_secs(3))
        .expect("parse error response");
    assert_error_response(&response, &json!(null), -32700, "parse error");
    core.shutdown();

    let log_text = read_log_text(&log_path);
    assert!(log_text.contains("malformed JSONL request rejected"));
    assert!(!log_text.contains("secret payload"));
}

#[test]
fn invalid_log_filter_falls_back_without_corrupting_stdout() {
    let temp_dir = TempDir::new().expect("creates log tempdir");
    let log_path = temp_dir.path().join("core-invalid-filter.log");
    let mut core = CoreProcess::new_with_env(vec![
        (
            "OTTAM_CORE_LOG_FILE".to_string(),
            log_path.to_string_lossy().into_owned(),
        ),
        (
            "OTTAM_LOG".to_string(),
            "not a valid filter[".to_string(),
        ),
    ]);

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
    core.shutdown();

    let log_text = read_log_text(&log_path);
    assert!(log_text.contains("invalid log filter; using quiet default"));
    assert!(log_text.contains("initialize accepted"));
}

#[test]
fn input_close_is_logged_without_stdout_noise() {
    let temp_dir = TempDir::new().expect("creates log tempdir");
    let log_path = temp_dir.path().join("core-eof.log");
    let core = CoreProcess::new_with_env(vec![(
        "OTTAM_CORE_LOG_FILE".to_string(),
        log_path.to_string_lossy().into_owned(),
    )]);

    core.shutdown();

    let log_text = read_log_text(&log_path);
    assert!(log_text.contains("JSONL input closed; stdio server exiting"));
}

#[test]
fn invalid_request_envelopes_preserve_valid_ids() {
    let mut core = CoreProcess::new();
    let numeric = core.send(json!({
        "id": 42,
        "params": {}
    }));
    assert_error_response(&numeric, &json!(42), -32600, "invalid request");

    let string = core.send(json!({
        "id": "bad-envelope",
        "method": 99,
        "params": {}
    }));
    assert_error_response(&string, &json!("bad-envelope"), -32600, "invalid request");

    let null_id = core.send(json!({
        "id": null,
        "method": 99,
        "params": {}
    }));
    assert_error_response(&null_id, &json!(null), -32600, "invalid request");
}

#[test]
fn invalid_params_preserve_the_contract_across_dispatch_shapes() {
    let temp_dir = TempDir::new().expect("creates log tempdir");
    let log_path = temp_dir.path().join("core-invalid-params.log");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_CORE_LOG_FILE".to_string(),
        log_path.to_string_lossy().into_owned(),
    )]);
    let raw_params = "__decoder_secret_payload__";
    let cases = [
        (
            "immediate handler",
            json!("bad-init"),
            "initialize",
            json!({
                "clientInfo": false,
                "capabilities": {},
                "secretPayload": raw_params,
            }),
        ),
        (
            "workspace decoder",
            json!(202),
            "workspace.list",
            json!(raw_params),
        ),
        (
            "optional deferred-capable handler",
            json!("bad-engine-list"),
            "engine.list",
            json!({
                "workspaceID": raw_params,
                "refreshRuntime": "not-a-boolean",
            }),
        ),
        (
            "agent dispatch",
            json!(404),
            "agent.list",
            json!(raw_params),
        ),
    ];

    for (shape, id, method, params) in cases {
        let response = core.send(json!({
            "id": id,
            "method": method,
            "params": params,
        }));
        assert_error_response(&response, &id, -32602, "invalid params: ");
        assert!(
            response.get("jsonrpc").is_none(),
            "{shape} changed the existing response envelope: {response}"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("invalid params: ")),
            "{shape} changed the invalid-params message prefix: {response}"
        );
        assert!(
            response["error"].get("data").is_none(),
            "{shape} added structured data to the decode error: {response}"
        );
    }

    let initialize = core.initialize();
    assert_success_response(&initialize, &json!(1));
    core.shutdown();

    let log_text = read_log_text(&log_path);
    let warnings = log_text
        .lines()
        .filter(|line| line.contains("rejected invalid params"))
        .collect::<Vec<_>>();
    assert_eq!(
        warnings.len(),
        2,
        "only former handler-macro paths should warn: {warnings:?}"
    );
    for method in ["initialize", "engine.list"] {
        assert!(
            warnings.iter().any(|line| line.contains(method)),
            "missing method-aware warning for {method}: {warnings:?}"
        );
    }
    for method in ["workspace.list", "agent.list"] {
        assert!(
            warnings.iter().all(|line| !line.contains(method)),
            "previously silent decoder gained a warning for {method}: {warnings:?}"
        );
    }
    assert!(
        !log_text.contains(raw_params),
        "invalid-params logging included the raw payload"
    );
}

#[test]
fn unknown_method_returns_method_not_found() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 99,
        "method": "project.nope",
        "params": {}
    }));
    assert_error_response(&response, &json!(99), -32601, "method not found");
}

#[test]
fn removed_exploration_method_returns_method_not_found() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": "legacy-exploration",
        "method": "exploration.list",
        "params": {"projectID": "project_legacy"}
    }));
    assert_error_response(
        &response,
        &json!("legacy-exploration"),
        -32601,
        "method not found",
    );
}

#[test]
fn notification_without_id_produces_no_response() {
    let mut core = CoreProcess::new();
    core.write_notification(json!({
        "method": "initialized",
        "params": {}
    }));
    core.expect_no_response(Duration::from_millis(250));
}

#[test]
fn initialized_with_id_is_rejected_as_invalid_request() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": "not-a-notification",
        "method": "initialized",
        "params": {}
    }));
    assert_error_response(
        &response,
        &json!("not-a-notification"),
        -32601,
        "method not found",
    );
}

#[test]
fn oversize_jsonl_message_returns_error_and_closes_connection() {
    let mut core = CoreProcess::new();
    let oversize = " ".repeat(1_048_577);
    core.write_raw_line(&oversize);
    let response = core
        .read_response_timeout(Duration::from_secs(3))
        .expect("oversize response");
    assert_error_response(
        &response,
        &json!(null),
        -32001,
        "JSONL message exceeds 1048576 bytes",
    );
    core.shutdown();
}
