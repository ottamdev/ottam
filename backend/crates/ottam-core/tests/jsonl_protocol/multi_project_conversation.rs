use crate::support::{assert_success_response, CoreProcess, FakeAcpRuntime, GitRepoFixture};
use ottam_core::storage::schema::conversation_run_sources;
use ottam_core::storage::Database;
use diesel::prelude::*;
use serde_json::json;
use std::collections::HashSet;

#[cfg(unix)]
#[tokio::test]
async fn conversation_snapshots_two_projects_and_stays_within_its_workspace() {
    let primary = GitRepoFixture::init();
    primary.write("primary.txt", "primary\n");
    primary.commit_all("primary");
    let context = GitRepoFixture::init();
    context.write("context.txt", "context\n");
    context.commit_all("context");
    let runtime = FakeAcpRuntime::new();
    let ottam_home = tempfile::TempDir::new().expect("OTTAM_HOME");
    let mut core = CoreProcess::new_in_home_with_env(ottam_home.path(), runtime.environment());
    core.initialize();

    let primary_opened = core.open_path(primary.path());
    let primary_id = primary_opened["project"]["projectID"]
        .as_str()
        .expect("primary projectID")
        .to_string();
    let primary_workspace_id = core.workspace_id(&primary_id);
    let context_opened = core.open_path(context.path());
    let context_id = context_opened["project"]["projectID"]
        .as_str()
        .expect("context projectID")
        .to_string();
    let other_workspace_id = core.workspace_id(&context_id);
    let added = core.send(json!({
        "id": 701,
        "method": "workspace.addProject",
        "params": {"workspaceID": primary_workspace_id, "path": context.path()}
    }));
    assert_success_response(&added, &json!(701));

    let created = core.send(json!({
        "id": 702,
        "method": "conversation.create",
        "params": {
            "workspaceID": primary_workspace_id,
            "workingRoot": {
                "kind": "project",
                "projectID": primary_id,
                "sourceID": core.project_source_id(&primary_id)
            },
            "contextProjectIDs": [context_id],
            "message": {"body": "Use both project roots", "idempotencyKey": "multi-project-create"}
        }
    }));
    assert_success_response(&created, &json!(702));
    let conversation_id = created["result"]["conversation"]["conversation"]["conversationID"]
        .as_str()
        .expect("conversationID");
    assert_eq!(
        created["result"]["conversation"]["projects"]
            .as_array()
            .expect("project links")
            .len(),
        2
    );
    let input_id = created["result"]["initialInput"]["runInputID"]
        .as_str()
        .expect("runInputID");
    let started = core.send(json!({
        "id": 703,
        "method": "conversation.startRun",
        "params": {
            "workspaceID": primary_workspace_id,
            "conversationID": conversation_id,
            "engineID": "codex",
            "runInputID": input_id
        }
    }));
    assert_success_response(&started, &json!(703));
    let run_id = started["result"]["run"]["runID"]
        .as_str()
        .expect("runID")
        .to_string();

    let mut completed = false;
    for attempt in 0..50 {
        let request_id = 710 + attempt;
        let (response, _) = core.send_for_id(
            json!({
                "id": request_id,
                "method": "conversation.get",
                "params": {
                    "workspaceID": primary_workspace_id,
                    "conversationID": conversation_id
                }
            }),
            json!(request_id),
        );
        assert_success_response(&response, &json!(request_id));
        completed = response["result"]["conversation"]["latestRunPage"]["runs"]
            .as_array()
            .and_then(|runs| {
                runs.iter()
                    .find(|run| run["runID"].as_str() == Some(run_id.as_str()))
            })
            .and_then(|run| run["status"].as_str())
            == Some("completed");
        if completed {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        completed,
        "run should complete before inspecting its source snapshot"
    );

    let absent = core.send(json!({
        "id": 704,
        "method": "conversation.list",
        "params": {"workspaceID": other_workspace_id}
    }));
    assert_success_response(&absent, &json!(704));
    assert!(absent["result"]["conversations"]
        .as_array()
        .expect("other Workspace conversations")
        .is_empty());

    core.shutdown();
    let database = Database::open(ottam_home.path().to_path_buf())
        .expect("reopens shared database after Core shutdown");
    let (snapshot_count, project_count) = database
        .run("run_sources.inspect", move |connection| {
            let source_projects = conversation_run_sources::table
                .filter(conversation_run_sources::run_id.eq(&run_id))
                .select(conversation_run_sources::project_id)
                .load::<Option<String>>(connection)?;
            let snapshot_count = source_projects.len();
            let project_count = source_projects
                .into_iter()
                .flatten()
                .collect::<HashSet<_>>()
                .len();
            Ok((snapshot_count, project_count))
        })
        .await
        .expect("inspects run source snapshots through Diesel");
    assert_eq!(snapshot_count, 2);
    assert_eq!(project_count, 2);
}
