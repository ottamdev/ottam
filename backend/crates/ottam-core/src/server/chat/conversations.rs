use super::attachments::{messages_to_wire, unlink_attachment_files, wire_attachments};
use super::ids::{
    parse_attachment_id, parse_conversation_id, parse_project_id, parse_run_id, unknown_run,
};
use super::projection::{project_working_root, ResolvedWorkingRoot};
use super::wire::{
    event_to_delta, input_to_wire, iso, link_to_wire, message_with_input_to_wire, run_to_wire,
    session_to_wire,
};
use super::{ensure_workspace_active, ChatService};
use crate::core_error::CoreError;
use crate::server::workspaces::workspace_summary;
use crate::storage::database::{read_snapshot, recorded_transaction};
use crate::storage::ensure_workspace_paths;
use crate::storage::error::StorageError;
use crate::storage::models::{
    Conversation, ConversationId, ConversationProject, ConversationProjectRole, ConversationRun,
    ConversationRunId, WorkingRootKind as DomainWorkingRootKind, WorkspaceId,
};
use crate::storage::repositories::{
    AttachmentRepository, ConversationListRecord, ConversationRepository,
    ConversationRootSelection, CreateConversationParams, InitialMessage, NamedAgentRepository,
    PageRequest, ProjectRepository, RunRepository, WorkspaceRepository,
};
use chrono::Utc;
use ottam_protocol::{
    AgentSessionPage, ConversationAttachmentID, ConversationCreateResult, ConversationDetail,
    ConversationID, ConversationInitialMessage, ConversationListEventsResult,
    ConversationMessagePage, ConversationPostMessageResult, ConversationRunID,
    ConversationRunInputPage, ConversationRunPage, ConversationRunSummary, ConversationSummary,
    ProjectID, WorkingRootAvailability, WorkingRootSelection, WorkingRootSummary, WorkspaceID,
};
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;
use std::path::Path;

/// Latest-page bounds for `ConversationDetail`.
const DETAIL_MESSAGE_LIMIT: usize = 50;
const DETAIL_COLLECTION_LIMIT: usize = 20;
/// Default page bounds for the cursor-paged list methods.
const DEFAULT_LIST_LIMIT: usize = 100;
const MESSAGE_BYTE_BUDGET: usize = 512 * 1024;
const EVENT_BYTE_BUDGET: usize = 256 * 1024;
const COLLECTION_BYTE_BUDGET: usize = 64 * 1024;

// ----------------------------------------------------------------------
// Shared load/summarize helpers (one connection, no protocol in repos)
// ----------------------------------------------------------------------

/// The detail is five separate pages, and a caller reads them as one answer: a
/// run reported `completed` beside a message page that predates the assistant
/// reply finalized with it is a lie the client has no way to detect. Take the
/// whole thing from one snapshot.
fn load_detail(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
) -> Result<ConversationDetail, StorageError> {
    read_snapshot(connection, |connection| {
        load_detail_from_snapshot(connection, home, workspace_id, conversation_id)
    })
}

