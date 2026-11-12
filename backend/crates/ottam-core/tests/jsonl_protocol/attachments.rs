use crate::support::{
    assert_error_data_kind, assert_success_response, CoreProcess, FakeAcpRuntime, GitRepoFixture,
};
use ottam_core::storage::schema::conversation_attachments;
use diesel::connection::{Connection, SimpleConnection};
use diesel::sqlite::SqliteConnection;
use diesel::{QueryDsl, RunQueryDsl};
use serde_json::json;
use std::path::Path;

const PNG_HEADER: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn write_png(directory: &Path, name: &str) -> String {
    let mut bytes = PNG_HEADER.to_vec();
    bytes.extend_from_slice(b"ottam test image payload");
    let path = directory.join(name);
    std::fs::write(&path, bytes).expect("writes a test PNG");
    path.to_string_lossy().into_owned()
}

fn write_image(directory: &Path, name: &str, bytes: &[u8]) -> String {
    let path = directory.join(name);
    std::fs::write(&path, bytes).expect("writes test image");
    path.to_string_lossy().into_owned()
}

fn create_workspace(core: &mut CoreProcess, id: i64) -> String {
    let created = core.send(json!({
        "id": id,
        "method": "workspace.create",
        "params": {"displayName": "Attachments"}
    }));
    assert_success_response(&created, &json!(id));
    created["result"]["workspace"]["workspaceID"]
        .as_str()
        .expect("workspaceID")
        .to_string()
}

#[test]
fn create_attachment_accepts_supported_signatures_and_source_metadata() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 880);

    let cases = [
        (
            "png",
            "image/png",
            PNG_HEADER.as_slice(),
            "pasted",
            None,
            ".png",
        ),
        (
            "jpeg",
            "image/jpeg",
            &[0xff, 0xd8, 0xff, 0x00],
            "dropped",
            Some("/tmp/dropped.jpg"),
            ".jpg",
        ),
        (
            "gif",
            "image/gif",
            b"GIF89a",
            "picked",
            Some("/tmp/picked.gif"),
            ".gif",
        ),
        ("gif87a", "image/gif", b"GIF87a", "pasted", None, ".gif"),
        (
            "webp",
            "image/webp",
            b"RIFF\x04\x00\x00\x00WEBP",
            "pasted",
            None,
            ".webp",
        ),
    ];

    for (index, (name, mime, bytes, source_kind, origin_path, extension)) in
        cases.into_iter().enumerate()
    {
        let source_path = write_image(scratch.path(), &format!("source-{name}"), bytes);
        let response = core.send(json!({
            "id": 881 + index as i64,
            "method": "conversation.createAttachment",
            "params": {
                "workspaceID": workspace_id,
                "sourcePath": source_path,
                "mimeType": mime,
                "width": 10,
                "height": 20,
                "sourceKind": source_kind,
                "originPath": origin_path,
                "idempotencyKey": format!("signature-{name}")
            }
        }));
        assert_success_response(&response, &json!(881 + index as i64));
        let attachment = &response["result"]["attachment"];
        assert_eq!(attachment["mimeType"], json!(mime));
        assert_eq!(attachment["sourceKind"], json!(source_kind));
        assert_eq!(attachment["originPath"], json!(origin_path));
        let stored_path = attachment["path"].as_str().expect("stored path");
        assert!(
            stored_path.ends_with(extension),
            "unexpected path: {stored_path}"
        );
        assert_eq!(std::fs::read(stored_path).expect("stored bytes"), bytes);
    }

    for (index, (source_kind, origin_path)) in [
        ("pasted", Some("/tmp/pasted.png")),
        ("dropped", None),
        ("picked", None),
    ]
    .into_iter()
    .enumerate()
    {
        let source_path = write_png(scratch.path(), &format!("invalid-origin-{index}.png"));
        let response = core.send(json!({
            "id": 890 + index as i64,
            "method": "conversation.createAttachment",
            "params": {
                "workspaceID": workspace_id,
                "sourcePath": source_path,
                "mimeType": "image/png",
                "width": 10,
                "height": 20,
                "sourceKind": source_kind,
                "originPath": origin_path,
                "idempotencyKey": format!("invalid-origin-{index}")
            }
        }));
        assert_error_data_kind(&response, "storageConstraint");
    }

    for (index, bytes) in [b"RIFF".as_slice(), b"RIFF\x04\x00\x00\x00NOPE"]
        .into_iter()
        .enumerate()
    {
        let source_path = write_image(scratch.path(), &format!("invalid-webp-{index}"), bytes);
        let response = core.send(json!({
            "id": 893 + index as i64,
            "method": "conversation.createAttachment",
            "params": {
                "workspaceID": workspace_id,
                "sourcePath": source_path,
                "mimeType": "image/webp",
                "width": 10,
                "height": 20,
                "sourceKind": "pasted",
                "idempotencyKey": format!("invalid-webp-{index}")
            }
        }));
        assert_error_data_kind(&response, "attachmentTypeUnsupported");
    }
}

