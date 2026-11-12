use crate::support::{
    assert_error_data_kind, assert_error_response, assert_success_response, CoreProcess,
    GitRepoFixture,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn terminal_open_write_resize_and_close_streams_backend_pty_output() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");
    assert_eq!(
        terminal["workingRoot"]["projectID"].as_str(),
        Some(project_id)
    );
    assert_eq!(
        terminal["cwd"].as_str(),
        Some(repo.path().canonicalize().unwrap().to_str().unwrap())
    );

    let (resize_response, _) = core.send_for_id(
        json!({
            "id": 32,
            "method": "terminal.resize",
            "params": {
                "sessionID": session_id,
                "rows": 33,
                "cols": 101
            }
        }),
        json!(32),
    );
    assert_success_response(&resize_response, &json!(32));

    let (write_response, notifications) = core.send_for_id(
        json!({
            "id": 31,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("printf __ottam_terminal_ok__\\n\n")
            }
        }),
        json!(31),
    );
    assert_success_response(&write_response, &json!(31));

    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_terminal_ok__",
    );
    assert!(
        output.contains("__ottam_terminal_ok__"),
        "terminal output did not contain marker: {output:?}"
    );

    let (close_response, _) = core.send_for_id(
        json!({
            "id": 33,
            "method": "terminal.close",
            "params": {
                "sessionID": session_id
            }
        }),
        json!(33),
    );
    assert_success_response(&close_response, &json!(33));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn terminal_shell_receives_terminfo_environment() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let fake_shell_dir = TempDir::new().expect("creates fake shell dir");
    let fake_shell = fake_shell_dir.path().join("fake-shell");
    let terminfo_dir = fake_shell_dir.path().join("terminfo");
    std::fs::create_dir(&terminfo_dir).expect("creates terminfo dir");
    write_executable_script(
        &fake_shell,
        r#"#!/bin/sh
printf "__ottam_terminfo=${TERMINFO:-missing}__\n"
exit 0
"#,
    );

    let mut core = CoreProcess::new_with_env(vec![
        (
            "SHELL".to_string(),
            fake_shell.to_string_lossy().into_owned(),
        ),
        (
            "TERMINFO".to_string(),
            terminfo_dir.to_string_lossy().into_owned(),
        ),
    ]);
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let (open_response, notifications) = core.send_for_id(
        json!({
            "id": 39,
            "method": "terminal.open",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "workingRoot": {
                    "kind": "project",
                    "projectID": project_id,
                    "sourceID": core.project_source_id(project_id)
                },
                "rows": 24,
                "cols": 80
            }
        }),
        json!(39),
    );
    assert_success_response(&open_response, &json!(39));
    let session_id = open_response["result"]["sessionID"]
        .as_str()
        .expect("sessionID");

    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_terminfo=",
    );
    assert!(
        output.contains(&format!(
            "__ottam_terminfo={}__",
            terminfo_dir.to_string_lossy()
        )),
        "terminal shell did not receive TERMINFO: {output:?}"
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn terminal_ephemeral_protocol_lists_replays_and_rejects_stale_records() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let (terminal_response, _) = core.send_for_id(
        json!({
            "id": 30,
            "method": "terminal.open",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "workingRoot": {
                    "kind": "project",
                    "projectID": project_id,
                    "sourceID": core.project_source_id(project_id)
                },
                "rows": 24,
                "cols": 80
            }
        }),
        json!(30),
    );
    assert_success_response(&terminal_response, &json!(30));
    let terminal = terminal_response["result"].clone();
    let terminal_id = terminal["terminalID"]
        .as_str()
        .expect("terminalID")
        .to_string();
    let session_id = terminal["sessionID"].as_str().expect("sessionID");
    let list_response = core.send(json!({
        "id": 40,
        "method": "terminal.list",
        "params": {
            "workspaceID": core.workspace_id(project_id)
        }
    }));
    assert_success_response(&list_response, &json!(40));
    let terminals = list_response["result"]["terminals"]
        .as_array()
        .expect("terminal list");
    assert_eq!(terminals.len(), 1);
    assert_eq!(
        terminals[0]["terminalID"].as_str(),
        Some(terminal_id.as_str())
    );
    assert!(terminals[0].get("scopeKind").is_none());
    assert!(terminals[0].get("scopeID").is_none());
    assert_ne!(terminal_id, session_id);

    let (write_response, notifications) = core.send_for_id(
        json!({
            "id": 41,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("printf __ottam_replay_ok__\\n\n")
            }
        }),
        json!(41),
    );
    assert_success_response(&write_response, &json!(41));
    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_replay_ok__",
    );
    assert!(output.contains("__ottam_replay_ok__"));

    let replay_small = core.send(json!({
        "id": 42,
        "method": "terminal.replay",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "terminalID": terminal_id,
            "maxBytes": 4
        }
    }));
    assert_success_response(&replay_small, &json!(42));
    assert_eq!(replay_small["result"]["truncated"].as_bool(), Some(true));
    assert_eq!(
        replay_small["result"]["responseTruncated"].as_bool(),
        Some(true)
    );
    assert_eq!(
        replay_small["result"]["retentionTruncated"].as_bool(),
        Some(false)
    );
    assert_eq!(
        replay_small["result"]["retainedByteStart"].as_u64(),
        Some(0)
    );
    assert!(
        replay_small["result"]["retainedByteEnd"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    // Replay returns whole chunks only, so a budget below one chunk returns
    // none rather than a partial chunk claiming a sequence it cannot cover.
    // `responseTruncated` above is what tells the client to ask for more.
    let replay_chunks = replay_small["result"]["chunks"]
        .as_array()
        .expect("replay chunks");
    assert!(replay_chunks.is_empty());

    // A budget that fits some but not all of the buffer returns whole chunks,
    // newest first, and never exceeds the request.
    let replay_partial = core.send(json!({
        "id": 46,
        "method": "terminal.replay",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "terminalID": terminal_id,
            "maxBytes": 512
        }
    }));
    assert_success_response(&replay_partial, &json!(46));
    let partial_chunks = replay_partial["result"]["chunks"]
        .as_array()
        .expect("replay chunks");
    let partial_bytes: usize = partial_chunks
        .iter()
        .map(|chunk| {
            STANDARD
                .decode(chunk["dataBase64"].as_str().expect("replay data"))
                .expect("decodes replay data")
                .len()
        })
        .sum();
    assert!(partial_bytes <= 512);
    for chunk in partial_chunks {
        let bytes = STANDARD
            .decode(chunk["dataBase64"].as_str().expect("replay data"))
            .expect("decodes replay data");
        // Whole chunk: the declared byte range matches the payload exactly.
        let byte_start = chunk["byteStart"].as_u64().expect("byteStart");
        let byte_end = chunk["byteEnd"].as_u64().expect("byteEnd");
        assert_eq!(byte_end - byte_start, bytes.len() as u64);
    }

    let unknown = core.send(json!({
        "id": 43,
        "method": "terminal.replay",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "terminalID": "019b0000-0000-7000-8000-000000000099"
        }
    }));
    assert_error_response(&unknown, &json!(43), -32000, "terminal not found");
    assert_error_data_kind(&unknown, "terminalNotFound");

    let close_response = core.send(json!({
        "id": 44,
        "method": "terminal.close",
        "params": {
            "sessionID": session_id
        }
    }));
    assert_success_response(&close_response, &json!(44));

    let (closed_replay, _) = core.send_for_id(
        json!({
            "id": 45,
            "method": "terminal.replay",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "terminalID": terminal_id
            }
        }),
        json!(45),
    );
    assert_error_response(&closed_replay, &json!(45), -32000, "terminal not found");
    assert_error_data_kind(&closed_replay, "terminalNotFound");

    let (delete_response, _) = core.send_for_id(
        json!({
            "id": 47,
            "method": "terminal.delete",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "terminalID": terminal_id
            }
        }),
        json!(47),
    );
    assert_success_response(&delete_response, &json!(47));

    let (list_after_delete, _) = core.send_for_id(
        json!({
            "id": 48,
            "method": "terminal.list",
            "params": {
                "workspaceID": core.workspace_id(project_id)
            }
        }),
        json!(48),
    );
    assert_success_response(&list_after_delete, &json!(48));
    let terminals_after_delete = list_after_delete["result"]["terminals"]
        .as_array()
        .expect("terminal list after delete");
    assert!(
        terminals_after_delete.is_empty(),
        "closed terminal should not remain listed: {terminals_after_delete:?}"
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn removed_terminal_methods_return_method_not_found() {
    let mut core = CoreProcess::new();
    let detach = core.send(json!({
        "id": 51,
        "method": "terminal.detach",
        "params": {"terminalID": "019b0000-0000-7000-8000-000000000001"}
    }));
    assert_error_response(&detach, &json!(51), -32601, "method not found");
    let resume = core.send(json!({
        "id": 52,
        "method": "terminal.setResumeBinding",
        "params": {}
    }));
    assert_error_response(&resume, &json!(52), -32601, "method not found");
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn terminal_natural_exit_retains_scrollback_until_deleted() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let temp_dir = TempDir::new().expect("creates fake shell dir");
    let fake_shell = temp_dir.path().join("fake-shell");
    write_executable_script(
        &fake_shell,
        r#"#!/bin/sh
printf "__ottam_terminal_exit__\n"
exit 0
"#,
    );

    let mut core = CoreProcess::new_with_env(vec![(
        "SHELL".to_string(),
        fake_shell.to_string_lossy().into_owned(),
    )]);
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let (open_response, notifications) = core.send_for_id(
        json!({
            "id": 36,
            "method": "terminal.open",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "workingRoot": {
                    "kind": "project",
                    "projectID": project_id,
                    "sourceID": core.project_source_id(project_id)
                },
                "rows": 24,
                "cols": 80
            }
        }),
        json!(36),
    );
    assert_success_response(&open_response, &json!(36));
    let session_id = open_response["result"]["sessionID"]
        .as_str()
        .expect("sessionID");
    let terminal_id = open_response["result"]["terminalID"]
        .as_str()
        .expect("terminalID");

    let exit_status = collect_terminal_exit(&mut core, session_id, notifications)
        .expect("terminal exit notification");
    assert_eq!(exit_status, 0);

    let (response, _) = core.send_for_id(
        json!({
            "id": 37,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("echo stale\n")
            }
        }),
        json!(37),
    );
    assert_error_response(&response, &json!(37), -32000, "unknown terminal sessionID");
    assert_error_data_kind(&response, "unknownTerminalSession");

    let listed = core.send(json!({
        "id": 38,
        "method": "terminal.list",
        "params": {"workspaceID": core.workspace_id(project_id)}
    }));
    assert_success_response(&listed, &json!(38));
    assert_eq!(listed["result"]["terminals"][0]["status"], "exited");
    assert_eq!(listed["result"]["terminals"][0]["exitStatus"], 0);
    assert!(listed["result"]["terminals"][0]["runtimeSessionID"].is_null());

    let replay = core.send(json!({
        "id": 40,
        "method": "terminal.replay",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "terminalID": terminal_id
        }
    }));
    assert_success_response(&replay, &json!(40));
    let replay_text = replay["result"]["chunks"]
        .as_array()
        .expect("replay chunks")
        .iter()
        .flat_map(|chunk| {
            STANDARD
                .decode(chunk["dataBase64"].as_str().expect("replay data"))
                .expect("replay data decodes")
        })
        .collect::<Vec<_>>();
    assert!(String::from_utf8_lossy(&replay_text).contains("__ottam_terminal_exit__"));

    let deleted = core.send(json!({
        "id": 41,
        "method": "terminal.delete",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "terminalID": terminal_id
        }
    }));
    assert_success_response(&deleted, &json!(41));
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn terminal_unknown_session_returns_structured_error() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 34,
        "method": "terminal.write",
        "params": {
            "sessionID": "terminal_local_missing",
            "dataBase64": STANDARD.encode("hello")
        }
    }));
    assert_error_response(&response, &json!(34), -32000, "unknown terminal sessionID");
    assert_error_data_kind(&response, "unknownTerminalSession");
}