fn load_detail_from_snapshot(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
) -> Result<ConversationDetail, StorageError> {
    let conversation = active_conversation(connection, workspace_id, conversation_id)?;
    let links = ConversationRepository::projects(connection, conversation_id)?;
    let summary = summarize_conversation(connection, home, &conversation, &links)?;
    let workspace = workspace_summary(WorkspaceRepository::get(connection, workspace_id)?);
    let projects = links
        .iter()
        .filter(|link| link.removed_at.is_none())
        .map(link_to_wire)
        .collect();
    let message_page = ConversationRepository::list_messages(
        connection,
        conversation_id,
        &PageRequest::first(DETAIL_MESSAGE_LIMIT, MESSAGE_BYTE_BUDGET),
    )?;
    let run_page = RunRepository::list_runs(
        connection,
        conversation_id,
        &PageRequest::first(DETAIL_COLLECTION_LIMIT, COLLECTION_BYTE_BUDGET),
    )?;
    let runs = run_page
        .items
        .into_iter()
        .map(|run| run_summary(connection, run))
        .collect::<Result<Vec<_>, _>>()?;
    let input_page = RunRepository::list_inputs(
        connection,
        conversation_id,
        &PageRequest::first(DETAIL_COLLECTION_LIMIT, COLLECTION_BYTE_BUDGET),
    )?;
    let session_page = RunRepository::list_sessions(
        connection,
        conversation_id,
        &PageRequest::first(DETAIL_COLLECTION_LIMIT, COLLECTION_BYTE_BUDGET),
    )?;
    Ok(ConversationDetail {
        conversation: summary,
        workspace,
        projects,
        latest_message_page: ConversationMessagePage {
            messages: messages_to_wire(
                connection,
                home,
                workspace_id,
                conversation_id,
                message_page.items,
            )?,
            next_cursor: message_page.next_cursor,
        },
        latest_run_page: ConversationRunPage {
            runs,
            next_cursor: run_page.next_cursor,
        },
        latest_run_input_page: ConversationRunInputPage {
            run_inputs: input_page.items.into_iter().map(input_to_wire).collect(),
            next_cursor: input_page.next_cursor,
        },
        latest_agent_session_page: AgentSessionPage {
            sessions: session_page
                .items
                .into_iter()
                .map(session_to_wire)
                .collect(),
            next_cursor: session_page.next_cursor,
        },
    })
}

pub(super) fn active_conversation(
    connection: &mut SqliteConnection,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
) -> Result<Conversation, StorageError> {
    let conversation = ConversationRepository::get(connection, workspace_id, conversation_id)
        .map_err(|error| {
            if error.kind == "storageNotFound" {
                StorageError::new("unknownConversationID", "Conversation is unavailable")
                    .recoverable(true)
            } else {
                error
            }
        })?;
    if conversation.archived_at.is_some() {
        return Err(
            StorageError::new("unknownConversationID", "Conversation is unavailable")
                .recoverable(true),
        );
    }
    Ok(conversation)
}

fn ensure_working_root_available(
    connection: &mut SqliteConnection,
    home: &Path,
    workspace_id: &WorkspaceId,
    conversation_id: &ConversationId,
) -> Result<WorkingRootSummary, StorageError> {
    let conversation = active_conversation(connection, workspace_id, conversation_id)?;
    let links = ConversationRepository::projects(connection, conversation_id)?;
    let root = working_root_summary(connection, home, &conversation, &links)?;
    if root.availability != WorkingRootAvailability::Available {
        return Err(StorageError::new(
            "workingRootUnavailable",
            "the Conversation's working root is unavailable",
        )
        .recoverable(true));
    }
    Ok(root)
}

fn summarize_conversation(
    connection: &mut SqliteConnection,
    home: &Path,
    conversation: &Conversation,
    links: &[ConversationProject],
) -> Result<ConversationSummary, StorageError> {
    // A named agent's child stores no seed: it draws its agent's current
    // tile, found by id so a rename keeps it, and a Shuffle in Settings
    // reaches every child.
    let named_seed = match (
        conversation.delegated_avatar_seed,
        &conversation.delegated_named_agent_id,
    ) {
        (None, Some(id)) => NamedAgentRepository::get(connection, id)?
            .and_then(|named| named.avatar_seed)
            .and_then(|seed| u32::try_from(seed).ok()),
        _ => None,
    };
    Ok(ConversationSummary {
        conversation_id: ConversationID(conversation.id.to_string()),
        workspace_id: WorkspaceID(conversation.workspace_id.to_string()),
        working_root: working_root_summary(connection, home, conversation, links)?,
        title: conversation.title.clone(),
        created_at: iso(conversation.created_at),
        updated_at: iso(conversation.updated_at),
        always_allow_tool_actions: conversation.always_allow_tool_actions,
        parent_conversation_id: conversation
            .parent_conversation_id
            .map(|id| ConversationID(id.to_string())),
        delegated_agent_name: conversation.delegated_agent_name.clone(),
        delegated_task_title: conversation.delegated_task_title.clone(),
        delegated_avatar_seed: conversation.delegated_avatar_seed.or(named_seed),
        delegation_key: conversation.delegation_key.clone(),
        delegated_from_run_id: conversation
            .delegated_from_run_id
            .clone()
            .map(ConversationRunID),
        delegation_activity: None,
        deferred_report_run_id: None,
    })
}