#[test]
fn attachment_dimensions_are_clamped_and_oversize_bytes_are_rejected() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 895);

    let source_path = write_png(scratch.path(), "dimensions.png");
    let clamped = core.send(json!({
        "id": 896,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 0,
            "height": u32::MAX,
            "sourceKind": "pasted",
            "idempotencyKey": "clamped-dimensions"
        }
    }));
    assert_success_response(&clamped, &json!(896));
    assert_eq!(clamped["result"]["attachment"]["width"], json!(1));
    assert_eq!(clamped["result"]["attachment"]["height"], json!(20_000));

    let oversize_path = scratch.path().join("oversize.png");
    let oversize = std::fs::File::create(&oversize_path).expect("creates oversize source");
    oversize
        .set_len((3 * 1024 * 1024 + 1) as u64)
        .expect("sizes oversize source");
    let rejected = core.send(json!({
        "id": 897,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": oversize_path,
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "oversize-bytes"
        }
    }));
    assert_error_data_kind(&rejected, "attachmentTooLarge");
    let attachment_path = clamped["result"]["attachment"]["path"]
        .as_str()
        .expect("clamped attachment path");
    let attachments = Path::new(attachment_path)
        .parent()
        .expect("attachment directory");
    assert_eq!(
        std::fs::read_dir(attachments)
            .expect("lists attachments")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .count(),
        1,
        "oversize source wrote attachment bytes"
    );
}

#[test]
fn failed_attachment_row_insert_removes_the_file_written_first() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let home = tempfile::TempDir::new().expect("home");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();
    let workspace_id = create_workspace(&mut core, 898);

    let database_path = home.path().join("ottam.sqlite");
    let mut connection =
        SqliteConnection::establish(database_path.to_str().expect("UTF-8 database path"))
            .expect("opens database directly");
    connection
        .batch_execute(
            "CREATE TRIGGER fail_attachment_insert \
             BEFORE INSERT ON conversation_attachments \
             BEGIN SELECT RAISE(ABORT, 'forced attachment insert failure'); END;",
        )
        .expect("installs failure trigger");
    drop(connection);

    let source_path = write_png(scratch.path(), "insert-failure.png");
    let failed = core.send(json!({
        "id": 899,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "insert-failure"
        }
    }));
    assert_eq!(failed["id"], json!(899));
    assert!(
        failed.get("error").is_some(),
        "insert unexpectedly succeeded: {failed}"
    );
    assert!(
        failed.get("result").is_none(),
        "failure also carried a result: {failed}"
    );

    let attachments = home
        .path()
        .join("workspaces")
        .join(&workspace_id)
        .join("attachments");
    let stored_files = std::fs::read_dir(&attachments)
        .expect("attachment directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    assert!(
        stored_files.is_empty(),
        "row insert failure left unreferenced bytes: {stored_files:?}"
    );
    assert!(Path::new(&source_path).is_file(), "source file was moved");

    let mut connection =
        SqliteConnection::establish(database_path.to_str().expect("UTF-8 database path"))
            .expect("reopens database directly");
    let row_count = conversation_attachments::table
        .count()
        .get_result::<i64>(&mut connection)
        .expect("counts attachment rows");
    assert_eq!(row_count, 0, "failed insert left a durable row");
    connection
        .batch_execute("DROP TRIGGER fail_attachment_insert;")
        .expect("removes failure trigger");
    drop(connection);
    let retried = core.send(json!({
        "id": 900,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "insert-failure"
        }
    }));
    assert_success_response(&retried, &json!(900));
}

