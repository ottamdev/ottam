use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    Conversation, ConversationId, ConversationMessage, ConversationMessageId,
    ConversationMessageRow, ConversationProject, ConversationProjectRow, ConversationRow,
    ConversationRunId, ConversationRunInput, ConversationRunInputId, ConversationRunInputRow,
    MessageRole, MessageSource, NewConversationMessageRow, NewConversationProjectRow,
    NewConversationRow, NewConversationRunInputRow, Project, ProjectId, ProjectRow, ProjectSource,
    ProjectSourceRow, RunInputKind, WorkingRootKind, WorkspaceId, WorkspaceProject,
    WorkspaceProjectRow,
};
use crate::storage::repositories::attachments::AttachmentRepository;
use crate::storage::repositories::core_settings::{
    CoreSettingsRepository, ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT,
};
use crate::storage::repositories::pagination::{assemble_page, Page, PageRequest};
use crate::storage::schema::{
    agent_sessions, conversation_messages, conversation_projects, conversation_run_inputs,
    conversation_run_sources, conversation_runs, conversations, project_sources, projects,
    run_events, transcript_items, workspace_projects,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// Spec byte caps (Database Growth And Payload Policy).
pub const USER_MESSAGE_MAX_BYTES: usize = 512 * 1024;
pub const JSON_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

pub struct ConversationRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationRootSelection<'a> {
    WorkspaceFiles,
    Project(&'a ProjectId),
}

#[derive(Debug, Clone, Copy)]
pub struct InitialMessage<'a> {
    pub body: &'a str,
    pub idempotency_key: &'a str,
}

#[derive(Debug, Clone)]
pub struct CreateConversationParams<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub title: &'a str,
    pub root: ConversationRootSelection<'a>,
    pub context_projects: &'a [ProjectId],
    pub initial_message: Option<InitialMessage<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationCreateRecord {
    pub conversation: Conversation,
    pub projects: Vec<ConversationProject>,
    pub initial_input: Option<MessageWithInput>,
}

/// The bounded repository graph needed to render one Conversation summary.
/// `ConversationRepository::list_summary_records` loads the entire list in a
/// fixed number of queries so Chat list latency does not grow as an N+1 path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationListRecord {
    pub conversation: Conversation,
    pub primary_link: Option<ConversationProject>,
    pub project: Option<Project>,
    pub source: Option<ProjectSource>,
    pub membership: Option<WorkspaceProject>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageWithInput {
    pub message: ConversationMessage,
    pub input: ConversationRunInput,
    pub replayed: bool,
}

