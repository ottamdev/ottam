use crate::support::{assert_error_data_kind, assert_error_response, CoreProcess, GitRepoFixture};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[test]
fn rust_core_process_opens_project_and_loads_diff_by_project_source_id() {
    let repo = GitRepoFixture::init();
    repo.write("tracked.txt", "one\n");
    repo.commit_all("initial");
    repo.write("tracked.txt", "one\ntwo\n");

    let mut core = CoreProcess::new();
    let initialize = core.initialize();
    assert_eq!(
        initialize["result"]["serverInfo"]["name"].as_str(),
        Some("ottam-core")
    );
    for method in [
        "conversation.promoteArtifact",
        "conversation.captureDecision",
        "spec.list",
        "spec.get",
        "spec.create",
        "task.list",
        "task.create",
        "taskGraph.get",
        "memory.list",
        "memory.create",
        "decision.list",
        "decision.create",
    ] {
        let response = core.send(json!({"id": 90, "method": method, "params": {}}));
        assert_error_response(&response, &json!(90), -32601, "method not found");
    }
    core.send_initialized();

    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    assert_eq!(
        uuid::Uuid::parse_str(project_id).unwrap().get_version_num(),
        7
    );
    assert_eq!(
        opened["source"]["canonicalPath"].as_str(),
        Some(repo.path().canonicalize().unwrap().to_str().unwrap())
    );

    let diff = core.diff_by_project_source_id(source_id);
    assert_eq!(diff["files"][0]["path"].as_str(), Some("tracked.txt"));
    assert_eq!(diff["additions"].as_i64(), Some(1));

    assert!(!core.ottam_home_path().join("registry.json").exists());
    assert!(core.ottam_home_path().join("ottam.sqlite").exists());

    core.shutdown();
}

#[test]
fn git_diff_snapshot_without_project_source_id_returns_invalid_params_error() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 5,
        "method": "git.diffSnapshot",
        "params": {}
    }));
    assert_error_response(&response, &json!(5), -32602, "invalid params");
}

#[test]
fn git_diff_snapshot_with_repo_path_is_rejected() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 7,
        "method": "git.diffSnapshot",
        "params": {
            "repoPath": repo.path().to_string_lossy()
        }
    }));
    assert_error_response(&response, &json!(7), -32602, "invalid params");
}

#[test]
fn git_diff_snapshot_with_project_source_id_and_repo_path_is_rejected() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    let response = core.send(json!({
        "id": 12,
        "method": "git.diffSnapshot",
        "params": {
            "projectSourceID": source_id,
            "repoPath": repo.path().to_string_lossy()
        }
    }));
    assert_error_response(&response, &json!(12), -32602, "invalid params");
}

#[test]
fn git_diff_snapshot_with_unknown_project_source_id_returns_error() {
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 6,
        "method": "git.diffSnapshot",
        "params": {
            "projectSourceID": "019b0000-0000-7000-8000-000000000099"
        }
    }));
    assert_error_response(&response, &json!(6), -32000, "record was not found");
    assert_error_data_kind(&response, "storageNotFound");
}

#[test]
fn project_open_path_for_non_git_directory_returns_structured_error() {
    // The app launcher sets TMPDIR inside the checkout. Keep this fixture
    // outside any ancestor Git worktree so it is genuinely non-Git.
    let directory = tempfile::tempdir_in("/tmp").expect("creates non-git dir outside the checkout");
    let mut core = CoreProcess::new();
    let response = core.send(json!({
        "id": 8,
        "method": "project.openPath",
        "params": {
            "path": directory.path().to_string_lossy()
        }
    }));
    assert_error_response(&response, &json!(8), -32000, "not a git repository");
    assert_error_data_kind(&response, "notGitRepository");
    assert_eq!(
        response["error"]["data"]["path"].as_str(),
        Some(directory.path().to_string_lossy().as_ref())
    );
}