fn summarize_conversation_record(
    home: &Path,
    record: ConversationListRecord,
    named_seeds: &HashMap<String, u32>,
) -> Result<ConversationSummary, StorageError> {
    let working_root = working_root_summary_from_record(home, &record.conversation, &record)?;
    let conversation = record.conversation;
    let delegated_avatar_seed = conversation.delegated_avatar_seed.or_else(|| {
        conversation
            .delegated_named_agent_id
            .as_ref()
            .and_then(|id| named_seeds.get(id).copied())
    });
    Ok(ConversationSummary {
        conversation_id: ConversationID(conversation.id.to_string()),
        workspace_id: WorkspaceID(conversation.workspace_id.to_string()),
        working_root,
        title: conversation.title,
        created_at: iso(conversation.created_at),
        updated_at: iso(conversation.updated_at),
        always_allow_tool_actions: conversation.always_allow_tool_actions,
        parent_conversation_id: conversation
            .parent_conversation_id
            .map(|id| ConversationID(id.to_string())),
        delegated_agent_name: conversation.delegated_agent_name,
        delegated_task_title: conversation.delegated_task_title,
        delegated_avatar_seed,
        delegation_key: conversation.delegation_key,
        delegated_from_run_id: conversation.delegated_from_run_id.map(ConversationRunID),
        delegation_activity: None,
        deferred_report_run_id: None,
    })
}

fn working_root_summary_from_record(
    home: &Path,
    conversation: &Conversation,
    record: &ConversationListRecord,
) -> Result<WorkingRootSummary, StorageError> {
    match conversation.working_root_kind {
        DomainWorkingRootKind::WorkspaceFiles => {
            let paths = ensure_workspace_paths(home, &conversation.workspace_id)?;
            Ok(project_working_root(ResolvedWorkingRoot::WorkspaceFiles {
                canonical_path: &paths.files,
            }))
        }
        DomainWorkingRootKind::Project => Ok(project_working_root(ResolvedWorkingRoot::Project {
            primary_link: record.primary_link.as_ref(),
            project: record.project.as_ref(),
            source: record.source.as_ref(),
            membership: record.membership.as_ref(),
        })),
    }
}

pub(super) fn working_root_summary(
    connection: &mut SqliteConnection,
    home: &Path,
    conversation: &Conversation,
    links: &[ConversationProject],
) -> Result<WorkingRootSummary, StorageError> {
    match conversation.working_root_kind {
        DomainWorkingRootKind::WorkspaceFiles => {
            let paths = ensure_workspace_paths(home, &conversation.workspace_id)?;
            Ok(project_working_root(ResolvedWorkingRoot::WorkspaceFiles {
                canonical_path: &paths.files,
            }))
        }
        DomainWorkingRootKind::Project => {
            let primary_link = links
                .iter()
                .find(|link| link.role == ConversationProjectRole::Primary);
            let Some(link) = primary_link else {
                return Ok(project_working_root(ResolvedWorkingRoot::Project {
                    primary_link: None,
                    project: None,
                    source: None,
                    membership: None,
                }));
            };
            let project =
                optional_not_found(ProjectRepository::get_project(connection, &link.project_id))?;
            let source =
                optional_not_found(ProjectRepository::get_source(connection, &link.source_id))?;
            let membership = optional_not_found(WorkspaceRepository::membership(
                connection,
                &conversation.workspace_id,
                &link.project_id,
            ))?;
            Ok(project_working_root(ResolvedWorkingRoot::Project {
                primary_link,
                project: project.as_ref(),
                source: source.as_ref(),
                membership: membership.as_ref(),
            }))
        }
    }
}

fn optional_not_found<T>(result: Result<T, StorageError>) -> Result<Option<T>, StorageError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind == "storageNotFound" => Ok(None),
        Err(error) => Err(error),
    }
}

fn primary_project_id(
    connection: &mut SqliteConnection,
    conversation_id: &ConversationId,
) -> Result<Option<ProjectID>, StorageError> {
    Ok(
        ConversationRepository::projects(connection, conversation_id)?
            .into_iter()
            .find(|link| link.role == ConversationProjectRole::Primary && link.removed_at.is_none())
            .map(|link| ProjectID(link.project_id.to_string())),
    )
}

