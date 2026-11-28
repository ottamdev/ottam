use crate::support::{
    assert_error_data_kind, assert_success_response, run_git, CoreProcess, FakeAcpRuntime,
    FakePromptBehavior, GitRepoFixture,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn create_worktree_conversation(
    core: &mut CoreProcess,
    project_id: &str,
    message: &str,
) -> Value {
    let response = core.send(json!({
        "id": 60,
        "method": "conversation.create",
        "params": {
            "workspaceID": core.workspace_id(project_id),
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": core.project_source_id(project_id)
            },
            "message": {"body": message, "idempotencyKey": format!("worktree-{message}")},
            "createWorktree": true
        }
    }));
    assert_success_response(&response, &json!(60));
    response["result"]["conversation"].clone()
}

fn worktree_path(conversation: &Value) -> PathBuf {
    PathBuf::from(
        conversation["conversation"]["workingRoot"]["canonicalPath"]
            .as_str()
            .expect("canonical path"),
    )
}

fn worktree_source(conversation: &Value) -> String {
    conversation["conversation"]["workingRoot"]["sourceID"]
        .as_str()
        .expect("source id")
        .to_string()
}

fn session_cwds(runtime: &FakeAcpRuntime) -> Vec<String> {
    runtime
        .request_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|request| request["method"] == "session/new")
        .filter_map(|request| request["params"]["cwd"].as_str().map(str::to_string))
        .collect()
}

fn wait_for_sessions(runtime: &FakeAcpRuntime, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let cwds = session_cwds(runtime);
        if cwds.len() >= count {
            return cwds;
        }
        assert!(Instant::now() < deadline, "only {} sessions opened", cwds.len());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn first_input_id(conversation: &Value) -> String {
    conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input")
        .to_string()
}

fn worktree_status(core: &mut CoreProcess, workspace_id: &str, source_id: &str) -> Value {
    let response = core.send(json!({
        "id": 61,
        "method": "worktree.status",
        "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
    }));
    assert_success_response(&response, &json!(61));
    response["result"].clone()
}

fn diff_snapshot(core: &mut CoreProcess, source_id: &str, id: u64) -> Value {
    let (response, _) = core.send_for_id(
        json!({
            "id": id,
            "method": "git.diffSnapshot",
            "params": {"projectSourceID": source_id}
        }),
        json!(id),
    );
    assert_success_response(&response, &json!(id));
    response["result"].clone()
}

fn delete_conversation(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    children: &str,
) -> Value {
    let response = core.send(json!({
        "id": 62,
        "method": "conversation.delete",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "children": children
        }
    }));
    assert_success_response(&response, &json!(62));
    response["result"].clone()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    !run_git(repo, ["branch", "--list", branch]).trim().is_empty()
}