#[test]
fn corrupt_legacy_registry_is_ignored() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let ottam_home = TempDir::new().expect("creates OTTAM_HOME");
    std::fs::write(ottam_home.path().join("registry.json"), "{not json")
        .expect("writes corrupt registry");

    let mut core = CoreProcess::new_in_home(ottam_home.path());
    let opened = core.open_path(repo.path());
    assert!(opened["project"]["projectID"].is_string());
    assert_eq!(
        std::fs::read_to_string(ottam_home.path().join("registry.json")).unwrap(),
        "{not json"
    );
}

#[test]
fn git_output_limit_returns_structured_error() {
    let repo = GitRepoFixture::init();
    repo.write("tracked.txt", "base\n");
    repo.commit_all("initial");
    repo.write("tracked.txt", &"changed\n".repeat(200));

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_GIT_OUTPUT_MAX_BYTES".to_string(),
        "128".to_string(),
    )]);
    let opened = core.open_path(repo.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    let response = core.send(json!({
        "id": 10,
        "method": "git.diffSnapshot",
        "params": {
            "projectSourceID": source_id
        }
    }));
    assert_error_response(&response, &json!(10), -32000, "output exceeded");
    assert_error_data_kind(&response, "gitOutputLimitExceeded");
}

#[cfg(unix)]
#[test]
fn git_output_limit_over_pipe_capacity_returns_structured_error_without_timeout() {
    let temp_dir = TempDir::new().expect("creates fake git dir");
    let fake_git = temp_dir.path().join("fake-git");
    write_executable_script(
        &fake_git,
        r###"#!/bin/sh
repo=""
if [ "$1" = "-C" ]; then
  repo="$2"
  shift 2
fi
case "$1 $2" in
  "rev-parse --show-toplevel")
    printf "%s\n" "$repo"
    ;;
  "rev-parse --abbrev-ref")
    printf "main\n"
    ;;
  "rev-parse --verify")
    printf "HEAD\n"
    ;;
  "status --porcelain=v1")
    printf "## main\n"
    ;;
  "diff --no-ext-diff")
    perl -e 'print "diff --git a/tracked.txt b/tracked.txt\n"; for (1..4096) { print "+", ("x" x 100), "\n"; }'
    ;;
  *)
    exit 0
    ;;
esac
"###,
    );

    let mut core = CoreProcess::new_with_env(vec![
        (
            "OTTAM_GIT".to_string(),
            fake_git.to_string_lossy().into_owned(),
        ),
        (
            "OTTAM_GIT_OUTPUT_MAX_BYTES".to_string(),
            "128".to_string(),
        ),
        // Far past the harness's 3s response deadline: a response can only
        // arrive in time through the limit error, never through the timeout.
        ("OTTAM_GIT_TIMEOUT_MS".to_string(), "10000".to_string()),
    ]);
    let opened = core.open_path(temp_dir.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    let response = core.send(json!({
        "id": 13,
        "method": "git.diffSnapshot",
        "params": {
            "projectSourceID": source_id
        }
    }));
    assert_error_response(&response, &json!(13), -32000, "output exceeded");
    assert_error_data_kind(&response, "gitOutputLimitExceeded");
}

#[cfg(unix)]
#[test]
fn slow_git_output_over_limit_returns_output_limit_before_timeout() {
    let temp_dir = TempDir::new().expect("creates fake git dir");
    let fake_git = temp_dir.path().join("fake-git");
    write_executable_script(
        &fake_git,
        r###"#!/bin/sh
repo=""
if [ "$1" = "-C" ]; then
  repo="$2"
  shift 2
fi
case "$1 $2" in
  "rev-parse --show-toplevel")
    printf "%s\n" "$repo"
    ;;
  "rev-parse --abbrev-ref")
    printf "main\n"
    ;;
  "rev-parse --verify")
    printf "HEAD\n"
    ;;
  "status --porcelain=v1")
    printf "## main\n"
    ;;
  "diff --no-ext-diff")
    printf "diff --git a/tracked.txt b/tracked.txt\n"
    i=0
    while [ "$i" -lt 4 ]; do
      printf "+xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n"
      i=$((i + 1))
    done
    sleep 10
    ;;
  *)
    exit 0
    ;;
