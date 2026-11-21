use ottam_core::storage::database::MIGRATIONS;
use ottam_core::storage::repositories::{ProjectRepository, WorkspaceRepository};
use ottam_core::storage::schema::project_sources;
use ottam_core::storage::Database;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use diesel_migrations::MigrationHarness;
use std::collections::BTreeSet;
use tempfile::TempDir;
use uuid::Uuid;

fn timestamp(milliseconds: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(milliseconds).expect("valid test timestamp")
}

#[tokio::test]
async fn compatible_databases_reopen_with_or_without_an_unused_generation_marker() {
    use ottam_core::storage::repositories::*;
    for marker in [None, Some(3), Some(5)] {
        let home = TempDir::new().unwrap();
        let database = Database::open(home.path().to_path_buf()).unwrap();
        let (workspace, conversation, message) = database
            .run("test.compatibleHome", move |connection| {
                let now = chrono::Utc::now();
                let workspace = WorkspaceRepository::create(connection, "Before checkpoints", now)?;
                let created = ConversationRepository::create(
                    connection,
                    &CreateConversationParams {
                        workspace_id: &workspace.id,
                        title: "Preserved",
                        root: ConversationRootSelection::WorkspaceFiles,
                        context_projects: &[],
                        initial_message: Some(InitialMessage {
                            body: "durable legacy input",
                            idempotency_key: "legacy",
                        }),
                    },
                    now,
                )?;
                if let Some(generation) = marker {
                    connection.batch_execute(
                        "CREATE TABLE ottam_schema_generation (
                    singleton INTEGER PRIMARY KEY, generation BIGINT NOT NULL);",
                    )?;
                    diesel::sql_query("INSERT INTO ottam_schema_generation VALUES (1, ?)")
                        .bind::<BigInt, _>(generation)
                        .execute(connection)?;
                }
                Ok((
                    workspace.id,
                    created.conversation.id,
                    created.initial_input.unwrap().message.id,
                ))
            })
            .await
            .unwrap();
        drop(database);
        let reopened = Database::open(home.path().to_path_buf()).unwrap();
        let path = home.path().join("ottam.sqlite");
        let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        let saved =
            ConversationRepository::get_message(&mut connection, &conversation, &message).unwrap();
        assert_eq!(saved.body, "durable legacy input");
        assert_eq!(
            WorkspaceRepository::get(&mut connection, &workspace)
                .unwrap()
                .display_name,
            "Before checkpoints"
        );
        let marker_count = diesel::sql_query("SELECT count(*) AS value FROM sqlite_schema WHERE name = 'ottam_schema_generation'")
        .get_result::<IntegerValue>(&mut connection).unwrap().value;
        assert_eq!(marker_count, i64::from(marker.is_some()));
        if let Some(generation) = marker {
            let saved_marker = diesel::sql_query(
                "SELECT generation AS value FROM ottam_schema_generation WHERE singleton = 1",
            )
            .get_result::<IntegerValue>(&mut connection)
            .unwrap();
            assert_eq!(saved_marker.value, generation);
        }
        drop(reopened);
    }
}

#[derive(QueryableByName)]
struct TextValue {
    #[diesel(sql_type = Text)]
    value: String,
}

#[derive(QueryableByName)]
struct IntegerValue {
    #[diesel(sql_type = BigInt)]
    value: i64,
}

#[derive(QueryableByName)]
struct QueryPlanStep {
    #[diesel(sql_type = Text)]
    detail: String,
}

#[tokio::test]
async fn message_history_uses_a_conversation_index() {
    let home = TempDir::new().unwrap();
    let database = Database::open(home.path().to_path_buf()).unwrap();
    database.run("test.messageQueryPlan", |connection| {
        let plan = diesel::sql_query("EXPLAIN QUERY PLAN SELECT m.id FROM conversation_messages m JOIN transcript_items t ON t.id = m.id WHERE m.conversation_id = 'example' ORDER BY t.position DESC LIMIT 50")
            .load::<QueryPlanStep>(connection)?;
        assert!(plan.iter().any(|step| step.detail.contains("SEARCH m USING") && step.detail.contains("conversation_id=?")),
            "message history must not scan messages from every Conversation");
        Ok(())
    }).await.unwrap();
}

