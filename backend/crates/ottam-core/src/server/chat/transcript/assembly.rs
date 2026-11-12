//! SQLite-free normalization, shared by durable and live transcript assembly.
use super::subagents::StoredMetadata;
use super::{decode, encode, is_backgrounded, normalize_tool, stamp_background};
use crate::storage::{
    repositories::transcript::{TranscriptDetail, TranscriptRecord, TranscriptRow},
    StorageError,
};
use ottam_protocol::{
    AgentUpdate, AsyncTaskState, SubagentThreadSource, SubagentThreadState,
    TranscriptBackgroundTask, TranscriptPlanEntry, TranscriptTool,
};
use serde_json::Value;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct AssemblyState {
    pub latest: Option<TranscriptRow>,
    pub tool: Option<TranscriptRecord>,
    pub plan: Option<TranscriptRecord>,
}

/// A body is either appended, replaced, or unchanged. Coalescing must apply these
/// changes to the owning revision before discarding earlier transport deltas.
pub(super) struct AssemblyChange {
    pub row: TranscriptRow,
    pub detail: TranscriptDetail,
    pub body: Option<String>,
    pub replace: bool,
    /// Whether the row may become the latest row (`takes_latest`), judged once.
    pub takes_latest: bool,
}

/// Pure transcript normalization. All I/O belongs to the caller.
pub(super) struct TranscriptAssembler<'a> {
    pub state: AssemblyState,
    pub changes: Vec<AssemblyChange>,
    /// Tool items whose background-task state this assembly changed, with the
    /// new state, so the caller can track live tasks without decoding tools.
    pub background: Vec<(String, AsyncTaskState)>,
    pub conversation: &'a str,
    pub run: &'a str,
    pub now: i64,
    pub position: i64,
}

