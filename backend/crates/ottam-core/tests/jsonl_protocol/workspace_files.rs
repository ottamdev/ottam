use crate::support::{
    assert_error_data_kind, assert_success_response, CoreProcess, GitRepoFixture,
};
use serde_json::json;

#[test]
fn empty_workspace_chat_and_terminal_share_managed_files_root() {
    let mut core = CoreProcess::new();
    core.initialize();
    let created = core.send(json!({
        "id": 80,
        "method": "workspace.create",
        "params": {"displayName": "Empty Workspace"}
    }));
    assert_success_response(&created, &json!(80));
    let workspace = &created["result"];
    let workspace_id = workspace["workspace"]["workspaceID"]
        .as_str()
        .expect("workspaceID");
    let files_path = workspace["workspaceFiles"]["canonicalPath"]
        .as_str()
        .expect("Workspace files path");
    assert!(std::path::Path::new(files_path).is_dir());
    std::fs::remove_dir(files_path).expect("removes empty managed files directory");
    let repaired = core.send(json!({
        "id": 805,
        "method": "workspace.open",
        "params": {"workspaceID": workspace_id}
    }));
    assert_success_response(&repaired, &json!(805));
    assert!(std::path::Path::new(files_path).is_dir());

    let conversation = core.send(json!({
        "id": 81,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {"kind": "workspaceFiles"},
            "message": {"body": "Work without a Project", "idempotencyKey": "jsonl-test-files-root"}
        }
    }));
    assert_success_response(&conversation, &json!(81));
    assert_eq!(
        conversation["result"]["conversation"]["conversation"]["workingRoot"]["canonicalPath"]
            .as_str(),
        Some(files_path)
    );
    assert!(
        conversation["result"]["conversation"]["conversation"]["workingRoot"]["projectID"]
            .is_null()
    );

    let terminal = core.send(json!({
        "id": 82,
        "method": "terminal.open",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {"kind": "workspaceFiles"},
            "rows": 24,
            "cols": 80
        }
    }));
    assert_success_response(&terminal, &json!(82));
    assert_eq!(terminal["result"]["cwd"].as_str(), Some(files_path));
    let terminal_id = terminal["result"]["terminalID"]
        .as_str()
        .expect("terminalID");
    assert_eq!(
        terminal["result"]["workingRoot"]["canonicalPath"].as_str(),
        Some(files_path)
    );
    let terminal_directory = std::path::Path::new(files_path)
        .parent()
        .expect("Workspace root")
        .join("terminals");
    assert!(!terminal_directory.exists());

    let sentinel = std::path::Path::new(files_path).join("shared.txt");
    std::fs::write(&sentinel, "shared").expect("writes shared Workspace file");
    let session_id = terminal["result"]["sessionID"].as_str().expect("sessionID");
    let closed = core.send(json!({
        "id": 83,
        "method": "terminal.close",
        "params": {"sessionID": session_id}
    }));
    assert_success_response(&closed, &json!(83));
    let (deleted, _) = core.send_for_id(
        json!({
            "id": 84,
            "method": "terminal.delete",
            "params": {
                "workspaceID": workspace_id,
                "terminalID": terminal_id
            }
        }),
        json!(84),
    );
    assert_success_response(&deleted, &json!(84));
    assert_eq!(
        std::fs::read_to_string(&sentinel).expect("shared Workspace file remains"),
        "shared"
    );
    assert!(!terminal_directory.exists());
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn workspace_open_reports_unsafe_managed_files_path() {
    use std::os::unix::fs::symlink;

    let mut core = CoreProcess::new();
    core.initialize();
    let created = core.send(json!({
        "id": 85,
        "method": "workspace.create",
        "params": {"displayName": "Unsafe Workspace"}
    }));
    assert_success_response(&created, &json!(85));
    let workspace_id = created["result"]["workspace"]["workspaceID"]
        .as_str()
        .expect("workspaceID");
    let files_path = std::path::Path::new(
        created["result"]["workspaceFiles"]["canonicalPath"]
            .as_str()
            .expect("Workspace files path"),
    );
    std::fs::remove_dir(files_path).expect("removes empty managed files directory");
    let outside = tempfile::TempDir::new().expect("outside directory");
    symlink(outside.path(), files_path).expect("installs unsafe files symlink");

    let opened = core.send(json!({
        "id": 86,
        "method": "workspace.open",
        "params": {"workspaceID": workspace_id}
    }));
    assert_error_data_kind(&opened, "workspaceFilesUnsafe");
    core.shutdown();
}

