use super::{
    conversations::active_conversation, ids::parse_conversation_id, wire::iso, ChatService,
};
use crate::core_error::CoreError;
use crate::storage::database::read_snapshot;
use crate::storage::error::StorageError;
use crate::storage::repositories::transcript::{
    TranscriptDetail, TranscriptRecord, TranscriptRepository as Repo, PAGE_RECORDS,
};
use chrono::{TimeZone, Utc};
use ottam_protocol::*;
use diesel::SqliteConnection;
use serde_json::Value;
use std::sync::OnceLock;
use uuid::Uuid;

mod assembly;
mod live;
#[cfg(test)]
mod payload_tests;
#[cfg(test)]
mod sqlite_payload_tests;
#[cfg(test)]
mod subagent_tests;
mod subagents;
pub(in crate::server) use live::{image_call, TranscriptCoordinator};
pub(in crate::server) use subagents::{
    native_subagent_id, native_thread_id, NATIVE_ANCHOR_KEY, NATIVE_FOLLOW_UP_STAMP,
    NATIVE_THREAD_STAMP,
};

fn core_instance() -> String {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE.get_or_init(|| Uuid::now_v7().to_string()).clone()
}

const TRANSCRIPT_PAGE_BYTES: usize = 5 * 1024 * 1024;

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value)
        .map_err(|_| StorageError::new("storageCorrupt", "Invalid typed transcript record"))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value)
        .map_err(|_| StorageError::new("storageConstraint", "Invalid transcript record"))
}

fn saved_session_states(
    connection: &mut SqliteConnection,
    conversation: &str,
) -> Result<Vec<ConversationSessionState>, StorageError> {
    use diesel::prelude::*;
    #[derive(QueryableByName)]
    struct Saved {
        #[diesel(sql_type = diesel::sql_types::Text)]
        run_id: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        signal: String,
    }
    let rows = diesel::sql_query(
        "SELECT r.id AS run_id, json_extract(e.payload_json, '$.signal') AS signal
        FROM conversation_runs r JOIN run_events e ON e.run_id = r.id
        WHERE r.conversation_id = ? AND e.kind = 'sessionMetadata'
        AND json_type(e.payload_json, '$.signal') = 'object'
        AND e.sequence = (SELECT MAX(newer.sequence) FROM run_events newer
            WHERE newer.run_id = r.id AND newer.kind = 'sessionMetadata'
            AND json_type(newer.payload_json, '$.signal') = 'object')
        ORDER BY r.started_at DESC, r.id DESC LIMIT 32",
    )
    .bind::<diesel::sql_types::Text, _>(conversation)
    .load::<Saved>(connection)?;
    rows.into_iter()
        .map(|row| {
            let mut signal: ConversationSessionSignal = decode(&row.signal)?;
            signal.activity = None;
            Ok(ConversationSessionState {
                run_id: ConversationRunID(row.run_id),
                signal,
                live: false,
            })
        })
        .collect()
}

fn capture_status(
    connection: &mut SqliteConnection,
    conversation: &str,
    visible_runs: &[String],
    include_recent: bool,
) -> Result<Vec<TranscriptCaptureStatus>, StorageError> {
    use crate::storage::schema::{conversation_runs as runs, transcript_capture as capture};
    use diesel::prelude::*;
    let mut query = runs::table
        .left_join(capture::table)
        .filter(runs::conversation_id.eq(conversation))
        .into_boxed();
    if !include_recent {
        query = query.filter(runs::id.eq_any(visible_runs));
    }
    let values = query
        .order(runs::queued_at.desc())
        .limit(200)
        .select((
            runs::id,
            runs::status,
            capture::state.nullable(),
            capture::gaps_json.nullable(),
        ))
        .load::<(String, String, Option<String>, Option<String>)>(connection)?;
    values
        .into_iter()
        .map(|(run, status, state, gaps)| {
            let gaps: Vec<TranscriptCoverage> = decode(gaps.as_deref().unwrap_or("[]"))?;
            let problem = if state.as_deref() == Some("partial") {
                Some(partial_capture_problem(&gaps).into())
            } else if state.as_deref() == Some("open")
                && !matches!(status.as_str(), "queued" | "running")
            {
                Some("Capture may be incomplete".into())
            } else {
                None
            };
            Ok(TranscriptCaptureStatus {
                run_id: ConversationRunID(run),
                state,
                problem,
                gaps,
            })
        })
        .collect()
}

fn partial_capture_problem(gaps: &[TranscriptCoverage]) -> &'static str {
    if gaps.is_empty() {
        "Some tool details could not be saved completely"
    } else {
        "Some transcript output was not saved"
    }
}

fn to_wire(record: TranscriptRecord) -> Result<ConversationTranscriptItem, StorageError> {
    to_wire_with_live(record, false)
}

fn to_wire_with_live(
    record: TranscriptRecord,
    live: bool,
) -> Result<ConversationTranscriptItem, StorageError> {
    let row = record.row;
    let mut item = ConversationTranscriptItem {
        item_id: row.id,
        conversation_id: ConversationID(row.conversation_id),
        run_id: row.run_id.map(ConversationRunID),
        run_status: record
            .run_status
            .map(|status| decode(&encode(&status)?))
            .transpose()?,
        position: row.position as u64,
        revision: row.revision as u64,
        committed_revision: Some(record.committed_revision as u64),
        kind: decode(&encode(&row.kind)?)?,
        completion: decode(&encode(&row.completion)?)?,
        created_at: iso(Utc
            .timestamp_millis_opt(row.created_at)
            .single()
            .ok_or_else(|| StorageError::new("storageCorrupt", "Invalid transcript timestamp"))?),
        updated_at: iso(Utc
            .timestamp_millis_opt(row.updated_at)
            .single()
            .ok_or_else(|| StorageError::new("storageCorrupt", "Invalid transcript timestamp"))?),
        provider_message_id: row.provider_message_id,
        adapter_metadata: Default::default(),
        subagent_thread_id: None,
        body_preview: record.preview,
        body_byte_count: row.body_byte_count as u64,
        preview_omitted: false,
        message: None,
        tool: None,
        payloads: Vec::new(),
        plan: None,
        permission: None,
        gap: None,
    };
    let mut metadata: subagents::StoredMetadata = decode(&row.adapter_metadata_json)?;
    // Core's routing field, not the adapter's: the item carries it once.
    item.subagent_thread_id = metadata.adapter.subagent_thread_id.take();
    item.adapter_metadata = metadata.adapter;
    match record.detail {
        TranscriptDetail::None => {}
        TranscriptDetail::Tool { json, .. } => {
            let mut tool: TranscriptTool = decode(&json)?;
            if !live {
                for payload in &mut tool.payloads {
                    if payload.availability == "pending" {
                        payload.availability = "unavailable".into();
                        payload.reason = Some("unfinishedOutput".into());
                    }
                }
            }
            if item.kind == TranscriptItemKind::Image {
                item.payloads = tool.payloads;
            } else {
                item.tool = Some(tool);
            }
        }
        TranscriptDetail::Plan(json) => item.plan = Some(decode(&json)?),
        TranscriptDetail::Permission { json, .. } => item.permission = Some(decode(&json)?),
        TranscriptDetail::Gap {
            end_position,
            dropped,
            reason,
        } => {
            item.gap = Some(TranscriptGap {
                end_position: end_position as u64,
                dropped_updates: dropped as u64,
                reason,
            })
        }
    }
    subagents::read_disconnected(&mut item);
    Ok(item)
}

fn unavailable(
    role: &str,
    byte_count: Option<u64>,
    upstream_truncated: Option<bool>,
) -> TranscriptPayload {
    TranscriptPayload {
        role: role.into(),
        availability: "unavailable".into(),
        reason: Some("notCaptured".into()),
        payload_id: None,
        byte_count,
        upstream_truncated,
        locally_truncated: false,
        revision: 0,
        parts: Vec::new(),
    }
}

/// Both adapters stamp a backgrounded tool call; Codex omits the AIR
/// `version`, so the stamp is read without one.
fn is_backgrounded(payload: &Value) -> bool {
    payload
        .pointer("/_meta/jetbrains/air/asyncTasks/backgrounded")
        .and_then(Value::as_bool)
        == Some(true)
}

/// Lifecycle frames own the state, so the stamp only marks a tool that has no
/// task yet.
fn stamp_background(tool: &mut TranscriptTool, payload: &Value) {
    if tool.background_task.is_none() && is_backgrounded(payload) {
        tool.background_task = Some(TranscriptBackgroundTask {
            id: None,
            state: AsyncTaskState::Running,
            output_file_path: None,
        });
    }
}

/// Settles a question still pending as not answered. `true` when it was.
fn settle_question(tool: &mut TranscriptTool) -> bool {
    match tool.question.as_mut() {
        Some(question)
            if question.state == ottam_protocol::TranscriptQuestionState::Pending =>
        {
            question.state = ottam_protocol::TranscriptQuestionState::NotAnswered;
            true
        }
        _ => false,
    }
}

fn normalize_tool(prior: Option<TranscriptTool>, payload: &Value) -> TranscriptTool {
    let mut tool = prior.unwrap_or_else(|| TranscriptTool {
        provider_tool_call_id: None,
        name: None,
        kind: None,
        title: None,
        status: None,
        arguments: Value::Null,
        locations: vec![],
        started_at: None,
        completed_at: None,
        exit_code: None,
        partial_metadata: payload.get("title").is_none() || payload.get("kind").is_none(),
        payloads: vec![unavailable("output", None, None)],
        background_task: None,
        subagent_thread: None,
        subagent_follow_up: None,
        question: None,
    });
    stamp_background(&mut tool, payload);
    for (key, field) in [
        ("toolCallId", &mut tool.provider_tool_call_id),
        ("name", &mut tool.name),
        ("kind", &mut tool.kind),
        ("title", &mut tool.title),
        ("status", &mut tool.status),
        ("startedAt", &mut tool.started_at),
        ("completedAt", &mut tool.completed_at),
    ] {
        if let Some(value) = payload.get(key) {
            *field = value.as_str().map(str::to_owned);
        }
    }
    if let Some(value) = payload.get("exitCode") {
        tool.exit_code = value.as_i64();
    }
    if let Some(value) = payload.get("locations") {
        tool.locations = value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|location| {
                Some(TranscriptLocation {
                    path: location.get("path")?.as_str()?.into(),
                    line: location.get("line").and_then(Value::as_u64),
                })
            })
            .collect();
    }
    if let Some(input) = payload.get("rawInput") {
        let mut input = input.clone();
        let file_change = super::payloads::is_file_change(
            tool.name.as_deref(),
            tool.kind.as_deref(),
            tool.title.as_deref(),
            super::payloads::is_device_call(payload),
            &input,
        );
        tool.payloads
            .retain(|payload| payload.role != "fileChangeInput");
        if file_change {
            let (removed, bytes) = strip_file_content(&mut input);
            if removed {
                tool.payloads
                    .push(unavailable("fileChangeInput", bytes, None));
            }
        }
        tool.arguments = input;
    }
    // Content can contain diffs, images, or terminal references. None is an
    // ordinary argument; never serialize the envelope as a tool result.
    if payload.get("rawOutput").is_some() || payload.get("content").is_some() {
        let count = payload
            .get("rawOutput")
            .and_then(Value::as_str)
            .map(|text| text.len() as u64);
        let upstream = payload
            .get("truncated")
            .and_then(Value::as_bool)
            .or_else(|| {
                payload
                    .pointer("/rawOutput/truncated")
                    .and_then(Value::as_bool)
            });
        tool.payloads.retain(|payload| payload.role != "output");
        tool.payloads.push(unavailable("output", count, upstream));
    }
    subagents::track(&mut tool, payload);
    tool
}

fn strip_file_content(value: &mut Value) -> (bool, Option<u64>) {
    match value {
        Value::Object(object) => {
            let mut removed = false;
            let mut bytes = Some(0);
            for key in [
                "content",
                "contents",
                "text",
                "oldText",
                "newText",
                "old_string",
                "new_string",
                "old_text",
                "new_text",
                "patch",
                "diff",
                "file_contents",
            ] {
                if let Some(value) = object.remove(key) {
                    removed = true;
                    bytes = bytes
                        .zip(value.as_str())
                        .map(|(count, text)| count + text.len() as u64);
                }
            }
            for child in object
                .values_mut()
                .filter(|value| value.is_object() || value.is_array())
            {
                let (child_removed, child_bytes) = strip_file_content(child);
                removed |= child_removed;
                bytes = bytes.zip(child_bytes).map(|(a, b)| a + b);
            }
            (removed, bytes)
        }
        Value::Array(values) => {
            let mut removed = false;
            let mut bytes = Some(0);
            // Ordinary arrays can contain paths, flags, and extension names.
            // Descend into structured entries to find named content fields;
            // their scalar siblings are ordinary arguments, not file bodies.
            for value in values
                .iter_mut()
                .filter(|value| value.is_object() || value.is_array())
            {
                let (child_removed, child_bytes) = strip_file_content(value);
                removed |= child_removed;
                bytes = bytes.zip(child_bytes).map(|(a, b)| a + b);
            }
            (removed, bytes)
        }
        Value::String(text) => {
            let count = text.len() as u64;
            *value = Value::Null;
            (true, Some(count))
        }
        _ => (false, Some(0)),
    }
}