#[test]
fn attachments_are_copied_into_the_workspace_store_and_ride_the_message() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 900);

    let source_path = write_png(scratch.path(), "pasted.png");
    let created = core.send(json!({
        "id": 901,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 1200,
            "height": 800,
            "sourceKind": "pasted",
            "idempotencyKey": "attachment-key-1"
        }
    }));
    assert_success_response(&created, &json!(901));
    let attachment = &created["result"]["attachment"];
    let attachment_id = attachment["attachmentID"].as_str().expect("attachmentID");
    let stored_path = attachment["path"].as_str().expect("path");
    assert_eq!(attachment["mimeType"].as_str(), Some("image/png"));
    assert_eq!(attachment["width"].as_u64(), Some(1200));
    assert!(
        stored_path.contains(&format!("workspaces/{workspace_id}/attachments")),
        "attachment bytes live in the Workspace store, not the working root: {stored_path}"
    );
    assert!(Path::new(stored_path).is_file());
    assert!(
        Path::new(&source_path).is_file(),
        "Core copies the source; a move would unlink the caller's own file"
    );
    std::fs::remove_file(&source_path).expect("simulates caller cleanup after a lost response");
    let retried = core.send(json!({
        "id": 914,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 1200,
            "height": 800,
            "sourceKind": "pasted",
            "idempotencyKey": "attachment-key-1"
        }
    }));
    assert_success_response(&retried, &json!(914));
    assert_eq!(
        retried["result"]["attachment"]["attachmentID"], attachment["attachmentID"],
        "a retried attachment request must return the original draft"
    );
    assert_eq!(retried["result"]["attachment"]["path"], attachment["path"]);

    let conversation = core.send(json!({
        "id": 902,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {"kind": "workspaceFiles"},
            "message": {
                "body": "",
                "attachmentIDs": [attachment_id],
                "idempotencyKey": "attachment-conversation-1" // gitleaks:allow
            }
        }
    }));
    assert_success_response(&conversation, &json!(902));
    let conversation_id = conversation["result"]["conversation"]["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    // An image on its own is a complete message.
    let messages = conversation["result"]["conversation"]["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["body"].as_str(), Some(""));
    assert_eq!(
        messages[0]["attachments"][0]["attachmentID"].as_str(),
        Some(attachment_id)
    );

    // A second attachment posted with a later message keeps composer order.
    let first = write_png(scratch.path(), "first.png");
    let second = write_png(scratch.path(), "second.png");
    let mut ids = Vec::new();
    for (index, path) in [first, second].into_iter().enumerate() {
        let response = core.send(json!({
            "id": 910 + index as i64,
            "method": "conversation.createAttachment",
            "params": {
                "workspaceID": workspace_id,
                "sourcePath": path,
                "mimeType": "image/png",
                "width": 100,
                "height": 100,
                "sourceKind": "dropped",
                "originPath": "/Users/someone/shot.png",
                "idempotencyKey": format!("later-attachment-key-{index}")
            }
        }));
        assert_success_response(&response, &json!(910 + index as i64));
        ids.push(
            response["result"]["attachment"]["attachmentID"]
                .as_str()
                .expect("attachmentID")
                .to_string(),
        );
    }
    let posted = core.send(json!({
        "id": 912,
        "method": "conversation.postMessage",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "message": "and these two",
            "attachmentIDs": [ids[1], ids[0]],
            "idempotencyKey": "attachment-post-1"
        }
    }));
    assert_success_response(&posted, &json!(912));
    let attachments = posted["result"]["message"]["attachments"]
        .as_array()
        .expect("attachments");
    assert_eq!(
        attachments
            .iter()
            .map(|item| item["attachmentID"].as_str().expect("attachmentID"))
            .collect::<Vec<_>>(),
        vec![ids[1].as_str(), ids[0].as_str()],
        "attachments come back in the order the composer sent, not paste order"
    );
    let page = core.send(json!({
        "id": 918,
        "method": "conversation.listMessages",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "limit": 50
        }
    }));
    assert_success_response(&page, &json!(918));
    let page_messages = page["result"]["page"]["messages"]
        .as_array()
        .expect("message page");
    assert_eq!(page_messages.len(), 2);
    assert_eq!(
        page_messages[0]["attachments"]
            .as_array()
            .expect("later attachments")
            .iter()
            .map(|item| item["attachmentID"].as_str().expect("attachmentID"))
            .collect::<Vec<_>>(),
        vec![ids[1].as_str(), ids[0].as_str()]
    );
    assert_eq!(
        page_messages[1]["attachments"][0]["attachmentID"].as_str(),
        Some(attachment_id)
    );
    let exact_replay = core.send(json!({
        "id": 915,
        "method": "conversation.postMessage",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "message": "and these two",
            "attachmentIDs": [ids[1], ids[0]],
            "idempotencyKey": "attachment-post-1"
        }
    }));
    assert_success_response(&exact_replay, &json!(915));

    let changed = write_png(scratch.path(), "changed.png");
    let changed_attachment = core.send(json!({
        "id": 916,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": changed,
            "mimeType": "image/png",
            "width": 100,
            "height": 100,
            "sourceKind": "pasted",
            "idempotencyKey": "attachment-key-changed"
        }
    }));
    assert_success_response(&changed_attachment, &json!(916));
    let changed_id = changed_attachment["result"]["attachment"]["attachmentID"]
        .as_str()
        .expect("changed attachment ID");
    let conflicting_replay = core.send(json!({
        "id": 917,
        "method": "conversation.postMessage",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "message": "and these two",
            "attachmentIDs": [changed_id],
            "idempotencyKey": "attachment-post-1"
        }
    }));
    assert_error_data_kind(&conflicting_replay, "idempotencyConflict");

    // Deleting the Conversation takes the bytes with it.
    let stored = attachments
        .iter()
        .map(|item| item["path"].as_str().expect("path").to_string())
        .collect::<Vec<_>>();
    let deleted = core.send(json!({
        "id": 913,
        "method": "conversation.delete",
        "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
    }));
    assert_success_response(&deleted, &json!(913));
    for path in stored {
        assert!(
            !Path::new(&path).exists(),
            "attachment bytes outlived the Conversation: {path}"
        );
    }
}