impl TranscriptAssembler<'_> {
    pub fn new_row(&mut self, kind: &str) -> Result<TranscriptRow, StorageError> {
        Ok(TranscriptRow {
            id: Uuid::now_v7().to_string(),
            conversation_id: self.conversation.into(),
            run_id: Some(self.run.into()),
            position: self.position,
            revision: 1,
            kind: kind.into(),
            completion: "open".into(),
            provider_message_id: None,
            adapter_metadata_json: "{}".into(),
            body_byte_count: 0,
            created_at: self.now,
            updated_at: self.now,
        })
    }

    pub fn apply(&mut self, update: AgentUpdate) -> Result<Vec<String>, StorageError> {
        let mut changed = Vec::new();
        let text = match &update {
            AgentUpdate::RichMessageChunk {
                text,
                message_id,
                reasoning,
                ..
            } => Some((
                if *reasoning {
                    "reasoning"
                } else {
                    "assistantText"
                },
                text.as_str(),
                message_id,
            )),
            AgentUpdate::AgentMessageChunk { text, message_id } => {
                Some(("assistantText", text.as_str(), message_id))
            }
            AgentUpdate::AgentThoughtChunk { text, message_id } => {
                Some(("reasoning", text.as_str(), message_id))
            }
            _ => None,
        };
        if let Some((kind, text, message_id)) = text {
            let metadata = match &update {
                AgentUpdate::RichMessageChunk { metadata, .. } => Some(metadata),
                _ => None,
            };
            if text.is_empty() && metadata.is_none() {
                return Ok(changed);
            }
            let last = self.state.latest.clone();
            let continuing = last.as_ref().is_some_and(|row| {
                row.kind == kind
                    && (row.completion == "open" || text.is_empty())
                    && row.run_id.as_deref() == Some(self.run)
                    && row.provider_message_id == *message_id
                    && metadata.is_none_or(|incoming| {
                        super::subagents::same_segment(&row.adapter_metadata_json, incoming)
                    })
            });
            // A metadata-only observation can revise an existing segment but
            // must never manufacture an empty history row.
            if text.is_empty() && !continuing {
                return Ok(changed);
            }
            let mut row = if continuing {
                let mut row = last.expect("checked last segment");
                row.revision += 1;
                row
            } else {
                self.close(&mut changed)?;
                let mut row = self.new_row(kind)?;
                row.provider_message_id = message_id.clone();
                row
            };
            row.body_byte_count += text.len() as i64;
            if let Some(metadata) = metadata {
                let mut stored: StoredMetadata = decode(&row.adapter_metadata_json)?;
                let prior = &mut stored.adapter;
                prior.session_id = metadata.session_id.clone().or(prior.session_id.take());
                prior.phase = metadata.phase.clone().or(prior.phase.take());
                prior.parent_message_id = metadata
                    .parent_message_id
                    .clone()
                    .or(prior.parent_message_id.take());
                prior.parent_tool_call_id = metadata
                    .parent_tool_call_id
                    .clone()
                    .or(prior.parent_tool_call_id.take());
                stored.resolve_thread(metadata.subagent_thread_id.as_deref());
                row.adapter_metadata_json = encode(&stored)?;
            }
            row.updated_at = self.now;
            self.save(&row, TranscriptDetail::None, Some(text.to_owned()), false);
            changed.push(row.id);
            return Ok(changed);
        }
        match update {
            AgentUpdate::ToolCallUpdate { payload }
                if self.state.tool.is_none() && hidden_control_receipt(&payload) =>
            {
                tracing::debug!(run = self.run, "dropped a native subagent's launch receipt");
            }
            // A subagent's end names its anchor, which lives in the run it
            // spawned in; this run has none to revise.
            AgentUpdate::ToolCallUpdate { payload }
                if self.state.tool.is_none()
                    && payload.get(super::subagents::NATIVE_ANCHOR_KEY).is_some() =>
            {
                tracing::debug!(
                    run = self.run,
                    "dropped a subagent's end without its anchor"
                );
            }
            AgentUpdate::ToolCall { payload } | AgentUpdate::ToolCallUpdate { payload } => {
                let provider = payload.get("toolCallId").and_then(Value::as_str);
                let existing = self.state.tool.take();
                debug_assert!(existing.as_ref().is_none_or(|record| match &record.detail {
                    TranscriptDetail::Tool { provider_id, .. } =>
                        provider_id.as_deref() == provider,
                    _ => false,
                }));
                let (mut row, prior) = if let Some(record) = existing {
                    let mut row = record.row;
                    row.revision += 1;
                    let tool = match record.detail {
                        TranscriptDetail::Tool { json, .. } => {
                            Some(decode::<TranscriptTool>(&json)?)
                        }
                        _ => None,
                    };
                    (row, tool)
                } else {
                    (
                        self.new_row(
                            if payload.get("_ottamImage").and_then(Value::as_bool) == Some(true)
                            {
                                "image"
                            } else {
                                "tool"
                            },
                        )?,
                        None,
                    )
                };
                let prior_task = background_state(prior.as_ref());
                let mut tool = normalize_tool(prior, &payload);
                self.observe_background(&row.id, prior_task, &tool);
                if row.kind == "image" {
                    row.provider_message_id = payload
                        .get("messageID")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                let incoming = crate::agent::metadata::transcript_metadata(&payload);
                let mut metadata: StoredMetadata = decode(&row.adapter_metadata_json)?;
                metadata.adapter.parent_tool_call_id = incoming
                    .parent_tool_call_id
                    .or(metadata.adapter.parent_tool_call_id);
                metadata.adapter.session_id = payload
                    .get("ottamSessionID")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(metadata.adapter.session_id);
                metadata.resolve_thread(
                    payload
                        .get(super::subagents::NATIVE_THREAD_STAMP)
                        .and_then(Value::as_str),
                );
                // A subagent's call is not the parent's, and neither is
                // progress on a call that started before the open segment —
                // a background task, or omp's subagent batch, reporting while
                // the agent keeps thinking. Either way the parent's open text
                // segment stays open around it; closing it would cut the
                // segment into a row per chunk.
                let reports_earlier_call = self
                    .state
                    .latest
                    .as_ref()
                    .is_some_and(|latest| latest.position > row.position);
                if metadata.subagent_thread_id().is_none() && !reports_earlier_call {
                    self.close(&mut changed)?;
                }
                if let Some(thread) = tool.subagent_thread.as_mut() {
                    thread.parent_thread_id = thread
                        .parent_thread_id
                        .take()
                        .or(metadata.adapter.subagent_thread_id.clone());
                }
                row.adapter_metadata_json = encode(&metadata)?;
                let arguments = payload
                    .get("rawInput")
                    .map(|_| encode(&tool.arguments))
                    .transpose()?;
                tool.arguments = Value::Null;
                if let Some(arguments) = &arguments {
                    row.body_byte_count = arguments.len() as i64;
                }
                row.updated_at = self.now;
                row.completion = match tool.status.as_deref() {
                    Some("completed") => "complete",
                    Some("failed") => "partial",
                    _ => "open",
                }
                .into();
                self.save(
                    &row,
                    TranscriptDetail::Tool {
                        provider_id: tool.provider_tool_call_id.clone(),
                        json: encode(&tool)?,
                    },
                    arguments,
                    true,
                );
                changed.push(row.id);
            }
            AgentUpdate::Plan { payload } => {
                let thread = payload
                    .get(super::subagents::NATIVE_THREAD_STAMP)
                    .and_then(Value::as_str);
                // A subagent's plan is not the parent's: the parent's open
                // text segment stays open around it.
                if thread.is_none() {
                    self.close(&mut changed)?;
                }
                let mut row = match self.state.plan.take() {
                    Some(record) => {
                        let mut row = record.row;
                        row.revision += 1;
                        row
                    }
                    None => {
                        let mut row = self.new_row("plan")?;
                        let mut metadata: StoredMetadata = decode(&row.adapter_metadata_json)?;
                        metadata.resolve_thread(thread);
                        row.adapter_metadata_json = encode(&metadata)?;
                        row
                    }
                };
                row.updated_at = self.now;
                let entries: Vec<TranscriptPlanEntry> = payload
                    .get("entries")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|entry| TranscriptPlanEntry {
                        content: entry
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        priority: entry
                            .get("priority")
                            .and_then(Value::as_str)
                            .unwrap_or("medium")
                            .into(),
                        status: entry
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending")
                            .into(),
                    })
                    .collect();
                row.completion = if entries.iter().all(|entry| entry.status == "completed") {
                    "complete"
                } else {
                    "open"
                }
                .into();
                self.save(&row, TranscriptDetail::Plan(encode(&entries)?), None, false);
                changed.push(row.id);
            }
            AgentUpdate::StreamGap { dropped } => {
                self.close(&mut changed)?;
                let mut row = self.new_row("gap")?;
                row.completion = "partial".into();
                self.save(
                    &row,
                    TranscriptDetail::Gap {
                        end_position: row.position,
                        dropped: dropped.min(i64::MAX as u64) as i64,
                        reason: "Adapter stream updates were lost".into(),
                    },
                    None,
                    false,
                );
                changed.push(row.id);
            }
            AgentUpdate::AsyncTaskSpawned {
                async_task_id,
                output_file_path,
                ..
            }
            | AgentUpdate::AsyncTaskProgress {
                async_task_id,
                output_file_path,
                ..
            } => {
                self.revise_background_task(async_task_id, None, output_file_path, &mut changed)?
            }
            AgentUpdate::AsyncTaskStateUpdate {
                async_task_id,
                state,
                output_file_path,
                ..
            } => self.revise_background_task(
                async_task_id,
                Some(state),
                output_file_path,
                &mut changed,
            )?,
            _ => {}
        }
        Ok(changed)
    }

    /// Revises only `tool.backgroundTask` on the caller-supplied tool. Status,
    /// completion, and body stay as the tool updates left them, and an open
    /// text segment stays open: the task runs beside the turn, not in it.
    fn revise_background_task(
        &mut self,
        async_task_id: String,
        state: Option<AsyncTaskState>,
        output_file_path: Option<String>,
        changed: &mut Vec<String>,
    ) -> Result<(), StorageError> {
        // A task that names no tool of this run anchors nothing; it never
        // manufactures a row.
        let Some(TranscriptRecord {
            row,
            detail: TranscriptDetail::Tool { provider_id, json },
            ..
        }) = self.state.tool.take()
        else {
            tracing::debug!(
                run = self.run,
                async_task_id,
                "async task update names no tool of this run"
            );
            return Ok(());
        };
        let mut tool: TranscriptTool = decode(&json)?;
        let prior = tool.background_task.clone();
        let task = tool
            .background_task
            .get_or_insert(TranscriptBackgroundTask {
                id: None,
                state: AsyncTaskState::Running,
                output_file_path: None,
            });
        task.id.get_or_insert(async_task_id);
        if let Some(state) = state {
            task.state = state;
        }
        if output_file_path.is_some() {
            task.output_file_path = output_file_path;
        }
        self.save_background(row, provider_id, prior, tool, changed)
    }

    /// A closed run takes only the background stamp from a tool update; its
    /// status, body, and arguments stay as the run left them. `false` when the
    /// update carries no stamp, so the caller reports it refused.
    pub fn stamp_background(
        &mut self,
        payload: &Value,
        changed: &mut Vec<String>,
    ) -> Result<bool, StorageError> {
        if !is_backgrounded(payload) {
            return Ok(false);
        }
        let Some(TranscriptRecord {
            row,
            detail: TranscriptDetail::Tool { provider_id, json },
            ..
        }) = self.state.tool.take()
        else {
            return Ok(true);
        };
        let mut tool: TranscriptTool = decode(&json)?;
        let prior = tool.background_task.clone();
        stamp_background(&mut tool, payload);
        self.save_background(row, provider_id, prior, tool, changed)?;
        Ok(true)
    }

    /// A native subagent's end on its anchor, in a run that closed or no
    /// longer holds the root: only the call's status and thread change.
    pub fn revise_subagent(
        &mut self,
        payload: &Value,
        changed: &mut Vec<String>,
    ) -> Result<bool, StorageError> {
        let Some(TranscriptRecord {
            mut row,
            detail: TranscriptDetail::Tool { provider_id, json },
            ..
        }) = self.state.tool.take()
        else {
            return Ok(true);
        };
        let prior: TranscriptTool = decode(&json)?;
        if prior.subagent_thread.is_none() {
            return Ok(false);
        }
        let tool = normalize_tool(Some(prior.clone()), payload);
        if tool == prior {
            return Ok(true);
        }
        row.revision += 1;
        row.updated_at = self.now;
        row.completion = match tool.status.as_deref() {
            Some("failed") => "partial",
            _ => "complete",
        }
        .into();
        self.save(
            &row,
            TranscriptDetail::Tool {
                provider_id,
                json: encode(&tool)?,
            },
            None,
            false,
        );
        changed.push(row.id);
        Ok(true)
    }

    /// Revises only `tool.question`. A settled question never goes back to
    /// pending, whatever order the two records arrive in.
    pub fn revise_question(
        &mut self,
        question: ottam_protocol::TranscriptQuestion,
        changed: &mut Vec<String>,
    ) -> Result<(), StorageError> {
        let Some(TranscriptRecord {
            mut row,
            detail: TranscriptDetail::Tool { provider_id, json },
            ..
        }) = self.state.tool.take()
        else {
            return Ok(());
        };
        let mut tool: TranscriptTool = decode(&json)?;
        let settled = tool.question.as_ref().is_some_and(|prior| {
            prior.request_id == question.request_id
                && prior.state != ottam_protocol::TranscriptQuestionState::Pending
        });
        if tool.question.as_ref() == Some(&question)
            || (settled && question.state == ottam_protocol::TranscriptQuestionState::Pending)
        {
            return Ok(());
        }
        tool.question = Some(question);
        row.revision += 1;
        row.updated_at = self.now;
        self.save(
            &row,
            TranscriptDetail::Tool {
                provider_id,
                json: encode(&tool)?,
            },
            None,
            false,
        );
        changed.push(row.id);
        Ok(())
    }

    /// Core lost the task: its adapter or session died before it reported an
    /// end. A task that already ended keeps its state. A native subagent's
    /// anchor that core lost reads as disconnected.
    pub fn lose_background_task(&mut self, changed: &mut Vec<String>) -> Result<(), StorageError> {
        let Some(TranscriptRecord {
            mut row,
            detail: TranscriptDetail::Tool { provider_id, json },
            ..
        }) = self.state.tool.take()
        else {
            return Ok(());
        };
        let mut tool: TranscriptTool = decode(&json)?;
        if let Some(thread) = tool
            .subagent_thread
            .as_mut()
            .filter(|thread| thread.source == SubagentThreadSource::NativeSession)
        {
            if !thread.state.is_live() {
                return Ok(());
            }
            thread.state = SubagentThreadState::Disconnected;
            row.revision += 1;
            row.updated_at = self.now;
            self.save(
                &row,
                TranscriptDetail::Tool {
                    provider_id,
                    json: encode(&tool)?,
                },
                None,
                false,
            );
            changed.push(row.id);
            return Ok(());
        }
        let prior = tool.background_task.clone();
        match &mut tool.background_task {
            Some(task) if task.state.is_live() => task.state = AsyncTaskState::Lost,
            _ => return Ok(()),
        }
        self.save_background(row, provider_id, prior, tool, changed)
    }

    /// Saves a revision that changed only `backgroundTask`. A frame that
    /// repeats what the row already says changes nothing.
    fn save_background(
        &mut self,
        mut row: TranscriptRow,
        provider_id: Option<String>,
        prior: Option<TranscriptBackgroundTask>,
        tool: TranscriptTool,
        changed: &mut Vec<String>,
    ) -> Result<(), StorageError> {
        if tool.background_task == prior {
            return Ok(());
        }
        self.observe_background(&row.id, prior.map(|task| task.state), &tool);
        row.revision += 1;
        row.updated_at = self.now;
        self.save(
            &row,
            TranscriptDetail::Tool {
                provider_id,
                json: encode(&tool)?,
            },
            None,
            false,
        );
        changed.push(row.id);
        Ok(())
    }

    fn observe_background(
        &mut self,
        item: &str,
        prior: Option<AsyncTaskState>,
        tool: &TranscriptTool,
    ) {
        if let Some(state) = background_state(Some(tool)).filter(|state| Some(*state) != prior) {
            self.background.push((item.to_owned(), state));
        }
    }

    pub fn close(&mut self, changed: &mut Vec<String>) -> Result<(), StorageError> {
        if let Some(mut row) = self.state.latest.clone() {
            if matches!(row.kind.as_str(), "assistantText" | "reasoning")
                && row.completion == "open"
            {
                row.completion = "complete".into();
                row.revision += 1;
                row.updated_at = self.now;
                self.save(&row, TranscriptDetail::None, None, false);
                changed.push(row.id);
            }
        }
        Ok(())
    }
    fn save(
        &mut self,
        row: &TranscriptRow,
        detail: TranscriptDetail,
        body: Option<String>,
        replace: bool,
    ) {
        let takes_latest = takes_latest(row);
        if takes_latest
            && self
                .state
                .latest
                .as_ref()
                .is_none_or(|latest| latest.position <= row.position)
        {
            self.state.latest = Some(row.clone());
        }
        self.changes.push(AssemblyChange {
            row: row.clone(),
            detail,
            body,
            replace,
            takes_latest,
        });
    }
}