async fn pooled_connection_pragmas(database: Database) -> (i64, i64, i64) {
    database
        .run("pragmas.verify", |connection| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let foreign_keys =
                diesel::sql_query("SELECT foreign_keys AS value FROM pragma_foreign_keys")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            let synchronous =
                diesel::sql_query("SELECT synchronous AS value FROM pragma_synchronous")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            let busy_timeout =
                diesel::sql_query("SELECT timeout AS value FROM pragma_busy_timeout")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            Ok((foreign_keys, synchronous, busy_timeout))
        })
        .await
        .expect("reads pooled connection pragmas")
}

#[tokio::test]
async fn baseline_has_exact_target_tables_integrity_and_connection_pragmas() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let tables = database
        .run("schema.verify", |connection| {
            let table_rows = diesel::sql_query(
                "SELECT name AS value FROM sqlite_schema \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .load::<TextValue>(connection)?;
            let integrity =
                diesel::sql_query("SELECT integrity_check AS value FROM pragma_integrity_check")
                    .load::<TextValue>(connection)?
                    .into_iter()
                    .map(|row| row.value)
                    .collect::<Vec<_>>();
            let foreign_key_failures =
                diesel::sql_query("SELECT COUNT(*) AS value FROM pragma_foreign_key_check")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            let journal_mode =
                diesel::sql_query("SELECT journal_mode AS value FROM pragma_journal_mode")
                    .get_result::<TextValue>(connection)?
                    .value;
            let synchronous =
                diesel::sql_query("SELECT synchronous AS value FROM pragma_synchronous")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            let migration_versions = diesel::sql_query(
                "SELECT version AS value FROM __diesel_schema_migrations ORDER BY version",
            )
            .load::<TextValue>(connection)?
            .into_iter()
            .map(|row| row.value)
            .collect::<Vec<_>>();
            Ok((
                table_rows
                    .into_iter()
                    .map(|row| row.value)
                    .collect::<BTreeSet<_>>(),
                integrity,
                foreign_key_failures,
                journal_mode,
                synchronous,
                migration_versions,
            ))
        })
        .await
        .expect("verifies schema");

    let expected = [
        "__diesel_schema_migrations",
        "agent_instances",
        "agent_sessions",
        "conversation_attachments",
        "conversation_messages",
        "conversation_projects",
        "conversation_run_inputs",
        "conversation_run_sources",
        "conversation_runs",
        "conversations",
        "core_settings",
        "mcp_servers",
        "named_agents",
        "transcript_items",
        "transcript_payloads",
        "transcript_body_chunks",
        "transcript_capture",
        "transcript_checkpoint_bodies",
        "transcript_checkpoint_heads",
        "transcript_tools",
        "transcript_plans",
        "transcript_permissions",
        "transcript_gaps",
        "transcript_cursors",
        "project_sources",
        "projects",
        "run_events",
        "workspace_mcp_disabled",
        "workspace_projects",
        "workspaces",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    assert_eq!(tables.0, expected);
    assert_eq!(tables.1, vec!["ok"]);
    assert_eq!(tables.2, 0);
    assert_eq!(tables.3, "wal");
    assert_eq!(tables.4, 1, "SQLite NORMAL is numeric value 1");
    // The applied set is asserted exactly so adding a migration is a deliberate,
    // reviewed change rather than something that lands unnoticed.
    assert_eq!(
        tables.5,
        vec![
            "00000000000000",
            "00000000000001",
            "00000000000002",
            "00000000000003"
        ]
    );
}

/// A home from before managed worktrees gains the two worktree columns on
/// startup, after a backup, and its sources read as plain checkouts.
#[tokio::test]
async fn a_home_before_worktrees_upgrades_with_plain_sources() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let source_id = database
        .run("test.beforeWorktrees", |connection| {
            let opened = ProjectRepository::open_local_git(
                connection,
                "/tmp/ottam-before-worktrees",
                "before-worktrees",
                chrono::Utc::now(),
            )?;
            Ok(opened.source.id)
        })
        .await
        .expect("seeds a project");
    drop(database);
    let path = home.path().join("ottam.sqlite");
    {
        let mut connection =
            SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
        connection
            .batch_execute(
                "ALTER TABLE project_sources DROP COLUMN worktree_cleaned_at; \
                 ALTER TABLE project_sources DROP COLUMN worktree_base_commit; \
                 ALTER TABLE project_sources DROP COLUMN worktree_branch; \
                 DELETE FROM __diesel_schema_migrations WHERE version = '00000000000003';",
            )
            .expect("rewinds to before worktrees");
    }

    let database = Database::open(home.path().to_path_buf()).expect("upgrades the home");

    assert!(home
        .path()
        .join("ottam-before-00000000000003.sqlite")
        .is_file());
    let source = database
        .run("test.afterWorktrees", move |connection| {
            ProjectRepository::get_source(connection, &source_id)
        })
        .await
        .expect("reads the source");
    assert!(source.is_primary);
    assert_eq!(source.worktree, None);
    drop(database);
    let mut connection =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
    assert_eq!(
        applied_migrations(&mut connection),
        vec![
            "00000000000000",
            "00000000000001",
            "00000000000002",
            "00000000000003"
        ]
    );
}