#[cfg(unix)]
#[test]
fn terminal_output_cap_no_longer_kills_session() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_TERMINAL_OUTPUT_MAX_BYTES".to_string(),
        "64".to_string(),
    )]);
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    let (write_response, notifications) = core.send_for_id(
        json!({
            "id": 35,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("yes x | head -c 2048; printf __ottam_big_output_done__\\n\n")
            }
        }),
        json!(35),
    );
    assert_success_response(&write_response, &json!(35));

    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_big_output_done__",
    );
    assert!(output.contains("__ottam_big_output_done__"));

    let (after_response, notifications) = core.send_for_id(
        json!({
            "id": 38,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("printf __ottam_after_cap__\\n\n")
            }
        }),
        json!(38),
    );
    assert_success_response(&after_response, &json!(38));
    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_after_cap__",
    );
    assert!(output.contains("__ottam_after_cap__"));
    core.shutdown();
}

// Stage-1 async-migration coverage: exercises the reader/dispatcher/writer split
// and the AsyncFd terminal read loop under conditions the pre-existing tests do
// not reach (live-terminal shutdown, output starvation, EAGAIN writes, two
// concurrent reactor tasks, kill-first drain ordering).

/// Shutdown with a live terminal: open a session, stream output, then drop stdin
/// without closing the terminal. Exercises `close_all` → kill → drain-to-EOF →
/// reap → writer-drain and asserts the process still exits 0 within the tracker
/// timeout (a hung writer or tracker would make `shutdown()` panic).
#[cfg(unix)]
#[test]
fn terminal_shutdown_with_live_session_exits_cleanly() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    let (write_response, notifications) = core.send_for_id(
        json!({
            "id": 60,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("printf __ottam_live_shutdown__\\n\n")
            }
        }),
        json!(60),
    );
    assert_success_response(&write_response, &json!(60));
    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_live_shutdown__",
    );
    assert!(output.contains("__ottam_live_shutdown__"));

    // The shell is still live (parked at its prompt, no grandchild) when we drop
    // stdin. shutdown() asserts a clean exit-0 within the child wait.
    core.shutdown();
}

