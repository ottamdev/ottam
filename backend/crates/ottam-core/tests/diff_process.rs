mod support;

use serde_json::json;
use std::time::Duration;
use support::{CoreProcess, GitRepoFixture};

#[test]
fn clean_repo_returns_empty_diff_arrays() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    assert_eq!(diff["files"].as_array().unwrap().len(), 0);
    assert_eq!(diff["untrackedFiles"].as_array().unwrap().len(), 0);
    assert_eq!(diff["omittedFiles"].as_array().unwrap().len(), 0);
}

#[test]
fn tracked_unstaged_and_staged_modifications_are_reported() {
    let repo = GitRepoFixture::init();
    repo.write("unstaged.txt", "one\n");
    repo.write("staged.txt", "alpha\n");
    repo.commit_all("initial");
    repo.write("unstaged.txt", "one\ntwo\n");
    repo.write("staged.txt", "alpha\nbeta\n");
    repo.git(["add", "staged.txt"]);

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    let paths: Vec<_> = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"unstaged.txt"));
    assert!(paths.contains(&"staged.txt"));
    assert_eq!(diff["additions"].as_i64(), Some(2));
}

#[test]
fn repo_with_no_commits_reports_staged_and_untracked_files() {
    let repo = GitRepoFixture::init();
    repo.write("staged.txt", "tracked before first commit\n");
    repo.git(["add", "staged.txt"]);
    repo.write("scratch.txt", "untracked before first commit\n");

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    let paths: Vec<_> = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"staged.txt"));
    assert!(paths.contains(&"scratch.txt"));
    assert_eq!(diff["untrackedFiles"][0].as_str(), Some("scratch.txt"));
}

#[test]
fn deleted_and_renamed_files_are_reported() {
    let repo = GitRepoFixture::init();
    repo.write("deleted.txt", "delete me\n");
    repo.write("old-name.txt", "rename me\n");
    repo.commit_all("initial");
    std::fs::remove_file(repo.path().join("deleted.txt")).unwrap();
    repo.git(["mv", "old-name.txt", "new-name.txt"]);

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    let deleted = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "deleted.txt")
        .expect("deleted file");
    assert_eq!(deleted["status"].as_str(), Some("deleted"));

    let renamed = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "new-name.txt")
        .expect("renamed file");
    assert_eq!(renamed["status"].as_str(), Some("renamed"));
    assert_eq!(renamed["oldPath"].as_str(), Some("old-name.txt"));
}

#[test]
fn untracked_text_binary_large_and_space_paths_are_classified() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    repo.write("notes/new file.txt", "hello\nworld\n");
    repo.write_bytes("image.bin", b"abc\0def");
    repo.write_bytes("large.txt", &vec![b'x'; 1_000_001]);

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    let text = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "notes/new file.txt")
        .expect("untracked text file");
    assert_eq!(text["status"].as_str(), Some("added"));

    let omitted = diff["omittedFiles"].as_array().unwrap();
    assert!(omitted
        .iter()
        .any(|file| file["path"] == "image.bin" && file["reason"] == "binary file"));
    assert!(omitted
        .iter()
        .any(|file| file["path"] == "large.txt" && file["reason"] == "file is too large"));
}

#[test]
fn git_c_style_quoted_paths_are_decoded() {
    let repo = GitRepoFixture::init();
    repo.write("quoted\tpath.txt", "one\n");
    repo.write("caf\u{e9}.txt", "one\n");
    repo.commit_all("initial");
    repo.write("quoted\tpath.txt", "one\ntwo\n");
    repo.write("caf\u{e9}.txt", "one\ntwo\n");

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    assert!(diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "quoted\tpath.txt"));
    assert!(diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "caf\u{e9}.txt"));
}

#[test]
fn renamed_git_c_style_quoted_paths_are_decoded() {
    let repo = GitRepoFixture::init();
    repo.write("old caf\u{e9}.txt", "rename me\n");
    repo.commit_all("initial");
    repo.git(["mv", "old caf\u{e9}.txt", "new caf\u{e9}.txt"]);

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    let renamed = diff["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == "new caf\u{e9}.txt")
        .expect("renamed file");
    assert_eq!(renamed["oldPath"].as_str(), Some("old caf\u{e9}.txt"));
}

#[cfg(unix)]
#[test]
fn special_untracked_files_are_omitted_without_hanging() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let link = repo.path().join("dev-null-link");
    repo.symlink(std::path::Path::new("/dev/null"), &link);
    repo.fifo("pending-input");
    let fifo_link = repo.path().join("pending-input-link");
    repo.symlink(&repo.path().join("pending-input"), &fifo_link);

    let mut core = CoreProcess::new();
    let diff = core.open_path_and_diff(repo.path());
    assert!(diff["omittedFiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "dev-null-link" && file["reason"] == "special file"));
    assert!(diff["omittedFiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["path"] == "pending-input-link" && file["reason"] == "special file"));
}

/// A snapshot spawns one `git` per untracked file. The dispatcher used to await
/// that inline, so every request sent behind it waited for the whole snapshot;
/// a few hundred untracked files stalled the UI for seconds. The snapshot now
/// answers off the dispatch loop, so later requests overtake it.
#[test]
fn a_slow_snapshot_does_not_delay_requests_sent_behind_it() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    for index in 0..400 {
        repo.write(&format!("untracked-{index}.txt"), "contents\n");
    }

    let mut core = CoreProcess::new();
    let opened = core.open_path(repo.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID")
        .to_string();

    core.write_message(json!({
        "id": 10,
        "method": "git.diffSnapshot",
        "params": {"projectSourceID": source_id}
    }));
    core.write_message(json!({"id": 11, "method": "workspace.list", "params": {}}));

    let first = core
        .read_response_timeout(Duration::from_secs(30))
        .expect("a response arrives");
    assert_eq!(
        first["id"],
        json!(11),
        "workspace.list should answer before the snapshot it was sent behind"
    );

    let (snapshot, _) = core.read_until_response_id(&json!(10), Duration::from_secs(60));
    assert_eq!(
        snapshot["result"]["untrackedFiles"]
            .as_array()
            .expect("untrackedFiles")
            .len(),
        400
    );
}

#[cfg(unix)]
#[test]
fn deferred_snapshot_queue_rejects_work_beyond_its_bound() {
    use std::os::unix::fs::PermissionsExt;

    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let scratch = tempfile::TempDir::new().expect("scratch");
    let wrapper = scratch.path().join("slow-git");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$3\" = \"status\" ]; then sleep 2; fi\nexec git \"$@\"\n",
    )
    .expect("writes git wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("makes git wrapper executable");

    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_GIT".to_string(),
        wrapper.to_string_lossy().into_owned(),
    )]);
    let opened = core.open_path(repo.path());
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID")
        .to_string();

    for id in 20..25 {
        core.write_message(json!({
            "id": id,
            "method": "git.diffSnapshot",
            "params": {"projectSourceID": source_id}
        }));
    }

    let mut saturated = 0;
    for _ in 20..25 {
        let response = core
            .read_response_timeout(Duration::from_secs(30))
            .expect("snapshot response");
        if response["error"]["code"] == -32002 {
            saturated += 1;
        }
    }
    assert!(
        saturated >= 1,
        "work beyond the four deferred slots must be rejected"
    );
}