/// A home on the session-model schema gains the unseen-run marker on
/// upgrade, backed up first and with its rows kept. The marker takes only a run of its own
/// conversation, goes with the conversation, and is not read back for an
/// archived workspace.
#[tokio::test]
async fn baseline_home_upgrades_to_the_unseen_run_marker() {
    use ottam_core::storage::models::{ConversationId, ConversationRunId, RunStatus, WorkspaceId};
    use ottam_core::storage::repositories::ConversationRepository;

    let home = TempDir::new().expect("home");
    let path = home.path().join("ottam.sqlite");
    let [workspace, finished, other, run, other_run] =
        std::array::from_fn::<_, 5, _>(|_| Uuid::now_v7().to_string());
    {
        let mut connection =
            SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
        connection
            .batch_execute("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .expect("configures SQLite");
        connection
            .run_next_migration(MIGRATIONS)
            .expect("applies the baseline");
        connection
            .run_next_migration(MIGRATIONS)
            .expect("applies the session model columns");
        assert_eq!(
            applied_migrations(&mut connection),
            ["00000000000000", "00000000000001"]
        );
        connection
            .batch_execute(&format!(
                "INSERT INTO workspaces VALUES ('{workspace}', 'Workspace', 0, 0, 0, NULL); \
                 INSERT INTO conversations \
                     (id, workspace_id, working_root_kind, title, created_at, updated_at) \
                     VALUES ('{finished}', '{workspace}', 'workspaceFiles', 'Finished', 0, 0), \
                            ('{other}', '{workspace}', 'workspaceFiles', 'Other', 0, 0); \
                 INSERT INTO conversation_runs \
                     (id, workspace_id, conversation_id, engine_id, status, queued_at, origin, error_json) \
                     VALUES ('{run}', '{workspace}', '{finished}', 'codex', 'failed', 1, 'user', \
                             '{{\"message\":\"Boom\"}}'), \
                            ('{other_run}', '{workspace}', '{other}', 'codex', 'completed', 1, 'user', NULL);"
            ))
            .expect("writes baseline rows");
    }

    let database = Database::open(home.path().to_path_buf()).expect("upgrades the home");
    assert!(home
        .path()
        .join("ottam-before-00000000000003.sqlite")
        .is_file());

    let workspace_id = WorkspaceId::try_from(workspace.as_str()).unwrap();
    let finished_id = ConversationId::try_from(finished.as_str()).unwrap();
    let other_id = ConversationId::try_from(other.as_str()).unwrap();
    let run_id = ConversationRunId::try_from(run.as_str()).unwrap();
    let other_run_id = ConversationRunId::try_from(other_run.as_str()).unwrap();
    let (applied, untouched, refused, both, after_delete, after_clear, archived) = database
        .run("test.unseenRuns", move |connection| {
            let applied = applied_migrations(connection);
            let untouched = ConversationRepository::unseen_runs(connection)?;
            assert!(!ConversationRepository::set_unseen_run(
                connection,
                &workspace_id,
                &finished_id,
                Some(&other_run_id),
            )?);
            let refused = ConversationRepository::unseen_runs(connection)?;
            ConversationRepository::set_unseen_run(
                connection,
                &workspace_id,
                &finished_id,
                Some(&run_id),
            )?;
            ConversationRepository::set_unseen_run(
                connection,
                &workspace_id,
                &other_id,
                Some(&other_run_id),
            )?;
            let both = ConversationRepository::unseen_runs(connection)?;
            ConversationRepository::delete(connection, &workspace_id, &other_id)?;
            let after_delete = ConversationRepository::unseen_runs(connection)?;
            ConversationRepository::clear_unseen_runs(connection, &workspace_id)?;
            let after_clear = ConversationRepository::unseen_runs(connection)?;
            ConversationRepository::set_unseen_run(
                connection,
                &workspace_id,
                &finished_id,
                Some(&run_id),
            )?;
            connection.batch_execute("UPDATE workspaces SET archived_at = 1")?;
            let archived = ConversationRepository::unseen_runs(connection)?;
            assert!(
                !ConversationRepository::set_unseen_run(
                    connection,
                    &workspace_id,
                    &finished_id,
                    Some(&run_id),
                )?,
                "an archived workspace takes no marker"
            );
            let failures =
                diesel::sql_query("SELECT COUNT(*) AS value FROM pragma_foreign_key_check")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            assert_eq!(failures, 0);
            Ok((
                applied,
                untouched,
                refused,
                both,
                after_delete,
                after_clear,
                archived,
            ))
        })
        .await
        .expect("reads and writes unseen runs");

    assert_eq!(
        applied,
        [
            "00000000000000",
            "00000000000001",
            "00000000000002",
            "00000000000003"
        ]
    );
    assert!(untouched.is_empty(), "the upgrade leaves no marker");
    assert!(refused.is_empty(), "another conversation's run is refused");
    assert_eq!(both.len(), 2);
    let failed = both
        .iter()
        .find(|record| record.conversation_id.to_string() == finished)
        .expect("the finished conversation is unseen");
    assert_eq!(failed.workspace_id.to_string(), workspace);
    assert_eq!(failed.run_id.to_string(), run);
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(failed.title, "Finished");
    assert_eq!(failed.error_json.as_deref(), Some("{\"message\":\"Boom\"}"));
    assert_eq!(after_delete.len(), 1);
    assert_eq!(after_delete[0].conversation_id.to_string(), finished);
    assert!(after_clear.is_empty());
    assert!(archived.is_empty());
}

/// A home that ran only the baseline gains the session model columns on
/// startup, after a backup, keeping its sessions with nothing confirmed yet.
#[tokio::test]
async fn baseline_home_upgrades_to_session_model_columns() {
    use ottam_core::storage::repositories::*;
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let (workspace_id, conversation_id) = database
        .run("test.baselineHome", |connection| {
            let now = chrono::Utc::now();
            let workspace = WorkspaceRepository::create(connection, "Baseline", now)?;
            let created = ConversationRepository::create(
                connection,
                &CreateConversationParams {
                    workspace_id: &workspace.id,
                    title: "Before session models",
                    root: ConversationRootSelection::WorkspaceFiles,
                    context_projects: &[],
                    initial_message: None,
                },
                now,
            )?;
            Ok((workspace.id.to_string(), created.conversation.id.to_string()))
        })
        .await
        .expect("seeds the home");
    drop(database);
    let path = home.path().join("ottam.sqlite");
    {
        let mut connection =
            SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
        diesel::sql_query(
            "INSERT INTO agent_sessions (id, workspace_id, conversation_id, engine_id, \
                 external_session_id, source_scope_fingerprint, status, created_at, \
                 last_used_at, current_mode_id) \
             VALUES ('01900000-0000-7000-8000-000000000001', ?, ?, 'claude', 'external', \
                 'scope', 'active', 1, 1, 'plan')",
        )
        .bind::<Text, _>(&workspace_id)
        .bind::<Text, _>(&conversation_id)
        .execute(&mut connection)
        .expect("inserts a session");
        // Back to the baseline's shape: the columns gone, the version unrun.
        connection
            .batch_execute(
                "ALTER TABLE agent_sessions DROP COLUMN current_thought_level_id; \
                 ALTER TABLE agent_sessions DROP COLUMN current_model_id; \
                 DELETE FROM __diesel_schema_migrations WHERE version = '00000000000001';",
            )
            .expect("rewinds to the baseline");
    }

    Database::open(home.path().to_path_buf()).expect("upgrades the baseline home");

    assert!(home
        .path()
        .join("ottam-before-00000000000003.sqlite")
        .is_file());
    let mut connection =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
    assert_eq!(
        applied_migrations(&mut connection),
        vec![
            "00000000000000",
            "00000000000001",
            "00000000000002",
            "00000000000003"
        ]
    );
    let kept = diesel::sql_query(
        "SELECT COUNT(*) AS value FROM agent_sessions \
         WHERE current_mode_id = 'plan' \
           AND current_model_id IS NULL AND current_thought_level_id IS NULL",
    )
    .get_result::<IntegerValue>(&mut connection)
    .expect("reads the session");
    assert_eq!(kept.value, 1);
}

/// A session's confirmed level belongs to its model: a different model
/// reported without a level drops the old one, the same model keeps it, and
/// either value alone leaves the other as stored.
#[tokio::test]
async fn session_model_keeps_levels_with_their_model() {
    use ottam_core::storage::models::AgentSessionId;
    use ottam_core::storage::repositories::*;
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let session_id = "01900000-0000-7000-8000-000000000002";
    database
        .run("test.sessionModel", move |connection| {
            let now = chrono::Utc::now();
            let workspace = WorkspaceRepository::create(connection, "Models", now)?;
            let created = ConversationRepository::create(
                connection,
                &CreateConversationParams {
                    workspace_id: &workspace.id,
                    title: "Models",
                    root: ConversationRootSelection::WorkspaceFiles,
                    context_projects: &[],
                    initial_message: None,
                },
                now,
            )?;
            diesel::sql_query(
                "INSERT INTO agent_sessions (id, workspace_id, conversation_id, engine_id, \
                     external_session_id, source_scope_fingerprint, status, created_at, \
                     last_used_at) \
                 VALUES (?, ?, ?, 'claude', 'external', 'scope', 'active', 1, 1)",
            )
            .bind::<Text, _>(session_id)
            .bind::<Text, _>(workspace.id.to_string())
            .bind::<Text, _>(created.conversation.id.to_string())
            .execute(connection)?;
            Ok(())
        })
        .await
        .expect("seeds a session");
    let set = |model: Option<&'static str>, level: Option<&'static str>| {
        let database = &database;
        async move {
            database
                .run("test.sessionModel", move |connection| {
                    let id = AgentSessionId::try_from(session_id).expect("session id");
                    RunRepository::set_session_model(connection, &id, model, level)?;
                    let stored: (Option<String>, Option<String>) = diesel::sql_query(
                        "SELECT current_model_id AS model, current_thought_level_id AS level \
                         FROM agent_sessions WHERE id = ?",
                    )
                    .bind::<Text, _>(session_id)
                    .get_result::<StoredSelection>(connection)
                    .map(|row| (row.model, row.level))?;
                    Ok(stored)
                })
                .await
                .expect("sets the model")
        }
    };
    let pair = |model: &str, level: Option<&str>| {
        (Some(model.to_string()), level.map(str::to_string))
    };
    assert_eq!(set(Some("opus"), Some("xhigh")).await, pair("opus", Some("xhigh")));
    assert_eq!(set(Some("opus"), None).await, pair("opus", Some("xhigh")), "same model");
    assert_eq!(set(None, Some("high")).await, pair("opus", Some("high")), "level alone");
    assert_eq!(set(Some("haiku"), None).await, pair("haiku", None), "a new model's levels are its own");
    assert_eq!(set(None, None).await, pair("haiku", None), "nothing reported");
}

#[derive(QueryableByName)]
struct StoredSelection {
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    model: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    level: Option<String>,
}

fn applied_migrations(connection: &mut SqliteConnection) -> Vec<String> {
    diesel::sql_query("SELECT version AS value FROM __diesel_schema_migrations ORDER BY version")
        .load::<TextValue>(connection)
        .expect("reads applied migrations")
        .into_iter()
        .map(|row| row.value)
        .collect()
}

/// Startup backs a home up before it runs a pending migration, so a migration
/// that then fails leaves a verified copy and the home itself in place.
#[test]
fn pending_migration_is_backed_up_before_it_runs() {
    let home = TempDir::new().expect("home");
    let path = home.path().join("ottam.sqlite");
    {
        let mut connection =
            SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
        connection
            .batch_execute("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .expect("configures SQLite");
        connection
            .run_pending_migrations(MIGRATIONS)
            .expect("applies the baseline");
        // The tables stay but the baseline reads as pending, so startup backs
        // up and then fails to create them again.
        diesel::sql_query("DELETE FROM __diesel_schema_migrations")
            .execute(&mut connection)
            .expect("forgets the baseline");
    }

    assert!(Database::open(home.path().to_path_buf()).is_err());

    // Named for the newest migration the binary embeds.
    let backup = home.path().join("ottam-before-00000000000003.sqlite");
    assert!(backup.is_file());
    let coordinated = std::fs::read_dir(home.path().join("backups"))
        .expect("lists coordinated backups")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("verified").is_file())
        .count();
    assert_eq!(coordinated, 1);
    let mut connection =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
    assert!(applied_migrations(&mut connection).is_empty());
    let count = diesel::sql_query(
        "SELECT COUNT(*) AS value FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workspaces'",
    )
    .get_result::<IntegerValue>(&mut connection)
    .expect("counts workspaces");
    assert_eq!(count.value, 1);
}

/// A home that ran the earlier baseline still has the two columns it dropped,
/// and Diesel never reruns that version. Startup drops them, keeping the rows.
#[test]
fn startup_drops_columns_removed_from_the_baseline() {
    let home = TempDir::new().expect("home");
    let path = home.path().join("ottam.sqlite");
    {
        let mut connection =
            SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
        connection
            .batch_execute("PRAGMA journal_mode = WAL;")
            .expect("configures SQLite");
        connection
            .run_pending_migrations(MIGRATIONS)
            .expect("applies the baseline");
        connection
            .batch_execute(
                "ALTER TABLE mcp_servers ADD COLUMN oauth_json TEXT; \
                 ALTER TABLE conversation_attachments ADD COLUMN content_digest TEXT NOT NULL \
                     DEFAULT '0000000000000000000000000000000000000000000000000000000000000000' \
                     CHECK (length(content_digest) = 64); \
                 INSERT INTO mcp_servers (id, name, transport_json, oauth_json, created_at, updated_at) \
                     VALUES ('server', 'Server', '{}', NULL, 1, 1);",
            )
            .expect("recreates the earlier baseline's columns");
    }

    Database::open(home.path().to_path_buf()).expect("opens the earlier home");

    let mut connection =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
    for (table, column) in [
        ("mcp_servers", "oauth_json"),
        ("conversation_attachments", "content_digest"),
    ] {
        let present = diesel::sql_query(
            "SELECT COUNT(*) AS value FROM pragma_table_info(?) WHERE name = ?",
        )
        .bind::<Text, _>(table)
        .bind::<Text, _>(column)
        .get_result::<IntegerValue>(&mut connection)
        .expect("reads table info");
        assert_eq!(present.value, 0, "{table}.{column} survived startup");
    }
    let servers = diesel::sql_query("SELECT COUNT(*) AS value FROM mcp_servers")
        .get_result::<IntegerValue>(&mut connection)
        .expect("counts servers");
    assert_eq!(servers.value, 1);
}

#[test]
fn database_startup_does_not_create_the_legacy_projects_directory() {
    let home = TempDir::new().expect("home");
    Database::open(home.path().to_path_buf()).expect("opens database");

    assert!(!home.path().join("projects").exists());
}

#[tokio::test]
async fn every_pooled_connection_uses_required_pragmas() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let results = tokio::join!(
        pooled_connection_pragmas(database.clone()),
        pooled_connection_pragmas(database.clone()),
        pooled_connection_pragmas(database.clone()),
        pooled_connection_pragmas(database),
    );
    for pragmas in [results.0, results.1, results.2, results.3] {
        assert_eq!(pragmas, (1, 1, 5_000));
    }
}