#[test]
fn create_attachment_rejects_unsafe_paths_and_mismatched_types() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 920);

    let relative = core.send(json!({
        "id": 921,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": "relative/shot.png",
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "relative"
        }
    }));
    assert_error_data_kind(&relative, "attachmentPathUnsafe");

    let real = write_png(scratch.path(), "real.png");
    let link = scratch.path().join("link.png");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let symlinked = core.send(json!({
        "id": 922,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": link.to_string_lossy(),
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "symlink"
        }
    }));
    assert_error_data_kind(&symlinked, "attachmentPathUnsafe");

    // A file whose declared type contradicts its magic bytes never lands.
    let mismatched = core.send(json!({
        "id": 923,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": real,
            "mimeType": "image/jpeg",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "mismatch"
        }
    }));
    assert_error_data_kind(&mismatched, "attachmentTypeUnsupported");

    let text_path = scratch.path().join("notes.png");
    std::fs::write(&text_path, b"this is not an image at all").expect("writes decoy");
    let decoy = core.send(json!({
        "id": 924,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": text_path.to_string_lossy(),
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "decoy"
        }
    }));
    assert_error_data_kind(&decoy, "attachmentTypeUnsupported");
}

#[test]
fn discarding_an_attachment_removes_its_bytes_and_is_idempotent() {
    let scratch = tempfile::TempDir::new().expect("scratch");
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 930);

    let source_path = write_png(scratch.path(), "discard.png");
    let created = core.send(json!({
        "id": 931,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "discard-key"
        }
    }));
    assert_success_response(&created, &json!(931));
    let attachment_id = created["result"]["attachment"]["attachmentID"]
        .as_str()
        .expect("attachmentID")
        .to_string();
    let stored_path = created["result"]["attachment"]["path"]
        .as_str()
        .expect("path")
        .to_string();
    assert!(Path::new(&stored_path).is_file());

    let discarded = core.send(json!({
        "id": 932,
        "method": "conversation.discardAttachment",
        "params": {"workspaceID": workspace_id, "attachmentID": attachment_id}
    }));
    assert_success_response(&discarded, &json!(932));
    assert_eq!(discarded["result"]["discarded"].as_bool(), Some(true));
    assert!(!Path::new(&stored_path).exists());

    // The app retries discard on a dropped response; the retry must not error.
    let again = core.send(json!({
        "id": 933,
        "method": "conversation.discardAttachment",
        "params": {"workspaceID": workspace_id, "attachmentID": attachment_id}
    }));
    assert_success_response(&again, &json!(933));
    assert_eq!(again["result"]["discarded"].as_bool(), Some(false));
}