#[cfg(unix)]
#[test]
fn a_worktree_conversation_and_its_child_work_in_their_own_checkout() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "worktrees\n");
    repo.commit_all("initial");
    let head = repo.git(["rev-parse", "HEAD"]).trim().to_string();
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let primary_source = core.project_source_id(&project_id);
    let workspace_id = core.workspace_id(&project_id);

    let created = create_worktree_conversation(&mut core, &project_id, "Fix the login page");
    let root = &created["conversation"]["workingRoot"];
    let branch = root["worktree"]["branch"].as_str().expect("branch").to_string();
    assert!(
        branch.starts_with("ottam/fix-the-login-page-"),
        "branch named from the message: {branch}"
    );
    assert_eq!(root["worktree"]["baseCommit"].as_str(), Some(head.as_str()));
    assert_eq!(root["availability"], "available");
    assert_ne!(root["sourceID"].as_str(), Some(primary_source.as_str()));
    let path = worktree_path(&created);
    let worktrees = home.path().canonicalize().unwrap().join("worktrees");
    assert!(path.starts_with(&worktrees), "{path:?} under {worktrees:?}");
    assert_eq!(run_git(&path, ["branch", "--show-current"]).trim(), branch);
    assert!(run_git(repo.path(), ["worktree", "list", "--porcelain"])
        .contains(path.to_str().unwrap()));
    assert!(run_git(repo.path(), ["status", "--porcelain"]).trim().is_empty());
    let source_id = worktree_source(&created);

    // A terminal opens in the worktree.
    let (terminal, _) = core.send_for_id(
        json!({
            "id": 63,
            "method": "terminal.open",
            "params": {
                "workspaceID": workspace_id,
                "workingRoot": {"kind": "project", "projectID": project_id, "sourceID": source_id},
                "rows": 24,
                "cols": 80
            }
        }),
        json!(63),
    );
    assert_success_response(&terminal, &json!(63));
    assert_eq!(terminal["result"]["cwd"].as_str(), path.to_str());
    assert!(terminal["result"]["workingRoot"]["worktree"]["branch"].is_string());

    // The agent's session opens in the worktree.
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let started = core.start_conversation_run(
        &project_id,
        &conversation_id,
        "codex",
        &first_input_id(&created),
    );
    let run_id = started["run"]["runID"].as_str().unwrap().to_string();
    core.wait_for_run_status(&workspace_id, &conversation_id, &run_id, &["completed"]);
    assert_eq!(wait_for_sessions(&runtime, 1), vec![path.to_string_lossy().into_owned()]);

    // A delegated child works where its parent does.
    runtime.set_next_mcp_call("ottam", "delegate", json!({"prompt": "Check the tests"}));
    let posted = core.post_conversation_message(&project_id, &conversation_id, "Delegate it");
    let input_id = posted["input"]["runInputID"].as_str().unwrap().to_string();
    let started = core.start_conversation_run(&project_id, &conversation_id, "codex", &input_id);
    let run_id = started["run"]["runID"].as_str().unwrap().to_string();
    core.wait_for_run_status(&workspace_id, &conversation_id, &run_id, &["completed"]);
    let cwds = wait_for_sessions(&runtime, 2);
    assert_eq!(cwds[1], path.to_string_lossy(), "the child's session cwd");
    let conversations = core.list_conversations(&project_id);
    let child = conversations
        .as_array()
        .unwrap()
        .iter()
        .find(|conversation| conversation["parentConversationID"] == conversation_id.as_str())
        .expect("the delegated child");
    assert_eq!(child["workingRoot"]["sourceID"].as_str(), Some(source_id.as_str()));

    // Review reads the worktree, and the project's checkout stays clean.
    std::fs::write(path.join("README.md"), "worktrees, edited\n").unwrap();
    let review = diff_snapshot(&mut core, &source_id, 64);
    assert!(
        review["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "README.md"),
        "{review}"
    );
    let primary_review = diff_snapshot(&mut core, &primary_source, 65);
    assert!(primary_review["files"].as_array().unwrap().is_empty(), "{primary_review}");

    // Status counts what removal would discard.
    std::fs::write(path.join("scratch.txt"), "draft\n").unwrap();
    let status = worktree_status(&mut core, &workspace_id, &source_id);
    assert_eq!(status["branch"].as_str(), Some(branch.as_str()));
    assert_eq!(status["uncommittedChanges"], 2);
    assert_eq!(status["commits"], 0);
    assert_eq!(status["branchKept"], false);
    assert_eq!(status["unbranchedCommits"], 0);
    assert_eq!(status["conversationCount"], 2);

    // Deleting the parent alone keeps the worktree for the child.
    let deleted = delete_conversation(&mut core, &workspace_id, &conversation_id, "keep");
    assert!(deleted.get("worktree").is_none(), "{deleted}");
    assert!(path.exists());
    let child_id = child["conversationID"].as_str().unwrap().to_string();
    // The last conversation takes it, and its commitless branch, along.
    let deleted = delete_conversation(&mut core, &workspace_id, &child_id, "keep");
    assert_eq!(deleted["worktree"]["removed"], true, "{deleted}");
    assert_eq!(deleted["worktree"]["branchKept"], false);
    assert!(!path.exists());
    assert!(!branch_exists(repo.path(), &branch));
    assert!(!run_git(repo.path(), ["worktree", "list", "--porcelain"])
        .contains(path.to_str().unwrap()));
    core.shutdown();
}

