mod support;

use diesel::connection::SimpleConnection;
use diesel::{Connection, ExpressionMethods, QueryDsl, RunQueryDsl};
use serde_json::{json, Value};
use support::{
    assert_error_data_kind, assert_error_response, assert_success_response, CoreProcess,
    GitRepoFixture,
};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug)]
enum WorkingRootParityCase {
    WorkspaceFiles,
    AvailableProject,
    NoPrimaryLink,
    MissingProject,
    MissingSource,
    RemovedMembership,
    ArchivedSource,
    CorruptSource,
}

#[test]
fn workspace_protocol_manages_windows_memberships_and_restart_identity() {
    let repo_a = GitRepoFixture::init();
    let repo_b = GitRepoFixture::init();
    let repo_c = GitRepoFixture::init();
    repo_a.commit_empty("initial A");
    repo_b.commit_empty("initial B");
    repo_c.commit_empty("initial C");
    let home = TempDir::new().expect("home");
    let mut core = CoreProcess::new_in_home(home.path());
    let initialized = core.initialize();
    let first_core_id = initialized["result"]["coreInstanceID"]
        .as_str()
        .expect("CoreInstanceID")
        .to_string();

    let opened_a = core.open_path(repo_a.path());
    let project_a = opened_a["project"]["projectID"].as_str().unwrap();
    let default_workspace = opened_a["createdWorkspace"]["workspaceID"]
        .as_str()
        .expect("default Workspace")
        .to_string();
    assert_eq!(opened_a["workspaces"].as_array().unwrap().len(), 1);

    let created = core.send(json!({
        "id": 30,
        "method": "workspace.create",
        "params": {"displayName": "Delivery"}
    }));
    assert_success_response(&created, &json!(30));
    let delivery_workspace = created["result"]["workspace"]["workspaceID"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(created["result"]["projects"].as_array().unwrap().is_empty());

    let added_a = core.send(json!({
        "id": 31,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": delivery_workspace,
            "path": repo_a.path().to_string_lossy()
        }
    }));
    assert_success_response(&added_a, &json!(31));
    assert_eq!(added_a["result"]["projectID"], project_a);
    assert_eq!(
        added_a["result"]["workspace"]["projects"][0]["sortOrder"],
        0
    );
    let added_a_at = added_a["result"]["workspace"]["projects"][0]["addedAt"].clone();

    let added_b = core.send(json!({
        "id": 32,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": delivery_workspace,
            "path": repo_b.path().to_string_lossy()
        }
    }));
    assert_success_response(&added_b, &json!(32));
    let project_b = added_b["result"]["projectID"].as_str().unwrap().to_string();
    assert_eq!(
        added_b["result"]["workspace"]["projects"][1]["sortOrder"],
        1
    );

    let idempotent = core.send(json!({
        "id": 33,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": delivery_workspace,
            "path": repo_a.path().to_string_lossy()
        }
    }));
    assert_success_response(&idempotent, &json!(33));
    assert_eq!(
        idempotent["result"]["workspace"]["projects"][0]["sortOrder"],
        0
    );
    assert_eq!(
        idempotent["result"]["workspace"]["projects"][1]["sortOrder"],
        1
    );
    assert_eq!(
        idempotent["result"]["workspace"]["projects"][0]["addedAt"],
        added_a_at
    );

    let removed_a = core.send(json!({
        "id": 330,
        "method": "workspace.removeProject",
        "params": {
            "workspaceID": delivery_workspace,
            "projectID": project_a
        }
    }));
    assert_success_response(&removed_a, &json!(330));
    assert_eq!(
        removed_a["result"]["projects"][0]["project"]["projectID"],
        project_b
    );

    let readded_a = core.send(json!({
        "id": 331,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": delivery_workspace,
            "path": repo_a.path().to_string_lossy()
        }
    }));
    assert_success_response(&readded_a, &json!(331));
    assert_eq!(
        readded_a["result"]["workspace"]["projects"][0]["project"]["projectID"],
        project_b
    );
    assert_eq!(
        readded_a["result"]["workspace"]["projects"][1]["project"]["projectID"],
        project_a
    );
    assert_eq!(
        readded_a["result"]["workspace"]["projects"][1]["sortOrder"],
        2
    );

    let reopened_a = core.open_path(repo_a.path());
    assert!(reopened_a["createdWorkspace"].is_null());
    assert_eq!(reopened_a["workspaces"].as_array().unwrap().len(), 2);

    let renamed = core.send(json!({
        "id": 34,
        "method": "workspace.update",
        "params": {
            "workspaceID": delivery_workspace,
            "displayName": "Release"
        }
    }));
    assert_success_response(&renamed, &json!(34));
    assert_eq!(renamed["result"]["workspace"]["displayName"], "Release");

    let removed = core.send(json!({
        "id": 35,
        "method": "workspace.removeProject",
        "params": {
            "workspaceID": delivery_workspace,
            "projectID": project_b
        }
    }));
    assert_success_response(&removed, &json!(35));
    assert_eq!(removed["result"]["projects"].as_array().unwrap().len(), 1);

    let archived = core.send(json!({
        "id": 36,
        "method": "workspace.archive",
        "params": {"workspaceID": delivery_workspace}
    }));
    assert_success_response(&archived, &json!(36));
    assert!(archived["result"]["archivedAt"].is_number());

    let open_archived = core.send(json!({
        "id": 37,
        "method": "workspace.open",
        "params": {"workspaceID": delivery_workspace}
    }));
    assert_error_response(&open_archived, &json!(37), -32000, "archived");
    assert_error_data_kind(&open_archived, "storageConstraint");

    let add_to_archived = core.send(json!({
        "id": 371,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": delivery_workspace,
            "path": repo_a.path().to_string_lossy()
        }
    }));
    assert_error_response(&add_to_archived, &json!(371), -32000, "archived");
    assert_error_data_kind(&add_to_archived, "storageConstraint");

    let cross_workspace_remove = core.send(json!({
        "id": 372,
        "method": "workspace.removeProject",
        "params": {
            "workspaceID": default_workspace,
            "projectID": project_b
        }
    }));
    assert_error_response(&cross_workspace_remove, &json!(372), -32000, "not found");
    assert_error_data_kind(&cross_workspace_remove, "storageNotFound");

    let listed = core.send(json!({
        "id": 38,
        "method": "workspace.list",
        "params": {"includeArchived": true}
    }));
    assert_success_response(&listed, &json!(38));
    let listed = listed["result"]["workspaces"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    // The list row carries the same paths the detail does, removal included.
    let delivery_row = listed
        .iter()
        .find(|item| item["workspace"]["workspaceID"] == delivery_workspace)
        .expect("archived Workspace is listed");
    assert_eq!(
        delivery_row["projectPaths"],
        json!([removed["result"]["projects"][0]["source"]["canonicalPath"]])
    );

    let restored = core.send(json!({
        "id": 39,
        "method": "workspace.restore",
        "params": {"workspaceID": delivery_workspace}
    }));
    assert_success_response(&restored, &json!(39));
    assert!(restored["result"]["archivedAt"].is_null());

    let unknown_workspace = "019f0000-0000-7000-8000-000000000099";
    let rolled_back_add = core.send(json!({
        "id": 391,
        "method": "workspace.addProject",
        "params": {
            "workspaceID": unknown_workspace,
            "path": repo_c.path().to_string_lossy()
        }
    }));
    assert_error_response(&rolled_back_add, &json!(391), -32000, "not found");
    assert_error_data_kind(&rolled_back_add, "storageNotFound");
    let opened_c = core.open_path(repo_c.path());
    assert!(
        opened_c["createdWorkspace"].is_object(),
        "failed add must roll back Project creation"
    );

    core.shutdown();

    let mut restarted = CoreProcess::new_in_home(home.path());
    let initialized = restarted.initialize();
    let second_core_id = initialized["result"]["coreInstanceID"]
        .as_str()
        .expect("CoreInstanceID");
    assert_ne!(first_core_id, second_core_id);
    let opened = restarted.send(json!({
        "id": 40,
        "method": "workspace.open",
        "params": {"workspaceID": delivery_workspace}
    }));
    assert_success_response(&opened, &json!(40));
    assert_eq!(opened["result"]["workspace"]["displayName"], "Release");
    assert_eq!(opened["result"]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(
        opened["result"]["projects"][0]["project"]["projectID"],
        project_a
    );
    assert_ne!(default_workspace, delivery_workspace);
}

/// Project membership is presentation state; a stored `working_root` is durable.
/// Adding or removing a project reshapes what the sidebar renders and what the
/// UI offers, but must never rewrite where an existing Conversation lives —
/// otherwise a chat's transcript would no longer match its root.
#[test]
fn adding_or_removing_a_project_never_rewrites_a_stored_working_root() {
    let repo_a = GitRepoFixture::init();
    let repo_b = GitRepoFixture::init();
    repo_a.commit_empty("initial A");
    repo_b.commit_empty("initial B");
    let home = TempDir::new().expect("home");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();

    let opened_a = core.open_path(repo_a.path());
    let project_a = opened_a["project"]["projectID"]
        .as_str()
        .unwrap()
        .to_string();
    let source_a = opened_a["source"]["projectSourceID"]
        .as_str()
        .unwrap()
        .to_string();
    let workspace = opened_a["createdWorkspace"]["workspaceID"]
        .as_str()
        .expect("default Workspace")
        .to_string();

    // One conversation at the project root and one at Workspace files — the
    // mixed population a single-project Workspace actually holds.
    let create = |core: &mut CoreProcess, id: i64, title: &str, root: Value| {
        let response = core.send(json!({
            "id": id,
            "method": "conversation.create",
            "params": {"workspaceID": workspace, "workingRoot": root, "title": title}
        }));
        assert_success_response(&response, &json!(id));
        response["result"]["conversation"]["conversation"]["conversationID"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let project_chat = create(
        &mut core,
        60,
        "rooted at the project",
        json!({"kind": "project", "projectID": project_a, "sourceID": source_a}),
    );
    let files_chat = create(
        &mut core,
        61,
        "rooted at workspace files",
        json!({"kind": "workspaceFiles"}),
    );

    let roots_now = |core: &mut CoreProcess| -> Vec<(String, Value)> {
        let response = core.send(json!({
            "id": 62,
            "method": "conversation.list",
            "params": {"workspaceID": workspace}
        }));
        assert_success_response(&response, &json!(62));
        let mut rows: Vec<(String, Value)> = response["result"]["conversations"]
            .as_array()
            .expect("conversations")
            .iter()
            .map(|row| {
                (
                    row["conversationID"].as_str().unwrap().to_string(),
                    row["workingRoot"].clone(),
                )
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    };

    let before = roots_now(&mut core);
    assert_eq!(before.len(), 2);
    assert!(before.iter().any(|(id, _)| id == &project_chat));
    assert!(before.iter().any(|(id, _)| id == &files_chat));

    // 1 -> 2 projects: the chooser reappears and the sidebar regroups.
    let added = core.send(json!({
        "id": 63,
        "method": "workspace.addProject",
        "params": {"workspaceID": workspace, "path": repo_b.path().to_string_lossy()}
    }));
    assert_success_response(&added, &json!(63));
    let project_b = added["result"]["projectID"].as_str().unwrap().to_string();
    assert_eq!(
        roots_now(&mut core),
        before,
        "adding a project rewrote a stored working_root"
    );

    // 2 -> 1 projects: the layout collapses back.
    let removed = core.send(json!({
        "id": 64,
        "method": "workspace.removeProject",
        "params": {"workspaceID": workspace, "projectID": project_b}
    }));
    assert_success_response(&removed, &json!(64));
    assert_eq!(
        roots_now(&mut core),
        before,
        "removing a project rewrote a stored working_root"
    );

    // The case that actually exercises the rule: remove the project a
    // Conversation is rooted at. `availability` is derived at read time and is
    // expected to flip to `detached`, but every identity field is durable — the
    // chat still points at the same directory, which is what lets the sidebar
    // keep showing it and mark the mismatch instead of dropping it.
    let removed_a = core.send(json!({
        "id": 65,
        "method": "workspace.removeProject",
        "params": {"workspaceID": workspace, "projectID": project_a}
    }));
    assert_success_response(&removed_a, &json!(65));

    let after = roots_now(&mut core);
    assert_eq!(
        after.len(),
        before.len(),
        "removing a project dropped a Conversation from the Workspace"
    );
    for ((before_id, before_root), (after_id, after_root)) in before.iter().zip(after.iter()) {
        assert_eq!(before_id, after_id);
        for field in ["kind", "projectID", "sourceID", "canonicalPath"] {
            assert_eq!(
                before_root[field], after_root[field],
                "removing a project rewrote working_root.{field}"
            );
        }
    }
    let orphaned = after
        .iter()
        .find(|(id, _)| id == &project_chat)
        .expect("the project-rooted Conversation survives its project leaving");
    assert_eq!(orphaned.1["availability"], "detached");
    assert_eq!(orphaned.1["projectID"], project_a.as_str());

    core.shutdown();
}

/// `conversation.list` uses a batched projection while `conversation.get`
/// resolves the same root from individual repository reads. Keep those two
/// paths in lockstep for every root state a temporary database can safely
/// characterize, including dangling identities and corrupt rows.
#[test]
fn conversation_list_and_get_keep_working_root_parity() {
    for case in [
        WorkingRootParityCase::WorkspaceFiles,
        WorkingRootParityCase::AvailableProject,
        WorkingRootParityCase::NoPrimaryLink,
        WorkingRootParityCase::MissingProject,
        WorkingRootParityCase::MissingSource,
        WorkingRootParityCase::RemovedMembership,
        WorkingRootParityCase::ArchivedSource,
        WorkingRootParityCase::CorruptSource,
    ] {
        assert_working_root_parity(case);
    }
}

fn assert_working_root_parity(case: WorkingRootParityCase) {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("home");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();

    let opened = core.open_path(repo.path());
    let project_id = opened["project"]["projectID"]
        .as_str()
        .expect("projectID")
        .to_string();
    let source_id = opened["source"]["projectSourceID"]
        .as_str()
        .expect("projectSourceID")
        .to_string();
    let workspace_id = opened["createdWorkspace"]["workspaceID"]
        .as_str()
        .expect("workspaceID")
        .to_string();
    let root = match case {
        WorkingRootParityCase::WorkspaceFiles => json!({"kind": "workspaceFiles"}),
        _ => json!({
            "kind": "project",
            "projectID": project_id,
            "sourceID": source_id,
        }),
    };
    let created = core.send(json!({
        "id": 80,
        "method": "conversation.create",
        "params": {
            "workspaceID": workspace_id,
            "workingRoot": root,
            "title": format!("root parity: {case:?}")
        }
    }));
    assert_success_response(&created, &json!(80));
    let conversation_id = created["result"]["conversation"]["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID")
        .to_string();

    let database_path = home.path().join("ottam.sqlite");
    let mut connection = diesel::sqlite::SqliteConnection::establish(
        database_path.to_str().expect("UTF-8 database path"),
    )
    .expect("opens fixture database");
    // Missing Project/Source are deliberate temporary-database fixtures. The
    // production connection still has foreign keys enabled; disabling them on
    // this isolated seeding connection is what lets the read paths' current
    // dangling-identity behavior be characterized.
    connection
        .batch_execute("PRAGMA foreign_keys = OFF")
        .expect("disables fixture foreign keys");
    match case {
        WorkingRootParityCase::WorkspaceFiles | WorkingRootParityCase::AvailableProject => {}
        WorkingRootParityCase::NoPrimaryLink => {
            diesel::delete(
                ottam_core::storage::schema::conversation_projects::table.filter(
                    ottam_core::storage::schema::conversation_projects::conversation_id
                        .eq(&conversation_id),
                ),
            )
            .execute(&mut connection)
            .expect("removes primary link");
        }
        WorkingRootParityCase::MissingProject => {
            diesel::delete(ottam_core::storage::schema::projects::table.find(&project_id))
                .execute(&mut connection)
                .expect("removes Project fixture row");
        }
        WorkingRootParityCase::MissingSource => {
            diesel::delete(
                ottam_core::storage::schema::project_sources::table.find(&source_id),
            )
            .execute(&mut connection)
            .expect("removes ProjectSource fixture row");
        }
        WorkingRootParityCase::RemovedMembership => {
            diesel::update(
                ottam_core::storage::schema::workspace_projects::table
                    .find((&workspace_id, &project_id)),
            )
            .set(ottam_core::storage::schema::workspace_projects::removed_at.eq(Some(90_i64)))
            .execute(&mut connection)
            .expect("removes Workspace membership");
        }
        WorkingRootParityCase::ArchivedSource => {
            diesel::update(
                ottam_core::storage::schema::project_sources::table.find(&source_id),
            )
            .set(ottam_core::storage::schema::project_sources::archived_at.eq(Some(90_i64)))
            .execute(&mut connection)
            .expect("archives ProjectSource");
        }
        WorkingRootParityCase::CorruptSource => {
            diesel::update(
                ottam_core::storage::schema::project_sources::table.find(&source_id),
            )
            .set(ottam_core::storage::schema::project_sources::last_seen_at.eq(i64::MAX))
            .execute(&mut connection)
            .expect("corrupts ProjectSource timestamp");
        }
    }
    drop(connection);

    let listed = core.send(json!({
        "id": 81,
        "method": "conversation.list",
        "params": {"workspaceID": workspace_id}
    }));
    let fetched = core.send(json!({
        "id": 82,
        "method": "conversation.get",
        "params": {
            "workspaceID": workspace_id,
            "conversationID": conversation_id
        }
    }));

    if matches!(case, WorkingRootParityCase::CorruptSource) {
        assert_error_data_kind(&listed, "storageCorrupt");
        assert_error_data_kind(&fetched, "storageCorrupt");
        core.shutdown();
        return;
    }

    assert_success_response(&listed, &json!(81));
    assert_success_response(&fetched, &json!(82));
    let list_root = listed["result"]["conversations"][0]["workingRoot"].clone();
    let get_root = fetched["result"]["conversation"]["conversation"]["workingRoot"].clone();
    assert_eq!(list_root, get_root, "list/get root drift for {case:?}");

    match case {
        WorkingRootParityCase::WorkspaceFiles => {
            assert_eq!(list_root["kind"], "workspaceFiles");
            assert_eq!(list_root["availability"], "available");
            assert!(list_root["projectID"].is_null());
            assert!(list_root["sourceID"].is_null());
        }
        WorkingRootParityCase::AvailableProject => {
            assert_eq!(list_root["availability"], "available");
            assert_eq!(list_root["projectID"], project_id);
            assert_eq!(list_root["sourceID"], source_id);
        }
        WorkingRootParityCase::NoPrimaryLink => {
            assert_eq!(list_root["availability"], "detached");
            assert!(list_root["projectID"].is_null());
            assert!(list_root["sourceID"].is_null());
            assert_eq!(list_root["canonicalPath"], "");
        }
        WorkingRootParityCase::MissingProject => {
            assert_eq!(list_root["displayName"], "Detached project");
            assert_eq!(list_root["availability"], "available");
            assert_eq!(list_root["projectID"], project_id);
            assert_eq!(list_root["sourceID"], source_id);
        }
        WorkingRootParityCase::MissingSource => {
            assert_eq!(list_root["availability"], "detached");
            assert_eq!(list_root["projectID"], project_id);
            assert_eq!(list_root["sourceID"], source_id);
            assert_eq!(list_root["canonicalPath"], "");
        }
        WorkingRootParityCase::RemovedMembership | WorkingRootParityCase::ArchivedSource => {
            assert_eq!(list_root["availability"], "detached");
            assert_eq!(list_root["projectID"], project_id);
            assert_eq!(list_root["sourceID"], source_id);
            assert!(!list_root["canonicalPath"]
                .as_str()
                .expect("canonicalPath")
                .is_empty());
        }
        WorkingRootParityCase::CorruptSource => unreachable!(),
    }

    core.shutdown();
}

/// The other half of the single-project-workspace acceptance criteria: the app
/// resolves *which* root new work gets, and this pins that Core stores exactly
/// what it was handed — for every entry point, at every root kind, and still
/// after a restart.
///
/// Worth a test of its own because the app-side claim is unfalsifiable from the
/// UI: a composer label reading "ottam" is not evidence that `working_root`
/// says so. The two entry points are covered together because they were written
/// separately and have drifted before.
#[test]
fn every_entry_point_persists_the_working_root_it_was_created_with() {
    let repo = GitRepoFixture::init();
    repo.commit_empty("initial");
    let home = TempDir::new().expect("home");
    let mut core = CoreProcess::new_in_home(home.path());
    core.initialize();

    let opened = core.open_path(repo.path());
    let project = opened["project"]["projectID"].as_str().unwrap().to_string();
    let source = opened["source"]["projectSourceID"]
        .as_str()
        .unwrap()
        .to_string();
    let workspace = opened["createdWorkspace"]["workspaceID"]
        .as_str()
        .expect("default Workspace")
        .to_string();
    let repo_path = std::fs::canonicalize(repo.path())
        .expect("canonical repo path")
        .to_string_lossy()
        .to_string();

    let project_root = json!({"kind": "project", "projectID": project, "sourceID": source});
    let files_root = json!({"kind": "workspaceFiles"});

    let mut chat = |id: i64, title: &str, root: &Value| -> (String, Value) {
        let response = core.send(json!({
            "id": id,
            "method": "conversation.create",
            "params": {"workspaceID": workspace, "workingRoot": root, "title": title}
        }));
        assert_success_response(&response, &json!(id));
        let created = &response["result"]["conversation"]["conversation"];
        (
            created["conversationID"].as_str().unwrap().to_string(),
            created["workingRoot"].clone(),
        )
    };
    let (project_chat, project_chat_root) = chat(70, "chat at the project", &project_root);
    let (files_chat, files_chat_root) = chat(71, "chat at workspace files", &files_root);

    // `send_for_id` rather than `send` for every terminal call: opening a
    // terminal spawns a real shell that starts emitting `terminal.outputDelta`
    // immediately, and `send` buffers whatever it reads past for the *next*
    // reader to trip over. `send_for_id` drops the interleaved notifications
    // instead, which is why the rest of the suite uses it around terminals.
    let mut terminal = |id: i64, root: &Value| -> (String, String, Value) {
        let (response, _notifications) = core.send_for_id(
            json!({
                "id": id,
                "method": "terminal.open",
                "params": {"workspaceID": workspace, "workingRoot": root, "rows": 24, "cols": 80}
            }),
            json!(id),
        );
        assert_success_response(&response, &json!(id));
        (
            response["result"]["terminalID"]
                .as_str()
                .unwrap()
                .to_string(),
            response["result"]["sessionID"]
                .as_str()
                .unwrap()
                .to_string(),
            response["result"]["workingRoot"].clone(),
        )
    };
    let (project_terminal, project_session, project_terminal_root) = terminal(72, &project_root);
    let (files_terminal, files_session, files_terminal_root) = terminal(73, &files_root);

    // What each root kind resolves to, independent of what was echoed back —
    // otherwise this would only prove Core is self-consistent.
    for (label, root) in [
        ("conversation", &project_chat_root),
        ("terminal", &project_terminal_root),
    ] {
        assert_eq!(root["kind"], "project", "{label} lost its project root");
        assert_eq!(root["projectID"], project.as_str());
        assert_eq!(root["sourceID"], source.as_str());
        assert_eq!(
            root["canonicalPath"].as_str(),
            Some(repo_path.as_str()),
            "{label} rooted at the project does not point at the checkout"
        );
    }
    for (label, root) in [
        ("conversation", &files_chat_root),
        ("terminal", &files_terminal_root),
    ] {
        assert_eq!(
            root["kind"], "workspaceFiles",
            "{label} lost its files root"
        );
        assert!(
            root["projectID"].is_null(),
            "{label} at workspace files claims a project"
        );
        // The asymmetry the spec is built around: a chat rooted here cannot see
        // the repository at all, so this must never silently be the checkout.
        assert_ne!(
            root["canonicalPath"].as_str(),
            Some(repo_path.as_str()),
            "{label} at workspace files resolved to the project checkout"
        );
    }

    let conversation_roots = |core: &mut CoreProcess, id: i64| -> Vec<(String, Value)> {
        let response = core.send(json!({
            "id": id,
            "method": "conversation.list",
            "params": {"workspaceID": workspace}
        }));
        assert_success_response(&response, &json!(id));
        let mut rows: Vec<(String, Value)> = response["result"]["conversations"]
            .as_array()
            .expect("conversations")
            .iter()
            .map(|row| {
                (
                    row["conversationID"].as_str().unwrap().to_string(),
                    row["workingRoot"].clone(),
                )
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    };

    let terminal_roots = |core: &mut CoreProcess, id: i64| -> Vec<(String, Value)> {
        let (response, _notifications) = core.send_for_id(
            json!({
                "id": id,
                "method": "terminal.list",
                "params": {"workspaceID": workspace}
            }),
            json!(id),
        );
        assert_success_response(&response, &json!(id));
        let mut rows: Vec<(String, Value)> = response["result"]["terminals"]
            .as_array()
            .expect("terminals")
            .iter()
            .map(|row| {
                (
                    row["terminalID"].as_str().unwrap().to_string(),
                    row["workingRoot"].clone(),
                )
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    };

    let stored_chats = conversation_roots(&mut core, 74);
    let stored_terminals = terminal_roots(&mut core, 75);

    let root_of = |rows: &[(String, Value)], id: &str| -> Value {
        rows.iter()
            .find(|(row_id, _)| row_id == id)
            .unwrap_or_else(|| panic!("{id} missing from the listing"))
            .1
            .clone()
    };
    for (id, created, rows, label) in [
        (
            &project_chat,
            &project_chat_root,
            &stored_chats,
            "project chat",
        ),
        (&files_chat, &files_chat_root, &stored_chats, "files chat"),
        (
            &project_terminal,
            &project_terminal_root,
            &stored_terminals,
            "project terminal",
        ),
        (
            &files_terminal,
            &files_terminal_root,
            &stored_terminals,
            "files terminal",
        ),
    ] {
        assert_eq!(
            root_of(rows, id),
            *created,
            "{label} was listed with a different root than it was created with"
        );
    }

    // Quiet the shells before the remaining exchanges — a live PTY streaming
    // output is the interleaving hazard the `send_for_id` note above describes.
    for (id, session) in [(76, &project_session), (77, &files_session)] {
        let (closed, _notifications) = core.send_for_id(
            json!({"id": id, "method": "terminal.close", "params": {"sessionID": session}}),
            json!(id),
        );
        assert_success_response(&closed, &json!(id));
    }

    core.shutdown();

    // Durability, not just in-memory agreement. Terminals are deliberately not
    // re-checked here: a Core restart drops live PTY sessions, so `terminal.list`
    // is empty by design (see retained_surfaces.rs).
    let mut restarted = CoreProcess::new_in_home(home.path());
    restarted.initialize();
    let after_restart = conversation_roots(&mut restarted, 78);
    assert_eq!(
        after_restart, stored_chats,
        "a Conversation's working_root did not survive a Core restart"
    );

    restarted.shutdown();
}
