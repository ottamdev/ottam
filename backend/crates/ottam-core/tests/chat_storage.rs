use ottam_core::storage::fingerprint::{source_scope_fingerprint, ScopeRoot};
use ottam_core::storage::models::{
    AgentSessionStatus, AttachmentKind, AttachmentMimeType, AttachmentSourceKind,
    ConversationAttachmentId, ConversationId, ConversationRunInputId, DeliveryEvidence, DeliveryMode, InputResolution,
    MessageRole, ProjectId, ProjectSourceId, RunEventKind, RunInputDelivery, RunInputKind,
    RunInputStatus, RunOrigin, RunSourceRole, RunStatus, SessionOrigin, WorkingRootKind,
    WorkspaceId,
};
use ottam_core::storage::repositories::{
    AttachmentRepository, AutonomousRunParams, ConversationRepository, ConversationRootSelection,
    CreateAttachmentParams, CreateConversationParams, CreateRunParams, DeliveryResolutionAction,
    FinalizeRunParams, InitialMessage, InterruptFollowupParams, Page, PageRequest,
    ProjectRepository, RunRepository, RunSourceSnapshot, WorkspaceRepository,
    USER_MESSAGE_MAX_BYTES,
};
use ottam_core::storage::{Database, StorageError};
use diesel::connection::{Connection, InstrumentationEvent, SimpleConnection};
use diesel::prelude::*;
use diesel::sql_types::BigInt;
use diesel::sqlite::SqliteConnection;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

fn timestamp(milliseconds: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(milliseconds).expect("valid test timestamp")
}

#[derive(QueryableByName)]
struct IntegerValue {
    #[diesel(sql_type = BigInt)]
    value: i64,
}

#[derive(Clone)]
struct Seed {
    workspace_id: WorkspaceId,
    other_workspace_id: WorkspaceId,
    project_id: ProjectId,
    source_id: ProjectSourceId,
    project_path: String,
}

fn seed(connection: &mut SqliteConnection) -> Result<Seed, StorageError> {
    let opened =
        ProjectRepository::open_local_git(connection, "/tmp/chat-repo", "chat-repo", timestamp(1))?;
    let workspace = WorkspaceRepository::create(connection, "Chat", timestamp(2))?;
    WorkspaceRepository::add_project(
        connection,
        &workspace.id,
        &opened.project.id,
        &opened.source.id,
        0,
        timestamp(3),
    )?;
    let other = WorkspaceRepository::create(connection, "Other", timestamp(4))?;
    Ok(Seed {
        workspace_id: workspace.id,
        other_workspace_id: other.id,
        project_id: opened.project.id,
        source_id: opened.source.id,
        project_path: "/tmp/chat-repo".to_string(),
    })
}

fn project_conversation(
    connection: &mut SqliteConnection,
    seed: &Seed,
    body: &str,
    key: &str,
    at: i64,
) -> Result<
    (
        ConversationId,
        ottam_core::storage::repositories::MessageWithInput,
    ),
    StorageError,
> {
    let record = ConversationRepository::create(
        connection,
        &CreateConversationParams {
            workspace_id: &seed.workspace_id,
            title: "Thread",
            root: ConversationRootSelection::Project(&seed.project_id),
            context_projects: &[],
            initial_message: Some(InitialMessage {
                body,
                idempotency_key: key,
            }),
        },
        timestamp(at),
    )?;
    let initial = record.initial_input.expect("initial input");
    Ok((record.conversation.id, initial))
}

fn snapshot<'a>(seed: &'a Seed) -> RunSourceSnapshot<'a> {
    RunSourceSnapshot {
        root_kind: WorkingRootKind::Project,
        project_id: Some(&seed.project_id),
        source_id: Some(&seed.source_id),
        canonical_path: &seed.project_path,
        role: RunSourceRole::Primary,
    }
}

fn scope_fingerprint(seed: &Seed) -> String {
    let source_id = seed.source_id.to_string();
    source_scope_fingerprint(&[ScopeRoot {
        kind: WorkingRootKind::Project,
        source_id: Some(&source_id),
        canonical_path: &seed.project_path,
    }])
    .expect("fingerprint")
}

fn assert_descending_page_contract<T>(
    connection: &mut SqliteConnection,
    expected: &[String],
    mut load: impl FnMut(&mut SqliteConnection, &PageRequest<'_>) -> Result<Page<T>, StorageError>,
    id: impl Fn(&T) -> String,
) -> Result<Vec<T>, StorageError> {
    let first = load(connection, &PageRequest::first(2, usize::MAX))?;
    assert_eq!(
        first.items.iter().map(&id).collect::<Vec<_>>(),
        expected[..2]
    );
    let cursor = first.next_cursor.expect("continuation");
    assert_eq!(cursor, expected[1], "cursor names the last included item");

    let rest = load(
        connection,
        &PageRequest {
            limit: 10,
            byte_budget: usize::MAX,
            cursor: Some(&cursor),
        },
    )?;
    assert_eq!(
        rest.items.iter().map(&id).collect::<Vec<_>>(),
        expected[2..],
        "cursor boundary is exclusive"
    );
    assert!(rest.next_cursor.is_none());

    let byte_limited = load(connection, &PageRequest::first(10, 1))?;
    assert_eq!(byte_limited.items.len(), 1);
    assert!(byte_limited.next_cursor.is_some());

    let mut items = first.items;
    items.extend(rest.items);
    Ok(items)
}

#[tokio::test]
async fn conversation_creation_links_projects_and_seeds_pending_input() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;

            let projects = ConversationRepository::projects(connection, &conversation_id)?;
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].source_id, seed.source_id);
            assert_eq!(initial.message.position, 0);
            assert_eq!(initial.message.role, MessageRole::User);
            assert_eq!(initial.input.status(), RunInputStatus::Pending);
            assert_eq!(initial.input.kind, RunInputKind::Initial);

            // Workspace-files conversations have no project links.
            let files = ConversationRepository::create(
                connection,
                &CreateConversationParams {
                    workspace_id: &seed.workspace_id,
                    title: "Files",
                    root: ConversationRootSelection::WorkspaceFiles,
                    context_projects: &[],
                    initial_message: None,
                },
                timestamp(11),
            )?;
            assert!(files.projects.is_empty());
            assert!(files.initial_input.is_none());

            // A project without active membership in the Workspace is rejected.
            let error = ConversationRepository::create(
                connection,
                &CreateConversationParams {
                    workspace_id: &seed.other_workspace_id,
                    title: "Cross",
                    root: ConversationRootSelection::Project(&seed.project_id),
                    context_projects: &[],
                    initial_message: None,
                },
                timestamp(12),
            )
            .expect_err("cross-workspace project");
            assert_eq!(error.kind, "storageConstraint");

            // The conversation is invisible from the other Workspace.
            let error =
                ConversationRepository::get(connection, &seed.other_workspace_id, &conversation_id)
                    .expect_err("cross-workspace get");
            assert_eq!(error.kind, "storageNotFound");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn post_message_orders_ordinals_replays_idempotency_and_caps_size() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, first) =
                project_conversation(connection, &seed, "first", "key-1", 10)?;

            let second = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "second",
                "key-2",
                timestamp(20),
            )?;
            assert_eq!(second.message.position, 1);

            let replay = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "ignored body",
                "key-1",
                timestamp(30),
            )?;
            assert_eq!(replay.message.id, first.message.id);
            assert_eq!(replay.input.id, first.input.id);

            let oversized = "a".repeat(USER_MESSAGE_MAX_BYTES + 1);
            let error = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &oversized,
                "key-3",
                timestamp(40),
            )
            .expect_err("oversized message");
            assert_eq!(error.kind, "messageTooLarge");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn one_active_run_and_pending_input_are_enforced() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let params = CreateRunParams {
                workspace_id: &seed.workspace_id,
                conversation_id: &conversation_id,
                engine_id: "claude",
                model_id: None,
                request_options_json: None,
                pending_input_id: &initial.input.id,
                sources: &[snapshot(&seed)],
            };
            let run = RunRepository::create_run_with_snapshot(connection, &params, timestamp(20))?;
            assert_eq!(run.status, RunStatus::Queued);
            assert_eq!(RunRepository::run_sources(connection, &run.id)?.len(), 1);

            let error = RunRepository::create_run_with_snapshot(connection, &params, timestamp(21))
                .expect_err("second active run");
            assert_eq!(error.kind, "runAlreadyActive");

            // A run requires exactly one primary source.
            let error = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    sources: &[],
                    ..params.clone()
                },
                timestamp(22),
            )
            .expect_err("missing primary");
            assert_eq!(error.kind, "storageConstraint");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn atomic_run_start_rolls_back_when_sending_transition_fails() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            connection.batch_execute(
                "CREATE TEMP TRIGGER reject_sending_input
                 BEFORE UPDATE OF status ON conversation_run_inputs
                 WHEN NEW.status = 'sending'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected sending failure');
                 END;",
            )?;
            let error = RunRepository::create_run_with_snapshot_and_mark_input_sending(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                DeliveryMode::Prompt,
                timestamp(20),
            )
            .expect_err("injected sending failure rolls back the run");
            assert!(!error.kind.is_empty());
            assert!(RunRepository::active_run(connection, &conversation_id)?.is_none());
            let input =
                ConversationRepository::get_input(connection, &conversation_id, &initial.input.id)?;
            assert_eq!(input.status(), RunInputStatus::Pending);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn concurrent_run_creation_admits_exactly_one() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    let (seed, conversation_id, input_id) = database
        .run("seed", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            Ok((seed, conversation_id, initial.input.id))
        })
        .await
        .expect("seed");

    let attempt = |database: Database, seed: Seed, conversation_id: ConversationId| async move {
        database
            .run("race", move |connection| {
                RunRepository::create_run_with_snapshot(
                    connection,
                    &CreateRunParams {
                        workspace_id: &seed.workspace_id,
                        conversation_id: &conversation_id,
                        engine_id: "claude",
                        model_id: None,
                        request_options_json: None,
                        pending_input_id: &input_id,
                        sources: &[snapshot(&seed)],
                    },
                    timestamp(20),
                )
            })
            .await
    };
    let (first, second) = tokio::join!(
        attempt(database.clone(), seed.clone(), conversation_id),
        attempt(database.clone(), seed.clone(), conversation_id)
    );
    let successes = [&first, &second]
        .iter()
        .filter(|result| result.is_ok())
        .count();
    assert_eq!(successes, 1, "exactly one run may be created");
    let failure = [first, second]
        .into_iter()
        .find_map(Result::err)
        .expect("one failure");
    assert!(
        matches!(failure.kind, "runAlreadyActive" | "storageConflict"),
        "unexpected race failure kind: {}",
        failure.kind
    );
}