impl ChatService {
    pub(in crate::server) fn record_session_signal(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
        mut incoming: ConversationSessionSignal,
    ) -> Option<ConversationSessionSignal> {
        let mut states = self
            .transcript_coordinator
            .conversations
            .lock()
            .expect("transcript state");
        let state = states.get_mut(&conversation.0)?;
        if state.closed_runs.contains(&run.0) {
            return None;
        }
        if state
            .session_states
            .get(&run.0)
            .is_some_and(|prior| prior.signal.session_id != incoming.session_id)
        {
            return None;
        }
        if let Some(failure) = &incoming.failure {
            // AIR revisions are session-scoped. Legacy errors have no revision
            // and must be compared by content within this Run, not assigned 0.
            let scope = if failure.revision.is_some() {
                incoming.session_id.clone()
            } else {
                format!("{}:{}", run.0, incoming.session_id)
            };
            let duplicate = if let Some((_, saved)) = state
                .failure_revisions
                .iter_mut()
                .find(|(session, saved)| session == &scope && saved.id == failure.id)
            {
                let duplicate = match (saved.revision, failure.revision) {
                    (Some(saved), Some(incoming)) => saved >= incoming,
                    _ => saved == failure,
                };
                if !duplicate {
                    *saved = failure.clone();
                }
                duplicate
            } else {
                if state.failure_revisions.len() == 64 {
                    state.failure_revisions.pop_front();
                }
                state.failure_revisions.push_back((scope, failure.clone()));
                false
            };
            if duplicate {
                incoming.failure = None;
                if incoming.activity.as_deref() == Some("retrying") {
                    incoming.activity = None;
                }
            }
        }
        let prior = state.session_states.get(&run.0);
        let mut signal = prior.map(|prior| prior.signal.clone()).unwrap_or_default();
        if !signal.session_id.is_empty() && signal.session_id != incoming.session_id {
            return None;
        }
        signal.session_id = incoming.session_id;
        if incoming.activity.is_some() {
            signal.activity = incoming.activity;
        }
        if let Some(failure) = incoming.failure {
            if failure.severity == "error" && signal.activity.as_deref() == Some("retrying") {
                signal.activity = None;
            }
            signal.failure = Some(failure);
        }
        if incoming.limit.is_some() {
            signal.limit = incoming.limit;
        }
        if prior.is_some_and(|prior| prior.signal == signal) {
            return None;
        }
        // One active Run per Conversation; discard completed live observations.
        let closed = state.closed_runs.clone();
        state.session_states.retain(|run, _| !closed.contains(run));
        let observation = ConversationSessionState {
            run_id: run.clone(),
            signal: signal.clone(),
            live: true,
        };
        state
            .session_states
            .insert(run.0.clone(), observation.clone());
        state.sequence += 1;
        if let Some(outbound) = &self.transcript_outbound {
            let delta = ConversationTranscriptDeltaParams {
                session_states: vec![observation],
                workspace_id: self.workspace_id.clone(),
                conversation_id: conversation.clone(),
                core_instance_id: state.instance.clone(),
                sequence: state.sequence,
                sequence_start: None,
                items: Vec::new(),
                body_appends: Vec::new(),
                capture: Vec::new(),
                recovery_required: false,
                background_tasks: Vec::new(),
                subagent_threads: Vec::new(),
            };
            let _ = outbound.notify(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
                    delta,
                ))
                .expect("session signal"),
            );
        }
        Some(signal)
    }

    pub(in crate::server) async fn capture_permission(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
        payload: &Value,
    ) -> Result<(), CoreError> {
        self.transcript_coordinator
            .permission(self, conversation, run, payload)
            .await
    }
    /// Completion may settle tools anywhere in a Run. Publish those revisions
    /// in bounded position pages, and the newest items for input/permission boundaries.
    pub(super) async fn publish_transcript_boundary(&self, conversation: &ConversationID) {
        let result: Result<(), CoreError> = async {
            let mut page = self
                .list_transcript(ConversationListTranscriptParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    limit: Some(2),
                    cursor: None,
                    subagent_thread_id: None,
                })
                .await?;
            self.transcript_coordinator
                .observe_admission(conversation, &mut page.items)?;
            let Some(outbound) = &self.transcript_outbound else {
                return Ok(());
            };
            let delta = ConversationTranscriptDeltaParams {
                workspace_id: self.workspace_id.clone(),
                session_states: page.session_states,
                conversation_id: conversation.clone(),
                core_instance_id: page.core_instance_id,
                sequence: page.sequence,
                sequence_start: None,
                items: page.items,
                body_appends: vec![],
                capture: page.capture,
                recovery_required: false,
                background_tasks: Vec::new(),
                subagent_threads: page.subagent_threads,
            };
            let _ = outbound.notify(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
                    delta,
                ))
                .expect("typed delta"),
            );
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(kind = %error.kind, "Could not publish committed transcript boundary");
        }
    }

    pub(in crate::server) async fn list_transcript(
        &self,
        params: ConversationListTranscriptParams,
    ) -> Result<ConversationListTranscriptResult, CoreError> {
        let _terminal_handoff = self.transcript_coordinator.terminal_handoff.read().await;
        let wire_conversation = params.conversation_id.clone();
        let limit = params
            .limit
            .map_or(PAGE_RECORDS, |limit| limit as usize)
            .clamp(1, PAGE_RECORDS);
        let thread = params.subagent_thread_id.clone();
        let workspace = self.workspace()?;
        let conversation = parse_conversation_id(&params.conversation_id)?;
        let expected_instance = self
            .transcript_instance
            .clone()
            .unwrap_or_else(core_instance);
        let before = params
            .cursor
            .as_deref()
            .map(|cursor| {
                let position = if let Some((instance, position)) = cursor.split_once(':') {
                    if instance != expected_instance {
                        return Err(CoreError::new(
                            "transcriptInstanceChanged",
                            "Reload the transcript after Core restarted",
                        ));
                    }
                    position
                } else {
                    cursor
                };
                position
                    .parse::<i64>()
                    .map_err(|_| CoreError::new("invalidParams", "Invalid transcript cursor"))
            })
            .transpose()?;
        let home = self.home.clone();
        // Freeze the live view and its watermark BEFORE reading SQLite. A final
        // checkpoint may retire its memory capture entry during the read; never
        // stamp that older database capture state with a later live sequence.
        let (live_items, live_capture, live_sequence, live_sessions, live_background, live_threads) = {
            let states = self
                .transcript_coordinator
                .conversations
                .lock()
                .expect("transcript state");
            let background = self
                .transcript_coordinator
                .background_snapshot(&wire_conversation.0);
            if let Some(state) = states.get(&wire_conversation.0) {
                let items = state
                    .items
                    .values()
                    .rev()
                    .filter(|item| before.is_none_or(|before| item.record.row.position < before))
                    // A thread's page filters after the merge, by its family.
                    .take(if thread.is_some() { usize::MAX } else { limit })
                    .map(|item| item.to_wire())
                    .collect::<Result<Vec<_>, _>>()?;
                (
                    items,
                    state.capture_status(),
                    Some(state.sequence),
                    state.session_states.values().cloned().collect::<Vec<_>>(),
                    background,
                    state.subagent_counts(),
                )
            } else {
                (
                    Vec::new(),
                    Vec::new(),
                    None,
                    Vec::new(),
                    background,
                    Vec::new(),
                )
            }
        };
        let sequence_floor = self.transcript_coordinator.minimum_snapshot_sequence();
        #[cfg(test)]
        {
            let gate = self
                .transcript_coordinator
                .snapshot_read_gate
                .lock()
                .expect("snapshot gate")
                .take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
        }
        let instance = self
            .transcript_instance
            .clone()
            .unwrap_or_else(core_instance);
        let live_anchors: Vec<_> = live_items
            .iter()
            .filter_map(subagents::wire_anchor)
            .collect();
        let (mut page, family) = self
            .database
            .run("conversation.listTranscript", move |connection| {
                read_snapshot(connection, |connection| {
                    active_conversation(connection, &workspace, &conversation)?;
                    let family = match &thread {
                        Some(thread) => {
                            let mut anchors =
                                Repo::thread_anchors(connection, &conversation.to_string())?;
                            anchors.extend(live_anchors);
                            Some(subagents::ThreadFamily::new(thread, &anchors))
                        }
                        None => None,
                    };
                    let (records, next_cursor) = Repo::list(
                        connection,
                        &conversation.to_string(),
                        before,
                        limit,
                        family
                            .as_ref()
                            .map(|family| (family.threads.as_slice(), family.anchors.as_slice())),
                    )?;
                    let mut items = records
                        .into_iter()
                        .map(to_wire)
                        .collect::<Result<Vec<_>, _>>()?;
                    for item in &mut items {
                        if item.kind == TranscriptItemKind::Message {
                            let id = crate::storage::models::ConversationMessageId::try_from(
                                item.item_id.clone(),
                            )?;
                            let message =
                                crate::storage::repositories::ConversationRepository::get_message(
                                    connection,
                                    &conversation,
                                    &id,
                                )?;
                            item.message = super::attachments::messages_to_wire(
                                connection,
                                &home,
                                &workspace,
                                &conversation,
                                vec![message],
                            )?
                            .pop();
                            if let Some(message) = &mut item.message {
                                message.body = item.body_preview.clone();
                            }
                        }
                    }
                    let visible_runs: Vec<_> = items
                        .iter()
                        .filter_map(|item| item.run_id.as_ref().map(|id| id.0.clone()))
                        .collect();
                    let capture = capture_status(
                        connection,
                        &conversation.to_string(),
                        &visible_runs,
                        before.is_none(),
                    )?;
                    Ok((
                        ConversationListTranscriptResult {
                            session_states: saved_session_states(
                                connection,
                                &conversation.to_string(),
                            )?,
                            core_instance_id: instance,
                            sequence: Repo::sequence(connection, &conversation.to_string())? as u64,
                            items,
                            next_cursor,
                            capture,
                            coverage: None,
                            background_tasks: Vec::new(),
                            subagent_threads: Vec::new(),
                        },
                        family,
                    ))
                })
            })
            .await
            .map_err(CoreError::from)?;
        // Before the merge, so only items read from storage are judged.
        self.transcript_coordinator.read_lost(&mut page.items);
        page.background_tasks = live_background;
        page.subagent_threads = live_threads;
        if let Some(sequence) = live_sequence {
            for observation in live_sessions {
                page.session_states
                    .retain(|saved| saved.run_id != observation.run_id);
                page.session_states.push(observation);
            }
            for wire in live_items {
                if family
                    .as_ref()
                    .is_some_and(|family| !family.contains(&wire))
                {
                    continue;
                }
                if let Some(existing) = page
                    .items
                    .iter_mut()
                    .find(|saved| saved.item_id == wire.item_id)
                {
                    if wire.revision > existing.revision {
                        *existing = wire;
                    }
                } else {
                    page.items.push(wire);
                }
            }
            page.sequence = sequence;
            for capture in live_capture {
                page.capture.retain(|saved| saved.run_id != capture.run_id);
                page.capture.push(capture);
            }
        } else {
            page.sequence = page.sequence.max(sequence_floor);
        }
        page.items
            .sort_by_key(|item| std::cmp::Reverse(item.position));
        // Bound capture range detail across a page too. A run-level partial
        // notice remains explicit even when its detailed positions are omitted.
        let mut remaining_ranges = 128;
        for capture in &mut page.capture {
            if capture.gaps.len() > remaining_ranges && remaining_ranges > 0 {
                let end = capture.gaps.last().map(|gap| gap.end_position).unwrap_or(0);
                capture.gaps.truncate(remaining_ranges);
                if let Some(last) = capture.gaps.last_mut() {
                    last.end_position = end;
                }
            } else {
                capture.gaps.truncate(remaining_ranges);
            }
            remaining_ranges -= capture.gaps.len();
        }
        let mut bytes = serde_json::to_vec(&page.capture).map_or(0, |value| value.len()) + 1024;
        let take = page
            .items
            .iter()
            .take(limit)
            .take_while(|item| {
                bytes += serde_json::to_vec(item).map_or(0, |value| value.len());
                bytes <= TRANSCRIPT_PAGE_BYTES
            })
            .count();
        if page.items.len() > take {
            page.items.truncate(take);
            page.next_cursor = page.items.last().map(|item| item.position.to_string());
        }
        page.coverage = page
            .items
            .first()
            .zip(page.items.last())
            .map(|(first, last)| TranscriptCoverage {
                start_position: last.position,
                end_position: first.position,
            });
        page.next_cursor = page
            .next_cursor
            .map(|position| format!("{}:{position}", page.core_instance_id));
        Ok(page)
    }

    pub(in crate::server) async fn read_transcript_body(
        &self,
        params: ConversationReadTranscriptBodyParams,
    ) -> Result<ConversationReadTranscriptBodyResult, CoreError> {
        let mut committed_prefix = None;
        {
            let states = self
                .transcript_coordinator
                .conversations
                .lock()
                .expect("transcript state");
            if let Some(state) = states
                .get(&params.conversation_id.0)
                .filter(|state| state.workspace.as_ref() == Some(&self.workspace_id))
            {
                if let Some(item) = state
                    .items
                    .values()
                    .find(|item| item.record.row.id == params.item_id)
                {
                    if !item.body_unchanged_since(params.revision as i64) {
                        return Err(CoreError::new(
                            "transcriptRevisionChanged",
                            "Reload the changed transcript item",
                        ));
                    }
                    if params.offset < item.body_start as u64 {
                        committed_prefix = Some((
                            item.body_start - params.offset as usize,
                            item.record.row.body_byte_count as u64,
                        ));
                    } else {
                        let (text, next_offset) = item.body(params.offset as usize, 65536)?;
                        return Ok(ConversationReadTranscriptBodyResult {
                            item_id: params.item_id,
                            revision: params.revision,
                            text,
                            next_offset,
                        });
                    }
                }
            }
        }
        let workspace = self.workspace()?;
        let conversation = parse_conversation_id(&params.conversation_id)?;
        let offset = i64::try_from(params.offset)
            .map_err(|_| CoreError::new("invalidParams", "Invalid body offset"))?;
        let revision = i64::try_from(params.revision)
            .map_err(|_| CoreError::new("invalidParams", "Invalid revision"))?;
        let requested_conversation = params.conversation_id.0.clone();
        let requested_item = params.item_id.clone();
        let result = self
            .database
            .run("conversation.readTranscriptBody", move |connection| {
                read_snapshot(connection, |connection| {
                    active_conversation(connection, &workspace, &conversation)?;
                    let saved_revision = if committed_prefix.is_some() {
                        Repo::get(connection, &conversation.to_string(), &params.item_id)?
                            .row
                            .revision
                    } else {
                        revision
                    };
                    let (text, mut next) = Repo::read_body(
                        connection,
                        &conversation.to_string(),
                        &params.item_id,
                        saved_revision,
                        offset,
                        committed_prefix.map_or(65536, |(limit, _)| limit.min(65536)),
                    )?;
                    if let Some((_, total)) = committed_prefix {
                        let end = params.offset + text.len() as u64;
                        next = (end < total).then_some(end as i64);
                    }
                    Ok(ConversationReadTranscriptBodyResult {
                        item_id: params.item_id.clone(),
                        revision: params.revision,
                        text,
                        next_offset: next.map(|offset| offset as u64),
                    })
                })
            })
            .await
            .map_err(CoreError::from)?;
        let states = self
            .transcript_coordinator
            .conversations
            .lock()
            .expect("transcript state");
        if states
            .get(&requested_conversation)
            .and_then(|state| {
                state
                    .items
                    .values()
                    .find(|item| item.record.row.id == requested_item)
            })
            .is_some_and(|item| !item.body_unchanged_since(revision))
        {
            return Err(CoreError::new(
                "transcriptRevisionChanged",
                "Reload the changed transcript item",
            ));
        }
        Ok(result)
    }

    pub(in crate::server) async fn record_transcript_update(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        update: AgentUpdate,
    ) -> Result<ConversationTranscriptDeltaParams, CoreError> {
        self.transcript_coordinator
            .update(self, conversation_id, run_id, update)
            .await
    }

    /// Records a background-task lifecycle update on the run that owns the
    /// task, open or closed, and publishes the delta. A closed run revises only
    /// `tool.backgroundTask`.
    pub(in crate::server) async fn record_task_lifecycle(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        update: AgentUpdate,
    ) -> Result<(), CoreError> {
        let delta = self
            .transcript_coordinator
            .update(self, conversation_id, run_id, update)
            .await?;
        self.publish_task_delta(delta);
        Ok(())
    }

    /// Marks a live task lost because its adapter or session died.
    pub(in crate::server) async fn record_task_lost(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        provider_tool_call_id: Option<String>,
        async_task_id: Option<String>,
    ) -> Result<(), CoreError> {
        let delta = self
            .transcript_coordinator
            .revise(
                self,
                conversation_id,
                run_id,
                live::TaskChange::Lost {
                    provider_tool_call_id,
                    async_task_id,
                },
            )
            .await?;
        self.publish_task_delta(delta);
        Ok(())
    }

    /// Records what became of a question on the tool call that asked it,
    /// in an open or a closed run.
    pub(in crate::server) async fn record_question(
        &self,
        conversation_id: &ConversationID,
        run_id: &ConversationRunID,
        provider_tool_call_id: String,
        question: ottam_protocol::TranscriptQuestion,
    ) -> Result<(), CoreError> {
        let delta = self
            .transcript_coordinator
            .revise(
                self,
                conversation_id,
                run_id,
                live::TaskChange::Question {
                    provider_tool_call_id,
                    question,
                },
            )
            .await?;
        self.publish_task_delta(delta);
        Ok(())
    }

    fn publish_task_delta(&self, delta: ConversationTranscriptDeltaParams) {
        if delta.items.is_empty()
            && delta.session_states.is_empty()
            && delta.body_appends.is_empty()
            && delta.capture.is_empty()
            && delta.background_tasks.is_empty()
            && !delta.recovery_required
        {
            return;
        }
        if let Some(outbound) = &self.transcript_outbound {
            let _ = outbound.notify(
                serde_json::to_value(ProtocolNotification::new(
                    NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA,
                    delta,
                ))
                .expect("typed transcript delta"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::recorded_transaction;
    use crate::storage::{models::*, repositories::*, Database};
    use diesel::prelude::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Page records come from set queries; each preview must still equal the
    /// item's own first body read, checkpointed or committed.
    #[tokio::test]
    async fn transcript_page_previews_match_each_items_first_body_read() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let previews_match = |chat: &ChatService, expect_heads: bool| {
            let conversation = conversation.0.clone();
            let database = chat.database.clone();
            async move {
                database
                    .run("test.previewParity", move |connection| {
                        use crate::storage::repositories::transcript::PREVIEW_BYTES;
                        use crate::storage::schema::transcript_checkpoint_heads as heads;
                        let heads = heads::table.count().get_result::<i64>(connection)?;
                        assert_eq!(heads > 0, expect_heads, "checkpoint heads: {heads}");
                        let (records, _) = Repo::list(connection, &conversation, None, 1000, None)?;
                        let kinds: Vec<_> = records
                            .iter()
                            .map(|record| record.row.kind.as_str())
                            .collect();
                        assert!(records.len() >= 6, "{kinds:?}");
                        for record in &records {
                            let (read, _) = Repo::read_body(
                                connection,
                                &conversation,
                                &record.row.id,
                                record.row.revision,
                                0,
                                PREVIEW_BYTES,
                            )?;
                            assert_eq!(record.preview, read, "{} preview", record.row.kind);
                            let single = Repo::get(connection, &conversation, &record.row.id)?;
                            assert_eq!(single.preview, record.preview);
                        }
                        assert!(records
                            .iter()
                            .any(|record| record.preview.len() == PREVIEW_BYTES));
                        Ok(())
                    })
                    .await
                    .unwrap()
            }
        };
        // The message preview's byte limit lands inside a two-byte character.
        let message = format!("a{}", "é".repeat(3000));
        chat.database
            .run("test.longMessage", move |connection| {
                use crate::storage::schema::conversation_messages as messages;
                diesel::update(messages::table)
                    .set(messages::body.eq(message))
                    .execute(connection)?;
                Ok(())
            })
            .await
            .unwrap();
        let long = "é".repeat(3000);
        for chunk in long.as_bytes().chunks(700) {
            // Splits land inside two-byte characters; the store keeps whole ones.
            chat.record_transcript_update(
                &conversation,
                &run,
                text(&String::from_utf8_lossy(chunk), Some("long")),
            )
            .await
            .unwrap();
        }
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::AgentThoughtChunk {
                text: "Shared reasoning".into(),
                message_id: None,
            },
        )
        .await
        .unwrap();
        chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCall {
            payload: json!({"toolCallId":"big","title":"Write","kind":"edit","rawInput":{"path":"a.rs","content":"x".repeat(9000)}}),
        })
        .await
        .unwrap();
        chat.record_transcript_update(&conversation, &run, AgentUpdate::Plan {
            payload: json!({"entries":[{"content":"Check previews","priority":"high","status":"in_progress"}]}),
        })
        .await
        .unwrap();
        chat.record_transcript_update(&conversation, &run, text("still streaming", Some("open")))
            .await
            .unwrap();
        chat.transcript_coordinator.drain().await;
        previews_match(&chat, true).await;

        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            json!(null),
        )
        .await
        .unwrap();
        chat.transcript_coordinator.drain().await;
        previews_match(&chat, true).await;

        // Rows saved before checkpoint staging keep their bodies in plain chunks.
        chat.database
            .run("test.unstageBodies", |connection| {
                use diesel::connection::SimpleConnection;
                connection.batch_execute("INSERT INTO transcript_body_chunks (item_id, byte_offset, body) SELECT b.item_id, b.byte_offset, b.body FROM transcript_checkpoint_bodies b JOIN transcript_checkpoint_heads h ON h.item_id = b.item_id AND h.revision = b.revision; DELETE FROM transcript_checkpoint_heads; DELETE FROM transcript_checkpoint_bodies;")?;
                Ok(())
            })
            .await
            .unwrap();
        previews_match(&chat, false).await;
    }

    #[tokio::test]
    async fn transcript_default_pages_load_a_thousand_records_live_and_after_reload() {
        let (_home, chat, conversation, run) = fixture().await;
        for index in 0..2150 {
            chat.record_transcript_update(
                &conversation,
                &run,
                text(
                    &format!("Saved response {index}"),
                    Some(&format!("message-{index}")),
                ),
            )
            .await
            .unwrap();
        }
        chat.transcript_coordinator.drain().await;
        let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
        for service in [&chat, &restored] {
            let request = |cursor, limit| ConversationListTranscriptParams {
                workspace_id: service.workspace_id.clone(),
                conversation_id: conversation.clone(),
                limit,
                cursor,
                subagent_thread_id: None,
            };
            let first = service.list_transcript(request(None, None)).await.unwrap();
            assert_eq!(first.items.len(), 1000);
            let second = service
                .list_transcript(request(first.next_cursor.clone(), None))
                .await
                .unwrap();
            assert_eq!(second.items.len(), 1000);
            assert!(second.items.last().unwrap().position < first.items.first().unwrap().position);
            let third = service
                .list_transcript(request(second.next_cursor.clone(), None))
                .await
                .unwrap();
            assert!(
                third.next_cursor.is_none(),
                "pages {} / {} / {} still continue at {:?}",
                first.items.len(),
                second.items.len(),
                third.items.len(),
                third.next_cursor
            );
            let saved: std::collections::HashSet<_> = first
                .items
                .iter()
                .chain(&second.items)
                .chain(&third.items)
                .filter(|item| item.kind == TranscriptItemKind::AssistantText)
                .map(|item| item.body_preview.clone())
                .collect();
            assert_eq!(
                saved.len(),
                2150,
                "Paging must not skip or duplicate saved responses"
            );
            let smaller = service
                .list_transcript(request(None, Some(50)))
                .await
                .unwrap();
            assert_eq!(
                smaller.items.len(),
                50,
                "Explicit smaller pages remain supported"
            );
        }
    }

    #[tokio::test]
    async fn transcript_partial_capture_warning_distinguishes_details_from_missing_ranges() {
        for missing_range in [false, true] {
            let (_home, chat, conversation, run) = fixture().await;
            chat.record_transcript_update(
                &conversation,
                &run,
                text("Retained assistant response", None),
            )
            .await
            .unwrap();
            chat.finalize_run(
                &conversation,
                &run,
                ConversationRunStatus::Completed,
                None,
                json!({}),
            )
            .await
            .unwrap();
            chat.transcript_coordinator.drain().await;
            let saved_run = run.0.clone();
            chat.database
                .run("test.partialCaptureWarning", move |connection| {
                    use crate::storage::schema::transcript_capture as capture;
                    diesel::update(capture::table.find(saved_run))
                        .set((
                            capture::state.eq("partial"),
                            capture::gaps_json.eq(if missing_range {
                                r#"[{"startPosition":40,"endPosition":41}]"#
                            } else {
                                "[]"
                            }),
                        ))
                        .execute(connection)?;
                    Ok(())
                })
                .await
                .unwrap();
            let restored = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
            let result = page(&restored, &conversation).await;
            let capture = result
                .capture
                .iter()
                .find(|capture| capture.run_id == run)
                .unwrap();
            assert_eq!(
                capture.problem.as_deref(),
                Some(if missing_range {
                    "Some transcript output was not saved"
                } else {
                    "Some tool details could not be saved completely"
                })
            );
            assert!(result
                .items
                .iter()
                .any(|item| item.body_preview == "Retained assistant response"));
        }
    }

    #[tokio::test]
    async fn transcript_snapshot_waits_for_terminal_memory_handoff_not_checkpoint() {
        use std::future::Future;
        for with_reply in [false, true] {
            let (_home, chat, conversation, run) = fixture().await;
            chat.transcript_coordinator.manual_checkpoints();
            if with_reply {
                chat.record_transcript_update(&conversation, &run, text("reply", None))
                    .await
                    .unwrap();
            }
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            *chat
                .transcript_coordinator
                .terminal_commit_gate
                .lock()
                .unwrap() = Some((entered_tx, release_rx));
            let finishing_chat = chat.clone();
            let finishing_conversation = conversation.clone();
            let finishing_run = run.clone();
            let finish = tokio::spawn(async move {
                finishing_chat
                    .finalize_run(
                        &finishing_conversation,
                        &finishing_run,
                        ConversationRunStatus::Completed,
                        None,
                        json!({}),
                    )
                    .await
                    .unwrap()
            });
            entered_rx.await.unwrap();
            assert_eq!(
                chat.conversation_run(&conversation, &run)
                    .await
                    .unwrap()
                    .status,
                ConversationRunStatus::Completed
            );
            let mut snapshot = Box::pin(page(&chat, &conversation));
            std::future::poll_fn(|cx| {
                assert!(
                    snapshot.as_mut().poll(cx).is_pending(),
                    "a terminal-status reader must not receive the pre-finalization transcript"
                );
                std::task::Poll::Ready(())
            })
            .await;
            release_tx.send(()).unwrap();
            finish.await.unwrap();
            let snapshot = snapshot.await;
            let boundary = snapshot
                .items
                .iter()
                .find(|item| item.kind == TranscriptItemKind::RunBoundary)
                .unwrap();
            assert_eq!(boundary.run_status, Some(ConversationRunStatus::Completed));
            assert_eq!(
                boundary.committed_revision,
                Some(0),
                "the snapshot must not wait for checkpoint persistence"
            );
            assert!(snapshot
                .items
                .iter()
                .filter(|item| item.run_id.as_ref() == Some(&run))
                .all(|item| item.completion == TranscriptCompletion::Complete));
        }
    }

    #[tokio::test]
    async fn transcript_restart_reserves_after_persisted_capture_loss_ranges() {
        let (home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.database
            .run("test.persistCaptureLoss", move |connection| {
                use crate::storage::schema::transcript_capture as capture;
                diesel::update(capture::table.find(run.0))
                    .set((
                        capture::state.eq("partial"),
                        capture::gaps_json.eq(r#"[{"startPosition":1,"endPosition":100}]"#),
                    ))
                    .execute(connection)?;
                Ok(())
            })
            .await
            .unwrap();
        drop(chat);
        let restarted = reopen(home.path()).await;
        assert_eq!(
            restarted
                .reserve_transcript_positions(&conversation.0, 1)
                .await
                .unwrap(),
            101
        );
    }

    #[tokio::test]
    async fn transcript_thousand_live_revisions_coalesce_into_one_checkpoint() {
        use diesel::connection::SimpleConnection;
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.database.run("test.countCheckpoints", |connection| {
            connection.batch_execute("CREATE TABLE checkpoint_probe (item_id TEXT); CREATE TRIGGER checkpoint_probe_insert AFTER INSERT ON transcript_items WHEN NEW.kind = 'assistantText' BEGIN INSERT INTO checkpoint_probe VALUES (NEW.id); END; CREATE TRIGGER checkpoint_probe_update AFTER UPDATE ON transcript_items WHEN NEW.kind = 'assistantText' BEGIN INSERT INTO checkpoint_probe VALUES (NEW.id); END;")?;
            Ok(())
        }).await.unwrap();
        for _ in 0..1000 {
            chat.record_transcript_update(&conversation, &run, text("x", None))
                .await
                .unwrap();
        }
        let before = page(&chat, &conversation).await;
        assert_eq!(before.items[0].revision, 1000);
        assert_eq!(before.items[0].committed_revision, Some(0));
        chat.transcript_coordinator.checkpoint().await;
        chat.transcript_coordinator.checkpoint().await;
        let after = page(&chat, &conversation).await;
        assert_eq!(after.items[0].committed_revision, Some(1000));
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            value: i64,
        }
        let writes = chat
            .database
            .run("test.checkpointCount", |connection| {
                Ok(
                    diesel::sql_query("SELECT count(*) AS value FROM checkpoint_probe")
                        .get_result::<Count>(connection)?
                        .value,
                )
            })
            .await
            .unwrap();
        assert_eq!(writes, 1);
    }

    #[tokio::test]
    async fn transcript_restart_masks_pre_agent_output_before_suppression_checkpoint() {
        let (home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("not authenticated", None))
            .await
            .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Failed,
            None,
            json!({"kind":"authRequired"}),
        )
        .await
        .unwrap();
        let workspace = chat.workspace_id.clone();
        drop(chat);
        let restarted = ChatService::new(reopen(home.path()).await, workspace);
        let snapshot = page(&restarted, &conversation).await;
        let text = snapshot
            .items
            .iter()
            .find(|item| item.kind == TranscriptItemKind::AssistantText)
            .unwrap();
        assert_eq!(text.completion, TranscriptCompletion::Suppressed);
        assert!(text.body_preview.is_empty());
        assert!(text.committed_revision.unwrap() < text.revision);
        let body = restarted
            .read_transcript_body(ConversationReadTranscriptBodyParams {
                workspace_id: restarted.workspace_id.clone(),
                conversation_id: conversation,
                item_id: text.item_id.clone(),
                revision: text.revision,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(body.text.is_empty());
    }

    #[tokio::test]
    async fn transcript_checkpoint_failure_keeps_streaming_and_execution_successful() {
        use diesel::connection::SimpleConnection;
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.database.run("test.failWrites", |connection| {
            connection.batch_execute("CREATE TRIGGER fail_capture BEFORE INSERT ON transcript_checkpoint_bodies BEGIN SELECT RAISE(FAIL, 'disk full'); END;")?;
            Ok(())
        }).await.unwrap();
        chat.record_transcript_update(&conversation, &run, text("A", None))
            .await
            .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        assert!(page(&chat, &conversation)
            .await
            .capture
            .iter()
            .any(|capture| capture.problem.is_some()));
        chat.record_transcript_update(&conversation, &run, text("🧠B", None))
            .await
            .unwrap();
        let finished = chat
            .finalize_run(
                &conversation,
                &run,
                ConversationRunStatus::Completed,
                None,
                json!({}),
            )
            .await
            .unwrap();
        assert_eq!(finished.status, ConversationRunStatus::Completed);
        let late = chat
            .record_transcript_update(&conversation, &run, text("late", None))
            .await
            .unwrap();
        assert!(late.items.is_empty());
        chat.database
            .run("test.recoverWrites", |connection| {
                connection.batch_execute("DROP TRIGGER fail_capture;")?;
                Ok(())
            })
            .await
            .unwrap();
        chat.transcript_coordinator.drain().await;
        let snapshot = page(&chat, &conversation).await;
        assert!(snapshot
            .capture
            .iter()
            .any(
                |capture| capture.state.as_deref() == Some("complete") && capture.problem.is_none()
            ));
        let id = snapshot
            .items
            .iter()
            .find(|item| item.kind == TranscriptItemKind::AssistantText)
            .unwrap()
            .item_id
            .clone();
        let conv = conversation.0.clone();
        let saved = chat
            .database
            .run("test.savedCapture", move |connection| {
                Repo::get(connection, &conv, &id)
            })
            .await
            .unwrap();
        assert_eq!(saved.preview, "A🧠B");
        assert_eq!(saved.row.completion, "complete");
    }

    #[tokio::test]
    async fn transcript_live_delivery_does_not_wait_for_a_locked_database() {
        use diesel::connection::SimpleConnection;
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.transcript_coordinator
            .prepare(&chat, &conversation)
            .await
            .unwrap();
        chat.transcript_coordinator.accept_run(&conversation, &run);
        let database = chat.database.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let blocker = tokio::spawn(async move {
            database
                .run("test.lockDatabase", move |connection| {
                    connection.batch_execute("BEGIN IMMEDIATE;")?;
                    let _ = entered_tx.send(());
                    let _ = release_rx.blocking_recv();
                    connection.batch_execute("ROLLBACK;")?;
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            chat.record_transcript_update(&conversation, &run, text("still streaming", None)),
        )
        .await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        assert_eq!(
            result.unwrap().unwrap().items[0].body_preview,
            "still streaming"
        );
    }

    #[tokio::test]
    async fn transcript_retention_overflow_is_explicit_and_starts_a_new_segment() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let huge = "x".repeat(17 * 1024 * 1024);
        let first = chat
            .record_transcript_update(&conversation, &run, text(&huge, None))
            .await
            .unwrap();
        let second = chat
            .record_transcript_update(&conversation, &run, text("retained", None))
            .await
            .unwrap();
        assert_ne!(first.items[0].item_id, second.items[0].item_id);
        let snapshot = page(&chat, &conversation).await;
        assert!(
            snapshot
                .capture
                .iter()
                .any(|capture| capture.state.as_deref() == Some("partial")
                    && !capture.gaps.is_empty())
        );
        assert_eq!(snapshot.items[0].body_preview, "retained");
    }

    #[tokio::test]
    async fn transcript_restart_after_execution_completion_reports_uncertain_capture() {
        let (home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("saved prefix", None))
            .await
            .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        chat.record_transcript_update(&conversation, &run, text(" unsaved suffix", None))
            .await
            .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            json!({}),
        )
        .await
        .unwrap();
        let workspace = chat.workspace_id.clone();
        drop(chat);
        let restarted = ChatService::new(reopen(home.path()).await, workspace);
        let snapshot = page(&restarted, &conversation).await;
        assert!(snapshot.capture.iter().any(|capture| capture.run_id == run
            && capture.state.as_deref() == Some("open")
            && capture.problem.as_deref() == Some("Capture may be incomplete")));
        let text = snapshot
            .items
            .iter()
            .find(|item| item.kind == TranscriptItemKind::AssistantText)
            .unwrap();
        assert_eq!(text.body_preview, "saved prefix");
        assert_eq!(text.run_status, Some(ConversationRunStatus::Completed));
    }

    #[tokio::test]
    async fn transcript_committed_prefixes_leave_memory_without_losing_utf8_offsets() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let prefix = "é🧠".repeat(12000);
        let first = chat
            .record_transcript_update(&conversation, &run, text(&prefix, None))
            .await
            .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        let second = chat
            .record_transcript_update(&conversation, &run, text(" suffix", None))
            .await
            .unwrap();
        assert_eq!(first.items[0].item_id, second.items[0].item_id);
        assert_eq!(second.body_appends[0].offset, prefix.len() as u64);
        // A new item carries its preview; growth carries only the append.
        assert!(!first.items[0].preview_omitted);
        assert!(!first.items[0].body_preview.is_empty());
        assert!(second.items[0].preview_omitted);
        assert!(second.items[0].body_preview.is_empty());
        let mut offset = 0;
        let mut body = String::new();
        loop {
            let page = chat
                .read_transcript_body(ConversationReadTranscriptBodyParams {
                    workspace_id: chat.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    item_id: second.items[0].item_id.clone(),
                    revision: second.items[0].revision,
                    offset,
                })
                .await
                .unwrap();
            body.push_str(&page.text);
            match page.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(body, prefix + " suffix");
        chat.transcript_coordinator.drain().await;
        let id = second.items[0].item_id.clone();
        let conv = conversation.0.clone();
        let revision = second.items[0].revision as i64;
        let suffix = chat
            .database
            .run("test.savedSuffix", move |connection| {
                Repo::read_body(connection, &conv, &id, revision, 72000, 65536)
            })
            .await
            .unwrap();
        assert_eq!(suffix.0, " suffix");
    }

    #[tokio::test]
    async fn live_appends_are_revision_tagged_bounded_utf8_and_committed() {
        let (_home, chat, conversation, run) = fixture().await;
        let body = "é🧠".repeat(24000);
        let delta = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::AgentMessageChunk {
                    text: body.clone(),
                    message_id: None,
                },
            )
            .await
            .unwrap();
        let item = delta.items.last().unwrap();
        let mut rebuilt = String::new();
        for append in &delta.body_appends {
            assert_eq!(append.item_id, item.item_id);
            assert_eq!(append.revision, item.revision);
            assert_eq!(append.offset, rebuilt.len() as u64);
            assert!(append.text.len() <= 65536);
            rebuilt.push_str(&append.text);
        }
        assert_eq!(rebuilt, body);
        let page = chat
            .read_transcript_body(ConversationReadTranscriptBodyParams {
                workspace_id: chat.workspace_id.clone(),
                conversation_id: conversation,
                item_id: item.item_id.clone(),
                revision: item.revision,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(body.starts_with(&page.text));
        assert!(page.text.len() > 4096);
    }

    #[tokio::test]
    async fn tool_progress_keeps_earlier_argument_reads_valid_until_new_arguments() {
        let (_home, chat, conversation, run) = fixture().await;
        let tool = |payload: Value| AgentUpdate::ToolCallUpdate { payload };
        let started = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::ToolCall {
                    payload: json!({"toolCallId":"read","title":"Read","kind":"read","status":"in_progress","rawInput":{"path":"a.rs"}}),
                },
            )
            .await
            .unwrap();
        let item = started.items[0].clone();
        let read = |revision: u64| {
            chat.read_transcript_body(ConversationReadTranscriptBodyParams {
                workspace_id: chat.workspace_id.clone(),
                conversation_id: conversation.clone(),
                item_id: item.item_id.clone(),
                revision,
                offset: 0,
            })
        };
        let progressed = chat
            .record_transcript_update(
                &conversation,
                &run,
                tool(json!({"toolCallId":"read","rawOutput":"line 1"})),
            )
            .await
            .unwrap();
        assert!(progressed.items[0].revision > item.revision);
        let page = read(item.revision)
            .await
            .expect("progress leaves the arguments unchanged");
        assert!(page.text.contains("a.rs"));

        let replaced = chat
            .record_transcript_update(
                &conversation,
                &run,
                tool(json!({"toolCallId":"read","rawInput":{"path":"b.rs"}})),
            )
            .await
            .unwrap();
        let stale = read(progressed.items[0].revision).await.unwrap_err();
        assert_eq!(stale.kind, "transcriptRevisionChanged");
        let page = read(replaced.items[0].revision).await.unwrap();
        assert!(page.text.contains("b.rs"));
    }

    pub(super) async fn fixture() -> (TempDir, ChatService, ConversationID, ConversationRunID) {
        let home = TempDir::new().unwrap();
        let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
        let (workspace, conversation, run) = database
            .run("test.transcriptSeed", |connection| {
                let now = Utc::now();
                let workspace = WorkspaceRepository::create(connection, "Transcript", now)?;
                let created = ConversationRepository::create(
                    connection,
                    &CreateConversationParams {
                        workspace_id: &workspace.id,
                        title: "Transcript",
                        root: ConversationRootSelection::WorkspaceFiles,
                        context_projects: &[],
                        initial_message: Some(InitialMessage {
                            body: "hello",
                            idempotency_key: "initial",
                        }),
                    },
                    now,
                )?;
                let run = RunRepository::create_run_with_snapshot(
                    connection,
                    &CreateRunParams {
                        workspace_id: &workspace.id,
                        conversation_id: &created.conversation.id,
                        engine_id: "engine",
                        model_id: None,
                        request_options_json: None,
                        pending_input_id: &created.initial_input.unwrap().input.id,
                        sources: &[RunSourceSnapshot {
                            root_kind: crate::storage::models::WorkingRootKind::WorkspaceFiles,
                            project_id: None,
                            source_id: None,
                            canonical_path: "/tmp/transcript-test",
                            role: RunSourceRole::Primary,
                        }],
                    },
                    now,
                )?;
                Ok((
                    WorkspaceID(workspace.id.to_string()),
                    ConversationID(created.conversation.id.to_string()),
                    ConversationRunID(run.id.to_string()),
                ))
            })
            .await
            .unwrap();
        (
            home,
            ChatService::new(database, workspace),
            conversation,
            run,
        )
    }

    pub(super) fn text(value: &str, id: Option<&str>) -> AgentUpdate {
        AgentUpdate::AgentMessageChunk {
            text: value.into(),
            message_id: id.map(str::to_owned),
        }
    }

    /// Opens `home` again after a simulated restart. A checkpoint the dropped
    /// coordinator's loop had in flight keeps the old database, and the home
    /// lock, until it ends.
    async fn reopen(home: &std::path::Path) -> Database {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match Database::open(home.to_path_buf()) {
                Ok(database) => return database,
                Err(error)
                    if error.kind == "coreAlreadyRunning"
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                Err(error) => panic!("reopens the database: {error:?}"),
            }
        }
    }

    async fn page(
        chat: &ChatService,
        conversation: &ConversationID,
    ) -> ConversationListTranscriptResult {
        chat.list_transcript(ConversationListTranscriptParams {
            workspace_id: chat.workspace_id.clone(),
            conversation_id: conversation.clone(),
            limit: Some(200),
            cursor: None,
            subagent_thread_id: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn session_signals_deduplicate_checkpoint_and_never_reload_activity() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, text("before", None))
            .await
            .unwrap();
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../agent/fixtures/session-signals.json"))
                .unwrap();
        let mut retry = crate::agent::metadata::session_signal(&fixtures[3]).unwrap();
        retry.session_id = "claude:session".into();
        let saved = chat
            .record_session_signal(&conversation, &run, retry.clone())
            .unwrap();
        assert!(chat
            .record_session_signal(&conversation, &run, retry.clone())
            .is_none());
        let before = page(&chat, &conversation).await;
        assert_eq!(
            before.session_states[0].signal.activity.as_deref(),
            Some("retrying")
        );
        chat.record_run_events(
            &conversation,
            &run,
            None,
            vec![(
                ConversationEventKind::SessionMetadata,
                serde_json::json!({"signal": saved}),
            )],
        )
        .await
        .unwrap();
        let cold = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
        let reloaded = page(&cold, &conversation).await;
        assert!(!reloaded.session_states[0].live);
        assert!(reloaded.session_states[0].signal.activity.is_none());
        assert!(reloaded.session_states[0].signal.failure.is_some());
        let progress = chat
            .record_transcript_update(&conversation, &run, text(" recovered", None))
            .await
            .unwrap();
        assert_eq!(
            progress.session_states[0].signal.activity.as_deref(),
            Some("active")
        );
        assert!(chat
            .record_session_signal(&conversation, &run, retry.clone())
            .is_none());
        let mut terminal = crate::agent::metadata::session_signal(&fixtures[5]).unwrap();
        terminal.session_id = retry.session_id.clone();
        chat.record_session_signal(&conversation, &run, terminal)
            .unwrap();
        assert!(chat
            .record_session_signal(&conversation, &run, retry)
            .is_none());
        chat.transcript_coordinator
            .finish(
                &chat,
                &conversation,
                &run,
                ConversationRunStatus::Completed,
                None,
            )
            .await
            .unwrap();
        let finished = page(&chat, &conversation).await;
        assert!(!finished.session_states[0].live);
        assert!(finished.session_states[0].signal.activity.is_none());
        let late = ConversationSessionSignal {
            session_id: "claude:session".into(),
            activity: Some("active".into()),
            ..Default::default()
        };
        assert!(chat
            .record_session_signal(&conversation, &run, late)
            .is_none());
    }

    #[tokio::test]
    async fn unversioned_failures_preserve_transitions_and_independent_runs() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, text("before", None))
            .await
            .unwrap();
        let legacy = |retry| {
            let mut signal = crate::agent::metadata::session_signal(&serde_json::json!({"_meta":{"codex":{"error":{"message":"Service unavailable", "willRetry":retry}}}})).unwrap();
            signal.session_id = "codex:session".into();
            signal
        };
        let warning = legacy(true);
        assert!(chat
            .record_session_signal(&conversation, &run, warning.clone())
            .is_some());
        assert!(chat
            .record_session_signal(&conversation, &run, warning.clone())
            .is_none());
        let terminal = legacy(false);
        let saved = chat
            .record_session_signal(&conversation, &run, terminal.clone())
            .unwrap();
        assert_eq!(saved.failure.unwrap().severity, "error");
        assert!(saved.activity.is_none());
        assert!(chat
            .record_session_signal(&conversation, &run, terminal.clone())
            .is_none());
        let next_run = ConversationRunID("another-run".into());
        assert!(chat
            .record_session_signal(&conversation, &next_run, terminal.clone())
            .is_some());

        // Repeated incident data cannot swallow an independent limit update.
        let mut repeated = terminal;
        repeated.limit = Some(ottam_protocol::ConversationSessionLimit {
            status: "rejected".into(),
            kind: None,
            resets_at: Some(2000000000),
        });
        assert!(chat
            .record_session_signal(&conversation, &run, repeated)
            .unwrap()
            .limit
            .is_some());
    }

    #[tokio::test]
    async fn initial_metadata_waits_for_text_without_inventing_rows_or_crossing_sessions() {
        let (_home, chat, conversation, run) = fixture().await;
        let rich = |text: &str, session: &str, phase: Option<&str>| AgentUpdate::RichMessageChunk {
            text: text.into(),
            message_id: Some("same-message".into()),
            reasoning: false,
            metadata: TranscriptAdapterMetadata {
                session_id: Some(session.into()),
                phase: phase.map(str::to_owned),
                parent_tool_call_id: phase.map(|_| "unresolved".into()),
                ..Default::default()
            },
        };
        chat.record_transcript_update(&conversation, &run, rich("", "one", Some("commentary")))
            .await
            .unwrap();
        assert!(page(&chat, &conversation)
            .await
            .items
            .iter()
            .all(|item| item.kind != TranscriptItemKind::AssistantText));
        chat.record_transcript_update(&conversation, &run, rich("other", "two", None))
            .await
            .unwrap();
        chat.record_transcript_update(&conversation, &run, rich("answer", "one", None))
            .await
            .unwrap();
        let mut result = page(&chat, &conversation).await;
        result
            .items
            .retain(|item| item.kind == TranscriptItemKind::AssistantText);
        result.items.sort_by_key(|item| item.position);
        assert_eq!(result.items.len(), 2);
        assert!(result.items[0].adapter_metadata.phase.is_none());
        assert_eq!(
            result.items[1].adapter_metadata.phase.as_deref(),
            Some("commentary")
        );
        assert_eq!(
            result.items[1]
                .adapter_metadata
                .parent_tool_call_id
                .as_deref(),
            Some("unresolved")
        );
        assert_eq!(result.items[1].body_preview, "answer");
    }

    /// A native subagent's text streams between its parent's chunks: each
    /// keeps one row, and Claude's receipt for the hidden launch call is no
    /// row at all.
    #[tokio::test]
    async fn native_subagent_text_keeps_its_own_segment() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let child = |value: &str| AgentUpdate::RichMessageChunk {
            text: value.into(),
            message_id: None,
            reasoning: false,
            metadata: TranscriptAdapterMetadata {
                subagent_thread_id: Some("subagent:child".into()),
                ..Default::default()
            },
        };
        for update in [
            text("Hello", Some("parent")),
            child("Reading"),
            AgentUpdate::ToolCallUpdate {
                payload: serde_json::json!({
                    "toolCallId": "toolu_hidden",
                    "status": "completed",
                    "_meta": {"claudeCode": {"toolName": "Agent", "toolResponse": {"status": "async_launched"}}}
                }),
            },
            text(" world", Some("parent")),
            child(" the README"),
        ] {
            chat.record_transcript_update(&conversation, &run, update)
                .await
                .unwrap();
        }
        chat.transcript_coordinator.checkpoint().await;
        let mut items = page(&chat, &conversation).await.items;
        items.sort_by_key(|item| item.position);
        let rows: Vec<_> = items
            .iter()
            .filter(|item| item.kind != TranscriptItemKind::Message)
            .map(|item| {
                (
                    item.kind,
                    item.body_preview.as_str(),
                    item.subagent_thread_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (TranscriptItemKind::AssistantText, "Hello world", None),
                (
                    TranscriptItemKind::AssistantText,
                    "Reading the README",
                    Some("subagent:child")
                ),
            ]
        );
        // Core routes by the field; the item carries it once.
        assert!(items
            .iter()
            .all(|item| item.adapter_metadata.subagent_thread_id.is_none()));
    }

    /// A subagent's plan is its thread's own: it never revises the parent's
    /// plan or closes the parent's text, and its image joins its thread.
    #[tokio::test]
    async fn native_subagent_plans_and_images_keep_their_thread() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let plan = |status: &str, thread: Option<&str>| {
            let mut payload = serde_json::json!({"entries": [{"content": "Work", "priority": "high", "status": status}]});
            if let Some(thread) = thread {
                payload[NATIVE_THREAD_STAMP] = thread.into();
            }
            AgentUpdate::Plan { payload }
        };
        let mut image = image_call(
            serde_json::json!({"type": "image", "mimeType": "image/png", "data": "AA=="}),
            None,
        );
        if let AgentUpdate::ToolCall { payload } = &mut image {
            payload["toolCallId"] = "image-1".into();
            payload[NATIVE_THREAD_STAMP] = "subagent:child".into();
        }
        for update in [
            text("Hello", Some("parent")),
            plan("pending", Some("subagent:child")),
            text(" world", Some("parent")),
            plan("pending", None),
            plan("completed", Some("subagent:child")),
            image,
        ] {
            chat.record_transcript_update(&conversation, &run, update)
                .await
                .unwrap();
        }
        chat.transcript_coordinator.checkpoint().await;
        let mut items = page(&chat, &conversation).await.items;
        items.sort_by_key(|item| item.position);
        let rows: Vec<_> = items
            .iter()
            .filter(|item| item.kind != TranscriptItemKind::Message)
            .map(|item| {
                (
                    item.kind,
                    item.plan.as_ref().map(|plan| plan[0].status.as_str()),
                    item.subagent_thread_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (TranscriptItemKind::AssistantText, None, None),
                (
                    TranscriptItemKind::Plan,
                    Some("completed"),
                    Some("subagent:child")
                ),
                (TranscriptItemKind::Plan, Some("pending"), None),
                (TranscriptItemKind::Image, None, Some("subagent:child")),
            ]
        );
        let parent_text = items
            .iter()
            .find(|item| item.kind == TranscriptItemKind::AssistantText)
            .unwrap();
        assert_eq!(parent_text.body_preview, "Hello world");
    }

    /// A subagent's permission decision is its thread's row: it never splits
    /// the parent's open text.
    #[tokio::test]
    async fn native_subagent_permission_keeps_its_thread() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("Hello", Some("parent")))
            .await
            .unwrap();
        chat.record_run_events(&conversation, &run, None, vec![(ConversationEventKind::PermissionResolved, json!({
            "subagentThreadID": "subagent:child",
            "transcriptPermission": {"requestID":"p", "title":"Write notes.txt", "outcomeLabel":"Yes", "allowed":true}
        }))]).await.unwrap();
        chat.record_transcript_update(&conversation, &run, text(" world", Some("parent")))
            .await
            .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        let mut items = page(&chat, &conversation).await.items;
        items.sort_by_key(|item| item.position);
        let rows: Vec<_> = items
            .iter()
            .filter(|item| item.kind != TranscriptItemKind::Message)
            .map(|item| {
                (
                    item.kind,
                    item.body_preview.as_str(),
                    item.subagent_thread_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (TranscriptItemKind::AssistantText, "Hello world", None),
                (TranscriptItemKind::Permission, "", Some("subagent:child")),
            ]
        );
    }

    #[tokio::test]
    async fn transcript_metadata_survives_checkpoint_and_offline_reload() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let metadata = TranscriptAdapterMetadata {
            session_id: Some("codex:one".into()),
            phase: Some("final_answer".into()),
            parent_tool_call_id: Some("unresolved".into()),
            ..Default::default()
        };
        let delta = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::RichMessageChunk {
                    text: "answer".into(),
                    message_id: Some("id".into()),
                    reasoning: false,
                    metadata: metadata.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(delta.body_appends[0].text, "answer");
        chat.transcript_coordinator.checkpoint().await;
        let cold = ChatService::new(chat.database.clone(), chat.workspace_id.clone());
        let reloaded = page(&cold, &conversation).await;
        let item = reloaded
            .items
            .iter()
            .find(|item| item.kind == TranscriptItemKind::AssistantText)
            .unwrap();
        assert_eq!(item.adapter_metadata, metadata);
        assert_eq!(item.body_preview, "answer");
    }

    #[tokio::test]
    async fn transcript_segments_share_identity_and_preserve_explicit_boundaries() {
        // Adapters that send no message id still segment at tool boundaries.
        for id in [Some("message-1"), None] {
            let (_home, chat, conversation, run) = fixture().await;
            let first = chat
                .record_transcript_update(&conversation, &run, text("Hello", id))
                .await
                .unwrap();
            let second = chat
                .record_transcript_update(&conversation, &run, text(" world", id))
                .await
                .unwrap();
            assert_eq!(first.items[0].item_id, second.items[0].item_id);
            assert_eq!(second.items[0].revision, 2);
            chat.record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::AgentThoughtChunk {
                    text: "Shared reasoning".into(),
                    message_id: None,
                },
            )
            .await
            .unwrap();
            chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCall { payload: json!({"toolCallId":"tool-1","title":"Read","kind":"read","rawInput":{"path":"example.txt"}}) }).await.unwrap();
            chat.record_transcript_update(&conversation, &run, text("After tool", id))
                .await
                .unwrap();
            let items = page(&chat, &conversation).await.items;
            assert_eq!(
                items.iter().map(|item| item.kind).collect::<Vec<_>>(),
                vec![
                    TranscriptItemKind::AssistantText,
                    TranscriptItemKind::Tool,
                    TranscriptItemKind::Reasoning,
                    TranscriptItemKind::AssistantText,
                    TranscriptItemKind::Message
                ]
            );
            assert_eq!(items[3].body_preview, "Hello world");
            assert_eq!(items[3].completion, TranscriptCompletion::Complete);
            assert_ne!(items[0].item_id, items[3].item_id);
            assert_eq!(items[2].body_preview, "Shared reasoning");
        }
    }

    #[tokio::test]
    async fn transcript_id_change_and_plan_updates_do_not_renumber() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, text("A", Some("a")))
            .await
            .unwrap();
        chat.record_transcript_update(&conversation, &run, text("B", Some("b")))
            .await
            .unwrap();
        let first = chat.record_transcript_update(&conversation, &run, AgentUpdate::Plan { payload: json!({"entries":[{"content":"Work","priority":"high","status":"pending"}]}) }).await.unwrap();
        let plan = first.items.last().unwrap();
        chat.record_transcript_update(&conversation, &run, text("C", None))
            .await
            .unwrap();
        let update = chat.record_transcript_update(&conversation, &run, AgentUpdate::Plan { payload: json!({"entries":[{"content":"Work","priority":"high","status":"completed"}]}) }).await.unwrap();
        let updated = update.items.last().unwrap();
        assert_eq!(
            (plan.item_id.as_str(), plan.position),
            (updated.item_id.as_str(), updated.position)
        );
        assert_eq!(updated.revision, plan.revision + 1);
        assert_eq!(updated.plan.as_ref().unwrap()[0].status, "completed");
    }

    #[tokio::test]
    async fn transcript_steering_order_survives_rejection_and_restart() {
        let (home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, text("A", None))
            .await
            .unwrap();
        let prepared = chat
            .prepare_steer(
                &conversation,
                &run,
                "steer".into(),
                vec![],
                "steer-key".into(),
            )
            .await
            .unwrap();
        let domain_conversation = parse_conversation_id(&conversation).unwrap();
        let input_id =
            ConversationRunInputId::try_from(prepared.input.run_input_id.0.clone()).unwrap();
        chat.database
            .run("test.rejectSteer", move |connection| {
                RunRepository::mark_input_failed(
                    connection,
                    &domain_conversation,
                    &input_id,
                    "adapterRejected",
                    Utc::now(),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let replay = chat
            .prepare_steer(
                &conversation,
                &run,
                "steer".into(),
                vec![],
                "steer-key".into(),
            )
            .await
            .unwrap();
        assert_eq!(prepared.input.run_input_id, replay.input.run_input_id);
        chat.record_transcript_update(&conversation, &run, text("B", None))
            .await
            .unwrap();
        let before = page(&chat, &conversation).await;
        assert_eq!(
            before
                .items
                .iter()
                .rev()
                .map(|item| item.position)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 4]
        );
        assert_eq!(before.items[1].message.as_ref().unwrap().body, "steer");
        let workspace_id = chat.workspace_id.clone();
        chat.transcript_coordinator.drain().await;
        drop(chat);
        let database = reopen(home.path()).await;
        let chat = ChatService::new(database, workspace_id);
        let after = page(&chat, &conversation).await;
        // Execution status is independently joined from durable run state.
        // Capture preserves content, identity, ordering, and revisions exactly.
        let mut before_items = before.items;
        let mut after_items = after.items;
        for item in before_items.iter_mut().chain(after_items.iter_mut()) {
            item.run_status = None;
            item.committed_revision = None;
        }
        assert_eq!(before_items, after_items);
        chat.record_transcript_update(&conversation, &run, text("new boundary", Some("new")))
            .await
            .unwrap();
        assert_eq!(page(&chat, &conversation).await.items[0].position, 5);
    }

    #[tokio::test]
    async fn transcript_revisions_are_atomic_stale_safe_and_owned() {
        let (_home, chat, conversation, run) = fixture().await;
        let first = chat
            .record_transcript_update(&conversation, &run, text("A", None))
            .await
            .unwrap()
            .items
            .remove(0);
        let id = first.item_id.clone();
        chat.transcript_coordinator.drain().await;
        let conv = conversation.0.clone();
        chat.database
            .run("test.revision", move |connection| {
                let original = Repo::get(connection, &conv, &id)?;
                let mut changed = original.row.clone();
                changed.revision += 1;
                changed.body_byte_count += 1;
                use crate::storage::schema::transcript_checkpoint_bodies as staged;
                diesel::insert_into(staged::table)
                    .values((
                        staged::conversation_id.eq(&conv),
                        staged::item_id.eq(&id),
                        staged::revision.eq(changed.revision),
                        staged::byte_offset.eq(0i64),
                        staged::body.eq("AB"),
                    ))
                    .execute(connection)?;
                let failure: Result<(), StorageError> =
                    recorded_transaction(connection, "test.rollback", |connection| {
                        Repo::publish_checkpoint(
                            connection,
                            &changed,
                            &TranscriptDetail::None,
                            changed.revision,
                        )?;
                        Err(StorageError::new("test", "rollback after body write"))
                    });
                assert!(failure.is_err());
                assert_eq!(Repo::get(connection, &conv, &id)?.preview, "A");
                assert!(recorded_transaction(
                    connection,
                    "test.save",
                    |connection| Repo::publish_checkpoint(
                        connection,
                        &changed,
                        &TranscriptDetail::None,
                        changed.revision
                    )
                )?);
                assert!(!Repo::publish_checkpoint(
                    connection,
                    &original.row,
                    &TranscriptDetail::None,
                    original.row.revision,
                )?);
                assert_eq!(Repo::get(connection, &conv, &id)?.preview, "AB");
                assert!(Repo::read_body(connection, &conv, &id, 1, 0, 65536).is_err());
                assert!(Repo::get(connection, "wrong-conversation", &id).is_err());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn transcript_concurrent_admission_is_ordered_and_failed_reservations_leave_holes() {
        let (_home, chat, conversation, run) = fixture().await;
        let failure: Result<(), StorageError> = chat
            .database
            .run_transcript(
                conversation.0.clone(),
                "test.failedAdmission",
                1,
                |_connection, _position| Err(StorageError::new("test", "admission rejected")),
            )
            .await;
        assert!(failure.is_err());
        let mut tasks = Vec::new();
        for index in 0..32 {
            let chat = chat.clone();
            let conversation = conversation.clone();
            let run = run.clone();
            tasks.push(tokio::spawn(async move {
                chat.record_transcript_update(
                    &conversation,
                    &run,
                    text(&format!("part-{index}"), Some(&index.to_string())),
                )
                .await
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let all = page(&chat, &conversation).await.items;
        assert_eq!(all.len(), 33);
        assert!(all.iter().all(|item| item.position != 1));
        let mut cursor = None;
        let mut ids = std::collections::HashSet::new();
        loop {
            let page = chat
                .list_transcript(ConversationListTranscriptParams {
                    workspace_id: chat.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    limit: Some(7),
                    cursor,
                    subagent_thread_id: None,
                })
                .await
                .unwrap();
            assert!(page.items.len() <= 7);
            for item in page.items {
                assert!(ids.insert(item.item_id));
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids.len(), all.len());
    }

    #[tokio::test]
    async fn transcript_active_tool_and_plan_survive_saved_and_unsaved_retention_pressure() {
        for saved in [false, true] {
            let (_home, chat, conversation, run) = fixture().await;
            chat.transcript_coordinator.manual_checkpoints();
            let initial = chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCall {
                payload: json!({"toolCallId":"retained-tool","rawInput":{"command":"ls"},"status":"in_progress"})
            }).await.unwrap().items.remove(0);
            let initial_plan = chat.record_transcript_update(&conversation, &run, AgentUpdate::Plan {
                payload: json!({"entries":[{"content":"Work","status":"pending","priority":"high"}]})
            }).await.unwrap().items.pop().unwrap();
            if saved {
                chat.transcript_coordinator.checkpoint().await;
                chat.transcript_coordinator.checkpoint().await;
            }
            chat.record_transcript_update(
                &conversation,
                &run,
                text(&"x".repeat(8 * 1024 * 1024), None),
            )
            .await
            .unwrap();
            let updated = chat
                .record_transcript_update(
                    &conversation,
                    &run,
                    AgentUpdate::ToolCallUpdate {
                        payload: json!({"toolCallId":"retained-tool","status":"completed"}),
                    },
                )
                .await
                .unwrap()
                .items
                .pop()
                .unwrap();
            assert_eq!(
                (&initial.item_id, initial.position),
                (&updated.item_id, updated.position)
            );
            assert_eq!(initial.body_preview, updated.body_preview);
            assert_eq!(initial.body_byte_count, updated.body_byte_count);
            let updated_plan = chat.record_transcript_update(&conversation, &run, AgentUpdate::Plan {
                payload: json!({"entries":[{"content":"Work","status":"completed","priority":"high"}]})
            }).await.unwrap().items.pop().unwrap();
            assert_eq!(
                (&initial_plan.item_id, initial_plan.position),
                (&updated_plan.item_id, updated_plan.position)
            );
            chat.transcript_coordinator.checkpoint().await;
            chat.transcript_coordinator.checkpoint().await;
            let body = chat
                .database
                .run("test.retainedToolBody", move |connection| {
                    Repo::read_body(
                        connection,
                        &conversation.0,
                        &initial.item_id,
                        updated.revision as i64,
                        0,
                        65536,
                    )
                })
                .await
                .unwrap();
            assert_eq!(body.0, "{\"command\":\"ls\"}");
        }
    }

    #[tokio::test]
    async fn transcript_over_budget_tool_replacement_preserves_accepted_identity_and_arguments() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        let initial = chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCall {
            payload: json!({"toolCallId":"bounded","rawInput":{"command":"ls"},"status":"in_progress"})
        }).await.unwrap().items.remove(0);
        for _ in 0..2 {
            let dropped = chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCallUpdate {
                payload: json!({"toolCallId":"bounded","rawInput":{"command":"x".repeat(8 * 1024 * 1024)}})
            }).await.unwrap();
            assert!(dropped.items.is_empty());
            let capture = dropped
                .capture
                .iter()
                .find(|capture| capture.run_id == run)
                .unwrap();
            assert_eq!(
                capture.gaps.len(),
                1,
                "repeated loss at one identity must not duplicate gap rows"
            );
            assert_eq!(capture.gaps[0].start_position, initial.position);
        }
        let completion = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::ToolCallUpdate {
                    payload: json!({"toolCallId":"bounded","status":"completed"}),
                },
            )
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(initial.item_id, completion.item_id);
        assert_eq!(initial.body_preview, completion.body_preview);
        assert_eq!(initial.revision + 1, completion.revision);
    }

    #[tokio::test]
    async fn transcript_snapshot_does_not_advance_capture_watermark_during_checkpoint() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, text("saved", None))
            .await
            .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            json!({}),
        )
        .await
        .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *chat
            .transcript_coordinator
            .snapshot_read_gate
            .lock()
            .unwrap() = Some((entered_tx, release_rx));
        let reader_chat = chat.clone();
        let reader_conversation = conversation.clone();
        let reader = tokio::spawn(async move { page(&reader_chat, &reader_conversation).await });
        entered_rx.await.unwrap();
        chat.transcript_coordinator.drain().await;
        let completed = page(&chat, &conversation).await;
        assert!(completed
            .capture
            .iter()
            .all(|status| status.problem.is_none()));
        release_tx.send(()).unwrap();
        let overlapping = reader.await.unwrap();
        assert!(
            overlapping.sequence < completed.sequence,
            "the older live capture must retain its earlier watermark"
        );
        assert!(completed
            .capture
            .iter()
            .any(|status| status.run_id == run && status.state.as_deref() == Some("complete")));
    }

    #[tokio::test]
    async fn transcript_partial_tool_updates_preserve_or_replace_saved_arguments() {
        let (_home, chat, conversation, run) = fixture().await;
        let initial = chat.record_transcript_update(&conversation, &run, AgentUpdate::ToolCallUpdate {
            payload: json!({"toolCallId":"unmatched","rawInput":{"command":"ls"},"status":"in_progress"})
        }).await.unwrap().items.remove(0);
        assert!(initial.tool.as_ref().unwrap().partial_metadata);
        let update = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::ToolCallUpdate {
                    payload: json!({"toolCallId":"unmatched","status":"completed"}),
                },
            )
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(initial.item_id, update.item_id);
        assert_eq!(initial.body_preview, update.body_preview);
        let cleared = chat
            .record_transcript_update(
                &conversation,
                &run,
                AgentUpdate::ToolCallUpdate {
                    payload: json!({"toolCallId":"unmatched","rawInput":{},"title":""}),
                },
            )
            .await
            .unwrap()
            .items
            .remove(0);
        assert_eq!(cleared.body_preview, "{}");
        assert_eq!(cleared.body_byte_count, 2);
        assert_eq!(cleared.tool.unwrap().title.as_deref(), Some(""));
    }

    #[test]
    fn transcript_file_change_arrays_preserve_ordinary_arguments() {
        let tool = normalize_tool(
            None,
            &json!({
                "toolCallId": "edit-1", "kind": "edit", "rawInput": {
                    "options": ["replace_all", "preserve_mode"],
                    "paths": ["a.rs", "b.rs"],
                    "extensions": [["rs", "md"]],
                    "edits": [{"path": "a.rs", "oldText": "before", "newText": "after"}]
                }
            }),
        );
        assert_eq!(
            tool.arguments,
            json!({
                "options": ["replace_all", "preserve_mode"],
                "paths": ["a.rs", "b.rs"],
                "extensions": [["rs", "md"]],
                "edits": [{"path": "a.rs"}]
            })
        );
        assert_eq!(
            tool.payloads
                .iter()
                .find(|p| p.role == "fileChangeInput")
                .unwrap()
                .byte_count,
            Some(11),
            "only the before/after contents count as uncaptured bytes"
        );

        let ordinary = normalize_tool(
            None,
            &json!({"kind":"edit", "rawInput":{"options":["dry-run"]}}),
        );
        assert_eq!(ordinary.arguments, json!({"options":["dry-run"]}));
        assert!(!ordinary
            .payloads
            .iter()
            .any(|p| p.role == "fileChangeInput"));

        // An MCP call is no file change, even named or captioned like one.
        let mcp = normalize_tool(
            None,
            &json!({"name":"mcp__linear_edit_issue", "kind":"other", "title":"Writing notes",
                    "rawInput":{"path":"notes.md", "content":"x"}}),
        );
        assert_eq!(mcp.arguments, json!({"path":"notes.md", "content":"x"}));
        assert!(!mcp.payloads.iter().any(|p| p.role == "fileChangeInput"));

        // Nor is an omp device run, whatever the device is called.
        let device = normalize_tool(
            None,
            &json!({"name":"report_issue", "kind":"other", "title":"Write up the crash",
                    "ottamDeviceCall": true, "rawInput":{"text":"crash on save"}}),
        );
        assert_eq!(device.arguments, json!({"text":"crash on save"}));
        assert!(!device.payloads.iter().any(|p| p.role == "fileChangeInput"));
    }

    #[test]
    fn transcript_tool_normalization_replaces_fields_and_never_copies_payload_bodies() {
        let secret = "large submitted file contents";
        let initial = normalize_tool(
            None,
            &json!({"toolCallId":"t","title":"Write file","kind":"edit", "rawInput":{"file_path":"x","content":secret,"replace_all":true},"rawOutput":"secret output","content":[{"type":"diff","newText":secret}], "truncated":true}),
        );
        let saved = encode(&initial).unwrap();
        assert!(!saved.contains(secret));
        assert!(!saved.contains("secret output"));
        assert_eq!(
            initial.arguments,
            json!({"file_path":"x","replace_all":true})
        );
        assert_eq!(
            initial
                .payloads
                .iter()
                .find(|p| p.role == "output")
                .unwrap()
                .upstream_truncated,
            Some(true)
        );
        let update = normalize_tool(Some(initial.clone()), &json!({"status":"failed"}));
        assert_eq!(update.arguments, initial.arguments);
        assert_eq!(update.title, initial.title);
        let cleared = normalize_tool(
            Some(update),
            &json!({"title":"", "rawInput":{}, "rawOutput":"", "locations":[]}),
        );
        assert_eq!(cleared.title.as_deref(), Some(""));
        assert_eq!(cleared.arguments, json!({}));
        assert_eq!(
            cleared
                .payloads
                .iter()
                .find(|p| p.role == "output")
                .unwrap()
                .byte_count,
            Some(0)
        );
        assert!(cleared
            .payloads
            .iter()
            .all(|p| !p.locally_truncated && p.availability == "unavailable"));
    }

    #[tokio::test]
    async fn transcript_large_utf8_bodies_page_without_size_truncation_or_raw_events() {
        let (_home, chat, conversation, run) = fixture().await;
        let body = "🦀reasoning\n".repeat(400_000);
        assert!(body.len() > 4 * 1024 * 1024);
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::AgentThoughtChunk {
                text: body.clone(),
                message_id: None,
            },
        )
        .await
        .unwrap();
        let item = page(&chat, &conversation).await.items.remove(0);
        assert!(item.body_preview.len() <= 4096);
        assert_eq!(item.body_byte_count, body.len() as u64);
        let mut offset = 0;
        let mut read = String::new();
        loop {
            let part = chat
                .read_transcript_body(ConversationReadTranscriptBodyParams {
                    workspace_id: chat.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    item_id: item.item_id.clone(),
                    revision: item.revision,
                    offset,
                })
                .await
                .unwrap();
            assert!(part.text.len() <= 65536);
            read.push_str(&part.text);
            match part.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(read, body);
        chat.database
            .run("test.rawEvents", |connection| {
                use crate::storage::schema::run_events;
                assert_eq!(run_events::table.count().get_result::<i64>(connection)?, 0);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pre_agent_failure_erases_narration_but_ordinary_failure_preserves_it() {
        for kind in [
            "authRequired",
            "runtimeMissing",
            "adapterNotInstalled",
            "transportError",
        ] {
            let (_home, chat, conversation, run) = fixture().await;
            let delta = chat
                .record_transcript_update(&conversation, &run, text("provider narration", None))
                .await
                .unwrap();
            let original = delta.items.last().unwrap();
            chat.finalize_run(
                &conversation,
                &run,
                ConversationRunStatus::Failed,
                None,
                json!({"kind": kind, "message": "failure"}),
            )
            .await
            .unwrap();
            let items = page(&chat, &conversation).await.items;
            let item = items
                .iter()
                .find(|item| item.item_id == original.item_id)
                .unwrap();
            assert!(item.revision > original.revision);
            let suppressed = kind != "transportError";
            assert_eq!(
                item.completion == TranscriptCompletion::Suppressed,
                suppressed
            );
            let body = chat
                .read_transcript_body(ConversationReadTranscriptBodyParams {
                    workspace_id: chat.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    item_id: item.item_id.clone(),
                    revision: item.revision,
                    offset: 0,
                })
                .await
                .unwrap();
            assert_eq!(
                body.text,
                if suppressed { "" } else { "provider narration" }
            );
            assert_eq!(items[0].kind, TranscriptItemKind::RunBoundary);
            assert_eq!(items[0].run_status, Some(ConversationRunStatus::Failed));
        }
    }

    #[tokio::test]
    async fn transcript_finalization_publishes_one_sequence_for_the_whole_batch() {
        for status in [
            ConversationRunStatus::Completed,
            ConversationRunStatus::Canceled,
        ] {
            let (_home, chat, conversation, run) = fixture().await;
            chat.transcript_coordinator.manual_checkpoints();
            let delta = chat
                .record_transcript_update(&conversation, &run, text("final text", None))
                .await
                .unwrap();
            chat.finalize_run(&conversation, &run, status, None, Value::Null)
                .await
                .unwrap();
            let final_page = page(&chat, &conversation).await;
            assert_eq!(
                final_page.sequence,
                delta.sequence + 1,
                "normal completion must not trigger Swift's missing-notification rescan"
            );
            assert_eq!(final_page.items[0].kind, TranscriptItemKind::RunBoundary);
            let settled = final_page
                .items
                .iter()
                .find(|item| item.item_id == delta.items[0].item_id)
                .unwrap();
            assert_eq!(settled.revision, delta.items[0].revision + 1);
            assert_ne!(settled.completion, TranscriptCompletion::Open);
        }
    }

    #[tokio::test]
    async fn empty_run_has_a_durable_terminal_boundary() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.transcript_coordinator.manual_checkpoints();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Canceled,
            Some(AgentStopReason::Cancelled),
            Value::Null,
        )
        .await
        .unwrap();
        let items = page(&chat, &conversation).await.items;
        assert_eq!(items[0].kind, TranscriptItemKind::RunBoundary);
        assert_eq!(items[0].run_status, Some(ConversationRunStatus::Canceled));
        assert!(items[0].position > items[1].position);
        assert_eq!(page(&chat, &conversation).await.items, items);
    }

    #[tokio::test]
    async fn transcript_publication_requires_a_recorded_item() {
        let (_home, chat, conversation, run) = fixture().await;
        let (outbound, mut notifications) = crate::server::outbound::test_sink();
        let chat = chat.with_transcript_delivery(Some(outbound), "test-core".into());
        chat.record_transcript_update(&conversation, &run, text("in progress", None))
            .await
            .unwrap();
        let before = page(&chat, &conversation).await;
        let events = chat
            .record_run_events(
                &conversation,
                &run,
                None,
                vec![
                    (
                        ConversationEventKind::SessionMetadata,
                        json!({"usage":{"used":1,"size":100}}),
                    ),
                    (
                        ConversationEventKind::RunStatus,
                        json!({"status":"completed"}),
                    ),
                    (
                        ConversationEventKind::PermissionResolved,
                        json!({"requestID":"legacy"}),
                    ),
                ],
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 3, "lifecycle events still persist");
        assert!(matches!(
            notifications.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(page(&chat, &conversation).await.items, before.items);
        assert_eq!(page(&chat, &conversation).await.sequence, before.sequence);

        chat.record_run_events(&conversation, &run, None, vec![
            (ConversationEventKind::SessionMetadata, json!({"currentModeID":"plan"})),
            (ConversationEventKind::PermissionResolved, json!({"transcriptPermission":{
                "requestID":"p", "title":"Run command", "outcomeLabel":"Allow once", "allowed":true
            }})),
        ]).await.unwrap();
        let notification = notifications
            .try_recv()
            .expect("permission boundary is published")
            .frame;
        assert_eq!(
            notification["method"],
            NOTIFICATION_CONVERSATION_TRANSCRIPT_DELTA
        );
        assert!(notification["params"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "permission"));
        assert!(matches!(
            notifications.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            page(&chat, &conversation).await.sequence,
            before.sequence + 1
        );
    }

    #[tokio::test]
    async fn transcript_permission_and_capture_completion_are_typed() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, text("partial", None))
            .await
            .unwrap();
        chat.record_run_events(&conversation, &run, None, vec![(ConversationEventKind::PermissionResolved, json!({"transcriptPermission":{
            "requestID":"p","providerToolCallID":"t","title":"Run command","outcomeLabel":"Reject once","allowed":false
        }}))]).await.unwrap();
        chat.record_transcript_update(&conversation, &run, text("unfinished", None))
            .await
            .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Canceled,
            Some(AgentStopReason::Cancelled),
            Value::Null,
        )
        .await
        .unwrap();
        let items = page(&chat, &conversation).await.items;
        assert_eq!(items[0].kind, TranscriptItemKind::RunBoundary);
        assert_eq!(items[0].run_status, Some(ConversationRunStatus::Canceled));
        assert_eq!(items[1].completion, TranscriptCompletion::Partial);
        assert_eq!(items[2].kind, TranscriptItemKind::Permission);
        assert!(!items[2].permission.as_ref().unwrap().allowed);
        assert_eq!(items[3].completion, TranscriptCompletion::Complete);
    }

    #[tokio::test]
    async fn a_plan_permission_keeps_its_plan() {
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_run_events(&conversation, &run, None, vec![(ConversationEventKind::PermissionResolved, json!({"transcriptPermission":{
            "requestID":"p","title":"Approve Plan","outcomeLabel":"Cancelled","allowed":false,"plan":"# Add TODO.md"
        }}))]).await.unwrap();
        let items = page(&chat, &conversation).await.items;
        let permission = items
            .iter()
            .find_map(|item| item.permission.as_ref())
            .expect("permission record");
        assert_eq!(permission.plan.as_deref(), Some("# Add TODO.md"));
    }

    fn background_call(id: &str) -> AgentUpdate {
        AgentUpdate::ToolCall {
            payload: json!({"toolCallId": id, "title": "npm run dev", "kind": "execute", "status": "pending"}),
        }
    }

    fn background_stamp(id: &str, status: &str) -> AgentUpdate {
        AgentUpdate::ToolCallUpdate {
            payload: json!({
                "toolCallId": id,
                "status": status,
                "_meta": {"jetbrains": {"air": {"asyncTasks": {"backgrounded": true}}}},
            }),
        }
    }

    fn task_state(id: &str, state: AsyncTaskState) -> AgentUpdate {
        AgentUpdate::AsyncTaskStateUpdate {
            async_task_id: format!("task_{id}"),
            state,
            provider_tool_call_id: Some(id.into()),
            summary: None,
            output_file_path: None,
        }
    }

    fn counts(background: &[ConversationRunBackgroundTasks]) -> Vec<(&str, u32)> {
        background
            .iter()
            .map(|count| (count.run_id.0.as_str(), count.live))
            .collect()
    }

    fn tool_item<'a>(
        items: &'a [ConversationTranscriptItem],
        id: &str,
    ) -> (&'a ConversationTranscriptItem, &'a TranscriptTool) {
        items
            .iter()
            .find_map(|item| {
                let tool = item.tool.as_ref()?;
                (tool.provider_tool_call_id.as_deref() == Some(id)).then_some((item, tool))
            })
            .unwrap_or_else(|| panic!("no tool item for {id}"))
    }

    /// Rule 4: a closed run takes only background fields, in memory while it
    /// holds the tool and straight in storage once it was evicted; either way
    /// the revision and the run's live count move.
    #[tokio::test]
    async fn background_task_revises_a_closed_run_in_memory_then_in_storage() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        for id in ["call_a", "call_b"] {
            chat.record_transcript_update(&conversation, &run, background_call(id))
                .await
                .unwrap();
        }
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            Value::Null,
        )
        .await
        .unwrap();
        let closed = page(&chat, &conversation).await;
        let (closed_a, closed_tool_a) = tool_item(&closed.items, "call_a");
        let (closed_a, closed_tool_a) = (closed_a.clone(), closed_tool_a.clone());

        // In memory: only the stamp lands, not the status it carries.
        let stamped = coordinator
            .update(
                &chat,
                &conversation,
                &run,
                background_stamp("call_a", "failed"),
            )
            .await
            .unwrap();
        assert_eq!(stamped.items.len(), 1);
        assert_eq!(stamped.items[0].revision, closed_a.revision + 1);
        assert_eq!(counts(&stamped.background_tasks), [(run.0.as_str(), 1)]);
        let tool = stamped.items[0].tool.as_ref().unwrap();
        assert_eq!(tool.status, closed_tool_a.status);
        assert_eq!(
            tool.background_task.as_ref().map(|task| task.state),
            Some(AsyncTaskState::Running)
        );
        // A plain tool update is not the closed run's to take.
        let refused = coordinator
            .update(
                &chat,
                &conversation,
                &run,
                AgentUpdate::ToolCallUpdate {
                    payload: json!({"toolCallId": "call_b", "status": "failed"}),
                },
            )
            .await
            .unwrap();
        assert!(refused.items.is_empty());

        // Evicted: the revision is written to storage under the writer.
        coordinator.drain().await;
        coordinator.evict(&conversation);
        let stopped = coordinator
            .update(
                &chat,
                &conversation,
                &run,
                task_state("call_a", AsyncTaskState::Stopped),
            )
            .await
            .unwrap();
        assert_eq!(stopped.items.len(), 1);
        assert_eq!(stopped.items[0].revision, closed_a.revision + 2);
        assert!(stopped.sequence > stamped.sequence);
        assert_eq!(counts(&stopped.background_tasks), [(run.0.as_str(), 0)]);
        assert!(!coordinator.conversations.lock().unwrap()[&conversation.0]
            .items
            .values()
            .any(|item| item.record.row.id == closed_a.item_id));

        let after = page(&chat, &conversation).await;
        let (item_a, tool_a) = tool_item(&after.items, "call_a");
        assert_eq!(item_a.revision, closed_a.revision + 2);
        assert_eq!(tool_a.status, closed_tool_a.status);
        assert_eq!(
            tool_a.background_task,
            Some(TranscriptBackgroundTask {
                id: Some("task_call_a".into()),
                state: AsyncTaskState::Stopped,
                output_file_path: None,
            })
        );
        let (_, tool_b) = tool_item(&after.items, "call_b");
        assert_eq!(tool_b.status.as_deref(), Some("pending"));
        assert_eq!(counts(&after.background_tasks), [(run.0.as_str(), 0)]);
    }

    /// Rule 4: a closed run's frames are bounded as an open run's are, and a
    /// tool evicted from a conversation memory still holds is revised in
    /// storage only once the checkpoint writer lets go, at the conversation's
    /// next sequence.
    #[tokio::test]
    async fn background_task_storage_revision_waits_for_the_writer_and_is_bounded() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        chat.record_transcript_update(&conversation, &run, background_call("call_a"))
            .await
            .unwrap();
        chat.record_transcript_update(
            &conversation,
            &run,
            task_state("call_a", AsyncTaskState::Running),
        )
        .await
        .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            Value::Null,
        )
        .await
        .unwrap();
        let closed = page(&chat, &conversation).await;
        let closed_a = tool_item(&closed.items, "call_a").0.clone();

        // A task id past the bound drops the frame; a path past it keeps the
        // prior path.
        let oversized = coordinator
            .update(
                &chat,
                &conversation,
                &run,
                AgentUpdate::AsyncTaskStateUpdate {
                    async_task_id: "t".repeat(5000),
                    state: AsyncTaskState::Failed,
                    provider_tool_call_id: Some("call_a".into()),
                    summary: None,
                    output_file_path: None,
                },
            )
            .await
            .unwrap();
        assert!(oversized.items.is_empty());
        let paused = coordinator
            .update(
                &chat,
                &conversation,
                &run,
                AgentUpdate::AsyncTaskStateUpdate {
                    async_task_id: "task_call_a".into(),
                    state: AsyncTaskState::Paused,
                    provider_tool_call_id: Some("call_a".into()),
                    summary: None,
                    output_file_path: Some("p".repeat(5000)),
                },
            )
            .await
            .unwrap();
        assert_eq!(paused.items[0].revision, closed_a.revision + 1);
        let task = |item: &ConversationTranscriptItem| {
            item.tool
                .as_ref()
                .and_then(|tool| tool.background_task.clone())
                .unwrap()
        };
        assert_eq!(task(&paused.items[0]).state, AsyncTaskState::Paused);
        assert_eq!(task(&paused.items[0]).output_file_path, None);

        // The conversation stays; only the saved tool leaves memory.
        coordinator.drain().await;
        let sequence = {
            let mut states = coordinator.conversations.lock().unwrap();
            let state = states.get_mut(&conversation.0).unwrap();
            state
                .items
                .retain(|_, item| item.record.row.id != closed_a.item_id);
            state.sequence
        };
        let writer = coordinator.hold_writer().await;
        let revision = tokio::spawn({
            let chat = chat.clone();
            let (conversation, run) = (conversation.clone(), run.clone());
            async move {
                chat.transcript_coordinator
                    .update(
                        &chat,
                        &conversation,
                        &run,
                        task_state("call_a", AsyncTaskState::Stopped),
                    )
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !revision.is_finished(),
            "a storage revision waits for the checkpoint writer"
        );
        drop(writer);
        let stopped = revision.await.unwrap().unwrap();
        assert_eq!(stopped.items.len(), 1);
        assert_eq!(stopped.items[0].revision, closed_a.revision + 2);
        assert_eq!(stopped.sequence, sequence + 1);
        assert_eq!(counts(&stopped.background_tasks), [(run.0.as_str(), 0)]);
        assert_eq!(task(&stopped.items[0]).state, AsyncTaskState::Stopped);
        assert!(!coordinator.conversations.lock().unwrap()[&conversation.0]
            .items
            .values()
            .any(|item| item.record.row.id == closed_a.item_id));
        let after = page(&chat, &conversation).await;
        assert_eq!(
            tool_item(&after.items, "call_a").0.revision,
            closed_a.revision + 2
        );
    }

    /// A question lives on the tool call that asked it: pending, then what
    /// the user did. A late pending never reopens it, the agent's own
    /// updates keep it, and the run's end settles one left pending.
    #[tokio::test]
    async fn a_question_records_on_its_call_and_the_run_end_settles_it() {
        use ottam_protocol::{
            TranscriptQuestion, TranscriptQuestionAnswer, TranscriptQuestionState,
        };
        let (_home, chat, conversation, run) = fixture().await;
        for id in ["call_asked", "call_answered"] {
            chat.record_transcript_update(&conversation, &run, background_call(id))
                .await
                .unwrap();
        }
        let question = |state, values: &[&str]| TranscriptQuestion {
            request_id: "request".into(),
            state,
            answers: vec![TranscriptQuestionAnswer {
                prompt: "Which approach?".into(),
                values: values.iter().map(|value| value.to_string()).collect(),
                secure: false,
            }],
        };
        for (id, record) in [
            (
                "call_asked",
                question(TranscriptQuestionState::Pending, &[]),
            ),
            (
                "call_answered",
                question(TranscriptQuestionState::Pending, &[]),
            ),
            (
                "call_answered",
                question(TranscriptQuestionState::Answered, &["Refactor"]),
            ),
            (
                "call_answered",
                question(TranscriptQuestionState::Pending, &[]),
            ),
            ("nobody", question(TranscriptQuestionState::Pending, &[])),
        ] {
            chat.record_question(&conversation, &run, id.into(), record)
                .await
                .unwrap();
        }
        chat.record_transcript_update(
            &conversation,
            &run,
            AgentUpdate::ToolCallUpdate {
                payload: json!({"toolCallId": "call_answered", "status": "completed"}),
            },
        )
        .await
        .unwrap();
        let open = page(&chat, &conversation).await;
        let state = |items: &[ConversationTranscriptItem], id: &str| {
            tool_item(items, id).1.question.clone().unwrap()
        };
        assert_eq!(
            state(&open.items, "call_asked").state,
            TranscriptQuestionState::Pending
        );
        let answered = state(&open.items, "call_answered");
        assert_eq!(answered.state, TranscriptQuestionState::Answered);
        assert_eq!(answered.answers[0].values, vec!["Refactor".to_string()]);

        chat.transcript_coordinator
            .finish(
                &chat,
                &conversation,
                &run,
                ConversationRunStatus::Canceled,
                None,
            )
            .await
            .unwrap();
        let finished = page(&chat, &conversation).await;
        assert_eq!(
            state(&finished.items, "call_asked").state,
            TranscriptQuestionState::NotAnswered
        );
        assert_eq!(
            state(&finished.items, "call_answered").state,
            TranscriptQuestionState::Answered
        );
    }

    /// An earlier core stopped with a question pending: finishing the run
    /// in storage, as startup recovery does, settles it.
    #[tokio::test]
    async fn a_stored_run_finish_settles_its_pending_question() {
        use ottam_protocol::{TranscriptQuestion, TranscriptQuestionState};
        let (_home, chat, conversation, run) = fixture().await;
        chat.record_transcript_update(&conversation, &run, background_call("call_asked"))
            .await
            .unwrap();
        chat.record_question(
            &conversation,
            &run,
            "call_asked".into(),
            TranscriptQuestion {
                request_id: "request".into(),
                state: TranscriptQuestionState::Pending,
                answers: Vec::new(),
            },
        )
        .await
        .unwrap();
        chat.transcript_coordinator.checkpoint().await;
        let (conversation_id, run_id) = (conversation.0.clone(), run.0.clone());
        let stored = chat
            .database
            .run("test.recoverQuestion", move |connection| {
                Repo::finish_run(connection, &run_id, "partial", 1)?;
                Repo::run_tool(connection, &conversation_id, &run_id, "call_asked")
            })
            .await
            .unwrap()
            .expect("stored tool");
        let TranscriptDetail::Tool { json, .. } = stored.detail else {
            panic!("tool detail");
        };
        let tool: TranscriptTool = decode(&json).unwrap();
        assert_eq!(
            tool.question.unwrap().state,
            TranscriptQuestionState::NotAnswered
        );
    }

    /// Rule 7: core marks only a live task lost, and a stored task this core
    /// never saw start reads lost, but only as read from storage.
    #[tokio::test]
    async fn background_task_loss_spares_ended_tasks_and_reads_from_storage_only() {
        let (_home, chat, conversation, run) = fixture().await;
        let coordinator = &chat.transcript_coordinator;
        coordinator.manual_checkpoints();
        for id in ["call_live", "call_done"] {
            chat.record_transcript_update(&conversation, &run, background_call(id))
                .await
                .unwrap();
            chat.record_transcript_update(
                &conversation,
                &run,
                task_state(id, AsyncTaskState::Running),
            )
            .await
            .unwrap();
        }
        chat.record_transcript_update(
            &conversation,
            &run,
            task_state("call_done", AsyncTaskState::Completed),
        )
        .await
        .unwrap();
        let open = page(&chat, &conversation).await;
        assert_eq!(counts(&open.background_tasks), [(run.0.as_str(), 1)]);

        let lose = |id: &str| live::TaskChange::Lost {
            provider_tool_call_id: Some(id.into()),
            async_task_id: None,
        };
        let spared = coordinator
            .revise(&chat, &conversation, &run, lose("call_done"))
            .await
            .unwrap();
        assert!(spared.items.is_empty());
        let lost = coordinator
            .revise(&chat, &conversation, &run, lose("call_live"))
            .await
            .unwrap();
        assert_eq!(counts(&lost.background_tasks), [(run.0.as_str(), 0)]);
        let state = |items: &[ConversationTranscriptItem], id: &str| {
            tool_item(items, id)
                .1
                .background_task
                .as_ref()
                .unwrap()
                .state
        };
        let after = page(&chat, &conversation).await;
        assert_eq!(state(&after.items, "call_live"), AsyncTaskState::Lost);
        assert_eq!(state(&after.items, "call_done"), AsyncTaskState::Completed);

        // A running task as an earlier core left it: an item memory serves
        // is not judged, and one read from storage reads lost.
        chat.record_transcript_update(&conversation, &run, background_call("call_old"))
            .await
            .unwrap();
        chat.record_transcript_update(
            &conversation,
            &run,
            task_state("call_old", AsyncTaskState::Running),
        )
        .await
        .unwrap();
        *coordinator.background.lock().unwrap() = live::BackgroundTasks::default();
        let in_memory = page(&chat, &conversation).await;
        assert_eq!(state(&in_memory.items, "call_old"), AsyncTaskState::Running);
        assert!(in_memory.background_tasks.is_empty());
        // A task this core saw start is still running when read from storage.
        chat.record_transcript_update(&conversation, &run, background_call("call_alive"))
            .await
            .unwrap();
        chat.record_transcript_update(
            &conversation,
            &run,
            task_state("call_alive", AsyncTaskState::Running),
        )
        .await
        .unwrap();
        chat.finalize_run(
            &conversation,
            &run,
            ConversationRunStatus::Completed,
            None,
            Value::Null,
        )
        .await
        .unwrap();
        coordinator.drain().await;
        coordinator.evict(&conversation);
        let stored = page(&chat, &conversation).await;
        assert_eq!(state(&stored.items, "call_old"), AsyncTaskState::Lost);
        assert_eq!(state(&stored.items, "call_done"), AsyncTaskState::Completed);
        assert_eq!(state(&stored.items, "call_alive"), AsyncTaskState::Running);
        assert_eq!(counts(&stored.background_tasks), [(run.0.as_str(), 1)]);
    }
}