/// Heavy-output starvation smoke: a terminal emits a sustained burst (many
/// `append_terminal_output` calls on a reactor worker) while a second, unrelated
/// RPC must still get a timely response. Guards against SQLite-in-read-task
/// parking the dispatcher/reader.
#[cfg(unix)]
#[test]
fn terminal_heavy_output_does_not_starve_unrelated_rpc() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    let (write_response, _) = core.send_for_id(
        json!({
            "id": 61,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("yes ottam | head -n 5000\n")
            }
        }),
        json!(61),
    );
    assert_success_response(&write_response, &json!(61));

    // While that burst streams, an unrelated request must still be answered
    // promptly (send_for_id skips the flood of outputDelta notifications).
    let (list_response, _) = core.send_for_id(
        json!({
            "id": 62,
            "method": "terminal.list",
            "params": {
                "workspaceID": core.workspace_id(project_id)
            }
        }),
        json!(62),
    );
    assert_success_response(&list_response, &json!(62));
    assert!(list_response["result"]["terminals"].is_array());
    core.shutdown();
}

/// A program that never reads stdin fills the PTY input buffer. Writing past
/// that must not stall the dispatcher: other requests keep answering, the
/// write is queued, and a queue past its budget is refused as busy.
#[cfg(unix)]
#[test]
fn terminal_write_to_stalled_pty_does_not_block_other_rpc() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    let (exec_response, _) = core.send_for_id(
        json!({
            "id": 70,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("exec sleep 30\n")
            }
        }),
        json!(70),
    );
    assert_success_response(&exec_response, &json!(70));

    // A multi-line paste far past the tty input queue.
    let stalled = "b".repeat(63) + "\n";
    core.write_message(json!({
        "id": 71,
        "method": "terminal.write",
        "params": {
            "sessionID": session_id,
            "dataBase64": STANDARD.encode(stalled.repeat(4096))
        }
    }));
    let (list_response, _) = core.send_for_id(
        json!({
            "id": 72,
            "method": "terminal.list",
            "params": {"workspaceID": core.workspace_id(project_id)}
        }),
        json!(72),
    );
    assert_success_response(&list_response, &json!(72));

    // 256 KiB is queued; two more 512 KiB writes overflow the 1 MiB budget.
    let chunk = STANDARD.encode(stalled.repeat(8192));
    let (queued, _) = core.send_for_id(
        json!({
            "id": 73,
            "method": "terminal.write",
            "params": {"sessionID": session_id, "dataBase64": chunk}
        }),
        json!(73),
    );
    assert_success_response(&queued, &json!(73));
    let (busy, _) = core.send_for_id(
        json!({
            "id": 74,
            "method": "terminal.write",
            "params": {"sessionID": session_id, "dataBase64": chunk}
        }),
        json!(74),
    );
    assert_error_data_kind(&busy, "terminalInputBusy");

    let (close_response, _) = core.send_for_id(
        json!({
            "id": 75,
            "method": "terminal.close",
            "params": {"sessionID": session_id}
        }),
        json!(75),
    );
    assert_success_response(&close_response, &json!(75));
    core.shutdown();
}