#[tokio::test]
async fn sessions_resume_by_engine_and_fingerprint_and_reject_mismatch() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let fingerprint = scope_fingerprint(&seed);

            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            let session = RunRepository::create_session(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "claude",
                Some("ext-1"),
                &fingerprint,
                timestamp(21),
            )?;
            let running = RunRepository::mark_run_running(
                connection,
                &run.id,
                &session.id,
                SessionOrigin::New,
                timestamp(22),
            )?;
            assert_eq!(running.status, RunStatus::Running);
            assert_eq!(running.agent_session_id, Some(session.id));

            // A session with a different fingerprint cannot start the run.
            let mismatched = RunRepository::create_session(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "claude",
                Some("ext-2"),
                "deadbeef",
                timestamp(23),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &run.id,
                    status: RunStatus::Completed,
                    stop_reason: Some("endTurn"),
                    error_json: None,
                },
                timestamp(24),
            )?;
            let next = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "again",
                "key-2",
                timestamp(25),
            )?;
            let second_run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &next.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(26),
            )?;
            let error = RunRepository::mark_run_running(
                connection,
                &second_run.id,
                &mismatched.id,
                SessionOrigin::Continued,
                timestamp(27),
            )
            .expect_err("fingerprint mismatch");
            assert_eq!(error.kind, "sessionScopeMismatch");

            // The matching session resumes; sequential runs share it.
            let resumable = RunRepository::resolve_resumable_session(
                connection,
                &conversation_id,
                "claude",
                &fingerprint,
            )?
            .expect("resumable session");
            assert_eq!(resumable.id, session.id);
            let resumed = RunRepository::mark_run_running(
                connection,
                &second_run.id,
                &session.id,
                SessionOrigin::Continued,
                timestamp(28),
            )?;
            assert_eq!(resumed.agent_session_id, Some(session.id));

            // Ending the session removes it from resumption; engine switch
            // resolves independently.
            RunRepository::end_session(
                connection,
                &session.id,
                AgentSessionStatus::Lost,
                Some("adapterExited"),
                timestamp(29),
            )?;
            assert!(RunRepository::resolve_resumable_session(
                connection,
                &conversation_id,
                "claude",
                &fingerprint,
            )?
            .is_none());
            assert!(RunRepository::resolve_resumable_session(
                connection,
                &conversation_id,
                "codex",
                &fingerprint,
            )?
            .is_none());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn cleanup_session_query_is_not_limited_to_a_history_page() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, _) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let fingerprint = scope_fingerprint(&seed);
            for index in 0..501 {
                RunRepository::create_session(
                    connection,
                    &seed.workspace_id,
                    &conversation_id,
                    &format!("engine-{index}"),
                    Some(&format!("external-{index}")),
                    &fingerprint,
                    timestamp(20 + index),
                )?;
            }

            let sessions = RunRepository::all_sessions_for_cleanup(connection, &conversation_id)?;
            assert_eq!(sessions.len(), 501);
            assert!(sessions
                .iter()
                .any(|session| { session.external_session_id.as_deref() == Some("external-0") }));
            Ok(())
        })
        .await
        .expect("scenario");
}