#[test]
fn a_worktree_branch_with_commits_survives_its_conversation_and_a_relaunch() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Add a feature");
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let path = worktree_path(&created);
    let branch = created["conversation"]["workingRoot"]["worktree"]["branch"]
        .as_str()
        .unwrap()
        .to_string();
    core.shutdown();

    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    core.open_path(repo.path());
    let reloaded = core.get_conversation(&project_id, &conversation_id);
    assert_eq!(reloaded["conversation"]["workingRoot"]["availability"], "available");
    assert_eq!(
        reloaded["conversation"]["workingRoot"]["worktree"]["branch"].as_str(),
        Some(branch.as_str())
    );
    assert_eq!(worktree_path(&reloaded), path);

    std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
    run_git(&path, ["add", "-A"]);
    run_git(&path, ["commit", "-m", "feature"]);
    let status = worktree_status(&mut core, &workspace_id, &worktree_source(&created));
    assert_eq!(status["commits"], 1);
    assert_eq!(status["branchKept"], true);
    assert_eq!(status["unbranchedCommits"], 0);
    assert_eq!(status["uncommittedChanges"], 0);

    let deleted = delete_conversation(&mut core, &workspace_id, &conversation_id, "keep");
    assert_eq!(deleted["worktree"]["removed"], true, "{deleted}");
    assert_eq!(deleted["worktree"]["branchKept"], true);
    assert!(!path.exists());
    assert!(branch_exists(repo.path(), &branch));
    core.shutdown();
}

/// Status and removal read the same ref: commits on a detached HEAD leave the
/// recorded branch where it started, so removal deletes it, and status says
/// those commits are on no branch.
#[test]
fn commits_on_a_detached_head_are_reported_as_unbranched() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Detach and commit");
    let source_id = worktree_source(&created);
    let path = worktree_path(&created);
    let branch = created["conversation"]["workingRoot"]["worktree"]["branch"]
        .as_str()
        .unwrap()
        .to_string();
    run_git(&path, ["checkout", "--detach"]);
    std::fs::write(path.join("loose.txt"), "loose\n").unwrap();
    run_git(&path, ["add", "-A"]);
    run_git(&path, ["commit", "-m", "loose"]);
    let loose = run_git(&path, ["rev-parse", "HEAD"]).trim().to_string();

    let status = worktree_status(&mut core, &workspace_id, &source_id);
    assert_eq!(status["commits"], 0, "{status}");
    assert_eq!(status["branchKept"], false);
    assert_eq!(status["unbranchedCommits"], 1);

    let (removed, _) = core.send_for_id(
        json!({
            "id": 70,
            "method": "worktree.remove",
            "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
        }),
        json!(70),
    );
    assert_success_response(&removed, &json!(70));
    assert_eq!(removed["result"]["removal"]["removed"], true, "{removed}");
    assert_eq!(removed["result"]["removal"]["branchKept"], false);
    assert!(!branch_exists(repo.path(), &branch));
    // The loose commit outlives its folder under a recovery ref.
    let reference = format!("refs/ottam/removed/{}", branch.trim_start_matches("ottam/"));
    assert_eq!(
        removed["result"]["removal"]["message"],
        format!("Commits no branch held are kept under {reference}.")
    );
    assert_eq!(run_git(repo.path(), ["rev-parse", reference.as_str()]).trim(), loose);
    core.shutdown();
}

#[test]
fn removing_a_worktree_detaches_its_conversation() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Try something");
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let source_id = worktree_source(&created);
    let path = worktree_path(&created);
    std::fs::write(path.join("draft.txt"), "draft\n").unwrap();

    let (removed, _) = core.send_for_id(
        json!({
            "id": 64,
            "method": "worktree.remove",
            "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
        }),
        json!(64),
    );
    assert_success_response(&removed, &json!(64));
    assert_eq!(removed["result"]["removal"]["removed"], true, "{removed}");
    assert!(!path.exists());

    let detached = core.get_conversation(&project_id, &conversation_id);
    assert_eq!(detached["conversation"]["workingRoot"]["availability"], "detached");
    let terminal = core.send(json!({
        "id": 65,
        "method": "terminal.open",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {"kind": "project", "projectID": project_id, "sourceID": source_id},
            "rows": 24,
            "cols": 80
        }
    }));
    assert_error_data_kind(&terminal, "workingRootUnavailable");
    let status = core.send(json!({
        "id": 66,
        "method": "worktree.status",
        "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
    }));
    assert_error_data_kind(&status, "worktreeUnavailable");
    // A repeat removes nothing: a checkout that reused the path is not its.
    let again = core.send(json!({
        "id": 70,
        "method": "worktree.remove",
        "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
    }));
    assert_error_data_kind(&again, "worktreeUnavailable");
    // Review no longer resolves it, rather than caching it as a root again.
    let (snapshot, _) = core.send_for_id(
        json!({
            "id": 71,
            "method": "git.diffSnapshot",
            "params": {"projectSourceID": source_id}
        }),
        json!(71),
    );
    assert_error_data_kind(&snapshot, "worktreeUnavailable");

    // Its transcript stays, and deleting it later owes nothing more.
    let deleted = delete_conversation(&mut core, &workspace_id, &conversation_id, "keep");
    assert!(deleted.get("worktree").is_none(), "{deleted}");
    core.shutdown();
}