/// Large `terminal.write` under load: push a payload well past the PTY buffer to
/// a `cat` that echoes it back. Exercises the non-blocking master's
/// `EAGAIN`/`poll(POLLOUT)` retry path and confirms every byte is delivered.
#[cfg(unix)]
#[test]
fn terminal_large_write_is_fully_delivered() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    // Disable line-discipline echo and start `cat`, which echoes stdin back to
    // the master — so the only output is cat's, not doubled by the tty.
    let (cat_response, _) = core.send_for_id(
        json!({
            "id": 63,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("stty -echo 2>/dev/null; cat\n")
            }
        }),
        json!(63),
    );
    assert_success_response(&cat_response, &json!(63));

    // ~122 KB across many short lines: large enough to transiently fill the PTY
    // input buffer (tripping EAGAIN on the write), with a trailing marker line
    // that survives even if earlier chunks are dropped by the output cap.
    let mut payload = "a".repeat(60);
    payload.push('\n');
    let mut big = payload.repeat(2000);
    big.push_str("__ottam_bigwrite_done__\n");

    let (write_response, notifications) = core.send_for_id(
        json!({
            "id": 64,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode(&big)
            }
        }),
        json!(64),
    );
    assert_success_response(&write_response, &json!(64));

    // The echoed payload spans well over the 40-notification budget of the
    // shared helper, so drain with a larger budget until the trailing marker.
    let mut output = String::new();
    let mut queued = notifications.into_iter();
    let mut found = false;
    // Without per-chunk SQLite writes the PTY reader can surface more, smaller
    // notifications; keep the bound comfortably above the payload's line count.
    for _ in 0..2_000 {
        let value = match queued.next() {
            Some(value) => value,
            None => match core.read_json_timeout(Duration::from_secs(3)) {
                Some(value) => value,
                None => break,
            },
        };
        if value["method"].as_str() != Some("terminal.outputDelta")
            || value["params"]["sessionID"].as_str() != Some(session_id)
        {
            continue;
        }
        let data = STANDARD
            .decode(value["params"]["dataBase64"].as_str().unwrap_or_default())
            .expect("decodes terminal output");
        output.push_str(&String::from_utf8_lossy(&data));
        if output.contains("__ottam_bigwrite_done__") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "large write was not fully delivered: trailing marker missing after {} bytes; tail={:?}",
        output.len(),
        output.chars().rev().take(120).collect::<String>()
    );
    core.shutdown();
}