esac
"###,
    );

    let mut core = CoreProcess::new_with_env(vec![
        (
            "OTTAM_GIT".to_string(),
            fake_git.to_string_lossy().into_owned(),
        ),
        (
            "OTTAM_GIT_OUTPUT_MAX_BYTES".to_string(),
            "128".to_string(),
        ),
        // The git timeout and the script's sleep both sit far past the
        // harness's 3s response deadline: a response can only arrive in time
        // through the limit-triggered kill, never through the git timeout.
        ("OTTAM_GIT_TIMEOUT_MS".to_string(), "10000".to_string()),
    ]);
    let opened = core.open_path(temp_dir.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    let response = core.send(json!({
        "id": 15,
        "method": "git.diffSnapshot",
        "params": {
            "projectSourceID": source_id
        }
    }));
    assert_error_response(&response, &json!(15), -32000, "output exceeded");
    assert_error_data_kind(&response, "gitOutputLimitExceeded");
}

#[cfg(unix)]
#[test]
fn git_timeout_returns_structured_error() {
    let temp_dir = TempDir::new().expect("creates fake git dir");
    let fake_git = temp_dir.path().join("fake-git");
    std::fs::write(&fake_git, "#!/bin/sh\nsleep 2\n").expect("writes fake git");
    let mut permissions = std::fs::metadata(&fake_git)
        .expect("reads fake git metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_git, permissions).expect("marks fake git executable");

    let mut core = CoreProcess::new_with_env(vec![
        (
            "OTTAM_GIT".to_string(),
            fake_git.to_string_lossy().into_owned(),
        ),
        ("OTTAM_GIT_TIMEOUT_MS".to_string(), "25".to_string()),
    ]);
    let response = core.send(json!({
        "id": 11,
        "method": "project.openPath",
        "params": {
            "path": temp_dir.path().to_string_lossy()
        }
    }));
    assert_error_response(&response, &json!(11), -32000, "timed out");
    assert_error_data_kind(&response, "gitTimeout");
}

#[cfg(unix)]
#[test]
fn git_does_not_read_core_stdin() {
    // Core's stdin is the JSONL request stream. A git that reads stdin must see
    // EOF, not block on (or consume) the client's next request.
    let temp_dir = TempDir::new().expect("creates fake git dir");
    let fake_git = temp_dir.path().join("fake-git");
    write_executable_script(
        &fake_git,
        r###"#!/bin/sh
cat >/dev/null
repo=""
if [ "$1" = "-C" ]; then
  repo="$2"
  shift 2
fi
case "$1 $2" in
  "rev-parse --show-toplevel")
    printf "%s\n" "$repo"
    ;;
  "rev-parse --abbrev-ref")
    printf "main\n"
    ;;
  "rev-parse --verify")
    printf "HEAD\n"
    ;;
  *)
    exit 0
    ;;
esac
"###,
    );

    let mut core = CoreProcess::new_with_env(vec![
        (
            "OTTAM_GIT".to_string(),
            fake_git.to_string_lossy().into_owned(),
        ),
        // Past the harness's 3s response deadline: an inherited stdin blocks
        // `cat` until the timeout, so only a null stdin answers in time.
        ("OTTAM_GIT_TIMEOUT_MS".to_string(), "10000".to_string()),
    ]);
    let opened = core.open_path(temp_dir.path());
    assert!(
        opened["source"]["projectSourceID"].as_str().is_some(),
        "project opens: {opened}"
    );
}

#[cfg(unix)]
#[test]
fn missing_git_executable_returns_git_command_error() {
    let temp_dir = TempDir::new().expect("creates workspace dir");
    let missing_git = temp_dir.path().join("missing-git");

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_GIT".to_string(),
        missing_git.to_string_lossy().into_owned(),
    )]);
    let response = core.send(json!({
        "id": 14,
        "method": "project.openPath",
        "params": {
            "path": temp_dir.path().to_string_lossy()
        }
    }));
    assert_error_response(&response, &json!(14), -32000, "failed to run git");
    assert_error_data_kind(&response, "gitCommandFailed");
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