/// Claude → Codex → Claude leaves both sessions active, Codex's newer. A mode
/// change without a named session must reach the session the latest run uses.
#[tokio::test]
async fn latest_active_session_follows_the_latest_run_after_an_engine_switch() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "message-0", "key-0", 10)?;
            let fingerprint = scope_fingerprint(&seed);
            let mut sessions = Vec::new();
            let mut runs = Vec::new();
            for (index, engine) in ["claude", "codex", "claude"].into_iter().enumerate() {
                let index = index as i64;
                let input = if index == 0 {
                    initial.input.clone()
                } else {
                    ConversationRepository::post_message(
                        connection,
                        &seed.workspace_id,
                        &conversation_id,
                        &format!("message-{index}"),
                        &format!("key-{index}"),
                        timestamp(10 * index + 11),
                    )?
                    .input
                };
                let run = RunRepository::create_run_with_snapshot(
                    connection,
                    &CreateRunParams {
                        workspace_id: &seed.workspace_id,
                        conversation_id: &conversation_id,
                        engine_id: engine,
                        model_id: None,
                        request_options_json: None,
                        pending_input_id: &input.id,
                        sources: &[snapshot(&seed)],
                    },
                    timestamp(10 * index + 12),
                )?;
                if index < 2 {
                    let session = RunRepository::create_session(
                        connection,
                        &seed.workspace_id,
                        &conversation_id,
                        engine,
                        Some(&format!("ext-{engine}")),
                        &fingerprint,
                        timestamp(10 * index + 13),
                    )?;
                    RunRepository::mark_run_running(
                        connection,
                        &run.id,
                        &session.id,
                        SessionOrigin::New,
                        timestamp(10 * index + 14),
                    )?;
                    RunRepository::finalize_run(
                        connection,
                        &FinalizeRunParams {
                            run_id: &run.id,
                            status: RunStatus::Completed,
                            stop_reason: Some("endTurn"),
                            error_json: None,
                        },
                        timestamp(10 * index + 15),
                    )?;
                    sessions.push(session);
                }
                runs.push(run);
            }
            let (claude, codex) = (&sessions[0], &sessions[1]);

            // Back on Claude, before the run binds a session: the run's engine.
            let target = RunRepository::latest_active_session(connection, &conversation_id)?;
            assert_eq!(target.map(|session| session.id), Some(claude.id));

            RunRepository::mark_run_running(
                connection,
                &runs[2].id,
                &claude.id,
                SessionOrigin::Continued,
                timestamp(50),
            )?;
            let target = RunRepository::latest_active_session(connection, &conversation_id)?;
            assert_eq!(target.map(|session| session.id), Some(claude.id));
            assert_ne!(claude.id, codex.id);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn run_input_and_session_pages_preserve_order_cursors_budgets_and_history() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "message-0", "key-0", 10)?;
            let fingerprint = scope_fingerprint(&seed);
            let mut input = initial.input;
            let mut run_ids = Vec::new();
            let mut input_ids = Vec::new();

            for index in 0..4 {
                if index > 0 {
                    input = ConversationRepository::post_message(
                        connection,
                        &seed.workspace_id,
                        &conversation_id,
                        &format!("message-{index}"),
                        &format!("key-{index}"),
                        timestamp(10 + index),
                    )?
                    .input;
                }
                input_ids.push(input.id.to_string());
                let run = RunRepository::create_run_with_snapshot(
                    connection,
                    &CreateRunParams {
                        workspace_id: &seed.workspace_id,
                        conversation_id: &conversation_id,
                        engine_id: "claude",
                        model_id: None,
                        request_options_json: None,
                        pending_input_id: &input.id,
                        sources: &[snapshot(&seed)],
                    },
                    timestamp(20 + index),
                )?;
                run_ids.push(run.id.to_string());
                RunRepository::finalize_run(
                    connection,
                    &FinalizeRunParams {
                        run_id: &run.id,
                        status: RunStatus::Completed,
                        stop_reason: Some("endTurn"),
                        error_json: None,
                    },
                    timestamp(30 + index),
                )?;
            }

            let mut session_ids = Vec::new();
            for index in 0..3 {
                let session = RunRepository::create_session(
                    connection,
                    &seed.workspace_id,
                    &conversation_id,
                    &format!("engine-{index}"),
                    Some(&format!("external-{index}")),
                    &fingerprint,
                    timestamp(40 + index),
                )?;
                session_ids.push(session.id.to_string());
                if index == 0 {
                    RunRepository::end_session(
                        connection,
                        &session.id,
                        AgentSessionStatus::Closed,
                        Some("closed"),
                        timestamp(50),
                    )?;
                } else if index == 1 {
                    RunRepository::end_session(
                        connection,
                        &session.id,
                        AgentSessionStatus::Lost,
                        Some("lost"),
                        timestamp(51),
                    )?;
                }
            }

            let expected_runs = run_ids.iter().rev().cloned().collect::<Vec<_>>();
            assert_descending_page_contract(
                connection,
                &expected_runs,
                |connection, request| {
                    RunRepository::list_runs(connection, &conversation_id, request)
                },
                |run| run.id.to_string(),
            )?;

            let expected_inputs = input_ids.iter().rev().cloned().collect::<Vec<_>>();
            assert_descending_page_contract(
                connection,
                &expected_inputs,
                |connection, request| {
                    RunRepository::list_inputs(connection, &conversation_id, request)
                },
                |input| input.id.to_string(),
            )?;

            let expected_sessions = session_ids.iter().rev().cloned().collect::<Vec<_>>();
            let sessions = assert_descending_page_contract(
                connection,
                &expected_sessions,
                |connection, request| {
                    RunRepository::list_sessions(connection, &conversation_id, request)
                },
                |session| session.id.to_string(),
            )?;
            assert_eq!(sessions[0].status, AgentSessionStatus::Active);
            assert_eq!(sessions[1].status, AgentSessionStatus::Lost);
            assert_eq!(sessions[2].status, AgentSessionStatus::Closed);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn input_delivery_transitions_enforce_legality_and_ordering() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;

            let sending = RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;
            match &sending.delivery {
                RunInputDelivery::Sending { assigned } => {
                    assert_eq!(assigned.delivery_sequence, 1);
                    assert_eq!(assigned.target_run_id, run.id);
                }
                other => panic!("unexpected delivery: {other:?}"),
            }

            // Sending twice is illegal.
            let error = RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(22),
            )
            .expect_err("double sending");
            assert_eq!(error.kind, "inputTransitionInvalid");

            let delivered = RunRepository::mark_input_delivered(
                connection,
                &conversation_id,
                &initial.input.id,
                timestamp(23),
            )?;
            match &delivered.delivery {
                RunInputDelivery::Delivered {
                    evidence,
                    resolution,
                    ..
                } => {
                    assert_eq!(*evidence, DeliveryEvidence::AdapterConfirmed);
                    assert!(resolution.is_none());
                }
                other => panic!("unexpected delivery: {other:?}"),
            }

            // Delivered is terminal for adapter transitions.
            let error = RunRepository::mark_input_delivered(
                connection,
                &conversation_id,
                &initial.input.id,
                timestamp(24),
            )
            .expect_err("double delivered");
            assert_eq!(error.kind, "inputTransitionInvalid");

            // A steering input on the same run takes the next sequence.
            let steer = ConversationRepository::append_steer_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &run.id,
                "steer",
                "key-2",
                timestamp(25),
            )?;
            assert_eq!(steer.input.kind, RunInputKind::Steer);
            assert_eq!(steer.input.requested_against_run_id, Some(run.id));
            let steer_sending = RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &steer.input.id,
                &run.id,
                DeliveryMode::NativeSteer,
                timestamp(26),
            )?;
            match &steer_sending.delivery {
                RunInputDelivery::Sending { assigned } => {
                    assert_eq!(assigned.delivery_sequence, 2);
                    assert_eq!(assigned.delivery_mode, DeliveryMode::NativeSteer);
                }
                other => panic!("unexpected delivery: {other:?}"),
            }

            // A definitive rejection before acceptance fails the input.
            let failed = RunRepository::mark_input_failed(
                connection,
                &conversation_id,
                &steer.input.id,
                "adapterRejected",
                timestamp(27),
            )?;
            assert_eq!(failed.status(), RunInputStatus::Failed);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn an_undelivered_steer_returns_to_pending_with_no_trace_of_the_attempt() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::NativeSteer,
                timestamp(21),
            )?;

            let returned = RunRepository::mark_input_undelivered(
                connection,
                &conversation_id,
                &initial.input.id,
            )?;
            assert_eq!(returned.status(), RunInputStatus::Pending);
            assert_eq!(returned.delivery, RunInputDelivery::Pending);
            assert_eq!(returned.target_run_id(), None);
            // Nothing is outstanding: the agent answered, and the answer was
            // that it took nothing.
            assert!(!RunRepository::has_unresolved_delivery(
                connection,
                &conversation_id
            )?);

            // Free to be sent again, by whichever path can carry it.
            let resent = RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::InterruptThenPrompt,
                timestamp(22),
            )?;
            assert_eq!(resent.status(), RunInputStatus::Sending);

            let repeated = RunRepository::mark_input_delivered(
                connection,
                &conversation_id,
                &initial.input.id,
                timestamp(23),
            )?;
            assert_eq!(repeated.status(), RunInputStatus::Delivered);
            let error = RunRepository::mark_input_undelivered(
                connection,
                &conversation_id,
                &initial.input.id,
            )
            .expect_err("a delivered input cannot be un-delivered");
            assert_eq!(error.kind, "inputTransitionInvalid");
            Ok(())
        })
        .await
        .expect("undelivered steer returns to pending");
}