pub(super) fn conversation_run_checked(
    connection: &mut SqliteConnection,
    conversation_id: &ConversationId,
    run_id: &ConversationRunId,
) -> Result<ConversationRun, StorageError> {
    let run = RunRepository::get_run(connection, run_id).map_err(|_| unknown_run())?;
    if run.conversation_id != *conversation_id {
        return Err(unknown_run());
    }
    Ok(run)
}

pub(super) fn run_summary(
    connection: &mut SqliteConnection,
    run: ConversationRun,
) -> Result<ConversationRunSummary, StorageError> {
    // Wire run summaries report the *external* harness session id: it is the
    // identity the bridge resumes on and clients display.
    let external = match &run.agent_session_id {
        Some(session_id) => RunRepository::get_session(connection, session_id)?.external_session_id,
        None => None,
    };
    Ok(run_to_wire(run, external))
}

fn page_request(limit: Option<u32>, cursor: Option<&str>, byte_budget: usize) -> PageRequest<'_> {
    PageRequest {
        limit: limit.map_or(DEFAULT_LIST_LIMIT, |limit| limit as usize),
        byte_budget,
        cursor,
    }
}

fn clean_title(title: Option<&str>, message: Option<&str>) -> String {
    let candidate = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| message.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or("New conversation");
    candidate.chars().take(80).collect()
}