#[test]
fn a_repository_without_commits_cannot_start_a_worktree() {
    let repo = GitRepoFixture::init();
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let response = core.send(json!({
        "id": 67,
        "method": "conversation.create",
        "params": {
            "workspaceID": core.workspace_id(&project_id),
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": core.project_source_id(&project_id)
            },
            "message": {"body": "Start", "idempotencyKey": "no-commits"},
            "createWorktree": true
        }
    }));
    assert_error_data_kind(&response, "worktreeNoCommits");
    assert!(!home.path().join("worktrees").exists());
    assert_eq!(core.list_conversations(&project_id), json!([]));
    core.shutdown();
}

#[test]
fn a_worktree_cannot_be_opened_as_a_project() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let created = create_worktree_conversation(&mut core, &project_id, "Somewhere else");
    let path = worktree_path(&created);

    let response = core.send(json!({
        "id": 68,
        "method": "project.openPath",
        "params": {"path": path.join(".").to_string_lossy()}
    }));
    assert_error_data_kind(&response, "managedWorktreePath");
    let workspace = core.send(json!({
        "id": 69,
        "method": "workspace.get",
        "params": {"workspaceID": core.workspace_id(&project_id)}
    }));
    assert_eq!(
        workspace["result"]["projects"][0]["source"]["projectSourceID"].as_str(),
        Some(core.project_source_id(&project_id).as_str()),
        "the default source is still the primary checkout"
    );

    // Nor a folder there with no row, as a crash mid-create leaves.
    let stray = home.path().join("worktrees").join("stray").join("orphan");
    std::fs::create_dir_all(&stray).unwrap();
    run_git(&stray, ["init", "--quiet"]);
    let response = core.send(json!({
        "id": 72,
        "method": "project.openPath",
        "params": {"path": stray.to_string_lossy()}
    }));
    assert_error_data_kind(&response, "managedWorktreePath");
    let response = core.send(json!({
        "id": 73,
        "method": "workspace.addProject",
        "params": {"workspaceID": core.workspace_id(&project_id), "path": stray.to_string_lossy()}
    }));
    assert_error_data_kind(&response, "managedWorktreePath");
    core.shutdown();
}