#[tokio::test]
async fn delivery_unknown_resolution_supports_retry_and_mark_delivered() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;
            let unknown = RunRepository::mark_input_delivery_unknown(
                connection,
                &conversation_id,
                &initial.input.id,
                timestamp(22),
            )?;
            assert_eq!(unknown.status(), RunInputStatus::DeliveryUnknown);
            assert!(RunRepository::has_unresolved_delivery(
                connection,
                &conversation_id
            )?);

            // Retry supersedes the original and reuses the immutable Message.
            let resolved = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.input.id,
                DeliveryResolutionAction::Retry {
                    idempotency_key: "key-retry",
                },
                timestamp(23),
            )?;
            match &resolved.resolved_input.delivery {
                RunInputDelivery::Superseded { resolution, .. } => {
                    assert_eq!(*resolution, InputResolution::RetryCreated);
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
            let retry = resolved.retry.expect("retry input");
            assert_eq!(retry.message.id, initial.message.id);
            assert_eq!(retry.input.kind, RunInputKind::Retry);
            assert_eq!(retry.input.retry_of_input_id, Some(initial.input.id));
            assert_eq!(retry.input.status(), RunInputStatus::Pending);
            assert!(!RunRepository::has_unresolved_delivery(
                connection,
                &conversation_id
            )?);

            // Mark Delivered records user evidence on a second ambiguous input.
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &retry.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(24),
            )?;
            let requeued = RunRepository::resolve_input_after_launch_failure(
                connection,
                &conversation_id,
                &retry.input.id,
                "adapterUnavailable",
                timestamp(24),
            )?;
            assert!(requeued);
            let requeued =
                ConversationRepository::get_input(connection, &conversation_id, &retry.input.id)?;
            assert_eq!(requeued.status(), RunInputStatus::Pending);
            assert_eq!(requeued.target_run_id(), None);
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &retry.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(24),
            )?;
            RunRepository::mark_input_delivery_unknown(
                connection,
                &conversation_id,
                &retry.input.id,
                timestamp(25),
            )?;
            let resolved = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &retry.input.id,
                DeliveryResolutionAction::MarkDelivered,
                timestamp(26),
            )?;
            match &resolved.resolved_input.delivery {
                RunInputDelivery::Delivered {
                    evidence,
                    resolution,
                    ..
                } => {
                    assert_eq!(*evidence, DeliveryEvidence::UserMarkedDelivered);
                    assert_eq!(
                        resolution.map(|(resolution, _)| resolution),
                        Some(InputResolution::MarkedDelivered)
                    );
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
            assert!(resolved.retry.is_none());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn adapter_launch_failure_atomically_fails_an_initial_input() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-launch-failure", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;

            let requeued = RunRepository::resolve_input_after_launch_failure(
                connection,
                &conversation_id,
                &initial.input.id,
                "adapterUnavailable",
                timestamp(22),
            )?;
            assert!(!requeued);
            let failed =
                ConversationRepository::get_input(connection, &conversation_id, &initial.input.id)?;
            assert_eq!(failed.status(), RunInputStatus::Failed);

            let repeated = RunRepository::resolve_input_after_launch_failure(
                connection,
                &conversation_id,
                &initial.input.id,
                "adapterUnavailable",
                timestamp(23),
            )
            .expect_err("terminal input cannot transition twice");
            assert_eq!(repeated.kind, "inputTransitionInvalid");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn a_failed_input_can_be_retried_but_not_marked_delivered() {
    // An expired credential fails the input before the agent ever saw it, and
    // that used to be a dead end: the message could only be typed again.
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;
            let failed = RunRepository::mark_input_failed(
                connection,
                &conversation_id,
                &initial.input.id,
                "authRequired",
                timestamp(22),
            )?;
            assert_eq!(failed.status(), RunInputStatus::Failed);
            // A failure the adapter reported is not ambiguous, so it never
            // blocks new work the way deliveryUnknown does.
            assert!(!RunRepository::has_unresolved_delivery(
                connection,
                &conversation_id
            )?);
            // The run carrying it is over; only then is there room to retry.
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &run.id,
                    status: RunStatus::Failed,
                    stop_reason: None,
                    error_json: Some("{\"kind\":\"authRequired\"}"),
                },
                timestamp(23),
            )?;

            // There is nothing for the user to attest to, so Mark Delivered is
            // still refused.
            let rejected = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.input.id,
                DeliveryResolutionAction::MarkDelivered,
                timestamp(23),
            )
            .expect_err("a failed input has no delivery to claim");
            assert_eq!(rejected.kind, "inputTransitionInvalid");

            let resolved = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.input.id,
                DeliveryResolutionAction::Retry {
                    idempotency_key: "key-retry",
                },
                timestamp(24),
            )?;
            match &resolved.resolved_input.delivery {
                RunInputDelivery::Superseded { resolution, .. } => {
                    assert_eq!(*resolution, InputResolution::RetryCreated);
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
            let retry = resolved.retry.expect("retry input");
            assert_eq!(retry.message.id, initial.message.id);
            assert_eq!(retry.input.retry_of_input_id, Some(initial.input.id));
            assert_eq!(retry.input.status(), RunInputStatus::Pending);

            // A second press cannot fork the turn: the row it would supersede
            // is already superseded.
            let again = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.input.id,
                DeliveryResolutionAction::Retry {
                    idempotency_key: "key-retry-again",
                },
                timestamp(25),
            )
            .expect_err("the failed input was already resolved");
            assert_eq!(again.kind, "inputTransitionInvalid");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn a_failed_input_cannot_be_retried_while_the_conversation_is_busy() {
    // Superseding it anyway would strand the Conversation: the retry has no
    // slot to run in, and the pending input it leaves behind blocks every
    // further send until someone starts it by hand.
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let first = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &first.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;
            RunRepository::mark_input_failed(
                connection,
                &conversation_id,
                &initial.input.id,
                "authRequired",
                timestamp(22),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &first.id,
                    status: RunStatus::Failed,
                    stop_reason: None,
                    error_json: Some("{\"kind\":\"authRequired\"}"),
                },
                timestamp(23),
            )?;

            // A newer turn is under way when the user reaches back for the old one.
            let second = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "next",
                "key-2",
                timestamp(24),
            )?;
            RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &second.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(25),
            )?;

            let refused = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.input.id,
                DeliveryResolutionAction::Retry {
                    idempotency_key: "key-retry",
                },
                timestamp(26),
            )
            .expect_err("a busy conversation has nowhere to run the retry");
            assert_eq!(refused.kind, "runAlreadyActive");
            // Refused, not consumed: the failed row is still there to offer.
            let untouched =
                ConversationRepository::get_input(connection, &conversation_id, &initial.input.id)?;
            assert_eq!(untouched.status(), RunInputStatus::Failed);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn event_batches_advance_cursor_and_enforce_caps() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;

            let first = RunRepository::append_event_batch(
                connection,
                &run.id,
                &[
                    (RunEventKind::RunStatus, "{\"n\":1}".to_string()),
                    (RunEventKind::Usage, "{\"n\":2}".to_string()),
                ],
                timestamp(21),
            )?;
            assert_eq!(
                first.iter().map(|event| event.sequence).collect::<Vec<_>>(),
                vec![1, 2]
            );
            let second = RunRepository::append_event_batch(
                connection,
                &run.id,
                &[(RunEventKind::Usage, "{\"n\":3}".to_string())],
                timestamp(22),
            )?;
            assert_eq!(second[0].sequence, 3);
            assert_eq!(
                RunRepository::get_run(connection, &run.id)?.event_sequence_cursor,
                3
            );

            let oversized = format!("{{\"blob\":\"{}\"}}", "x".repeat(65 * 1024));
            let error = RunRepository::append_event_batch(
                connection,
                &run.id,
                &[(RunEventKind::RunStatus, oversized)],
                timestamp(23),
            )
            .expect_err("oversized payload");
            assert_eq!(error.kind, "payloadTooLarge");

            // Pagination traverses in sequence order under a byte budget.
            let page = RunRepository::list_events(connection, &run.id, &PageRequest::first(10, 8))?;
            assert_eq!(page.items.len(), 1);
            let cursor = page.next_cursor.expect("continuation");
            let rest = RunRepository::list_events(
                connection,
                &run.id,
                &PageRequest {
                    limit: 10,
                    byte_budget: 1024,
                    cursor: Some(&cursor),
                },
            )?;
            assert_eq!(
                rest.items
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![2, 3]
            );
            assert!(rest.next_cursor.is_none());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn conversation_event_pages_span_runs_in_order_with_exclusive_cursors() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "first", "key-1", 10)?;
            let first_run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            let first_events = RunRepository::append_event_batch(
                connection,
                &first_run.id,
                &[
                    (RunEventKind::RunStatus, "{}".to_string()),
                    (RunEventKind::Usage, "{}".to_string()),
                ],
                timestamp(21),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &first_run.id,
                    status: RunStatus::Completed,
                    stop_reason: Some("endTurn"),
                    error_json: None,
                },
                timestamp(22),
            )?;

            let followup = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "second",
                "key-2",
                timestamp(23),
            )?;
            let second_run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &followup.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(24),
            )?;
            let second_events = RunRepository::append_event_batch(
                connection,
                &second_run.id,
                &[
                    (RunEventKind::PermissionRequested, "{}".to_string()),
                    (RunEventKind::Usage, "{}".to_string()),
                ],
                timestamp(25),
            )?;

            let expected = first_events
                .iter()
                .chain(second_events.iter())
                .map(|event| event.id.to_string())
                .collect::<Vec<_>>();
            let first_page = RunRepository::list_conversation_events(
                connection,
                &conversation_id,
                &PageRequest::first(2, usize::MAX),
            )?;
            assert_eq!(
                first_page
                    .items
                    .iter()
                    .map(|event| event.id.to_string())
                    .collect::<Vec<_>>(),
                expected[..2]
            );
            assert!(first_page
                .items
                .iter()
                .all(|event| event.run_id == first_run.id));
            let cursor = first_page.next_cursor.expect("event continuation");
            assert_eq!(cursor, expected[1]);

            let second_page = RunRepository::list_conversation_events(
                connection,
                &conversation_id,
                &PageRequest {
                    limit: 10,
                    byte_budget: usize::MAX,
                    cursor: Some(&cursor),
                },
            )?;
            assert_eq!(
                second_page
                    .items
                    .iter()
                    .map(|event| event.id.to_string())
                    .collect::<Vec<_>>(),
                expected[2..]
            );
            assert!(second_page
                .items
                .iter()
                .all(|event| event.run_id == second_run.id));
            assert!(second_page.next_cursor.is_none());

            let byte_limited = RunRepository::list_conversation_events(
                connection,
                &conversation_id,
                &PageRequest::first(10, 2),
            )?;
            assert_eq!(byte_limited.items.len(), 1);
            assert!(byte_limited.next_cursor.is_some());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn finalize_run_preserves_lifecycle_without_creating_assistant_messages() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            let finalized = RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &run.id,
                    status: RunStatus::Completed,
                    stop_reason: Some("endTurn"),
                    error_json: None,
                },
                timestamp(21),
            )?;
            assert_eq!(finalized.status, RunStatus::Completed);
            let page = ConversationRepository::list_messages(
                connection,
                &conversation_id,
                &PageRequest::first(10, 1024 * 1024),
            )?;
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].role, MessageRole::User);

            // Finalization is single-shot.
            let error = RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &run.id,
                    status: RunStatus::Canceled,
                    stop_reason: None,
                    error_json: None,
                },
                timestamp(22),
            )
            .expect_err("double finalize");
            assert_eq!(error.kind, "storageConstraint");

            // Cancellation persists non-empty accumulated output as partial.
            let next = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "again",
                "key-2",
                timestamp(23),
            )?;
            let second = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &next.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(24),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &second.id,
                    status: RunStatus::Canceled,
                    stop_reason: Some("cancelled"),
                    error_json: None,
                },
                timestamp(25),
            )?;
            let page = ConversationRepository::list_messages(
                connection,
                &conversation_id,
                &PageRequest::first(10, 1024 * 1024),
            )?;
            assert!(page
                .items
                .iter()
                .all(|message| message.role == MessageRole::User));
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn interrupt_creates_followup_run_and_routes_pending_input() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            let steer = ConversationRepository::append_steer_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &run.id,
                "change course",
                "key-2",
                timestamp(21),
            )?;

            let record = RunRepository::interrupt_and_create_followup(
                connection,
                &InterruptFollowupParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    interrupted_run_id: &run.id,
                    pending_input_id: &steer.input.id,
                    retain_session: true,
                },
                timestamp(22),
            )?;
            assert_eq!(record.interrupted_run.status, RunStatus::Interrupted);
            assert_eq!(record.followup_run.status, RunStatus::Queued);
            match &record.input.delivery {
                RunInputDelivery::Sending { assigned } => {
                    assert_eq!(assigned.target_run_id, record.followup_run.id);
                    assert_eq!(assigned.delivery_mode, DeliveryMode::InterruptThenPrompt);
                }
                other => panic!("unexpected delivery: {other:?}"),
            }
            assert_eq!(record.input.requested_against_run_id, Some(run.id));

            // The follow-up carries the same immutable source snapshot.
            let sources = RunRepository::run_sources(connection, &record.followup_run.id)?;
            assert_eq!(sources.len(), 1);
            assert_eq!(sources[0].canonical_path, seed.project_path);

            let page = ConversationRepository::list_messages(
                connection,
                &conversation_id,
                &PageRequest::first(10, 1024 * 1024),
            )?;
            assert_eq!(
                page.items
                    .iter()
                    .map(|message| (message.position, message.body.as_str()))
                    .collect::<Vec<_>>(),
                vec![(1, "change course"), (0, "hello")]
            );
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn startup_recovery_fails_runs_and_surfaces_ambiguous_inputs_once() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::mark_input_sending(
                connection,
                &conversation_id,
                &initial.input.id,
                &run.id,
                DeliveryMode::Prompt,
                timestamp(21),
            )?;

            let counts = RunRepository::recover_on_startup(connection, timestamp(30))?;
            assert_eq!(counts.failed_runs, 1);
            assert_eq!(counts.ambiguous_inputs, 1);

            let recovered_run = RunRepository::get_run(connection, &run.id)?;
            assert_eq!(recovered_run.status, RunStatus::Failed);
            assert!(recovered_run
                .error_json
                .as_deref()
                .expect("typed error")
                .contains("coreRestarted"));
            let input =
                ConversationRepository::get_input(connection, &conversation_id, &initial.input.id)?;
            assert_eq!(input.status(), RunInputStatus::DeliveryUnknown);
            assert!(RunRepository::has_unresolved_delivery(
                connection,
                &conversation_id
            )?);

            // Recovery never retries: a second pass is a no-op.
            let counts = RunRepository::recover_on_startup(connection, timestamp(31))?;
            assert_eq!(counts.failed_runs, 0);
            assert_eq!(counts.ambiguous_inputs, 0);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn database_open_leaves_chat_recovery_to_the_repository_authority() {
    let home = TempDir::new().expect("home");
    let (conversation_id, run_id, input_id) = {
        let database = Database::open(home.path().to_path_buf()).expect("opens database");
        database
            .run("seedRecovery", |connection| {
                let seed = seed(connection)?;
                let (conversation_id, initial) =
                    project_conversation(connection, &seed, "hello", "key-1", 10)?;
                let run = RunRepository::create_run_with_snapshot(
                    connection,
                    &CreateRunParams {
                        workspace_id: &seed.workspace_id,
                        conversation_id: &conversation_id,
                        engine_id: "claude",
                        model_id: None,
                        request_options_json: None,
                        pending_input_id: &initial.input.id,
                        sources: &[snapshot(&seed)],
                    },
                    timestamp(20),
                )?;
                RunRepository::mark_input_sending(
                    connection,
                    &conversation_id,
                    &initial.input.id,
                    &run.id,
                    DeliveryMode::Prompt,
                    timestamp(21),
                )?;
                Ok((conversation_id, run.id, initial.input.id))
            })
            .await
            .expect("seeds recovery state")
    };

    let reopened = Database::open(home.path().to_path_buf()).expect("reopens database");
    reopened
        .run("verifyRecoveryAuthority", move |connection| {
            let untouched_run = RunRepository::get_run(connection, &run_id)?;
            assert_eq!(untouched_run.status, RunStatus::Queued);
            let untouched_input =
                ConversationRepository::get_input(connection, &conversation_id, &input_id)?;
            assert_eq!(untouched_input.status(), RunInputStatus::Sending);

            let counts = RunRepository::recover_on_startup(connection, timestamp(30))?;
            assert_eq!(counts.failed_runs, 1);
            assert_eq!(counts.ambiguous_inputs, 1);
            Ok(())
        })
        .await
        .expect("repository owns recovery");
}