#[test]
fn baseline_down_then_up_recreates_the_schema() {
    let home = TempDir::new().expect("home");
    let path = home.path().join("redo.sqlite");
    let mut connection =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens SQLite");
    connection
        .batch_execute("PRAGMA foreign_keys = ON")
        .expect("enables foreign keys");
    connection
        .run_pending_migrations(MIGRATIONS)
        .expect("applies baseline");
    connection
        .revert_all_migrations(MIGRATIONS)
        .expect("reverts baseline");
    connection
        .run_pending_migrations(MIGRATIONS)
        .expect("reapplies baseline");
    let count = diesel::sql_query(
        "SELECT COUNT(*) AS value FROM sqlite_schema \
         WHERE type = 'table' AND name = 'workspaces'",
    )
    .get_result::<IntegerValue>(&mut connection)
    .expect("counts workspaces");
    assert_eq!(count.value, 1);
}

#[test]
fn database_rejects_migration_versions_ahead_of_the_binary() {
    let home = TempDir::new().expect("home");
    {
        let database = Database::open(home.path().to_path_buf()).expect("opens database");
        let mut connection =
            SqliteConnection::establish(database.path().to_string_lossy().as_ref())
                .expect("opens SQLite");
        diesel::sql_query(
            "INSERT INTO __diesel_schema_migrations (version, run_on) \
             VALUES ('99999999999999', CURRENT_TIMESTAMP)",
        )
        .execute(&mut connection)
        .expect("inserts future migration");
    }

    let error = Database::open(home.path().to_path_buf())
        .err()
        .expect("future migration must fail");
    assert_eq!(error.kind, "storageMigrationFailed");
}