/// A `post-checkout` hook that fails leaves `git worktree add`'s checkout
/// registered; Core takes it back down with the branch.
#[cfg(unix)]
#[test]
fn a_failed_checkout_leaves_no_folder_branch_or_registration() {
    use std::os::unix::fs::PermissionsExt;

    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let hook = repo.path().join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();

    let (response, _) = core.send_for_id(
        json!({
            "id": 74,
            "method": "conversation.create",
            "params": {
                "workspaceID": core.workspace_id(&project_id),
                "workingRoot": {
                    "kind": "project",
                    "projectID": project_id,
                    "sourceID": core.project_source_id(&project_id)
                },
                "message": {"body": "Hook fails", "idempotencyKey": "hook-fails"},
                "createWorktree": true
            }
        }),
        json!(74),
    );
    assert!(response.get("error").is_some(), "{response}");
    // The project folder stays, so a concurrent create never loses it; no
    // checkout folder is left inside it.
    let checkouts = std::fs::read_dir(home.path().join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|project| std::fs::read_dir(project.path()).into_iter().flatten().flatten())
        .count();
    assert_eq!(checkouts, 0, "a checkout folder was left behind");
    assert!(run_git(repo.path(), ["branch", "--list", "ottam/*"]).trim().is_empty());
    assert_eq!(run_git(repo.path(), ["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    assert_eq!(core.list_conversations(&project_id), json!([]));
    core.shutdown();
}

/// A removal the filesystem refused stays owed by the archived row, and the
/// next launch finishes it.
#[cfg(unix)]
#[test]
fn a_refused_removal_is_retried_on_the_next_launch() {
    use std::os::unix::fs::PermissionsExt;

    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Refuse removal");
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let path = worktree_path(&created);
    let parent = path.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    // A privileged user deletes regardless; nothing to retry then.
    if std::fs::write(parent.join("probe"), "").is_ok() {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        core.shutdown();
        return;
    }

    let deleted = delete_conversation(&mut core, &workspace_id, &conversation_id, "keep");
    assert_eq!(deleted["worktree"]["removed"], false, "{deleted}");
    // Core's message owns the promise to retry.
    assert!(
        deleted["worktree"]["message"]
            .as_str()
            .is_some_and(|message| message.ends_with("Ottam tries again on its next launch.")),
        "{deleted}"
    );
    assert!(path.exists());
    core.shutdown();

    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "the owed removal never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    core.shutdown();
}

/// A removal that settled is recorded, so a later launch leaves alone a
/// branch someone made since under the same name at the same base.
#[test]
fn a_settled_removal_is_never_retried() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let head = repo.git(["rev-parse", "HEAD"]).trim().to_string();
    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Settle once");
    let source_id = worktree_source(&created);
    let branch = created["conversation"]["workingRoot"]["worktree"]["branch"]
        .as_str()
        .expect("branch")
        .to_string();
    let (removed, _) = core.send_for_id(
        json!({
            "id": 74,
            "method": "worktree.remove",
            "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
        }),
        json!(74),
    );
    assert_success_response(&removed, &json!(74));
    assert_eq!(removed["result"]["removal"]["removed"], true, "{removed}");
    assert!(!branch_exists(repo.path(), &branch));
    core.shutdown();

    repo.git(["branch", &branch, &head]);
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    // The startup retry runs off the dispatcher; give it time to act.
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(branch_exists(repo.path(), &branch), "a settled removal ran again");
        std::thread::sleep(Duration::from_millis(50));
    }
    core.shutdown();
}

/// Removal stops a background task that outlived its run in the checkout.
#[test]
fn removing_a_worktree_stops_its_background_tasks() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "background\n");
    repo.commit_all("initial");
    let home = TempDir::new().expect("OTTAM_HOME");
    let runtime = FakeAcpRuntime::new();
    runtime.set_next_prompt(FakePromptBehavior::AsyncTasksFollowUp);
    let mut core = CoreProcess::new_in_home_with_env(home.path(), runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let workspace_id = core.workspace_id(&project_id);
    let created = create_worktree_conversation(&mut core, &project_id, "Run in background");
    let conversation_id = created["conversation"]["conversationID"]
        .as_str()
        .unwrap()
        .to_string();
    let source_id = worktree_source(&created);
    core.start_conversation_run(&project_id, &conversation_id, "codex", &first_input_id(&created));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (page, _) = core.send_for_id(
            json!({
                "id": 75,
                "method": "conversation.listTranscript",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(75),
        );
        assert_success_response(&page, &json!(75));
        let live = page["result"]["backgroundTasks"]
            .as_array()
            .is_some_and(|counts| counts.iter().any(|count| count["live"].as_u64() > Some(0)));
        if live {
            break;
        }
        assert!(Instant::now() < deadline, "no background task went live");
        std::thread::sleep(Duration::from_millis(20));
    }

    let (removed, _) = core.send_for_id(
        json!({
            "id": 76,
            "method": "worktree.remove",
            "params": {"workspaceID": workspace_id, "projectSourceID": source_id}
        }),
        json!(76),
    );
    assert_success_response(&removed, &json!(76));
    assert_eq!(
        runtime
            .request_log()
            .matches("_session/async_task/stop")
            .count(),
        1
    );
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn worktree_base_branches_and_explicit_base_ref() {
    let repo = GitRepoFixture::init();
    // Ensure a predictable default branch name (`main`) regardless of git config.
    repo.git(["checkout", "-b", "main"]);
    repo.write("README.md", "base\n");
    repo.commit_all("initial on main");
    let main_commit = repo.git(["rev-parse", "HEAD"]).trim().to_string();

    repo.git(["checkout", "-b", "feat/auth"]);
    repo.write("auth.txt", "feature work\n");
    repo.commit_all("feature commit");
    let feat_commit = repo.git(["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(main_commit, feat_commit);

    // Dirty primary checkout: should be reported, and never copied into the worktree.
    repo.write("dirty.txt", "uncommitted\n");

    let home = TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().unwrap().to_string();
    let source_id = core.project_source_id(&project_id);
    let workspace_id = core.workspace_id(&project_id);

    let listed = core.send(json!({
        "id": 80,
        "method": "worktree.baseBranches",
        "params": {
            "workspaceID": workspace_id,
            "projectSourceID": source_id
        }
    }));
    assert_success_response(&listed, &json!(80));
    let result = &listed["result"];
    assert_eq!(result["currentBranch"].as_str(), Some("feat/auth"));
    assert!(result["detachedHead"].is_null());
    assert_eq!(result["defaultBranchRef"].as_str(), Some("refs/heads/main"));
    assert_eq!(result["primaryHasUncommittedChanges"], true);
    let branches = result["branches"].as_array().expect("branches");
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0]["refName"].as_str(), Some("refs/heads/feat/auth"));
    assert_eq!(branches[1]["refName"].as_str(), Some("refs/heads/main"));

    // Cut a worktree explicitly from main while primary sits on feat/auth.
    let created = core.send(json!({
        "id": 81,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": source_id
            },
            "message": {"body": "Fix from main", "idempotencyKey": "worktree-explicit-main"},
            "createWorktree": true,
            "worktreeBaseRef": "refs/heads/main"
        }
    }));
    assert_success_response(&created, &json!(81));
    let conversation = &created["result"]["conversation"];
    let root = &conversation["conversation"]["workingRoot"];
    assert_eq!(root["worktree"]["baseCommit"].as_str(), Some(main_commit.as_str()));
    let path = worktree_path(conversation);
    assert_eq!(run_git(&path, ["rev-parse", "HEAD"]).trim(), main_commit);
    assert!(!path.join("auth.txt").exists(), "feature branch commit should not be in main worktree");
    assert!(!path.join("dirty.txt").exists(), "uncommitted file should not be copied into worktree");

    let worktrees_before = run_git(repo.path(), ["worktree", "list", "--porcelain"]);
    let branches_before = run_git(repo.path(), ["branch", "--list", "ottam/*"]);

    // Missing and invalid base refs fail cleanly without leaving worktrees or branches behind.
    let missing = core.send(json!({
        "id": 82,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": source_id
            },
            "message": {"body": "Missing base", "idempotencyKey": "worktree-missing-base"},
            "createWorktree": true,
            "worktreeBaseRef": "refs/heads/does-not-exist"
        }
    }));
    assert_error_data_kind(&missing, "worktreeBaseRefNotFound");

    let invalid = core.send(json!({
        "id": 83,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": source_id
            },
            "message": {"body": "Invalid base", "idempotencyKey": "worktree-invalid-base"},
            "createWorktree": true,
            "worktreeBaseRef": "HEAD~1"
        }
    }));
    assert_error_data_kind(&invalid, "worktreeInvalidBaseRef");

    assert_eq!(run_git(repo.path(), ["worktree", "list", "--porcelain"]), worktrees_before);
    assert_eq!(run_git(repo.path(), ["branch", "--list", "ottam/*"]), branches_before);

    // Passing worktreeBaseRef without createWorktree is rejected.
    let misplaced = core.send(json!({
        "id": 84,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": source_id
            },
            "message": {"body": "No worktree", "idempotencyKey": "worktree-misplaced-base"},
            "createWorktree": false,
            "worktreeBaseRef": "refs/heads/main"
        }
    }));
    assert_error_data_kind(&misplaced, "invalidParams");

    // Cutting from a remote-tracking ref works cleanly.
    repo.git(["update-ref", "refs/remotes/origin/release", &main_commit]);
    let from_remote = core.send(json!({
        "id": 85,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": source_id
            },
            "message": {"body": "Release hotfix", "idempotencyKey": "worktree-remote-base"},
            "createWorktree": true,
            "worktreeBaseRef": "refs/remotes/origin/release"
        }
    }));
    assert_success_response(&from_remote, &json!(85));
    let remote_conv = &from_remote["result"]["conversation"];
    assert_eq!(
        remote_conv["conversation"]["workingRoot"]["worktree"]["baseCommit"].as_str(),
        Some(main_commit.as_str())
    );
    assert_eq!(run_git(&worktree_path(remote_conv), ["rev-parse", "HEAD"]).trim(), main_commit);

    core.shutdown();
}