#[tokio::test]
async fn conversation_summary_loading_is_batched_and_propagates_corrupt_roots() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (first_id, _) = project_conversation(connection, &seed, "first", "key-first", 10)?;

            let query_count = Arc::new(AtomicUsize::new(0));
            let observed_count = Arc::clone(&query_count);
            connection.set_instrumentation(move |event: InstrumentationEvent<'_>| {
                if matches!(event, InstrumentationEvent::StartQuery { .. }) {
                    observed_count.fetch_add(1, Ordering::SeqCst);
                }
            });

            let one = ConversationRepository::list_summary_records(connection, &seed.workspace_id)?;
            assert_eq!(one.len(), 1);
            let one_query_count = query_count.swap(0, Ordering::SeqCst);

            for index in 0..24 {
                project_conversation(
                    connection,
                    &seed,
                    &format!("message-{index}"),
                    &format!("key-{index}"),
                    20 + index,
                )?;
            }
            query_count.store(0, Ordering::SeqCst);
            let many =
                ConversationRepository::list_summary_records(connection, &seed.workspace_id)?;
            let many_query_count = query_count.load(Ordering::SeqCst);
            assert_eq!(many.len(), 25);
            assert_eq!(many_query_count, one_query_count);
            assert!(
                many_query_count <= 5,
                "summary list must use bounded queries"
            );

            diesel::update(
                ottam_core::storage::schema::project_sources::table
                    .find(seed.source_id.to_string()),
            )
            .set(ottam_core::storage::schema::project_sources::last_seen_at.eq(i64::MAX))
            .execute(connection)?;
            let error =
                ConversationRepository::list_summary_records(connection, &seed.workspace_id)
                    .expect_err("corrupt ProjectSource must not appear detached");
            assert_eq!(error.kind, "storageCorrupt");

            diesel::update(
                ottam_core::storage::schema::project_sources::table
                    .find(seed.source_id.to_string()),
            )
            .set(ottam_core::storage::schema::project_sources::last_seen_at.eq(1_i64))
            .execute(connection)?;
            diesel::delete(
                ottam_core::storage::schema::conversation_projects::table.filter(
                    ottam_core::storage::schema::conversation_projects::conversation_id
                        .eq(first_id.to_string()),
                ),
            )
            .execute(connection)?;
            let detached =
                ConversationRepository::list_summary_records(connection, &seed.workspace_id)?
                    .into_iter()
                    .find(|record| record.conversation.id == first_id)
                    .expect("first conversation");
            assert!(detached.primary_link.is_none());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn message_pages_respect_item_and_byte_budgets() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, _) = project_conversation(connection, &seed, "m0", "key-0", 10)?;
            for index in 1..5 {
                ConversationRepository::post_message(
                    connection,
                    &seed.workspace_id,
                    &conversation_id,
                    &format!("m{index}"),
                    &format!("key-{index}"),
                    timestamp(10 + index),
                )?;
            }

            let mut collected = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let request = PageRequest {
                    limit: 2,
                    byte_budget: 3,
                    cursor: cursor.as_deref(),
                };
                let page =
                    ConversationRepository::list_messages(connection, &conversation_id, &request)?;
                assert!(page.items.len() <= 2, "item limit respected");
                collected.extend(page.items.iter().map(|message| message.position));
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            assert_eq!(collected, vec![4, 3, 2, 1, 0]);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn conversation_deletion_removes_the_full_graph() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let fingerprint = scope_fingerprint(&seed);
            let run = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            let session = RunRepository::create_session(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "claude",
                Some("ext-1"),
                &fingerprint,
                timestamp(21),
            )?;
            RunRepository::mark_run_running(
                connection,
                &run.id,
                &session.id,
                SessionOrigin::New,
                timestamp(22),
            )?;
            RunRepository::append_event_batch(
                connection,
                &run.id,
                &[(RunEventKind::RunStatus, "{}".to_string())],
                timestamp(23),
            )?;

            ConversationRepository::delete(connection, &seed.workspace_id, &conversation_id)?;

            for (label, count_sql) in [
                (
                    "conversations",
                    "SELECT COUNT(*) AS value FROM conversations",
                ),
                (
                    "messages",
                    "SELECT COUNT(*) AS value FROM conversation_messages",
                ),
                ("runs", "SELECT COUNT(*) AS value FROM conversation_runs"),
                (
                    "inputs",
                    "SELECT COUNT(*) AS value FROM conversation_run_inputs",
                ),
                (
                    "sources",
                    "SELECT COUNT(*) AS value FROM conversation_run_sources",
                ),
                ("sessions", "SELECT COUNT(*) AS value FROM agent_sessions"),
                ("events", "SELECT COUNT(*) AS value FROM run_events"),
                (
                    "links",
                    "SELECT COUNT(*) AS value FROM conversation_projects",
                ),
            ] {
                let count = diesel::sql_query(count_sql)
                    .get_result::<IntegerValue>(connection)?
                    .value;
                assert_eq!(count, 0, "{label} not fully deleted");
            }

            // Shared Projects and Workspaces survive Conversation deletion.
            assert!(WorkspaceRepository::get(connection, &seed.workspace_id).is_ok());
            let violations =
                diesel::sql_query("SELECT COUNT(*) AS value FROM pragma_foreign_key_check")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            assert_eq!(violations, 0);
            Ok(())
        })
        .await
        .expect("scenario");
}