#[test]
fn database_startup_reports_corrupt_files_without_replacing_them() {
    let home = TempDir::new().expect("home");
    let path = home.path().join("ottam.sqlite");
    let invalid_contents = b"not a sqlite database";
    std::fs::write(&path, invalid_contents).expect("writes invalid database");

    let error = Database::open(home.path().to_path_buf())
        .err()
        .expect("corrupt database must fail");
    assert_eq!(error.kind, "storageCorrupt");
    assert_eq!(
        std::fs::read(path).expect("reads original file"),
        invalid_contents
    );
}

#[test]
fn database_startup_reports_busy_database() {
    let home = TempDir::new().expect("home");
    let path = home.path().join("ottam.sqlite");
    let mut blocker =
        SqliteConnection::establish(path.to_string_lossy().as_ref()).expect("opens blocker");
    // Initialize with this connection instead of a pool whose background
    // connection work can race the fixture's exclusive lock after it is dropped.
    blocker
        .batch_execute("PRAGMA journal_mode = WAL;")
        .expect("enables WAL");
    blocker
        .run_pending_migrations(MIGRATIONS)
        .expect("migrates database");
    blocker
        .batch_execute("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE;")
        .expect("locks database");

    let error = Database::open(home.path().to_path_buf())
        .err()
        .expect("busy database must fail");
    assert_eq!(error.kind, "storageBusy");
    blocker.batch_execute("ROLLBACK").expect("unlocks database");
}

