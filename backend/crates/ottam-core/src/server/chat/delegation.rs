//! What delegation keeps per conversation: the parent link, the message that
//! starts a child, and the notes children leave in their parent. The run
//! side lives in `server/agents/delegation.rs`.

use super::ids::{parse_conversation_id, parse_run_id};
use super::ChatService;
use crate::core_error::CoreError;
use crate::storage::models::{ConversationMessageId, MessageRole};
use crate::storage::repositories::{ConversationRepository, DelegationLink};
use chrono::Utc;
use ottam_protocol::{
    ConversationID, ConversationListTranscriptParams, ConversationMessageID,
    ConversationReadTranscriptBodyParams, ConversationRunID, ConversationRunInput,
    ConversationRunStatus, DelegationActivity, DelegationNote, DelegationTask, TranscriptItemKind,
};

/// A delegated child, as its parent lists it.
#[derive(Debug, Clone)]
pub(in crate::server) struct DelegatedChild {
    pub(in crate::server) conversation_id: ConversationID,
    pub(in crate::server) name: String,
    /// The task it was delegated, as its parent named it.
    pub(in crate::server) task_title: String,
}

/// What links a delegated child to its parent.
#[derive(Debug, Clone)]
pub(in crate::server) struct ChildLink {
    pub(in crate::server) parent: ConversationID,
    pub(in crate::server) agent_name: String,
    pub(in crate::server) task_title: String,
    pub(in crate::server) delegation_key: String,
    /// The parent's approval setting, which the child copies.
    pub(in crate::server) always_allow_tool_actions: bool,
    /// The seed the child's tile is drawn from: a random one ad hoc, none
    /// for a named agent's child, which draws its agent's current tile.
    pub(in crate::server) avatar_seed: Option<u32>,
    /// The named agent the child was started as, by id, so it keeps
    /// following that agent's tile when the agent is renamed.
    pub(in crate::server) named_agent_id: Option<String>,
    /// The parent's run that called `delegate`, so clients can show the
    /// child in that turn when the agent's transcript hides the call.
    pub(in crate::server) from_run: Option<ConversationRunID>,
}

/// A note no prompt has carried yet.
#[derive(Debug, Clone)]
pub(in crate::server) struct WaitingNote {
    pub(in crate::server) message_id: ConversationMessageID,
    pub(in crate::server) note: DelegationNote,
    /// The child's final message, as much as the parent's prompt carries.
    pub(in crate::server) report: String,
}

/// Finished runs' counts kept before the cache starts over.
const FINISHED_ACTIVITY_KEPT: usize = 1024;

/// The most of a child's final message read for its parent.
const REPORT_READ_LIMIT: usize = 64 * 1024;