/// Fails `input_id`'s run and retries it, returning the retry input.
fn fail_and_retry(
    connection: &mut SqliteConnection,
    seed: &Seed,
    conversation_id: &ConversationId,
    input_id: &ConversationRunInputId,
    key: &str,
    at: i64,
) -> Result<ConversationRunInputId, StorageError> {
    let run = RunRepository::create_run_with_snapshot(
        connection,
        &CreateRunParams {
            workspace_id: &seed.workspace_id,
            conversation_id,
            engine_id: "claude",
            model_id: None,
            request_options_json: None,
            pending_input_id: input_id,
            sources: &[snapshot(seed)],
        },
        timestamp(at),
    )?;
    RunRepository::mark_input_sending(
        connection,
        conversation_id,
        input_id,
        &run.id,
        DeliveryMode::Prompt,
        timestamp(at + 1),
    )?;
    RunRepository::mark_input_failed(
        connection,
        conversation_id,
        input_id,
        "authRequired",
        timestamp(at + 2),
    )?;
    RunRepository::finalize_run(
        connection,
        &FinalizeRunParams {
            run_id: &run.id,
            status: RunStatus::Failed,
            stop_reason: None,
            error_json: None,
        },
        timestamp(at + 3),
    )?;
    let resolved = RunRepository::resolve_undelivered_input(
        connection,
        &seed.workspace_id,
        conversation_id,
        input_id,
        DeliveryResolutionAction::Retry {
            idempotency_key: key,
        },
        timestamp(at + 4),
    )?;
    Ok(resolved.retry.expect("retry input").input.id)
}

/// Retry inputs reference the input they retry with ON DELETE RESTRICT, which
/// SQLite enforces row by row, so deleting the original before its retry used
/// to abort the whole Conversation delete.
async fn assert_deletes_after_retries(retries: i64) {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", move |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let mut input_id = initial.input.id;
            for attempt in 0..retries {
                input_id = fail_and_retry(
                    connection,
                    &seed,
                    &conversation_id,
                    &input_id,
                    &format!("key-retry-{attempt}"),
                    20 + attempt * 10,
                )?;
            }

            ConversationRepository::delete(connection, &seed.workspace_id, &conversation_id)?;

            for (label, count_sql) in [
                (
                    "conversations",
                    "SELECT COUNT(*) AS value FROM conversations",
                ),
                (
                    "inputs",
                    "SELECT COUNT(*) AS value FROM conversation_run_inputs",
                ),
                ("runs", "SELECT COUNT(*) AS value FROM conversation_runs"),
                (
                    "foreign key violations",
                    "SELECT COUNT(*) AS value FROM pragma_foreign_key_check",
                ),
            ] {
                let count = diesel::sql_query(count_sql)
                    .get_result::<IntegerValue>(connection)?
                    .value;
                assert_eq!(count, 0, "{label} left behind");
            }
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn conversation_deletion_removes_a_retried_input() {
    assert_deletes_after_retries(1).await;
}

#[tokio::test]
async fn conversation_deletion_removes_a_retry_chain() {
    assert_deletes_after_retries(2).await;
}

#[tokio::test]
async fn restrict_violations_classify_as_storage_constraints() {
    use ottam_core::storage::schema::conversation_run_inputs;
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            fail_and_retry(
                connection,
                &seed,
                &conversation_id,
                &initial.input.id,
                "key-retry",
                20,
            )?;
            let error: StorageError =
                diesel::delete(conversation_run_inputs::table.find(initial.input.id.to_string()))
                    .execute(connection)
                    .expect_err("the retry still references its original")
                    .into();
            assert_eq!(error.kind, "storageConstraint");
            Ok(())
        })
        .await
        .expect("scenario");
}

fn create_attachment(
    connection: &mut SqliteConnection,
    seed: &Seed,
    label: &str,
    byte_count: i64,
    at: i64,
) -> Result<ConversationAttachmentId, StorageError> {
    let id = ConversationAttachmentId::new_v7();
    AttachmentRepository::create_unclaimed(
        connection,
        &id,
        &CreateAttachmentParams {
            workspace_id: &seed.workspace_id,
            kind: AttachmentKind::Image,
            mime_type: AttachmentMimeType::Png,
            byte_count,
            width: 800,
            height: 600,
            relative_path: &format!("{label}.png"),
            idempotency_key: &format!("attachment-{label}"),
            source_kind: AttachmentSourceKind::Pasted,
            origin_path: None,
        },
        timestamp(at),
    )?;
    Ok(id)
}