#[tokio::test]
async fn repositories_preserve_project_identity_and_share_membership() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let opened = database
        .run("project.open", |connection| {
            let first = ProjectRepository::open_local_git(
                connection,
                "/tmp/repo-a",
                "repo-a",
                timestamp(10),
            )?;
            let reopened = ProjectRepository::open_local_git(
                connection,
                "/tmp/repo-a",
                "repo-a",
                timestamp(20),
            )?;
            let second_workspace =
                WorkspaceRepository::create(connection, "Shared", timestamp(30))?;
            let membership = WorkspaceRepository::add_project(
                connection,
                &second_workspace.id,
                &first.project.id,
                &first.source.id,
                0,
                timestamp(30),
            )?;
            Ok((first, reopened, second_workspace, membership))
        })
        .await
        .expect("repository transaction");

    assert_eq!(opened.0.project.id, opened.1.project.id);
    assert_eq!(opened.0.source.id, opened.1.source.id);
    assert_eq!(
        Uuid::parse_str(&opened.0.project.id.to_string())
            .expect("project UUID")
            .get_version_num(),
        7
    );
    assert_eq!(
        Uuid::parse_str(&opened.0.source.id.to_string())
            .expect("source UUID")
            .get_version_num(),
        7
    );
    assert_eq!(opened.1.project.last_opened_at, timestamp(20));
    assert_eq!(opened.1.project.updated_at, timestamp(10));
    assert_eq!(opened.1.source.updated_at, timestamp(10));
    assert_eq!(opened.1.source.last_seen_at, timestamp(20));
    assert_eq!(opened.3.workspace_id, opened.2.id);
    assert_eq!(opened.3.project_id, opened.0.project.id);
}