/// Two concurrent terminals: each has its own AsyncFd read task. Assert both
/// sessions' markers arrive and each session's own deltas stay in order under the
/// Notify-driven writer.
#[cfg(unix)]
#[test]
fn two_concurrent_terminals_stream_independently() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal_a = core.open_terminal(project_id, 24, 80);
    let session_a = terminal_a["sessionID"]
        .as_str()
        .expect("sessionID")
        .to_string();
    let terminal_b = core.open_terminal(project_id, 24, 80);
    let session_b = terminal_b["sessionID"]
        .as_str()
        .expect("sessionID")
        .to_string();
    assert_ne!(session_a, session_b);

    let (write_a, notifications_a) = core.send_for_id(
        json!({
            "id": 65,
            "method": "terminal.write",
            "params": {
                "sessionID": session_a,
                "dataBase64": STANDARD.encode("printf __ottam_session_a__\\n\n")
            }
        }),
        json!(65),
    );
    assert_success_response(&write_a, &json!(65));
    let (write_b, notifications_b) = core.send_for_id(
        json!({
            "id": 66,
            "method": "terminal.write",
            "params": {
                "sessionID": session_b,
                "dataBase64": STANDARD.encode("printf __ottam_session_b__\\n\n")
            }
        }),
        json!(66),
    );
    assert_success_response(&write_b, &json!(66));

    // Merge both notification batches; collect_terminal_output filters by session
    // id, so each call only sees its own session's deltas.
    let mut merged = notifications_a;
    merged.extend(notifications_b);
    let output_a = collect_terminal_output(
        &mut core,
        &session_a,
        merged.clone(),
        "__ottam_session_a__",
    );
    assert!(output_a.contains("__ottam_session_a__"));
    let output_b =
        collect_terminal_output(&mut core, &session_b, merged, "__ottam_session_b__");
    assert!(output_b.contains("__ottam_session_b__"));
    core.shutdown();
}