#[tokio::test]
async fn attachments_claim_in_composer_order_replay_for_their_owner_and_reject_another() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "look at this", "key-1", 10)?;

            let first = create_attachment(connection, &seed, "first", 1_024, 11)?;
            let second = create_attachment(connection, &seed, "second", 2_048, 12)?;

            // Claimed in the reverse of paste order: position follows the chip
            // order the caller passes, not created_at.
            let claimed = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[second, first],
                timestamp(13),
            )?;
            assert_eq!(claimed.len(), 2);
            assert_eq!(claimed[0].id, second);
            assert_eq!(claimed[0].ownership.ordinal(), Some(0));
            assert_eq!(claimed[1].id, first);
            assert_eq!(claimed[1].ownership.ordinal(), Some(1));

            // Re-claiming by the Message that already owns it replays rather
            // than conflicting: a retry carrying the same idempotency key
            // replays the Message and runs this claim again over the same ids.
            let replayed = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[second, first],
                timestamp(14),
            )?;
            assert_eq!(
                replayed.iter().map(|item| item.id).collect::<Vec<_>>(),
                vec![second, first]
            );
            assert_eq!(
                replayed[0].ownership.ordinal(),
                Some(0),
                "a replay must not renumber what it already claimed"
            );

            // A *different* Message is still refused.
            let other = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "a second message",
                "key-2",
                timestamp(15),
            )?;
            let error = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &other.message.id,
                &[first],
                timestamp(16),
            )
            .expect_err("an attachment belongs to exactly one Message");
            assert_eq!(error.kind, "storageConstraint");

            let grouped = AttachmentRepository::list_for_messages(
                connection,
                &conversation_id,
                &[initial.message.id],
            )?;
            let listed = grouped.get(&initial.message.id).expect("attachments");
            assert_eq!(
                listed.iter().map(|item| item.id).collect::<Vec<_>>(),
                vec![second, first],
                "list_for_messages returns composer order"
            );
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn attachment_lookup_for_message_pages_is_batched_and_preserves_each_message_order() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "message-0", "key-0", 10)?;
            let first = create_attachment(connection, &seed, "batch-0-first", 100, 11)?;
            let second = create_attachment(connection, &seed, "batch-0-second", 100, 12)?;
            AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[second, first],
                timestamp(13),
            )?;
            let mut expected = vec![(initial.message.id, vec![second, first])];

            let query_count = Arc::new(AtomicUsize::new(0));
            let observed_count = Arc::clone(&query_count);
            connection.set_instrumentation(move |event: InstrumentationEvent<'_>| {
                if matches!(event, InstrumentationEvent::StartQuery { .. }) {
                    observed_count.fetch_add(1, Ordering::SeqCst);
                }
            });
            let one = AttachmentRepository::list_for_messages(
                connection,
                &conversation_id,
                &[initial.message.id],
            )?;
            assert_eq!(one.len(), 1);
            let one_query_count = query_count.swap(0, Ordering::SeqCst);

            for index in 1..25 {
                let posted = ConversationRepository::post_message(
                    connection,
                    &seed.workspace_id,
                    &conversation_id,
                    &format!("message-{index}"),
                    &format!("key-{index}"),
                    timestamp(20 + index),
                )?;
                let first = create_attachment(
                    connection,
                    &seed,
                    &format!("batch-{index}-first"),
                    100,
                    100 + index * 3,
                )?;
                let second = create_attachment(
                    connection,
                    &seed,
                    &format!("batch-{index}-second"),
                    100,
                    101 + index * 3,
                )?;
                AttachmentRepository::claim(
                    connection,
                    &seed.workspace_id,
                    &conversation_id,
                    &posted.message.id,
                    &[second, first],
                    timestamp(102 + index * 3),
                )?;
                expected.push((posted.message.id, vec![second, first]));
            }

            query_count.store(0, Ordering::SeqCst);
            let message_ids = expected.iter().map(|(id, _)| *id).collect::<Vec<_>>();
            let many = AttachmentRepository::list_for_messages(
                connection,
                &conversation_id,
                &message_ids,
            )?;
            let many_query_count = query_count.load(Ordering::SeqCst);
            assert_eq!(many_query_count, one_query_count);
            assert!(many_query_count <= 1, "attachment lookup must stay batched");
            assert_eq!(many.len(), expected.len());
            for (message_id, attachment_ids) in expected {
                let listed = many.get(&message_id).expect("message attachment group");
                assert_eq!(
                    listed.iter().map(|item| item.id).collect::<Vec<_>>(),
                    attachment_ids
                );
                assert_eq!(
                    listed
                        .iter()
                        .map(|item| item.ownership.ordinal())
                        .collect::<Vec<_>>(),
                    vec![Some(0), Some(1)]
                );
            }
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn attachment_claim_enforces_the_per_message_caps() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;

            let mut ids = Vec::new();
            for index in 0..7 {
                ids.push(create_attachment(
                    connection,
                    &seed,
                    &format!("count-{index}"),
                    1_024,
                    20 + index,
                )?);
            }
            let error = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &ids,
                timestamp(30),
            )
            .expect_err("seven attachments exceed the count cap");
            assert_eq!(error.kind, "storageConstraint");

            let duplicate = ids[0];
            let error = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[duplicate, duplicate],
                timestamp(31),
            )
            .expect_err("the same attachment cannot be claimed twice");
            assert_eq!(error.kind, "storageConstraint");

            // Two attachments just under the per-attachment cap clear it
            // individually and blow the four-megabyte per-Message total.
            let heavy_first = create_attachment(connection, &seed, "heavy-1", 3_000_000, 40)?;
            let heavy_second = create_attachment(connection, &seed, "heavy-2", 3_000_000, 41)?;
            let error = AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[heavy_first, heavy_second],
                timestamp(42),
            )
            .expect_err("six megabytes exceed the per-Message total");
            assert_eq!(error.kind, "storageConstraint");
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn unclaimed_attachments_are_discardable_and_swept_by_age() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;

            let discarded = create_attachment(connection, &seed, "discarded", 1_024, 11)?;
            let removed = AttachmentRepository::discard_unclaimed(
                connection,
                &seed.workspace_id,
                &discarded,
            )?;
            assert_eq!(removed.as_deref(), Some("discarded.png"));
            // Discarding twice is not an error; the second call has nothing to
            // unlink, which is what the app's retry path needs.
            assert_eq!(
                AttachmentRepository::discard_unclaimed(
                    connection,
                    &seed.workspace_id,
                    &discarded
                )?,
                None
            );

            let claimed = create_attachment(connection, &seed, "claimed", 1_024, 12)?;
            AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[claimed],
                timestamp(13),
            )?;
            let error =
                AttachmentRepository::discard_unclaimed(connection, &seed.workspace_id, &claimed)
                    .expect_err("a sent attachment cannot be discarded");
            assert_eq!(error.kind, "storageConstraint");

            let stale = create_attachment(connection, &seed, "stale", 1_024, 14)?;
            let fresh = create_attachment(connection, &seed, "fresh", 1_024, 5_000)?;
            let swept = AttachmentRepository::sweep_unclaimed(
                connection,
                &seed.workspace_id,
                timestamp(1_000),
            )?;
            assert_eq!(swept, vec!["stale.png".to_string()]);
            assert!(
                AttachmentRepository::get(connection, &seed.workspace_id, &stale).is_err(),
                "the swept row is gone"
            );
            assert!(
                AttachmentRepository::get(connection, &seed.workspace_id, &fresh).is_ok(),
                "the sweep spares a draft the user may still be composing"
            );
            // A claimed attachment is never swept, however old it is.
            assert!(AttachmentRepository::get(connection, &seed.workspace_id, &claimed).is_ok());
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn deleting_a_conversation_returns_its_attachment_paths_to_unlink() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "look", "key-1", 10)?;
            let attachment = create_attachment(connection, &seed, "kept", 1_024, 11)?;
            AttachmentRepository::claim(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &initial.message.id,
                &[attachment],
                timestamp(12),
            )?;

            // The FK is RESTRICT, so this delete only succeeds because the
            // repository sweeps attachments before the Messages they hang on.
            let removed =
                ConversationRepository::delete(connection, &seed.workspace_id, &conversation_id)?;
            assert_eq!(removed, vec!["kept.png".to_string()]);
            let remaining =
                diesel::sql_query("SELECT COUNT(*) AS value FROM conversation_attachments")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            assert_eq!(remaining, 0);
            let violations =
                diesel::sql_query("SELECT COUNT(*) AS value FROM pragma_foreign_key_check")
                    .get_result::<IntegerValue>(connection)?
                    .value;
            assert_eq!(violations, 0);
            Ok(())
        })
        .await
        .expect("scenario");
}

// Transcript ordering, body capture, and restart cases live with the normalized
// assembler tests. Message admission and RunInput lifecycle tests above remain.

/// A user run that ran on a session of its own and completed, leaving the
/// session active for the agent to keep using.
fn completed_run_on_session(
    connection: &mut SqliteConnection,
    seed: &Seed,
    conversation_id: &ConversationId,
    input: &ottam_core::storage::models::ConversationRunInputId,
    at: i64,
) -> Result<
    (
        ottam_core::storage::models::ConversationRun,
        ottam_core::storage::models::AgentSession,
    ),
    StorageError,
