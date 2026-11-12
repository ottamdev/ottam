mod support;

use std::time::Duration;
use support::{CoreProcess, GitRepoFixture};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn equivalent_local_paths_keep_stable_project_and_source_identity() {
    let parent = TempDir::new().expect("creates parent tempdir");
    let repo = GitRepoFixture::init_at(parent.path().join("workspace"));
    repo.write("src/main.txt", "hello\n");
    repo.commit_all("initial");

    let nested = repo.nested_path("src");
    let symlink = parent.path().join("workspace-link");
    #[cfg(unix)]
    repo.symlink(repo.path(), &symlink);

    let ottam_home = TempDir::new().expect("creates OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_current_dir(ottam_home.path(), parent.path());
    core.initialize();
    core.send_initialized();

    let opened_root = core.open_path(repo.path());
    let opened_nested = core.open_path(&nested);
    let opened_relative = core.open_path_string("workspace");
    let mut opened = vec![opened_root, opened_nested, opened_relative];
    #[cfg(unix)]
    opened.push(core.open_path(&symlink));

    let project_id = opened[0]["project"]["projectID"]
        .as_str()
        .expect("projectID");
    let source_id = opened[0]["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID");
    assert_eq!(Uuid::parse_str(project_id).unwrap().get_version_num(), 7);
    assert_eq!(Uuid::parse_str(source_id).unwrap().get_version_num(), 7);
    for result in &opened {
        assert_eq!(result["project"]["projectID"].as_str(), Some(project_id));
        assert_eq!(
            result["source"]["projectSourceID"].as_str(),
            Some(source_id)
        );
    }
    assert!(!ottam_home.path().join("registry.json").exists());
    core.shutdown();

    let mut restarted = CoreProcess::new_in_home(ottam_home.path());
    restarted.initialize();
    let diff = restarted.diff_by_project_source_id(source_id);
    assert_eq!(
        diff["repoPath"].as_str(),
        Some(repo.path().canonicalize().unwrap().to_str().unwrap())
    );
}

#[test]
fn separate_homes_own_independent_databases() {
    let repo_a = GitRepoFixture::init();
    let repo_b = GitRepoFixture::init();
    repo_a.commit_empty("initial");
    repo_b.commit_empty("initial");
    let home_a = TempDir::new().expect("home A");
    let home_b = TempDir::new().expect("home B");
    let mut core_a = CoreProcess::new_in_home(home_a.path());
    let mut core_b = CoreProcess::new_in_home(home_b.path());

    let opened_a = core_a.open_path(repo_a.path());
    let opened_b = core_b.open_path(repo_b.path());
    assert_ne!(
        opened_a["project"]["projectID"],
        opened_b["project"]["projectID"]
    );
    assert!(home_a.path().join("ottam.sqlite").exists());
    assert!(home_b.path().join("ottam.sqlite").exists());
    assert!(!home_a.path().join("registry.json").exists());
    assert!(!home_b.path().join("registry.json").exists());
}

#[test]
fn second_core_for_the_same_home_is_rejected_before_database_access() {
    let home = TempDir::new().expect("home");
    let mut owner = CoreProcess::new_in_home(home.path());
    owner.initialize();
    let mut contender = CoreProcess::new_in_home_with_env(
        home.path(),
        vec![("OTTAM_LOG_TO_STDERR".to_string(), "1".to_string())],
    );
    let status = contender
        .wait_for_exit(Duration::from_secs(2))
        .expect("contending Core exits");
    assert!(!status.success());
    assert!(
        contender.wait_for_stderr_containing("coreAlreadyRunning", Duration::from_secs(2)),
        "contending Core should report coreAlreadyRunning, got:\n{}",
        contender.stderr_text()
    );
    // The log file is what a user actually reads when Core will not start, and
    // it is written by the same non-blocking worker as stderr. Exiting without
    // dropping the logging guard loses the reason from both.
    let logged = contender.log_text();
    assert!(
        logged.contains("coreAlreadyRunning"),
        "the fatal reason must survive into the log file, got:\n{logged}"
    );
    owner.shutdown();
}