#[test]
fn opening_a_workspace_reaps_attachment_files_with_no_row() {
    let mut core = CoreProcess::new();
    core.initialize();
    let workspace_id = create_workspace(&mut core, 940);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source_path = write_png(scratch.path(), "kept.png");
    let created = core.send(json!({
        "id": 941,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 10,
            "height": 10,
            "sourceKind": "pasted",
            "idempotencyKey": "kept-key"
        }
    }));
    assert_success_response(&created, &json!(941));
    let stored_path = created["result"]["attachment"]["path"]
        .as_str()
        .expect("path")
        .to_string();
    let attachments_root = Path::new(&stored_path)
        .parent()
        .expect("attachments directory")
        .to_path_buf();

    // Stand in for a crash between writing the bytes and inserting the row.
    // Backdated past the sweep's grace period: a freshly written file is
    // deliberately left alone, because it could belong to a createAttachment
    // that has not reached its insert yet.
    let orphan = attachments_root.join("orphan.png");
    std::fs::write(&orphan, PNG_HEADER).expect("writes orphan");
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&orphan)
        .expect("opens orphan")
        .set_times(std::fs::FileTimes::new().set_modified(aged))
        .expect("backdates orphan");

    // A file written just now must survive: this is the concurrent-create case.
    let fresh = attachments_root.join("fresh.png");
    std::fs::write(&fresh, PNG_HEADER).expect("writes fresh stray");

    let opened = core.send(json!({
        "id": 942,
        "method": "workspace.open",
        "params": {"workspaceID": workspace_id}
    }));
    assert_success_response(&opened, &json!(942));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while orphan.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!orphan.exists(), "an unreferenced file survived the sweep");
    assert!(
        fresh.is_file(),
        "the sweep took a file young enough to belong to an in-flight create"
    );
    assert!(
        Path::new(&stored_path).is_file(),
        "the sweep took a file that still has a row"
    );
}

fn wait_for_run_status(
    core: &mut CoreProcess,
    workspace_id: &str,
    conversation_id: &str,
    run_id: &str,
    expected: &[&str],
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut last = serde_json::Value::Null;
    loop {
        let (response, _) = core.send_for_id(
            json!({
                "id": 951,
                "method": "conversation.get",
                "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
            }),
            json!(951),
        );
        assert_success_response(&response, &json!(951));
        let detail = response["result"]["conversation"].clone();
        if let Some(run) = detail["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| runs.iter().find(|run| run["runID"] == run_id))
        {
            last = run["status"].clone();
            if expected.iter().any(|status| run["status"] == *status) {
                return run.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run {run_id} never reached {expected:?}; last status was {last}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn attached_images_reach_the_agent_as_image_blocks_before_the_text() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    let runtime = FakeAcpRuntime::new();
    let mut core = CoreProcess::new_with_env(runtime.environment());
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source_path = write_png(scratch.path(), "prompt.png");
    let expected_base64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(&source_path).expect("reads test PNG"))
    };
    let created = core.send(json!({
        "id": 950,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 640,
            "height": 480,
            "sourceKind": "pasted",
            "idempotencyKey": "prompt-attachment"
        }
    }));
    assert_success_response(&created, &json!(950));
    let attachment_id = created["result"]["attachment"]["attachmentID"]
        .as_str()
        .expect("attachmentID")
        .to_string();

    let conversation = core.send(json!({
        "id": 952,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": core.project_source_id(project_id)
            },
            "message": {
                "body": "What is wrong here?",
                "attachmentIDs": [attachment_id],
                "idempotencyKey": "prompt-attachment-message"
            }
        }
    }));
    assert_success_response(&conversation, &json!(952));
    let detail = &conversation["result"]["conversation"];
    let conversation_id = detail["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = detail["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");

    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["completed"],
    );

    let prompt = runtime
        .request_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|entry| entry["method"] == "session/prompt")
        .expect("the adapter received a session/prompt");
    let blocks = prompt["params"]["prompt"]
        .as_array()
        .expect("prompt content blocks");
    assert_eq!(
        blocks
            .iter()
            .map(|block| block["type"].as_str().expect("block type"))
            .collect::<Vec<_>>(),
        vec!["image", "text"],
        "images lead, because the text refers back to them"
    );
    assert_eq!(blocks[0]["mimeType"].as_str(), Some("image/png"));
    assert_eq!(blocks[0]["data"].as_str(), Some(expected_base64.as_str()));
    assert!(
        blocks[0]["uri"].is_null(),
        "a pasted image has no file on the user's disk to point at"
    );
    assert!(blocks[1]["text"]
        .as_str()
        .expect("text block")
        .contains("What is wrong here?"));

    core.shutdown();
}