> {
    let run = RunRepository::create_run_with_snapshot(
        connection,
        &CreateRunParams {
            workspace_id: &seed.workspace_id,
            conversation_id,
            engine_id: "claude",
            model_id: Some("opus"),
            request_options_json: Some(r#"{"modeID":"plan"}"#),
            pending_input_id: input,
            sources: &[snapshot(seed)],
        },
        timestamp(at),
    )?;
    let session = RunRepository::create_session(
        connection,
        &seed.workspace_id,
        conversation_id,
        "claude",
        Some(&format!("ext-{at}")),
        &scope_fingerprint(seed),
        timestamp(at + 1),
    )?;
    RunRepository::mark_run_running(
        connection,
        &run.id,
        &session.id,
        SessionOrigin::New,
        timestamp(at + 2),
    )?;
    let run = RunRepository::finalize_run(
        connection,
        &FinalizeRunParams {
            run_id: &run.id,
            status: RunStatus::Completed,
            stop_reason: Some("endTurn"),
            error_json: None,
        },
        timestamp(at + 3),
    )?;
    Ok((run, session))
}

#[tokio::test]
async fn autonomous_run_copies_scope_and_binds_the_continued_session() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let (continued, session) = completed_run_on_session(
                connection,
                &seed,
                &conversation_id,
                &initial.input.id,
                20,
            )?;
            assert_eq!(continued.origin, RunOrigin::User);

            let autonomous = RunRepository::create_autonomous_run(
                connection,
                &AutonomousRunParams {
                    conversation_id: &conversation_id,
                    continues_run_id: &continued.id,
                },
                timestamp(30),
            )?;
            assert_eq!(autonomous.origin, RunOrigin::Autonomous);
            assert_eq!(autonomous.status, RunStatus::Running);
            assert_eq!(autonomous.started_at, Some(timestamp(30)));
            assert_eq!(autonomous.agent_session_id, Some(session.id));
            assert_eq!(autonomous.session_origin, Some(SessionOrigin::Continued));
            assert_eq!(autonomous.engine_id, continued.engine_id);
            assert_eq!(autonomous.model_id, continued.model_id);
            assert_eq!(
                autonomous.request_options_json,
                continued.request_options_json
            );
            let sources = |connection: &mut SqliteConnection, run| {
                RunRepository::run_sources(connection, run).map(|sources| {
                    sources
                        .into_iter()
                        .map(|source| (source.role, source.source_id, source.canonical_path))
                        .collect::<Vec<_>>()
                })
            };
            assert_eq!(
                sources(connection, &autonomous.id)?,
                sources(connection, &continued.id)?
            );
            assert_eq!(
                RunRepository::active_run(connection, &conversation_id)?.map(|run| run.id),
                Some(autonomous.id)
            );

            // Storage still admits one active run: preempting an autonomous
            // run is the runtime's work, under its launch lock.
            let next = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "next",
                "key-2",
                timestamp(31),
            )?;
            let user_start = CreateRunParams {
                workspace_id: &seed.workspace_id,
                conversation_id: &conversation_id,
                engine_id: "claude",
                model_id: None,
                request_options_json: None,
                pending_input_id: &next.input.id,
                sources: &[snapshot(&seed)],
            };
            let refused =
                RunRepository::create_run_with_snapshot(connection, &user_start, timestamp(32))
                    .expect_err("one active run");
            assert_eq!(refused.kind, "runAlreadyActive");

            // A restart fails an autonomous run like any other, and frees the
            // Conversation for the user.
            let counts = RunRepository::recover_on_startup(connection, timestamp(33))?;
            assert_eq!(counts.failed_runs, 1);
            assert_eq!(counts.ambiguous_inputs, 0);
            let recovered = RunRepository::get_run(connection, &autonomous.id)?;
            assert_eq!(recovered.status, RunStatus::Failed);
            assert_eq!(recovered.origin, RunOrigin::Autonomous);
            let started =
                RunRepository::create_run_with_snapshot(connection, &user_start, timestamp(34))?;
            assert_eq!(started.origin, RunOrigin::User);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn autonomous_run_requires_an_active_session_and_no_active_run() {
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let open = |connection: &mut SqliteConnection, continues| {
                RunRepository::create_autonomous_run(
                    connection,
                    &AutonomousRunParams {
                        conversation_id: &conversation_id,
                        continues_run_id: continues,
                    },
                    timestamp(50),
                )
            };
            let run_count = |connection: &mut SqliteConnection| {
                diesel::sql_query("SELECT count(*) AS value FROM conversation_runs")
                    .get_result::<IntegerValue>(connection)
                    .map(|count| count.value)
            };

            // A run that never bound a session has nothing to continue.
            let unbound = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &initial.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(20),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &unbound.id,
                    status: RunStatus::Failed,
                    stop_reason: None,
                    error_json: Some("{\"kind\":\"authRequired\"}"),
                },
                timestamp(21),
            )?;
            let error = open(connection, &unbound.id).expect_err("no session");
            assert_eq!(error.kind, "sessionNotFound");

            let next = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "again",
                "key-2",
                timestamp(29),
            )?;
            let (continued, session) =
                completed_run_on_session(connection, &seed, &conversation_id, &next.input.id, 30)?;
            let busy = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "busy",
                "key-3",
                timestamp(40),
            )?;
            let active = RunRepository::create_run_with_snapshot(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &busy.input.id,
                    sources: &[snapshot(&seed)],
                },
                timestamp(41),
            )?;
            let error = open(connection, &continued.id).expect_err("a user run is active");
            assert_eq!(error.kind, "runAlreadyActive");
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &active.id,
                    status: RunStatus::Canceled,
                    stop_reason: Some("cancelled"),
                    error_json: None,
                },
                timestamp(42),
            )?;

            // The agent's session ended: there is no one to follow up.
            RunRepository::end_session(
                connection,
                &session.id,
                AgentSessionStatus::Lost,
                Some("adapterExited"),
                timestamp(43),
            )?;
            let before = run_count(connection)?;
            let error = open(connection, &continued.id).expect_err("a lost session");
            assert_eq!(error.kind, "sessionNotFound");
            assert_eq!(run_count(connection)?, before);
            Ok(())
        })
        .await
        .expect("scenario");
}

#[tokio::test]
async fn a_failed_input_can_be_retried_while_only_an_autonomous_run_is_active() {
    // The retry's start preempts the autonomous run, so it has a slot to run in.
    let home = TempDir::new().expect("home");
    let database = Database::open(home.path().to_path_buf()).expect("opens database");
    database
        .run("test", |connection| {
            let seed = seed(connection)?;
            let (conversation_id, initial) =
                project_conversation(connection, &seed, "hello", "key-1", 10)?;
            let (continued, _) = completed_run_on_session(
                connection,
                &seed,
                &conversation_id,
                &initial.input.id,
                20,
            )?;
            let failed = ConversationRepository::post_message(
                connection,
                &seed.workspace_id,
                &conversation_id,
                "next",
                "key-2",
                timestamp(30),
            )?;
            let failed_run = RunRepository::create_run_with_snapshot_and_mark_input_sending(
                connection,
                &CreateRunParams {
                    workspace_id: &seed.workspace_id,
                    conversation_id: &conversation_id,
                    engine_id: "claude",
                    model_id: None,
                    request_options_json: None,
                    pending_input_id: &failed.input.id,
                    sources: &[snapshot(&seed)],
                },
                DeliveryMode::Prompt,
                timestamp(31),
            )?
            .0;
            RunRepository::mark_input_failed(
                connection,
                &conversation_id,
                &failed.input.id,
                "authRequired",
                timestamp(32),
            )?;
            RunRepository::finalize_run(
                connection,
                &FinalizeRunParams {
                    run_id: &failed_run.id,
                    status: RunStatus::Failed,
                    stop_reason: None,
                    error_json: Some("{\"kind\":\"authRequired\"}"),
                },
                timestamp(33),
            )?;
            RunRepository::create_autonomous_run(
                connection,
                &AutonomousRunParams {
                    conversation_id: &conversation_id,
                    continues_run_id: &continued.id,
                },
                timestamp(34),
            )?;

            let resolved = RunRepository::resolve_undelivered_input(
                connection,
                &seed.workspace_id,
                &conversation_id,
                &failed.input.id,
                DeliveryResolutionAction::Retry {
                    idempotency_key: "key-retry",
                },
                timestamp(35),
            )?;
            assert_eq!(resolved.resolved_input.status(), RunInputStatus::Superseded);
            let retry = resolved.retry.expect("a retry input");
            assert_eq!(retry.input.status(), RunInputStatus::Pending);
            Ok(())
        })
        .await
        .expect("scenario");
}