impl ConversationRepository {
    pub fn create(
        connection: &mut SqliteConnection,
        params: &CreateConversationParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationCreateRecord, StorageError> {
        recorded_transaction(connection, "conversation.create", |connection| {
            Self::create_in_transaction(connection, params, now)
        })
    }

    /// The body of [`Self::create`] without its transaction, for callers
    /// that must commit additional work atomically with it.
    pub fn create_in_transaction(
        connection: &mut SqliteConnection,
        params: &CreateConversationParams<'_>,
        now: DateTime<Utc>,
    ) -> Result<ConversationCreateRecord, StorageError> {
        let workspace_id = params.workspace_id.to_string();
        let conversation_id = ConversationId::new_v7();
        let conversation_id_value = conversation_id.to_string();
        let now_ms = now.timestamp_millis();

        // A new conversation starts from the value last set in any conversation.
        let always_allow_tool_actions =
            CoreSettingsRepository::get_bool(connection, ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT)?
                .unwrap_or(false);
        let root_kind = match params.root {
            ConversationRootSelection::WorkspaceFiles => WorkingRootKind::WorkspaceFiles,
            ConversationRootSelection::Project(_) => WorkingRootKind::Project,
        };
        diesel::insert_into(conversations::table)
            .values(NewConversationRow {
                id: &conversation_id_value,
                workspace_id: &workspace_id,
                working_root_kind: root_kind.as_storage_str(),
                title: params.title,
                created_at: now_ms,
                updated_at: now_ms,
                archived_at: None,
                always_allow_tool_actions,
                parent_conversation_id: None,
                delegated_agent_name: None,
                delegation_key: None,
                delegated_task_title: None,
                delegated_avatar_seed: None,
                delegated_named_agent_id: None,
                delegated_from_run_id: None,
            })
            .execute(connection)?;

        let mut projects = Vec::new();
        if let ConversationRootSelection::Project(project_id) = params.root {
            projects.push(Self::link_project(
                connection,
                &conversation_id,
                params.workspace_id,
                project_id,
                "primary",
                now,
            )?);
        }
        for project_id in params.context_projects {
            projects.push(Self::link_project(
                connection,
                &conversation_id,
                params.workspace_id,
                project_id,
                "context",
                now,
            )?);
        }

        let initial_input = params
            .initial_message
            .map(|message| {
                Self::insert_user_message_with_input(
                    connection,
                    params.workspace_id,
                    &conversation_id,
                    message.body,
                    message.idempotency_key,
                    RunInputKind::Initial,
                    None,
                    None,
                    MessageSource::User,
                    0,
                    now,
                )
            })
            .transpose()?;

        Ok(ConversationCreateRecord {
            conversation: Self::get(connection, params.workspace_id, &conversation_id)?,
            projects,
            initial_input,
        })
    }

    fn link_project(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        workspace_id: &WorkspaceId,
        project_id: &ProjectId,
        role: &'static str,
        now: DateTime<Utc>,
    ) -> Result<ConversationProject, StorageError> {
        let workspace_id_value = workspace_id.to_string();
        let project_id_value = project_id.to_string();
        let membership_source: Option<String> = workspace_projects::table
            .filter(workspace_projects::workspace_id.eq(&workspace_id_value))
            .filter(workspace_projects::project_id.eq(&project_id_value))
            .filter(workspace_projects::removed_at.is_null())
            .select(workspace_projects::default_source_id)
            .first::<String>(connection)
            .optional()?;
        let source_id = membership_source.ok_or_else(|| {
            StorageError::new(
                "storageConstraint",
                "the selected Project is not an active member of the Workspace",
            )
            .recoverable(true)
        })?;
        let conversation_id_value = conversation_id.to_string();
        diesel::insert_into(conversation_projects::table)
            .values(NewConversationProjectRow {
                conversation_id: &conversation_id_value,
                workspace_id: &workspace_id_value,
                project_id: &project_id_value,
                source_id: &source_id,
                role,
                created_at: now.timestamp_millis(),
                removed_at: None,
            })
            .execute(connection)?;
        ConversationProject::try_from(
            conversation_projects::table
                .find((&conversation_id_value, &project_id_value))
                .select(ConversationProjectRow::as_select())
                .first::<ConversationProjectRow>(connection)?,
        )
    }

    pub fn get(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
    ) -> Result<Conversation, StorageError> {
        conversations::table
            .find(conversation_id.to_string())
            .filter(conversations::workspace_id.eq(workspace_id.to_string()))
            .select(ConversationRow::as_select())
            .first::<ConversationRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("Conversation"))?
            .try_into()
    }