impl ChatService {
    pub(in crate::server) async fn create_conversation(
        &self,
        working_root: WorkingRootSelection,
        context_project_ids: Vec<ProjectID>,
        title: Option<String>,
        message: Option<ConversationInitialMessage>,
    ) -> Result<ConversationCreateResult, CoreError> {
        let workspace_id = self.workspace()?;
        let home = self.home.clone();
        self.database
            .run("conversation.create", move |connection| {
                ensure_workspace_active(connection, &workspace_id)?;
                let root_project = match &working_root {
                    WorkingRootSelection::WorkspaceFiles => None,
                    WorkingRootSelection::Project { project_id, .. } => {
                        Some(parse_project_id(project_id)?)
                    }
                };
                let context_projects = context_project_ids
                    .iter()
                    .map(parse_project_id)
                    .collect::<Result<Vec<_>, _>>()?;
                let message_body = message
                    .as_ref()
                    .map(|message| message.body.trim())
                    .filter(|body| !body.is_empty());
                let attachment_ids = message
                    .as_ref()
                    .map(|message| {
                        message
                            .attachment_ids
                            .iter()
                            .map(parse_attachment_id)
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                let title = clean_title(title.as_deref(), message_body);
                let now = Utc::now();
                // One transaction over both: a rejected claim (over the total
                // byte cap, say) must take the Conversation down with it rather
                // than leave a half-formed one behind.
                let record =
                    recorded_transaction(connection, "conversation.create", |connection| {
                        let record = ConversationRepository::create_in_transaction(
                            connection,
                            &CreateConversationParams {
                                workspace_id: &workspace_id,
                                title: &title,
                                root: match &root_project {
                                    Some(project_id) => {
                                        ConversationRootSelection::Project(project_id)
                                    }
                                    None => ConversationRootSelection::WorkspaceFiles,
                                },
                                context_projects: &context_projects,
                                // An image-only first message still seeds an initial
                                // input, so the empty body has to reach the repository.
                                initial_message: match (&message, message_body) {
                                    (Some(message), Some(body)) => Some(InitialMessage {
                                        body,
                                        idempotency_key: &message.idempotency_key,
                                    }),
                                    (Some(message), None) if !attachment_ids.is_empty() => {
                                        Some(InitialMessage {
                                            body: "",
                                            idempotency_key: &message.idempotency_key,
                                        })
                                    }
                                    _ => None,
                                },
                            },
                            now,
                        )?;
                        if let Some(initial) = record.initial_input.as_ref() {
                            AttachmentRepository::claim(
                                connection,
                                &workspace_id,
                                &record.conversation.id,
                                &initial.message.id,
                                &attachment_ids,
                                now,
                            )?;
                        }
                        Ok(record)
                    })?;
                let detail =
                    load_detail(connection, &home, &workspace_id, &record.conversation.id)?;
                Ok(ConversationCreateResult {
                    conversation: detail,
                    initial_input: record
                        .initial_input
                        .map(|initial| input_to_wire(initial.input)),
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_conversations(
        &self,
    ) -> Result<Vec<ConversationSummary>, CoreError> {
        let workspace_id = self.workspace()?;
        let home = self.home.clone();
        let mut summaries: Vec<ConversationSummary> = self
            .database
            .run("conversation.list", move |connection| {
                ensure_workspace_active(connection, &workspace_id)?;
                let named_seeds: HashMap<String, u32> = NamedAgentRepository::list(connection)?
                    .into_iter()
                    .filter_map(|named| Some((named.id, u32::try_from(named.avatar_seed?).ok()?)))
                    .collect();
                ConversationRepository::list_summary_records(connection, &workspace_id)?
                    .into_iter()
                    .map(|record| summarize_conversation_record(&home, record, &named_seeds))
                    .collect()
            })
            .await
            .map_err(CoreError::from)?;
        // A child's card in its parent shows what its latest turn did. One
        // that can't be read shows no counts.
        let activities = futures::future::join_all(summaries.iter().map(|summary| async {
            match summary.delegated_agent_name {
                Some(_) => self
                    .delegation_activity(&summary.conversation_id)
                    .await
                    .ok(),
                None => None,
            }
        }))
        .await;
        for (summary, activity) in summaries.iter_mut().zip(activities) {
            summary.delegation_activity = activity;
        }
        // A child whose report of a turn the user drove still waits in its
        // parent offers Send to Parent on that turn.
        let parents: std::collections::HashSet<ConversationID> = summaries
            .iter()
            .filter(|summary| summary.delegated_agent_name.is_some())
            .filter_map(|summary| summary.parent_conversation_id.clone())
            .collect();
        let deferred = self.deferred_reports(parents).await.unwrap_or_default();
        for summary in &mut summaries {
            summary.deferred_report_run_id = deferred.get(&summary.conversation_id).cloned();
        }
        Ok(summaries)
    }

    pub(in crate::server) async fn conversation_detail(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<ConversationDetail, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let home = self.home.clone();
        let mut detail = self
            .database
            .run("conversation.get", move |connection| {
                load_detail(connection, &home, &workspace_id, &conversation_id)
            })
            .await
            .map_err(CoreError::from)?;
        let summary = &mut detail.conversation;
        if let (Some(_), Some(parent)) = (
            &summary.delegated_agent_name,
            &summary.parent_conversation_id,
        ) {
            let deferred = self
                .deferred_reports([parent.clone()].into())
                .await
                .unwrap_or_default();
            summary.deferred_report_run_id = deferred.get(&summary.conversation_id).cloned();
        }
        Ok(detail)
    }

    pub(in crate::server) async fn update_conversation_title(
        &self,
        conversation_id: &ConversationID,
        title: String,
    ) -> Result<(), CoreError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Ok(());
        }
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.updateTitle", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                ConversationRepository::update_title(
                    connection,
                    &conversation_id,
                    &clean_title(Some(&title), None),
                    Utc::now(),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn post_message(
        &self,
        conversation_id: &ConversationID,
        body: String,
        attachment_ids: Vec<ConversationAttachmentID>,
        idempotency_key: String,
    ) -> Result<ConversationPostMessageResult, CoreError> {
        let publish_conversation = conversation_id.clone();
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let home = self.home.clone();
        let result = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.postMessage",
                1,
                move |connection, position| {
                    let body = body.trim().to_string();
                    let attachment_ids = attachment_ids
                        .iter()
                        .map(parse_attachment_id)
                        .collect::<Result<Vec<_>, _>>()?;
                    // An image on its own is a complete message — "what's wrong
                    // with this?" is often carried entirely by the screenshot.
                    if body.is_empty() && attachment_ids.is_empty() {
                        return Err(StorageError::new(
                            "emptyConversationMessage",
                            "conversation message must not be empty",
                        )
                        .recoverable(true));
                    }
                    ensure_working_root_available(
                        connection,
                        &home,
                        &workspace_id,
                        &conversation_id,
                    )?;
                    let now = Utc::now();
                    // One transaction over both: a rejected claim rolls the Message
                    // and its pending RunInput back with it, rather than leaving a
                    // Message the user never successfully sent.
                    let (posted, claimed) = recorded_transaction(
                        connection,
                        "conversation.postMessage",
                        |connection| {
                            let posted = ConversationRepository::post_message_in_transaction(
                                connection,
                                &workspace_id,
                                &conversation_id,
                                &body,
                                &idempotency_key,
                                position,
                                now,
                            )?;
                            let claimed = if posted.replayed {
                                AttachmentRepository::validate_replay(
                                    connection,
                                    &conversation_id,
                                    &posted.message.id,
                                    &attachment_ids,
                                )?
                            } else {
                                AttachmentRepository::claim(
                                    connection,
                                    &workspace_id,
                                    &conversation_id,
                                    &posted.message.id,
                                    &attachment_ids,
                                    now,
                                )?
                            };
                            Ok((posted, claimed))
                        },
                    )?;
                    let attachments = wire_attachments(&home, &workspace_id, claimed)?;
                    Ok(message_with_input_to_wire(posted, attachments))
                },
            )
            .await
            .map_err(CoreError::from);
        if let Ok(record) = &result {
            self.transcript_coordinator
                .admitted(&publish_conversation, record.message.position as i64)?;
            self.publish_transcript_boundary(&publish_conversation)
                .await;
        }
        result
    }

    /// Writes the conversation's flag and the default new conversations take.
    pub(in crate::server) async fn set_always_allow_tool_actions(
        &self,
        conversation_id: &ConversationID,
        enabled: bool,
    ) -> Result<(), CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run(
                "conversation.setAlwaysAllowToolActions",
                move |connection| {
                    ConversationRepository::set_always_allow_tool_actions(
                        connection,
                        &workspace_id,
                        &conversation_id,
                        enabled,
                    )
                },
            )
            .await
            .map_err(CoreError::from)
    }

    /// Read on every permission request, so a change applies mid-turn.
    pub(in crate::server) async fn always_allow_tool_actions(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<bool, CoreError> {
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.alwaysAllowToolActions", move |connection| {
                ConversationRepository::always_allow_tool_actions(connection, &conversation_id)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn delete_conversation(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<(), CoreError> {
        let wire_conversation = conversation_id.clone();
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let home = self.home.clone();
        let sweep_workspace_id = workspace_id;
        self.transcript_coordinator
            .payloads
            .fence(&wire_conversation.0)
            .await;
        let removed = self
            .database
            .run("conversation.delete", move |connection| {
                ConversationRepository::delete(connection, &workspace_id, &conversation_id)
            })
            .await
            .map_err(|error| {
                self.transcript_coordinator
                    .payloads
                    .unfence(&wire_conversation.0);
                CoreError::from(error)
            })?;
        // Reclaim after the main commit: independent payload/attachment cleanup
        // would otherwise survive its rollback. Reconciliation retries leftovers.
        self.transcript_coordinator.forget(&wire_conversation);
        self.transcript_coordinator
            .payloads
            .forget_conversation(&wire_conversation.0);
        let payload_home = home.clone();
        let payload_workspace = self.workspace_id.0.clone();
        let payload_conversation = wire_conversation.0.clone();
        let _ = tokio::task::spawn_blocking(move || -> crate::storage::payload::Result<()> {
            crate::storage::payload::maintenance::Maintenance::open(&payload_home)?
                .delete_conversation(&payload_workspace, &payload_conversation)
        })
        .await;
        self.database.payload_worker().notify_changed();
        unlink_attachment_files(&home, &sweep_workspace_id, &removed);
        Ok(())
    }

    pub(in crate::server) async fn working_root(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<WorkingRootSummary, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let home = self.home.clone();
        self.database
            .run("conversation.workingRoot", move |connection| {
                let conversation =
                    active_conversation(connection, &workspace_id, &conversation_id)?;
                let links = ConversationRepository::projects(connection, &conversation_id)?;
                working_root_summary(connection, &home, &conversation, &links)
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_messages(
        &self,
        conversation_id: &ConversationID,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<ConversationMessagePage, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        let home = self.home.clone();
        self.database
            .run("conversation.listMessages", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let page = ConversationRepository::list_messages(
                    connection,
                    &conversation_id,
                    &page_request(limit, cursor.as_deref(), MESSAGE_BYTE_BUDGET),
                )?;
                Ok(ConversationMessagePage {
                    messages: messages_to_wire(
                        connection,
                        &home,
                        &workspace_id,
                        &conversation_id,
                        page.items,
                    )?,
                    next_cursor: page.next_cursor,
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_runs(
        &self,
        conversation_id: &ConversationID,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<ConversationRunPage, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.listRuns", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let page = RunRepository::list_runs(
                    connection,
                    &conversation_id,
                    &page_request(limit, cursor.as_deref(), COLLECTION_BYTE_BUDGET),
                )?;
                let runs = page
                    .items
                    .into_iter()
                    .map(|run| run_summary(connection, run))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ConversationRunPage {
                    runs,
                    next_cursor: page.next_cursor,
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_run_inputs(
        &self,
        conversation_id: &ConversationID,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<ConversationRunInputPage, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.listRunInputs", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let page = RunRepository::list_inputs(
                    connection,
                    &conversation_id,
                    &page_request(limit, cursor.as_deref(), COLLECTION_BYTE_BUDGET),
                )?;
                Ok(ConversationRunInputPage {
                    run_inputs: page.items.into_iter().map(input_to_wire).collect(),
                    next_cursor: page.next_cursor,
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_agent_sessions(
        &self,
        conversation_id: &ConversationID,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<AgentSessionPage, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.listAgentSessions", move |connection| {
                let _ = active_conversation(connection, &workspace_id, &conversation_id)?;
                let page = RunRepository::list_sessions(
                    connection,
                    &conversation_id,
                    &page_request(limit, cursor.as_deref(), COLLECTION_BYTE_BUDGET),
                )?;
                Ok(AgentSessionPage {
                    sessions: page.items.into_iter().map(session_to_wire).collect(),
                    next_cursor: page.next_cursor,
                })
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn list_events(
        &self,
        conversation_id: &ConversationID,
        run_id: Option<ConversationRunID>,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<ConversationListEventsResult, CoreError> {
        let workspace_id = self.workspace()?;
        let wire_workspace_id = self.workspace_id.clone();
        let wire_conversation_id = conversation_id.clone();
        let conversation_id = parse_conversation_id(conversation_id)?;
        let run_id = run_id.as_ref().map(parse_run_id).transpose()?;
        self.database
            .run("conversation.listEvents", move |connection| {
                let conversation =
                    active_conversation(connection, &workspace_id, &conversation_id)?;
                let _ = conversation;
                let project_id = primary_project_id(connection, &conversation_id)?;
                let request = page_request(limit, cursor.as_deref(), EVENT_BYTE_BUDGET);
                let page = match run_id {
                    Some(run_id) => {
                        let run = conversation_run_checked(connection, &conversation_id, &run_id)?;
                        RunRepository::list_events(connection, &run.id, &request)?
                    }
                    None => RunRepository::list_conversation_events(
                        connection,
                        &conversation_id,
                        &request,
                    )?,
                };
                Ok(ConversationListEventsResult {
                    events: page
                        .items
                        .into_iter()
                        .map(|event| {
                            event_to_delta(
                                event,
                                &wire_workspace_id,
                                &wire_conversation_id,
                                project_id.clone(),
                            )
                        })
                        .collect(),
                    next_cursor: page.next_cursor,
                })
            })
            .await
            .map_err(CoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_root_lookup_only_treats_not_found_as_detached() {
        let busy = StorageError::new("storageBusy", "the database is busy").recoverable(true);
        let propagated = optional_not_found::<()>(Err(busy))
            .expect_err("busy storage must not look like a detached working root");
        assert_eq!(propagated.kind, "storageBusy");
        assert_eq!(propagated.message, "the database is busy");
        assert!(propagated.recoverable);

        let missing = optional_not_found::<()>(Err(StorageError::not_found("ProjectSource")))
            .expect("a missing working-root row is represented as detached");
        assert!(missing.is_none());
    }
}