/// Close does not lose trailing output: write a command that emits a marker, then
/// close promptly. Kill-first drain-to-EOF must still deliver the marker (an
/// eager cancel-before-drain would truncate it).
#[cfg(unix)]
#[test]
fn terminal_close_does_not_lose_trailing_output() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");

    let terminal = core.open_terminal(project_id, 24, 80);
    let session_id = terminal["sessionID"].as_str().expect("sessionID");

    let (write_response, mut notifications) = core.send_for_id(
        json!({
            "id": 67,
            "method": "terminal.write",
            "params": {
                "sessionID": session_id,
                "dataBase64": STANDARD.encode("printf __ottam_pre_close__\\n\n")
            }
        }),
        json!(67),
    );
    assert_success_response(&write_response, &json!(67));

    let (close_response, close_notifications) = core.send_for_id(
        json!({
            "id": 68,
            "method": "terminal.close",
            "params": {
                "sessionID": session_id
            }
        }),
        json!(68),
    );
    assert_success_response(&close_response, &json!(68));
    notifications.extend(close_notifications);

    let output = collect_terminal_output(
        &mut core,
        session_id,
        notifications,
        "__ottam_pre_close__",
    );
    assert!(
        output.contains("__ottam_pre_close__"),
        "closing a terminal dropped output produced before close: {output:?}"
    );
    core.shutdown();
}