#[test]
fn a_model_without_vision_surfaces_as_a_failed_run() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "ottam\n");
    repo.commit_all("initial");

    // ACP cannot tell us a model lacks vision ahead of time — the refusal only
    // arrives at prompt time, and has to land as a failed run.
    let runtime = FakeAcpRuntime::new();
    let mut environment = runtime.environment();
    environment.push((
        "OTTAM_FAKE_ACP_REJECT_IMAGE".to_string(),
        "1".to_string(),
    ));
    let mut core = CoreProcess::new_with_env(environment);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source_path = write_png(scratch.path(), "refused.png");
    let created = core.send(json!({
        "id": 960,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 64,
            "height": 64,
            "sourceKind": "pasted",
            "idempotencyKey": "refused-attachment"
        }
    }));
    assert_success_response(&created, &json!(960));
    let attachment_id = created["result"]["attachment"]["attachmentID"]
        .as_str()
        .expect("attachmentID")
        .to_string();

    let conversation = core.send(json!({
        "id": 961,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": project_id,
                "sourceID": core.project_source_id(project_id)
            },
            "message": {
                "body": "Look at this",
                "attachmentIDs": [attachment_id],
                "idempotencyKey": "refused-message"
            }
        }
    }));
    assert_success_response(&conversation, &json!(961));
    let detail = &conversation["result"]["conversation"];
    let conversation_id = detail["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    let input_id = detail["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input");

    let started = core.start_conversation_run(project_id, conversation_id, "codex", input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID");
    let run = wait_for_run_status(
        &mut core,
        &workspace_id,
        conversation_id,
        run_id,
        &["failed"],
    );
    let failure = run.to_string();
    assert!(
        failure.contains("does not support image input"),
        "the adapter's own words reach the transcript: {failure}"
    );

    core.shutdown();
}

/// Steering accepts attachments too, and the claim has to land on the steer
/// Message — otherwise the IDs cross the wire, are silently ignored, and the
/// bytes sit unclaimed until the TTL sweep takes them.
#[cfg(unix)]
#[test]
fn steering_claims_its_attachments_onto_the_steer_message() {
    let repo = GitRepoFixture::init();
    repo.write("README.md", "steering with images\n");
    repo.commit_all("initial");
    let mut core = CoreProcess::new_with_env(vec![(
        "OTTAM_TEST_NATIVE_STEER_ENGINE".to_string(),
        "1".to_string(),
    )]);
    core.initialize();
    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"].as_str().expect("projectID");
    let workspace_id = core.workspace_id(project_id);

    let scratch = tempfile::TempDir::new().expect("scratch");
    let source_path = write_png(scratch.path(), "steer.png");
    let created = core.send(json!({
        "id": 960,
        "method": "conversation.createAttachment",
        "params": {
            "workspaceID": workspace_id,
            "sourcePath": source_path,
            "mimeType": "image/png",
            "width": 100,
            "height": 80,
            "sourceKind": "pasted",
            "idempotencyKey": "steer-attachment"
        }
    }));
    assert_success_response(&created, &json!(960));
    let attachment_id = created["result"]["attachment"]["attachmentID"]
        .as_str()
        .expect("attachmentID")
        .to_string();

    let conversation = core.create_conversation(
        project_id,
        Some("Steer with an image"),
        Some("Hold the native test turn"),
    );
    let conversation_id = conversation["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();
    let input_id = conversation["latestRunInputPage"]["runInputs"][0]["runInputID"]
        .as_str()
        .expect("initial input")
        .to_string();
    let started =
        core.start_conversation_run(project_id, &conversation_id, "test.native", &input_id);
    let run_id = started["run"]["runID"].as_str().expect("runID").to_string();
    wait_for_run_status(
        &mut core,
        &workspace_id,
        &conversation_id,
        &run_id,
        &["running"],
    );

    let steered = core.send(json!({
        "id": 961,
        "method": "conversation.steerRun",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id,
            "runID": run_id,
            "message": "Look at this instead",
            "attachmentIDs": [attachment_id],
            "idempotencyKey": "steer-with-attachment"
        }
    }));
    assert_success_response(&steered, &json!(961));

    let detail = core.send(json!({
        "id": 962,
        "method": "conversation.get",
        "params": {"workspaceID": workspace_id, "conversationID": conversation_id}
    }));
    assert_success_response(&detail, &json!(962));
    let steer_message = detail["result"]["conversation"]["latestMessagePage"]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["body"] == "Look at this instead")
        .expect("the steer message is persisted");
    let attachments = steer_message["attachments"]
        .as_array()
        .expect("attachments");
    assert_eq!(
        attachments.len(),
        1,
        "the steer message should own the attachment it was sent with"
    );
    assert_eq!(attachments[0]["attachmentID"], json!(attachment_id));

    core.shutdown();
}