impl ChatService {
    /// Links a just-created child to its parent; see
    /// [`ConversationRepository::mark_delegated`].
    pub(in crate::server) async fn mark_delegated(
        &self,
        child: &ConversationID,
        link: ChildLink,
        first_message: Option<(ConversationMessageID, DelegationTask)>,
    ) -> Result<(), CoreError> {
        let child = parse_conversation_id(child)?;
        let parent = parse_conversation_id(&link.parent)?;
        let first_message = first_message
            .map(|(id, task)| -> Result<_, CoreError> {
                let task = serde_json::to_string(&task).map_err(|_| {
                    CoreError::new("taskUnavailable", "the task could not be stored")
                })?;
                Ok((ConversationMessageId::try_from(id.0)?, task))
            })
            .transpose()?;
        self.database
            .run("conversation.markDelegated", move |connection| {
                ConversationRepository::mark_delegated(
                    connection,
                    &child,
                    DelegationLink {
                        parent_id: &parent,
                        agent_name: &link.agent_name,
                        task_title: &link.task_title,
                        delegation_key: &link.delegation_key,
                        always_allow_tool_actions: link.always_allow_tool_actions,
                        avatar_seed: link.avatar_seed,
                        named_agent_id: link.named_agent_id.as_deref(),
                        from_run_id: link.from_run.as_ref().map(|run| run.0.as_str()),
                    },
                    first_message.as_ref().map(|(id, task)| (id, task.as_str())),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    /// Unlinks a child from the `delegate` call that failed to start it,
    /// so the parent's cards match calls to the children they started.
    pub(in crate::server) async fn forget_delegation_key(
        &self,
        child: &ConversationID,
    ) -> Result<(), CoreError> {
        let child = parse_conversation_id(child)?;
        self.database
            .run("conversation.forgetDelegationKey", move |connection| {
                ConversationRepository::forget_delegation_key(connection, &child)
            })
            .await
            .map_err(CoreError::from)
    }

    /// The conversation that delegated this one, while it exists.
    pub(in crate::server) async fn parent_of(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<Option<ConversationID>, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.parent", move |connection| {
                Ok(
                    ConversationRepository::get(connection, &workspace_id, &conversation_id)?
                        .parent_conversation_id
                        .map(|id| ConversationID(id.to_string())),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    /// The name a child was delegated as, and its parent, when it is one.
    pub(in crate::server) async fn delegation_of(
        &self,
        conversation_id: &ConversationID,
    ) -> Result<Option<(ConversationID, String)>, CoreError> {
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(conversation_id)?;
        self.database
            .run("conversation.delegation", move |connection| {
                let conversation =
                    ConversationRepository::get(connection, &workspace_id, &conversation_id)?;
                Ok(conversation.parent_conversation_id.map(|parent| {
                    (
                        ConversationID(parent.to_string()),
                        conversation.delegated_agent_name.unwrap_or_default(),
                    )
                }))
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn children(
        &self,
        parent: &ConversationID,
    ) -> Result<Vec<DelegatedChild>, CoreError> {
        let workspace_id = self.workspace()?;
        let parent = parse_conversation_id(parent)?;
        self.database
            .run("conversation.children", move |connection| {
                Ok(
                    ConversationRepository::children(connection, &workspace_id, &parent)?
                        .into_iter()
                        .map(|child| DelegatedChild {
                            conversation_id: ConversationID(child.id.to_string()),
                            task_title: child.delegated_task_title.unwrap_or(child.title),
                            name: child.delegated_agent_name.unwrap_or_default(),
                        })
                        .collect(),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    /// Adds a note to the end of `conversation`'s transcript: a child's
    /// report or message in its parent, or a parent's message in its child.
    /// `report` is the message's body, which the prompt carries; the row
    /// shows the note's excerpt. A `delivered` note is only a record: no
    /// prompt will carry it.
    pub(in crate::server) async fn post_note(
        &self,
        parent: &ConversationID,
        note: &DelegationNote,
        report: String,
        delivered: bool,
    ) -> Result<(), CoreError> {
        let publish = parent.clone();
        let conversation_id = parse_conversation_id(parent)?;
        let note_json = serde_json::to_string(note)
            .map_err(|_| CoreError::new("noteUnavailable", "the note could not be stored"))?;
        let body = report;
        let message = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.postNote",
                1,
                move |connection, position| {
                    ConversationRepository::post_note(
                        connection,
                        &conversation_id,
                        &body,
                        &note_json,
                        delivered,
                        position,
                        Utc::now(),
                    )
                },
            )
            .await?;
        self.transcript_coordinator
            .admitted(&publish, message.position)?;
        self.publish_transcript_boundary(&publish).await;
        Ok(())
    }

    /// The prompt that delivers waiting notes, as a pending input of a
    /// message the transcript does not show.
    pub(in crate::server) async fn post_note_delivery(
        &self,
        parent: &ConversationID,
        body: String,
    ) -> Result<ConversationRunInput, CoreError> {
        let publish = parent.clone();
        let workspace_id = self.workspace()?;
        let conversation_id = parse_conversation_id(parent)?;
        let idempotency_key = uuid::Uuid::now_v7().to_string();
        let record = self
            .database
            .run_transcript(
                conversation_id.to_string(),
                "conversation.postNoteDelivery",
                1,
                move |connection, position| {
                    ConversationRepository::post_note_delivery(
                        connection,
                        &workspace_id,
                        &conversation_id,
                        &body,
                        &idempotency_key,
                        position,
                        Utc::now(),
                    )
                },
            )
            .await?;
        self.transcript_coordinator
            .admitted(&publish, record.message.position)?;
        self.publish_transcript_boundary(&publish).await;
        Ok(super::wire::input_to_wire(record.input))
    }

    pub(in crate::server) async fn waiting_notes(
        &self,
        parent: &ConversationID,
    ) -> Result<Vec<WaitingNote>, CoreError> {
        let conversation_id = parse_conversation_id(parent)?;
        self.database
            .run("conversation.waitingNotes", move |connection| {
                Ok(
                    ConversationRepository::undelivered_notes(connection, &conversation_id)?
                        .into_iter()
                        .filter(|message| message.role == MessageRole::System)
                        .filter_map(|message| {
                            let note = serde_json::from_str(message.note_json.as_deref()?).ok()?;
                            Some(WaitingNote {
                                message_id: ConversationMessageID(message.id.to_string()),
                                note,
                                report: message.body,
                            })
                        })
                        .collect(),
                )
            })
            .await
            .map_err(CoreError::from)
    }

    pub(in crate::server) async fn mark_notes_delivered(
        &self,
        notes: &[WaitingNote],
    ) -> Result<(), CoreError> {
        let ids = notes
            .iter()
            .map(|note| ConversationMessageId::try_from(note.message_id.0.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        self.database
            .run("conversation.markNotesDelivered", move |connection| {
                ConversationRepository::mark_notes_delivered(connection, &ids, Utc::now())
            })
            .await
            .map_err(CoreError::from)
    }

    /// Each child with a deferred report waiting in one of `parents`, and
    /// the turn that report is of: the latest, when several wait.
    pub(in crate::server) async fn deferred_reports(
        &self,
        parents: std::collections::HashSet<ConversationID>,
    ) -> Result<std::collections::HashMap<ConversationID, ConversationRunID>, CoreError> {
        let parents = parents
            .iter()
            .map(parse_conversation_id)
            .collect::<Result<Vec<_>, _>>()?;
        let notes = self
            .database
            .run("conversation.deferredReports", move |connection| {
                ConversationRepository::waiting_note_json(connection, &parents)
            })
            .await
            .map_err(CoreError::from)?;
        let mut deferred = std::collections::HashMap::<ConversationID, ConversationRunID>::new();
        for note in notes {
            let Ok(note) = serde_json::from_str::<DelegationNote>(&note) else {
                continue;
            };
            let (true, Some(run)) = (note.deferred, note.run_id) else {
                continue;
            };
            let latest = deferred
                .entry(note.child_conversation_id)
                .or_insert_with(|| run.clone());
            if run.0 > latest.0 {
                *latest = run;
            }
        }
        Ok(deferred)
    }

    /// Send to Parent: these deferred notes wake their parent like any
    /// other report.
    pub(in crate::server) async fn undefer_notes(
        &self,
        notes: &[WaitingNote],
    ) -> Result<(), CoreError> {
        let mut updates = Vec::with_capacity(notes.len());
        for waiting in notes {
            let id = ConversationMessageId::try_from(waiting.message_id.0.clone())?;
            let note = DelegationNote {
                deferred: false,
                ..waiting.note.clone()
            };
            let json = serde_json::to_string(&note)
                .map_err(|_| CoreError::new("noteUnavailable", "the note could not be stored"))?;
            updates.push((id, json));
        }
        self.database
            .run("conversation.undeferNotes", move |connection| {
                ConversationRepository::set_note_json(connection, &updates)
            })
            .await
            .map_err(CoreError::from)
    }

    /// What a child's latest run has done, for its card in the parent: the
    /// run's status and its own tool calls by kind. Tool calls inside its
    /// subagents count toward the subagent, not here.
    pub(in crate::server) async fn delegation_activity(
        &self,
        child: &ConversationID,
    ) -> Result<DelegationActivity, CoreError> {
        let detail = self.conversation_detail(child).await?;
        let Some(run) = detail.latest_run_page.runs.into_iter().next() else {
            return Ok(DelegationActivity::default());
        };
        let finished = !matches!(
            run.status,
            ConversationRunStatus::Queued | ConversationRunStatus::Running
        );
        if finished {
            if let Some(activity) = self.finished_activity().get(&run.run_id.0) {
                return Ok(activity.clone());
            }
        }
        let mut activity = DelegationActivity {
            latest_run_status: Some(run.status),
            ..DelegationActivity::default()
        };
        let page = self
            .list_transcript(ConversationListTranscriptParams {
                workspace_id: self.workspace_id.clone(),
                conversation_id: child.clone(),
                limit: None,
                cursor: None,
                subagent_thread_id: None,
            })
            .await?;
        for item in page.items {
            if item.kind != TranscriptItemKind::Tool
                || item.run_id.as_ref() != Some(&run.run_id)
                || item.subagent_thread_id.is_some()
            {
                continue;
            }
            let Some(tool) = item.tool else { continue };
            let count = if tool.subagent_thread.is_some() {
                &mut activity.subagents
            } else {
                match tool.kind.as_deref() {
                    Some("execute") => &mut activity.commands,
                    Some("read") => &mut activity.reads,
                    Some("edit" | "delete" | "move") => &mut activity.edits,
                    Some("search") => &mut activity.searches,
                    Some("fetch") => &mut activity.fetches,
                    Some("think") => continue,
                    _ => &mut activity.other,
                }
            };
            *count += 1;
        }
        if finished {
            let mut cache = self.finished_activity();
            if cache.len() >= FINISHED_ACTIVITY_KEPT {
                cache.clear();
            }
            cache.insert(run.run_id.0.clone(), activity.clone());
        }
        Ok(activity)
    }

    fn finished_activity(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, DelegationActivity>> {
        self.transcript_coordinator
            .finished_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The run's final message: its last assistant text, whole up to a
    /// bound. Empty when the run wrote none.
    pub(in crate::server) async fn final_report(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
    ) -> Result<String, CoreError> {
        parse_run_id(run)?;
        let page = self
            .list_transcript(ConversationListTranscriptParams {
                workspace_id: self.workspace_id.clone(),
                conversation_id: conversation.clone(),
                limit: None,
                cursor: None,
                subagent_thread_id: None,
            })
            .await?;
        // The page is newest first, and the run is the latest or near it.
        let Some(item) = page.items.into_iter().find(|item| {
            item.kind == TranscriptItemKind::AssistantText
                && item.run_id.as_ref() == Some(run)
                && item.subagent_thread_id.is_none()
        }) else {
            return Ok(String::new());
        };
        if item.body_byte_count as usize <= item.body_preview.len() {
            return Ok(item.body_preview);
        }
        let mut text = String::new();
        let mut offset = 0;
        loop {
            let read = self
                .read_transcript_body(ConversationReadTranscriptBodyParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    item_id: item.item_id.clone(),
                    revision: item.revision,
                    offset,
                })
                .await?;
            text.push_str(&read.text);
            match read.next_offset {
                Some(next) if text.len() < REPORT_READ_LIMIT && next > offset => offset = next,
                _ => break,
            }
        }
        Ok(text)
    }
}