#[test]
fn exact_project_readd_restores_pinned_chat_but_not_ephemeral_terminal() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "root\n");
    repo.commit_all("initial");
    let mut core = CoreProcess::new();
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);
    let conversation = core.create_conversation(project_id, None, Some("Pinned history"));
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let terminal = core.open_terminal(project_id, 24, 80);
    let terminal_id = terminal["terminalID"].as_str().expect("terminalID");
    assert_eq!(
        terminal["workingRoot"]["projectID"].as_str(),
        Some(project_id)
    );

    let (removed, _) = core.send_for_id(
        json!({
            "id": 90,
            "method": "workspace.removeProject",
            "params": {
                "workspaceID": workspace_id,
                "projectID": project_id
            }
        }),
        json!(90),
    );
    assert_success_response(&removed, &json!(90));
    let (detached, _) = core.send_for_id(
        json!({
            "id": 91,
            "method": "conversation.get",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id
            }
        }),
        json!(91),
    );
    assert_success_response(&detached, &json!(91));
    assert_eq!(
        detached["result"]["conversation"]["conversation"]["workingRoot"]["availability"].as_str(),
        Some("detached")
    );

    let (listed, _) = core.send_for_id(
        json!({
            "id": 92,
            "method": "terminal.list",
            "params": {"workspaceID": workspace_id}
        }),
        json!(92),
    );
    assert_success_response(&listed, &json!(92));
    assert!(listed["result"]["terminals"]
        .as_array()
        .expect("terminal list")
        .is_empty());
    let (blocked, _) = core.send_for_id(
        json!({
            "id": 93,
            "method": "terminal.replay",
            "params": {
                "workspaceID": workspace_id,
                "terminalID": terminal_id
            }
        }),
        json!(93),
    );
    assert_error_data_kind(&blocked, "terminalNotFound");

    let (readded, _) = core.send_for_id(
        json!({
            "id": 94,
            "method": "workspace.addProject",
            "params": {
                "workspaceID": workspace_id,
                "path": repo.path().to_string_lossy()
            }
        }),
        json!(94),
    );
    assert_success_response(&readded, &json!(94));
    let (restored, _) = core.send_for_id(
        json!({
            "id": 95,
            "method": "conversation.get",
            "params": {
                "workspaceID": workspace_id,
                "conversationID": conversation_id
            }
        }),
        json!(95),
    );
    assert_success_response(&restored, &json!(95));
    assert_eq!(
        restored["result"]["conversation"]["conversation"]["workingRoot"]["availability"].as_str(),
        Some("available")
    );
    let (replayed, _) = core.send_for_id(
        json!({
            "id": 96,
            "method": "terminal.replay",
            "params": {
                "workspaceID": workspace_id,
                "terminalID": terminal_id
            }
        }),
        json!(96),
    );
    assert_error_data_kind(&replayed, "terminalNotFound");
    core.shutdown();
}

#[test]
fn rejects_foreign_workspace_projects_and_escaped_terminal_cwds() {
    let first_repo = GitRepoFixture::init();
    first_repo.commit_empty("first");
    let second_repo = GitRepoFixture::init();
    second_repo.commit_empty("second");
    let mut core = CoreProcess::new();
    core.initialize();
    let first = core.open_path(first_repo.path());
    let second = core.open_path(second_repo.path());
    let first_project_id = first["project"]["projectID"]
        .as_str()
        .expect("first projectID");
    let second_project_id = second["project"]["projectID"]
        .as_str()
        .expect("second projectID");
    let first_workspace_id = core.workspace_id(first_project_id);

    let foreign = core.send(json!({
        "id": 100,
        "method": "conversation.create",
        "params": {
            "workspaceID": first_workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": second_project_id,
                "sourceID": core.project_source_id(second_project_id)
            },
            "message": {"body": "Must not cross Workspace boundaries", "idempotencyKey": "jsonl-test-foreign"}
        }
    }));
    assert_error_data_kind(&foreign, "storageConstraint");

    let detail = core.send(json!({
        "id": 101,
        "method": "workspace.get",
        "params": {"workspaceID": first_workspace_id}
    }));
    assert_success_response(&detail, &json!(101));
    let files_path = detail["result"]["workspaceFiles"]["canonicalPath"]
        .as_str()
        .expect("Workspace files path");
    let escaped_cwd = std::path::Path::new(files_path)
        .parent()
        .expect("Workspace root")
        .to_string_lossy();
    let escaped = core.send(json!({
        "id": 102,
        "method": "terminal.open",
        "params": {
            "workspaceID": first_workspace_id,
            "workingRoot": {"kind": "workspaceFiles"},
            "rows": 24,
            "cols": 80,
            "cwd": escaped_cwd
        }
    }));
    assert_error_data_kind(&escaped, "terminalCwdOutsideWorkingRoot");
    core.shutdown();
}