/// The latest row is the one the parent's next text may continue or its next
/// tool may close. A subagent's row is neither, so the parent's segment stays
/// open around it; the subagent's own text continues by its thread (see
/// `TranscriptCoordinator::record_update`).
fn takes_latest(row: &TranscriptRow) -> bool {
    decode::<StoredMetadata>(&row.adapter_metadata_json)
        .map_or(true, |metadata| metadata.subagent_thread_id().is_none())
}

/// Claude still reports a native subagent's hidden `Agent`/`Task` call as a
/// bare `tool_call_update` launch receipt. With no call to update, it would
/// manufacture an untitled tool row; the thread's anchor already shows it.
fn hidden_control_receipt(payload: &Value) -> bool {
    matches!(
        payload
            .pointer("/_meta/claudeCode/toolName")
            .and_then(Value::as_str),
        Some("Agent" | "Task")
    )
}

fn background_state(tool: Option<&TranscriptTool>) -> Option<AsyncTaskState> {
    tool?.background_task.as_ref().map(|task| task.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assembler(state: AssemblyState, position: i64) -> TranscriptAssembler<'static> {
        TranscriptAssembler {
            state,
            changes: Vec::new(),
            background: Vec::new(),
            conversation: "conversation",
            run: "run",
            now: 10,
            position,
        }
    }

    fn text(value: &str) -> AgentUpdate {
        AgentUpdate::AgentMessageChunk {
            text: value.into(),
            message_id: Some("provider-message".into()),
        }
    }

    #[test]
    fn transcript_metadata_revises_without_reordering_or_empty_rows() {
        let rich = |text: &str, session: &str, phase: Option<&str>| AgentUpdate::RichMessageChunk {
            text: text.into(),
            message_id: Some("same-id".into()),
            reasoning: false,
            metadata: ottam_protocol::TranscriptAdapterMetadata {
                session_id: Some(session.into()),
                phase: phase.map(str::to_owned),
                parent_tool_call_id: Some("unresolved".into()),
                ..Default::default()
            },
        };
        let mut first = assembler(AssemblyState::default(), 0);
        first.apply(rich("", "one", Some("commentary"))).unwrap();
        assert!(first.changes.is_empty());
        first
            .apply(rich("answer", "one", Some("commentary")))
            .unwrap();
        let original = first.changes.last().unwrap().row.clone();
        // The parent link threads the row, so it never takes the latest;
        // `record_update` hands the assembler the thread's row instead.
        let resumed = |assembler: TranscriptAssembler<'static>| AssemblyState {
            latest: assembler.changes.last().map(|change| change.row.clone()),
            ..assembler.state
        };
        let mut second = assembler(resumed(first), 1);
        second.apply(rich("", "one", Some("final_answer"))).unwrap();
        let revised = &second.changes[0].row;
        assert_eq!(revised.id, original.id);
        assert_eq!(revised.position, original.position);
        assert_eq!(revised.body_byte_count, original.body_byte_count);
        assert_eq!(revised.revision, original.revision + 1);
        let mut third = assembler(resumed(second), 2);
        third.apply(rich(" continued", "one", None)).unwrap();
        let metadata: ottam_protocol::TranscriptAdapterMetadata =
            decode(&third.changes[0].row.adapter_metadata_json).unwrap();
        assert_eq!(metadata.phase.as_deref(), Some("final_answer"));
        assert_eq!(metadata.parent_tool_call_id.as_deref(), Some("unresolved"));
        let mut other = assembler(resumed(third), 3);
        other.apply(rich("different session", "two", None)).unwrap();
        assert_ne!(other.changes.last().unwrap().row.id, original.id);
    }

    #[test]
    fn transcript_assembly_is_io_free_and_closes_before_a_new_kind() {
        let mut first = assembler(AssemblyState::default(), 0);
        first.apply(text("hello ")).unwrap();
        let id = first.changes[0].row.id.clone();
        let mut second = assembler(first.state, 1);
        second.apply(text("🌍")).unwrap();
        assert_eq!(second.changes[0].row.id, id);
        assert_eq!(second.changes[0].row.position, 0);
        assert_eq!(second.changes[0].row.body_byte_count, 10);
        assert_eq!(second.changes[0].row.revision, 2);
        assert_eq!(second.changes[0].body.as_deref(), Some("🌍"));

        let mut thought = assembler(second.state, 2);
        thought
            .apply(AgentUpdate::AgentThoughtChunk {
                text: "thinking".into(),
                message_id: None,
            })
            .unwrap();
        assert_eq!(thought.changes.len(), 2);
        assert_eq!(thought.changes[0].row.id, id);
        assert_eq!(thought.changes[0].row.completion, "complete");
        assert_eq!(thought.changes[0].row.revision, 3);
        assert!(thought.changes[0].body.is_none());
        assert_eq!(thought.changes[1].row.kind, "reasoning");
        assert_eq!(thought.changes[1].row.position, 2);
    }

    #[test]
    fn progress_on_an_earlier_call_does_not_split_the_open_segment() {
        let thought = |text: &str| AgentUpdate::AgentThoughtChunk {
            text: text.into(),
            message_id: None,
        };
        let mut started = assembler(AssemblyState::default(), 0);
        started
            .apply(AgentUpdate::ToolCall {
                payload: json!({"toolCallId":"batch", "title":"Mapping", "kind":"other", "status":"in_progress", "rawInput":{}}),
            })
            .unwrap();
        let tool = started.changes.pop().unwrap();
        let record = TranscriptRecord {
            committed_revision: 0,
            row: tool.row,
            detail: tool.detail,
            preview: String::new(),
            run_status: None,
        };

        let mut thinking = assembler(
            AssemblyState {
                latest: Some(record.row.clone()),
                ..Default::default()
            },
            1,
        );
        thinking.apply(thought("check the ")).unwrap();
        let segment = thinking.changes.pop().unwrap().row;

        // The batch reports progress while the agent is still thinking.
        let mut progress = assembler(
            AssemblyState {
                latest: Some(segment.clone()),
                tool: Some(record),
                plan: None,
            },
            2,
        );
        progress
            .apply(AgentUpdate::ToolCallUpdate {
                payload: json!({"toolCallId":"batch", "status":"in_progress"}),
            })
            .unwrap();
        assert_eq!(progress.changes.len(), 1, "only the tool row is revised");
        assert_eq!(progress.changes[0].row.kind, "tool");
        assert_eq!(progress.state.latest.as_ref().unwrap().id, segment.id);

        let mut more = assembler(
            AssemblyState {
                latest: progress.state.latest.clone(),
                ..Default::default()
            },
            3,
        );
        more.apply(thought("tests count.")).unwrap();
        let continued = &more.changes[0].row;
        assert_eq!(continued.id, segment.id);
        assert_eq!(continued.completion, "open");
        assert_eq!(
            continued.body_byte_count,
            "check the tests count.".len() as i64
        );

        // A new call still closes the segment.
        let mut next = assembler(
            AssemblyState {
                latest: Some(continued.clone()),
                ..Default::default()
            },
            4,
        );
        next.apply(AgentUpdate::ToolCall {
            payload: json!({"toolCallId":"read", "title":"Read", "kind":"read"}),
        })
        .unwrap();
        assert_eq!(next.changes[0].row.id, segment.id);
        assert_eq!(next.changes[0].row.completion, "complete");
    }

    #[test]
    fn transcript_assembly_distinguishes_omitted_and_replaced_tool_bodies() {
        let mut first = assembler(AssemblyState::default(), 0);
        first.apply(AgentUpdate::ToolCall { payload: json!({"toolCallId":"tool", "title":"Edit", "kind":"edit", "rawInput":{"paths":["a","b"]}}) }).unwrap();
        let change = first.changes.pop().unwrap();
        let id = change.row.id.clone();
        let byte_count = change.row.body_byte_count;
        assert!(change.replace);
        assert!(change.body.as_ref().unwrap().contains("paths"));
        let record = TranscriptRecord {
            committed_revision: 0,
            row: change.row,
            detail: change.detail,
            preview: String::new(),
            run_status: None,
        };
        let mut update = assembler(
            AssemblyState {
                latest: Some(record.row.clone()),
                tool: Some(record.clone()),
                plan: None,
            },
            1,
        );
        update
            .apply(AgentUpdate::ToolCallUpdate {
                payload: json!({"toolCallId":"tool", "status":"completed"}),
            })
            .unwrap();
        let updated = update.changes.pop().unwrap();
        assert_eq!(updated.row.id, id);
        assert_eq!(updated.row.body_byte_count, byte_count);
        assert!(updated.body.is_none());
        assert_eq!(updated.row.completion, "complete");
        let mut clear = assembler(
            AssemblyState {
                latest: None,
                tool: Some(record),
                plan: None,
            },
            2,
        );
        clear
            .apply(AgentUpdate::ToolCallUpdate {
                payload: json!({"toolCallId":"tool", "rawInput":{}}),
            })
            .unwrap();
        assert_eq!(clear.changes[0].body.as_deref(), Some("{}"));
    }

    #[test]
    fn async_task_revision_leaves_the_open_segment_and_tool_status_alone() {
        let mut first = assembler(AssemblyState::default(), 0);
        first
            .apply(AgentUpdate::ToolCall {
                payload: json!({"toolCallId":"tool", "title":"npm run dev", "status":"in_progress", "rawInput":{"command":"npm run dev"}}),
            })
            .unwrap();
        let change = first.changes.pop().unwrap();
        let tool = TranscriptRecord {
            committed_revision: 0,
            row: change.row,
            detail: change.detail,
            preview: String::new(),
            run_status: None,
        };
        let mut streaming = assembler(first.state, 1);
        streaming.apply(text("still talking")).unwrap();
        let segment = streaming.changes.pop().unwrap().row;

        let spawned = AgentUpdate::AsyncTaskSpawned {
            async_task_id: "task".into(),
            provider_tool_call_id: Some("tool".into()),
            name: None,
            task_type: None,
            description: None,
            output_file_path: Some("/tmp/task.output".into()),
            show_in_transcript: None,
            can_stop: None,
        };
        let mut lifecycle = assembler(
            AssemblyState {
                latest: Some(segment.clone()),
                tool: Some(tool.clone()),
                plan: None,
            },
            2,
        );
        lifecycle.apply(spawned).unwrap();
        // One change: the tool. The open text segment is not closed.
        assert_eq!(lifecycle.changes.len(), 1);
        let latest = lifecycle.state.latest.as_ref().unwrap();
        assert_eq!(
            (
                latest.id.as_str(),
                latest.revision,
                latest.completion.as_str()
            ),
            (segment.id.as_str(), segment.revision, "open")
        );
        let revised = lifecycle.changes.pop().unwrap();
        assert_eq!(revised.row.id, tool.row.id);
        assert_eq!(revised.row.revision, tool.row.revision + 1);
        assert_eq!(revised.row.completion, "open");
        assert_eq!(revised.row.body_byte_count, tool.row.body_byte_count);
        assert!(revised.body.is_none() && !revised.replace);
        let TranscriptDetail::Tool { json, .. } = revised.detail else {
            panic!("tool detail");
        };
        let revised: TranscriptTool = decode(&json).unwrap();
        assert_eq!(revised.status.as_deref(), Some("in_progress"));
        assert_eq!(
            revised.background_task,
            Some(TranscriptBackgroundTask {
                id: Some("task".into()),
                state: AsyncTaskState::Running,
                output_file_path: Some("/tmp/task.output".into()),
            })
        );

        // No tool for the update: nothing, not even a closed segment.
        let mut orphan = assembler(
            AssemblyState {
                latest: Some(segment),
                tool: None,
                plan: None,
            },
            3,
        );
        orphan
            .apply(AgentUpdate::AsyncTaskStateUpdate {
                async_task_id: "elsewhere".into(),
                state: AsyncTaskState::Completed,
                provider_tool_call_id: None,
                summary: None,
                output_file_path: None,
            })
            .unwrap();
        assert!(orphan.changes.is_empty());
    }
}