    pub fn list(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<Conversation>, StorageError> {
        conversations::table
            .filter(conversations::workspace_id.eq(workspace_id.to_string()))
            .filter(conversations::archived_at.is_null())
            .order((conversations::updated_at.desc(), conversations::id.desc()))
            .select(ConversationRow::as_select())
            .load::<ConversationRow>(connection)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    /// Load active Conversations and every row needed for their working-root
    /// summaries in at most five queries, independent of Conversation count.
    pub fn list_summary_records(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<ConversationListRecord>, StorageError> {
        let conversations = Self::list(connection, workspace_id)?;
        if conversations.is_empty() {
            return Ok(Vec::new());
        }

        let conversation_ids = conversations
            .iter()
            .map(|conversation| conversation.id.to_string())
            .collect::<Vec<_>>();
        let links = conversation_projects::table
            .filter(conversation_projects::conversation_id.eq_any(&conversation_ids))
            .filter(conversation_projects::role.eq("primary"))
            .order((
                conversation_projects::conversation_id.asc(),
                conversation_projects::created_at.asc(),
                conversation_projects::project_id.asc(),
            ))
            .select(ConversationProjectRow::as_select())
            .load::<ConversationProjectRow>(connection)?
            .into_iter()
            .map(ConversationProject::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        let project_ids = links
            .iter()
            .map(|link| link.project_id.to_string())
            .collect::<Vec<_>>();
        let source_ids = links
            .iter()
            .map(|link| link.source_id.to_string())
            .collect::<Vec<_>>();

        let loaded_projects = projects::table
            .filter(projects::id.eq_any(&project_ids))
            .select(ProjectRow::as_select())
            .load::<ProjectRow>(connection)?
            .into_iter()
            .map(Project::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let loaded_sources = project_sources::table
            .filter(project_sources::id.eq_any(&source_ids))
            .select(ProjectSourceRow::as_select())
            .load::<ProjectSourceRow>(connection)?
            .into_iter()
            .map(ProjectSource::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let memberships = workspace_projects::table
            .filter(workspace_projects::workspace_id.eq(workspace_id.to_string()))
            .filter(workspace_projects::project_id.eq_any(&project_ids))
            .select(WorkspaceProjectRow::as_select())
            .load::<WorkspaceProjectRow>(connection)?
            .into_iter()
            .map(WorkspaceProject::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        let mut links_by_conversation = HashMap::new();
        for link in links {
            links_by_conversation
                .entry(link.conversation_id)
                .or_insert(link);
        }
        let projects_by_id = loaded_projects
            .into_iter()
            .map(|project| (project.id, project))
            .collect::<HashMap<_, _>>();
        let sources_by_id = loaded_sources
            .into_iter()
            .map(|source| (source.id, source))
            .collect::<HashMap<_, _>>();
        let memberships_by_project = memberships
            .into_iter()
            .map(|membership| (membership.project_id, membership))
            .collect::<HashMap<_, _>>();

        Ok(conversations
            .into_iter()
            .map(|conversation| {
                let primary_link = links_by_conversation.remove(&conversation.id);
                let project = primary_link
                    .as_ref()
                    .and_then(|link| projects_by_id.get(&link.project_id))
                    .cloned();
                let source = primary_link
                    .as_ref()
                    .and_then(|link| sources_by_id.get(&link.source_id))
                    .cloned();
                let membership = primary_link
                    .as_ref()
                    .and_then(|link| memberships_by_project.get(&link.project_id))
                    .cloned();
                ConversationListRecord {
                    conversation,
                    primary_link,
                    project,
                    source,
                    membership,
                }
            })
            .collect())
    }

    pub fn projects(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Vec<ConversationProject>, StorageError> {
        conversation_projects::table
            .filter(conversation_projects::conversation_id.eq(conversation_id.to_string()))
            .order(conversation_projects::project_id.asc())
            .select(ConversationProjectRow::as_select())
            .load::<ConversationProjectRow>(connection)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    /// Message plus pending initial RunInput append: position allocation,
    /// idempotency enforcement, and Conversation timestamp update in one
    /// transaction. Replaying an idempotency key returns the existing intent.
    pub fn post_message(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        body: &str,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        recorded_transaction(connection, "conversation.postMessage", |connection| {
            let position = super::transcript::TranscriptRepository::next_position(
                connection,
                &conversation_id.to_string(),
            )?;
            Self::post_message_in_transaction(
                connection,
                workspace_id,
                conversation_id,
                body,
                idempotency_key,
                position,
                now,
            )
        })
    }

    /// The body of [`Self::post_message`] without its transaction, for callers
    /// that must commit additional work atomically with it.
    pub fn post_message_in_transaction(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        body: &str,
        idempotency_key: &str,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        Self::insert_user_message_with_input(
            connection,
            workspace_id,
            conversation_id,
            body,
            idempotency_key,
            RunInputKind::Initial,
            None,
            None,
            MessageSource::User,
            position,
            now,
        )
    }

    /// Steering user Message plus pending ConversationRunInput creation,
    /// requested against the Run that was active when the user steered.
    pub fn append_steer_message(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        requested_against_run_id: &ConversationRunId,
        body: &str,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        recorded_transaction(connection, "conversation.steerMessage", |connection| {
            let position = super::transcript::TranscriptRepository::next_position(
                connection,
                &conversation_id.to_string(),
            )?;
            Self::append_steer_message_in_transaction(
                connection,
                workspace_id,
                conversation_id,
                requested_against_run_id,
                body,
                idempotency_key,
                position,
                now,
            )
        })
    }

    /// The body of [`Self::append_steer_message`] without its transaction, for callers
    /// that must commit additional work atomically with it.
    #[allow(clippy::too_many_arguments)] // Mirrors the atomic Message/RunInput admission contract.
    pub fn append_steer_message_in_transaction(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        requested_against_run_id: &ConversationRunId,
        body: &str,
        idempotency_key: &str,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        if let Some(existing) =
            Self::input_by_idempotency_key(connection, conversation_id, idempotency_key)?
        {
            let message = Self::get_message(connection, conversation_id, &existing.message_id)?;
            return Ok(MessageWithInput {
                message,
                input: existing,
                replayed: true,
            });
        }
        let unresolved = conversation_run_inputs::table
            .filter(conversation_run_inputs::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_run_inputs::status.eq("deliveryUnknown"))
            .count()
            .get_result::<i64>(connection)?;
        if unresolved > 0 {
            return Err(StorageError::new(
                "deliveryUnresolved",
                "a ConversationRunInput has unresolved delivery; resolve it before new work",
            )
            .recoverable(true));
        }
        let active_run_id = conversation_runs::table
            .filter(conversation_runs::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_runs::status.eq_any(["queued", "running"]))
            .select(conversation_runs::id)
            .first::<String>(connection)
            .optional()?;
        match active_run_id.as_deref() {
            Some(active) if active == requested_against_run_id.to_string() => {}
            Some(_) => {
                return Err(StorageError::new(
                    "runMismatch",
                    "a different Conversation run is active",
                )
                .recoverable(true));
            }
            None => {
                return Err(StorageError::new(
                    "runNoLongerActive",
                    "the expected run is no longer active",
                )
                .recoverable(true));
            }
        }
        Self::insert_user_message_with_input(
            connection,
            workspace_id,
            conversation_id,
            body,
            idempotency_key,
            RunInputKind::Steer,
            Some(requested_against_run_id),
            None,
            MessageSource::User,
            position,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_user_message_with_input(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        body: &str,
        idempotency_key: &str,
        kind: RunInputKind,
        requested_against_run_id: Option<&ConversationRunId>,
        retry_of_input_id: Option<&ConversationRunInputId>,
        source: MessageSource,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        if body.len() > USER_MESSAGE_MAX_BYTES {
            return Err(StorageError::new(
                "messageTooLarge",
                "the message exceeds the 512 KiB user input limit",
            )
            .recoverable(true));
        }
        let conversation_id_value = conversation_id.to_string();
        let workspace_id_value = workspace_id.to_string();

        // Validate ownership before writing anything.
        let _ = Self::get(connection, workspace_id, conversation_id)?;

        if let Some(existing) =
            Self::input_by_idempotency_key(connection, conversation_id, idempotency_key)?
        {
            let message = Self::get_message(connection, conversation_id, &existing.message_id)?;
            return Ok(MessageWithInput {
                message,
                input: existing,
                replayed: true,
            });
        }

        let message_id = ConversationMessageId::new_v7();
        let message_id_value = message_id.to_string();
        let now_ms = now.timestamp_millis();
        super::transcript::TranscriptRepository::admit_message(
            connection,
            &conversation_id_value,
            &message_id_value,
            position,
            body.len() as i64,
            now_ms,
        )?;
        diesel::insert_into(conversation_messages::table)
            .values(NewConversationMessageRow {
                id: &message_id_value,
                conversation_id: &conversation_id_value,
                role: MessageRole::User.as_storage_str(),
                body,
                created_at: now_ms,
                source: source.as_storage_str(),
                note_json: None,
            })
            .execute(connection)?;

        let input_id = ConversationRunInputId::new_v7();
        let input_id_value = input_id.to_string();
        let requested_against = requested_against_run_id.map(ToString::to_string);
        let retry_of = retry_of_input_id.map(ToString::to_string);
        diesel::insert_into(conversation_run_inputs::table)
            .values(NewConversationRunInputRow {
                id: &input_id_value,
                workspace_id: &workspace_id_value,
                conversation_id: &conversation_id_value,
                message_id: &message_id_value,
                requested_against_run_id: requested_against.as_deref(),
                target_run_id: None,
                delivery_sequence: None,
                kind: kind.as_storage_str(),
                retry_of_input_id: retry_of.as_deref(),
                idempotency_key,
                status: "pending",
                delivery_mode: None,
                delivery_evidence: None,
                requested_at: now_ms,
                delivery_attempted_at: None,
                delivered_at: None,
                ambiguity_detected_at: None,
                failed_at: None,
                failure_reason: None,
                resolved_at: None,
                resolution: None,
            })
            .execute(connection)?;

        Self::touch(connection, conversation_id, now)?;

        Ok(MessageWithInput {
            message: Self::get_message(connection, conversation_id, &message_id)?,
            input: Self::get_input(connection, conversation_id, &input_id)?,
            replayed: false,
        })
    }

    pub fn update_title(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        title: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        diesel::update(conversations::table.find(conversation_id.to_string()))
            .set((
                conversations::title.eq(title),
                conversations::updated_at.eq(now.timestamp_millis()),
            ))
            .execute(connection)?;
        Ok(())
    }

    /// Sets the conversation's flag and makes it the default for new
    /// conversations. Leaves `updated_at` alone: a setting is not activity.
    pub fn set_always_allow_tool_actions(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        enabled: bool,
    ) -> Result<(), StorageError> {
        recorded_transaction(
            connection,
            "conversation.setAlwaysAllowToolActions",
            |connection| {
                let updated = diesel::update(
                    conversations::table
                        .find(conversation_id.to_string())
                        .filter(conversations::workspace_id.eq(workspace_id.to_string())),
                )
                .set(conversations::always_allow_tool_actions.eq(enabled))
                .execute(connection)?;
                if updated == 0 {
                    return Err(StorageError::not_found("conversation"));
                }
                CoreSettingsRepository::set_bool(
                    connection,
                    ALWAYS_ALLOW_TOOL_ACTIONS_DEFAULT,
                    enabled,
                )
            },
        )
    }

    pub fn always_allow_tool_actions(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<bool, StorageError> {
        Ok(conversations::table
            .find(conversation_id.to_string())
            .select(conversations::always_allow_tool_actions)
            .first::<bool>(connection)
            .optional()?
            .unwrap_or(false))
    }

    pub(crate) fn touch(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        diesel::update(conversations::table.find(conversation_id.to_string()))
            .set(conversations::updated_at.eq(now.timestamp_millis()))
            .execute(connection)?;
        Ok(())
    }

    pub fn get_message(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        message_id: &ConversationMessageId,
    ) -> Result<ConversationMessage, StorageError> {
        conversation_messages::table
            .inner_join(
                transcript_items::table.on(transcript_items::id.eq(conversation_messages::id)),
            )
            .filter(conversation_messages::id.eq(message_id.to_string()))
            .filter(conversation_messages::conversation_id.eq(conversation_id.to_string()))
            .select(ConversationMessageRow::as_select())
            .first::<ConversationMessageRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("ConversationMessage"))?
            .try_into()
    }

    pub fn get_input(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        input_id: &ConversationRunInputId,
    ) -> Result<ConversationRunInput, StorageError> {
        conversation_run_inputs::table
            .find(input_id.to_string())
            .filter(conversation_run_inputs::conversation_id.eq(conversation_id.to_string()))
            .select(ConversationRunInputRow::as_select())
            .first::<ConversationRunInputRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("ConversationRunInput"))?
            .try_into()
    }

    pub fn input_by_idempotency_key(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        idempotency_key: &str,
    ) -> Result<Option<ConversationRunInput>, StorageError> {
        conversation_run_inputs::table
            .filter(conversation_run_inputs::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_run_inputs::idempotency_key.eq(idempotency_key))
            .select(ConversationRunInputRow::as_select())
            .first::<ConversationRunInputRow>(connection)
            .optional()?
            .map(TryInto::try_into)
            .transpose()
    }

    /// Newest-first message page keyed by position; the caller reverses for
    /// transcript order. The cursor is the position to continue strictly
    /// below.
    pub fn list_messages(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        request: &PageRequest<'_>,
    ) -> Result<Page<ConversationMessage>, StorageError> {
        let mut query = conversation_messages::table
            .inner_join(
                transcript_items::table.on(transcript_items::id.eq(conversation_messages::id)),
            )
            .filter(conversation_messages::conversation_id.eq(conversation_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            let before: i64 = cursor.parse().map_err(|_| {
                StorageError::new("storageConstraint", "the message cursor is invalid")
                    .recoverable(true)
            })?;
            query = query.filter(transcript_items::position.lt(before));
        }
        let rows = query
            .order(transcript_items::position.desc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(ConversationMessageRow::as_select())
            .load::<ConversationMessageRow>(connection)?;
        let page = assemble_page(
            rows,
            request,
            |row| row.body.len(),
            |row| row.position.to_string(),
        );
        let items = page
            .items
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            items,
            next_cursor: page.next_cursor,
        })
    }

    /// Deleting a Conversation removes its children in dependency order in
    /// one transaction. Callers stop active work and close external sessions
    /// first.
    ///
    /// Returns the attachment file names the caller must unlink once the
    /// transaction commits — filesystem work inside the transaction would not
    /// roll back with it.
    pub fn delete(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
    ) -> Result<Vec<String>, StorageError> {
        recorded_transaction(connection, "conversation.delete", |connection| {
            let _ = Self::get(connection, workspace_id, conversation_id)?;
            let conversation_id_value = conversation_id.to_string();
            // Attachments reference Messages with ON DELETE RESTRICT, so they
            // have to go first or the Message delete below fails.
            let attachment_paths =
                AttachmentRepository::delete_for_conversation(connection, conversation_id)?;
            let run_ids = conversation_runs::table
                .filter(conversation_runs::conversation_id.eq(&conversation_id_value))
                .select(conversation_runs::id)
                .load::<String>(connection)?;
            diesel::delete(run_events::table.filter(run_events::run_id.eq_any(&run_ids)))
                .execute(connection)?;
            // Retry inputs reference the input they retry with ON DELETE
            // RESTRICT, which SQLite checks row by row, so the bulk delete
            // below fails if it reaches an original before its retry. Unlink
            // the chain first; the rows are gone before anything reads them.
            diesel::update(
                conversation_run_inputs::table
                    .filter(conversation_run_inputs::conversation_id.eq(&conversation_id_value))
                    .filter(conversation_run_inputs::retry_of_input_id.is_not_null()),
            )
            .set(conversation_run_inputs::retry_of_input_id.eq(None::<String>))
            .execute(connection)?;
            diesel::delete(
                conversation_run_inputs::table
                    .filter(conversation_run_inputs::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            diesel::delete(
                conversation_run_sources::table
                    .filter(conversation_run_sources::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            // Assistant Messages reference their producing Run with
            // ON DELETE RESTRICT, so remove all Messages after inputs but
            // before Runs.
            diesel::delete(
                conversation_messages::table
                    .filter(conversation_messages::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            diesel::delete(
                transcript_items::table
                    .filter(transcript_items::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            diesel::delete(
                conversation_runs::table
                    .filter(conversation_runs::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            diesel::delete(
                agent_sessions::table
                    .filter(agent_sessions::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            diesel::delete(
                conversation_projects::table
                    .filter(conversation_projects::conversation_id.eq(&conversation_id_value)),
            )
            .execute(connection)?;
            // Its delegated children stay, as top-level conversations.
            diesel::update(
                conversations::table
                    .filter(conversations::parent_conversation_id.eq(&conversation_id_value)),
            )
            .set(conversations::parent_conversation_id.eq(None::<String>))
            .execute(connection)?;
            diesel::delete(conversations::table.find(&conversation_id_value))
                .execute(connection)?;
            Ok(attachment_paths)
        })
    }
}

/// What links a delegated child to the parent that started it.
pub struct DelegationLink<'a> {
    pub parent_id: &'a ConversationId,
    pub agent_name: &'a str,
    pub task_title: &'a str,
    pub delegation_key: &'a str,
    /// The parent's approval setting, which the child copies.
    pub always_allow_tool_actions: bool,
    pub avatar_seed: Option<u32>,
    /// The named agent the child was started as; none ad hoc.
    pub named_agent_id: Option<&'a str>,
    /// The parent's run that called `delegate`, when one was active.
    pub from_run_id: Option<&'a str>,
}

/// Delegation: children linked to their parent, the message that starts a
/// child, and the notes children leave in their parent.
impl ConversationRepository {
    /// Links a new conversation to the parent that delegated it, with the
    /// approval setting it copied, and marks its first message as from
    /// Ottam.
    pub fn mark_delegated(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        link: DelegationLink<'_>,
        first_message: Option<(&ConversationMessageId, &str)>,
    ) -> Result<(), StorageError> {
        diesel::update(conversations::table.find(conversation_id.to_string()))
            .set((
                conversations::parent_conversation_id.eq(Some(link.parent_id.to_string())),
                conversations::delegated_agent_name.eq(Some(link.agent_name)),
                conversations::delegated_task_title.eq(Some(link.task_title)),
                conversations::delegation_key.eq(Some(link.delegation_key)),
                conversations::always_allow_tool_actions.eq(link.always_allow_tool_actions),
                conversations::delegated_avatar_seed.eq(link.avatar_seed.map(i64::from)),
                conversations::delegated_named_agent_id.eq(link.named_agent_id),
                conversations::delegated_from_run_id.eq(link.from_run_id),
            ))
            .execute(connection)?;
        // The first message carries the task; `note_json` holds a message's
        // one delegation record, which on a child's first message is this.
        if let Some((message_id, task_json)) = first_message {
            diesel::update(conversation_messages::table.find(message_id.to_string()))
                .set((
                    conversation_messages::source.eq(MessageSource::Delegation.as_storage_str()),
                    conversation_messages::note_json.eq(Some(task_json)),
                ))
                .execute(connection)?;
        }
        Ok(())
    }

    /// Clears the prompt digest of a child whose agent never started.
    pub fn forget_delegation_key(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<(), StorageError> {
        diesel::update(conversations::table.find(conversation_id.to_string()))
            .set(conversations::delegation_key.eq(None::<String>))
            .execute(connection)?;
        Ok(())
    }

    /// A parent's delegated children, oldest first.
    pub fn children(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        parent_id: &ConversationId,
    ) -> Result<Vec<Conversation>, StorageError> {
        conversations::table
            .filter(conversations::workspace_id.eq(workspace_id.to_string()))
            .filter(conversations::parent_conversation_id.eq(parent_id.to_string()))
            .filter(conversations::archived_at.is_null())
            .order((conversations::created_at.asc(), conversations::id.asc()))
            .select(ConversationRow::as_select())
            .load::<ConversationRow>(connection)?
            .into_iter()
            .map(Conversation::try_from)
            .collect()
    }

    /// Adds a note: a `system` message carrying it, at the end of the
    /// transcript. A `delivered` note is stored as already carried.
    ///
    /// One transaction over the transcript item and its message: a
    /// transcript read between the two would find a message item with no
    /// message.
    pub fn post_note(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        body: &str,
        note_json: &str,
        delivered: bool,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<ConversationMessage, StorageError> {
        recorded_transaction(connection, "conversation.postNote", |connection| {
            let message_id = ConversationMessageId::new_v7();
            let message_id_value = message_id.to_string();
            let conversation_id_value = conversation_id.to_string();
            let now_ms = now.timestamp_millis();
            super::transcript::TranscriptRepository::admit_message(
                connection,
                &conversation_id_value,
                &message_id_value,
                position,
                body.len() as i64,
                now_ms,
            )?;
            diesel::insert_into(conversation_messages::table)
                .values(NewConversationMessageRow {
                    id: &message_id_value,
                    conversation_id: &conversation_id_value,
                    role: MessageRole::System.as_storage_str(),
                    body,
                    created_at: now_ms,
                    source: MessageSource::User.as_storage_str(),
                    note_json: Some(note_json),
                })
                .execute(connection)?;
            if delivered {
                diesel::update(conversation_messages::table.find(&message_id_value))
                    .set(conversation_messages::note_delivered_at.eq(Some(now_ms)))
                    .execute(connection)?;
            }
            Self::touch(connection, conversation_id, now)?;
            Self::get_message(connection, conversation_id, &message_id)
        })
    }

    /// The prompt that delivers a parent's waiting notes: a pending input
    /// like a user message's, on a message the transcript does not show.
    /// One transaction, as in [`Self::post_message`].
    pub fn post_note_delivery(
        connection: &mut SqliteConnection,
        workspace_id: &WorkspaceId,
        conversation_id: &ConversationId,
        body: &str,
        idempotency_key: &str,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<MessageWithInput, StorageError> {
        recorded_transaction(connection, "conversation.postNoteDelivery", |connection| {
            Self::insert_user_message_with_input(
                connection,
                workspace_id,
                conversation_id,
                body,
                idempotency_key,
                RunInputKind::Initial,
                None,
                None,
                MessageSource::NoteDelivery,
                position,
                now,
            )
        })
    }

    /// The notes no prompt has carried yet, oldest first.
    pub fn undelivered_notes(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
    ) -> Result<Vec<ConversationMessage>, StorageError> {
        conversation_messages::table
            .inner_join(
                transcript_items::table.on(transcript_items::id.eq(conversation_messages::id)),
            )
            .filter(conversation_messages::conversation_id.eq(conversation_id.to_string()))
            .filter(conversation_messages::role.eq(MessageRole::System.as_storage_str()))
            .filter(conversation_messages::note_json.is_not_null())
            .filter(conversation_messages::note_delivered_at.is_null())
            .order(transcript_items::position.asc())
            .select(ConversationMessageRow::as_select())
            .load::<ConversationMessageRow>(connection)?
            .into_iter()
            .map(ConversationMessage::try_from)
            .collect()
    }

    /// The notes still waiting in any of `conversations`, as stored.
    pub fn waiting_note_json(
        connection: &mut SqliteConnection,
        conversations: &[ConversationId],
    ) -> Result<Vec<String>, StorageError> {
        Ok(conversation_messages::table
            .filter(
                conversation_messages::conversation_id
                    .eq_any(conversations.iter().map(ToString::to_string)),
            )
            .filter(conversation_messages::role.eq(MessageRole::System.as_storage_str()))
            .filter(conversation_messages::note_delivered_at.is_null())
            .select(conversation_messages::note_json)
            .load::<Option<String>>(connection)?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Rewrites notes in place, as when Send to Parent clears `deferred`.
    pub fn set_note_json(
        connection: &mut SqliteConnection,
        notes: &[(ConversationMessageId, String)],
    ) -> Result<(), StorageError> {
        for (id, note_json) in notes {
            diesel::update(conversation_messages::table.find(id.to_string()))
                .set(conversation_messages::note_json.eq(Some(note_json)))
                .execute(connection)?;
        }
        Ok(())
    }

    pub fn mark_notes_delivered(
        connection: &mut SqliteConnection,
        message_ids: &[ConversationMessageId],
        now: DateTime<Utc>,
    ) -> Result<(), StorageError> {
        let ids: Vec<String> = message_ids.iter().map(ToString::to_string).collect();
        diesel::update(conversation_messages::table.filter(conversation_messages::id.eq_any(ids)))
            .set(conversation_messages::note_delivered_at.eq(Some(now.timestamp_millis())))
            .execute(connection)?;
        Ok(())
    }
}