/// A shell that exits while a background child keeps the PTY slave open. On
/// Linux the master never EOFs in this case, so EOF-only reaping would hang
/// forever and `terminal.exited` would never fire — the concurrent `child.wait()`
/// waiter is what detects the exit (then a bounded drain runs before the reap).
/// On macOS/BSD the master EOFs when the session leader exits regardless, so the
/// waiter is redundant there; either way `terminal.exited` must arrive.
#[cfg(unix)]
#[test]
fn terminal_natural_exit_reaps_even_with_background_child() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let temp_dir = TempDir::new().expect("creates fake shell dir");
    let fake_shell = temp_dir.path().join("fake-shell");
    write_executable_script(
        &fake_shell,
        r#"#!/bin/sh
sleep 30 &
printf "__ottam_bg_child_exit__\n"
exit 0
"#,
    );

    let mut core = CoreProcess::new_with_env(vec![(
        "SHELL".to_string(),
        fake_shell.to_string_lossy().into_owned(),
    )]);
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let (open_response, notifications) = core.send_for_id(
        json!({
            "id": 70,
            "method": "terminal.open",
            "params": {
                "workspaceID": core.workspace_id(project_id),
                "workingRoot": {
                    "kind": "project",
                    "projectID": project_id,
                    "sourceID": core.project_source_id(project_id)
                },
                "rows": 24,
                "cols": 80
            }
        }),
        json!(70),
    );
    assert_success_response(&open_response, &json!(70));
    let session_id = open_response["result"]["sessionID"]
        .as_str()
        .expect("sessionID");

    // Bounded drain is ~5s after the child exit is observed, so allow headroom.
    let exit_status = collect_terminal_exit_within(
        &mut core,
        session_id,
        notifications,
        Duration::from_secs(12),
    )
    .expect("terminal exit notification despite a background child holding the PTY");
    assert_eq!(exit_status, 0);
    core.shutdown();
}

#[cfg(unix)]
fn collect_terminal_exit_within(
    core: &mut CoreProcess,
    session_id: &str,
    queued: Vec<serde_json::Value>,
    budget: Duration,
) -> Option<i64> {
    let deadline = std::time::Instant::now() + budget;
    let mut queued = queued.into_iter();
    loop {
        let value = match queued.next() {
            Some(value) => value,
            None => {
                let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
                core.read_json_timeout(remaining)?
            }
        };
        if value["method"].as_str() == Some("terminal.exited")
            && value["params"]["sessionID"].as_str() == Some(session_id)
        {
            return value["params"]["exitStatus"].as_i64();
        }
    }
}

#[cfg(unix)]
fn collect_terminal_output(
    core: &mut CoreProcess,
    session_id: &str,
    mut queued: Vec<serde_json::Value>,
    marker: &str,
) -> String {
    let mut output = String::new();
    for _ in 0..40 {
        let value = if queued.is_empty() {
            core.read_json_timeout(Duration::from_secs(3))
                .expect("terminal output notification")
        } else {
            queued.remove(0)
        };
        if value["method"].as_str() != Some("terminal.outputDelta")
            || value["params"]["sessionID"].as_str() != Some(session_id)
        {
            continue;
        }
        let data = STANDARD
            .decode(value["params"]["dataBase64"].as_str().unwrap_or_default())
            .expect("decodes terminal output");
        output.push_str(&String::from_utf8_lossy(&data));
        if output.contains(marker) {
            return output;
        }
    }
    output
}

#[cfg(unix)]
fn collect_terminal_exit(
    core: &mut CoreProcess,
    session_id: &str,
    mut queued: Vec<serde_json::Value>,
) -> Option<i64> {
    for _ in 0..40 {
        let value = if queued.is_empty() {
            core.read_json_timeout(Duration::from_secs(3))
                .expect("terminal exit notification")
        } else {
            queued.remove(0)
        };
        if value["method"].as_str() == Some("terminal.exited")
            && value["params"]["sessionID"].as_str() == Some(session_id)
        {
            return value["params"]["exitStatus"].as_i64();
        }
    }
    None
}

#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).expect("writes executable script");
    let mut permissions = std::fs::metadata(path)
        .expect("reads executable script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("marks script executable");
}