#[tokio::test]
async fn archived_sources_cannot_be_added_to_a_workspace() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let error = database
        .run("workspace.add_project", |connection| {
            let opened = ProjectRepository::open_local_git(
                connection,
                "/tmp/repo-b",
                "repo-b",
                timestamp(10),
            )?;
            let workspace =
                WorkspaceRepository::create(connection, "Archived source", timestamp(20))?;
            let source_id = opened.source.id.to_string();
            diesel::update(project_sources::table.find(&source_id))
                .set(project_sources::archived_at.eq(Some(30_i64)))
                .execute(connection)?;
            WorkspaceRepository::add_project(
                connection,
                &workspace.id,
                &opened.project.id,
                &opened.source.id,
                0,
                timestamp(30),
            )
        })
        .await
        .expect_err("archived source must be rejected");
    assert_eq!(error.kind, "storageConstraint");
}

#[tokio::test]
async fn workspace_lifecycle_orders_and_removes_memberships() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("workspace.lifecycle", |connection| {
            let first = ProjectRepository::open_local_git(
                connection,
                "/tmp/repo-d",
                "repo-d",
                timestamp(10),
            )?;
            let second = ProjectRepository::open_local_git(
                connection,
                "/tmp/repo-e",
                "repo-e",
                timestamp(20),
            )?;
            let workspace = WorkspaceRepository::create(connection, "Planning", timestamp(30))?;
            WorkspaceRepository::add_project(
                connection,
                &workspace.id,
                &first.project.id,
                &first.source.id,
                1,
                timestamp(31),
            )?;
            WorkspaceRepository::add_project(
                connection,
                &workspace.id,
                &second.project.id,
                &second.source.id,
                0,
                timestamp(32),
            )?;
            let memberships = WorkspaceRepository::list_memberships(connection, &workspace.id)?;
            assert_eq!(
                memberships
                    .iter()
                    .map(|membership| membership.project_id)
                    .collect::<Vec<_>>(),
                vec![second.project.id, first.project.id]
            );
            let empty = WorkspaceRepository::create(connection, "Empty", timestamp(33))?;
            let project_paths = WorkspaceRepository::project_paths(connection)?;
            assert_eq!(
                project_paths.get(&workspace.id),
                Some(&vec!["/tmp/repo-e".to_string(), "/tmp/repo-d".to_string()])
            );
            assert_eq!(project_paths.get(&empty.id), None);
            assert_eq!(
                WorkspaceRepository::get(connection, &workspace.id)?.updated_at,
                timestamp(32)
            );

            let renamed =
                WorkspaceRepository::rename(connection, &workspace.id, "Delivery", timestamp(40))?;
            assert_eq!(renamed.display_name, "Delivery");
            assert_eq!(
                WorkspaceRepository::set_archived(
                    connection,
                    &workspace.id,
                    Some(timestamp(50)),
                    timestamp(50),
                )?
                .archived_at,
                Some(timestamp(50))
            );
            assert!(WorkspaceRepository::set_archived(
                connection,
                &workspace.id,
                None,
                timestamp(60),
            )?
            .archived_at
            .is_none());

            let removed = WorkspaceRepository::remove_project(
                connection,
                &workspace.id,
                &second.project.id,
                timestamp(70),
            )?;
            assert_eq!(removed.removed_at, Some(timestamp(70)));
            assert_eq!(
                WorkspaceRepository::get(connection, &workspace.id)?.updated_at,
                timestamp(70)
            );
            let memberships = WorkspaceRepository::list_memberships(connection, &workspace.id)?;
            assert_eq!(memberships.len(), 1);
            assert_eq!(memberships[0].project_id, first.project.id);
            assert_eq!(
                WorkspaceRepository::project_paths(connection)?.get(&workspace.id),
                Some(&vec!["/tmp/repo-d".to_string()])
            );
            Ok(())
        })
        .await
        .expect("runs Workspace lifecycle");
}
