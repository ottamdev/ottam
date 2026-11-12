//! What the history tools read: a conversation's transcript as pages of
//! short entries, and one tool call's input and output. See
//! `docs/architecture/backend/mcp.md` § History Tools.
//!
//! Every read is bounded, so an agent asking about a long conversation
//! gets a page and a way to the next one, never the whole history.

use std::collections::HashMap;

use super::ChatService;
use crate::core_error::CoreError;
use ottam_protocol::{
    is_delegation_ack, ConversationHistoryReplay, ConversationID, ConversationListTranscriptParams,
    ConversationMessageID, ConversationReadTranscriptBodyParams,
    ConversationReadTranscriptPayloadParams, ConversationRole, ConversationTranscriptItem,
    DelegationNoteKind, MessageSource, TranscriptItemKind,
};

/// Entries a page holds when the caller asks for no number.
pub(in crate::server) const HISTORY_DEFAULT_ENTRIES: usize = 20;
/// The most entries one page holds.
pub(in crate::server) const HISTORY_MAX_ENTRIES: usize = 50;
/// The most of one entry's text a page shows.
const ENTRY_CHARS: usize = 2_000;
/// The most of one entry's text a one-entry page shows, which is how a cut
/// entry is read whole.
pub(in crate::server) const SINGLE_ENTRY_CHARS: usize = 64_000;
/// The most of a tool call's input an entry shows.
const TOOL_INPUT_CHARS: usize = 300;
/// The most of a tool call's input `tool_output` shows.
const TOOL_INPUT_FULL_CHARS: usize = 4_000;
/// The most output one `tool_output` page reads.
pub(in crate::server) const OUTPUT_PAGE_BYTES: u64 = 16_000;
/// Transcript records read at a time.
const RECORDS_PER_READ: u32 = 200;
/// The most records a page or a tool call's search looks through.
const RECORDS_SCANNED: usize = 20_000;

/// The most of earlier turns a replay carries, in characters.
const REPLAY_CHARS: usize = 96_000;

/// How a history entry is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryStyle {
    /// A history tool's page: a tool call shows its input.
    Page,
    /// Earlier turns a prompt replays: a tool call is one line.
    Replay,
}

/// Earlier turns as a prompt carries them, and what they hold.
pub(in crate::server) struct HistoryReplayText {
    pub text: String,
    pub replay: ConversationHistoryReplay,
}

/// One part of a tool call's saved text, as `tool_output` pages it.
struct SavedPart {
    payload_id: String,
    revision: u64,
    index: u32,
    byte_count: u64,
    /// The line it opens with, when a page starts it.
    heading: Option<String>,
}

/// Text cut to `chars`, saying how much was left out.
fn bounded(text: &str, chars: usize) -> String {
    let text = text.trim();
    let total = text.chars().count();
    if total <= chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(chars).collect();
    format!("{cut}… ({} more characters)", total - chars)
}

/// An entry's text cut to `chars`; a cut on a many-entry page names the
/// one-entry read that shows the rest.
fn entry_body(text: &str, chars: usize, position: u64) -> String {
    let cut = bounded(text, chars);
    if chars >= SINGLE_ENTRY_CHARS || text.trim().chars().count() <= chars {
        return cut;
    }
    format!(
        "{cut}\n(The rest: get_session_messages with before={} and limit=1.)",
        position + 1
    )
}

/// Where `bytes` would end without a UTF-8 character its end splits.
fn complete_chars(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for back in 1..=len.min(4) {
        let byte = bytes[len - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let width = match byte {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        return if width > back { len - back } else { len };
    }
    len
}

/// What a saved payload's unavailable reason means.
fn unsaved_reason(reason: &str) -> &str {
    match reason {
        "notCaptured" => "the agent did not send it to Ottam",
        "unfinishedOutput" => "the call had not finished when its run ended",
        "captureLimit" => "it was larger than Ottam keeps",
        "queueFull" | "writeFailed" => "Ottam could not store it in time",
        other => other,
    }
}

/// One line of a tool call's input.
fn one_line(text: &str, chars: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    bounded(&joined, chars)
}

impl ChatService {
    /// The engine each of `conversation`'s runs went to, by run id.
    async fn run_agents(
        &self,
        conversation: &ConversationID,
    ) -> Result<HashMap<String, String>, CoreError> {
        let mut agents = HashMap::new();
        let mut cursor = None;
        loop {
            let page = self.list_runs(conversation, None, cursor).await?;
            if page.runs.is_empty() {
                break;
            }
            for run in page.runs {
                agents.insert(run.run_id.0, run.engine_id.0);
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(agents)
    }

    /// A page of `conversation`'s history, newest first in the reading
    /// and oldest first in the text, ending with how to read further back.
    /// `before` is a position a previous page named.
    pub(in crate::server) async fn history_page(
        &self,
        conversation: &ConversationID,
        before: Option<u64>,
        limit: usize,
    ) -> Result<String, CoreError> {
        let limit = limit.clamp(1, HISTORY_MAX_ENTRIES);
        let chars = if limit == 1 {
            SINGLE_ENTRY_CHARS
        } else {
            ENTRY_CHARS
        };
        let agents = self.run_agents(conversation).await?;
        let mut entries: Vec<(u64, String)> = Vec::new();
        let mut cursor = before.map(|position| position.to_string());
        let mut scanned = 0;
        let mut more = false;
        'pages: loop {
            let page = self
                .list_transcript(ConversationListTranscriptParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    limit: Some(RECORDS_PER_READ),
                    cursor: cursor.clone(),
                    subagent_thread_id: None,
                })
                .await?;
            for item in &page.items {
                scanned += 1;
                if before.is_some_and(|before| item.position >= before) {
                    continue;
                }
                let Some(entry) = self
                    .history_entry(conversation, item, chars, EntryStyle::Page, &agents)
                    .await
                else {
                    continue;
                };
                if entries.len() == limit {
                    more = true;
                    break 'pages;
                }
                entries.push((item.position, entry));
            }
            match page.next_cursor {
                Some(next) if scanned < RECORDS_SCANNED && !page.items.is_empty() => {
                    cursor = Some(next);
                }
                Some(_) => {
                    more = true;
                    break;
                }
                None => break,
            }
        }
        if entries.is_empty() {
            return Ok(if before.is_some() {
                "There is nothing older in this conversation.".to_owned()
            } else {
                "This conversation has no messages yet.".to_owned()
            });
        }
        let oldest = entries.last().map(|(position, _)| *position).unwrap_or(0);
        let mut text = entries
            .into_iter()
            .rev()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>()
            .join("\n\n");
        text.push_str("\n\n");
        text.push_str(&if more {
            format!(
                "Older entries: call get_session_messages again with before={oldest}. \
                 A tool call's full output: get_session_toolcall_output with its call id."
            )
        } else {
            "This is the start of the conversation. A tool call's full output: \
             get_session_toolcall_output with its call id."
                .to_owned()
        });
        Ok(text)
    }

    /// The conversation's turns before the message `input`, for a session
    /// that never saw them: messages whole, tool calls as one line with no
    /// input or output, the newest kept within `REPLAY_CHARS`. `None` when
    /// nothing came before it; an error when the input isn't found.
    pub(in crate::server) async fn history_replay(
        &self,
        conversation: &ConversationID,
        input: &ConversationMessageID,
    ) -> Result<Option<HistoryReplayText>, CoreError> {
        let agents = self.run_agents(conversation).await?;
        let mut entries: Vec<(u64, String)> = Vec::new();
        let mut used = 0;
        let mut found_input = false;
        let mut truncated = false;
        let mut cursor = None;
        let mut scanned = 0;
        'pages: loop {
            let page = self
                .list_transcript(ConversationListTranscriptParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    limit: Some(RECORDS_PER_READ),
                    cursor: cursor.clone(),
                    subagent_thread_id: None,
                })
                .await?;
            for item in &page.items {
                scanned += 1;
                // Newest first: the input, and anything after it, is this
                // turn or a message still waiting its turn.
                if !found_input {
                    found_input = item
                        .message
                        .as_ref()
                        .is_some_and(|message| &message.message_id == input);
                    continue;
                }
                let Some(entry) = self
                    .history_entry(
                        conversation,
                        item,
                        SINGLE_ENTRY_CHARS,
                        EntryStyle::Replay,
                        &agents,
                    )
                    .await
                else {
                    continue;
                };
                // The newest entry always goes, cut at its own bound, so a
                // replay is never empty while there is something to send.
                if !entries.is_empty() && used + entry.len() > REPLAY_CHARS {
                    truncated = true;
                    break 'pages;
                }
                used += entry.len() + 2;
                entries.push((item.position, entry));
            }
            match page.next_cursor {
                Some(next) if scanned < RECORDS_SCANNED && !page.items.is_empty() => {
                    cursor = Some(next);
                }
                Some(_) => {
                    truncated = found_input;
                    break;
                }
                None => break,
            }
        }
        // Without the input there is no telling which turns came before it.
        if !found_input {
            return Err(CoreError::new(
                "messageNotFound",
                format!("message {} is not in the transcript", input.0),
            ));
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let oldest = entries.last().map(|(position, _)| *position).unwrap_or(0);
        let count = entries.len();
        let mut text = String::new();
        if truncated {
            text.push_str(&format!(
                "(Older entries are left out: get_session_messages with before={oldest} \
                 reads them.)\n\n"
            ));
        }
        text.push_str(
            &entries
                .into_iter()
                .rev()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        Ok(Some(HistoryReplayText {
            text,
            replay: ConversationHistoryReplay {
                entries: u32::try_from(count).unwrap_or(u32::MAX),
                truncated,
            },
        }))
    }

    /// One transcript record as a history entry; `None` for records the
    /// history leaves out: reasoning, plans, run boundaries, a subagent's
    /// own steps, and the prompts Ottam writes to deliver notes. Text is
    /// cut at `chars`.
    async fn history_entry(
        &self,
        conversation: &ConversationID,
        item: &ConversationTranscriptItem,
        chars: usize,
        style: EntryStyle,
        agents: &HashMap<String, String>,
    ) -> Option<String> {
        if item.subagent_thread_id.is_some() {
            return None;
        }
        let at = &item.created_at;
        match item.kind {
            TranscriptItemKind::Message => {
                let message = item.message.as_ref()?;
                let who = match (&message.role, &message.source, &message.note) {
                    (_, MessageSource::NoteDelivery, _) => return None,
                    (ConversationRole::System, _, Some(note)) => match note.kind {
                        DelegationNoteKind::Report => {
                            format!(
                                "report from {} ({})",
                                note.child_name, note.child_conversation_id.0
                            )
                        }
                        DelegationNoteKind::ChildMessage => format!(
                            "message from {} ({})",
                            note.child_name, note.child_conversation_id.0
                        ),
                        DelegationNoteKind::ParentMessage => {
                            format!("message from {}, the delegating agent", note.child_name)
                        }
                    },
                    (ConversationRole::System, _, None) => "Ottam".to_owned(),
                    (ConversationRole::User, MessageSource::Delegation, _) => {
                        "task from the delegating agent".to_owned()
                    }
                    (ConversationRole::User, _, _) => "user".to_owned(),
                };
                // A listed message carries only its preview.
                let body = if item.body_byte_count as usize > item.body_preview.len() {
                    self.item_text(conversation, item, chars).await
                } else {
                    message.body.clone()
                };
                Some(format!(
                    "[{at}] {who}:\n{}",
                    entry_body(&body, chars, item.position)
                ))
            }
            TranscriptItemKind::AssistantText => {
                let text = self.item_text(conversation, item, chars).await;
                // A conversation can switch agents, so say which one spoke.
                let who = match item.run_id.as_ref().and_then(|run| agents.get(&run.0)) {
                    Some(agent) => format!("agent ({agent})"),
                    None => "agent".to_owned(),
                };
                // A parent's acknowledgement of notes says nothing.
                (!text.trim().is_empty() && !is_delegation_ack(&text))
                    .then(|| format!("[{at}] {who}:\n{}", entry_body(&text, chars, item.position)))
            }
            TranscriptItemKind::Tool => {
                let tool = item.tool.as_ref()?;
                let name = tool
                    .title
                    .as_deref()
                    .or(tool.name.as_deref())
                    .unwrap_or("Tool");
                let status = tool.status.as_deref().unwrap_or("unknown");
                if style == EntryStyle::Replay {
                    // Ottam's own id: an earlier agent's call ids can
                    // collide with the current agent's.
                    let mut line = format!("[{at}] tool call {}: {name} (", item.item_id);
                    if let Some(kind) = &tool.kind {
                        line.push_str(&format!("{kind}, "));
                    }
                    line.push_str(&format!("{status})"));
                    if let Some(code) = tool.exit_code {
                        line.push_str(&format!(", exit code {code}"));
                    }
                    return Some(line);
                }
                let id = tool
                    .provider_tool_call_id
                    .clone()
                    .unwrap_or_else(|| item.item_id.clone());
                let input = self.tool_input(conversation, item, TOOL_INPUT_CHARS).await;
                let mut line = format!("[{at}] tool call {id}: {name} ({status})");
                if !input.is_empty() {
                    line.push_str(&format!("\ninput: {}", one_line(&input, TOOL_INPUT_CHARS)));
                }
                if let Some(code) = tool.exit_code {
                    line.push_str(&format!("\nexit code: {code}"));
                }
                Some(line)
            }
            TranscriptItemKind::Gap => Some(format!("[{at}] (part of the history was not saved)")),
            _ => None,
        }
    }

    /// An item's body, read until `chars` characters past the preview.
    async fn item_text(
        &self,
        conversation: &ConversationID,
        item: &ConversationTranscriptItem,
        chars: usize,
    ) -> String {
        if item.body_byte_count as usize <= item.body_preview.len() {
            return item.body_preview.clone();
        }
        let mut text = String::new();
        let mut offset = 0;
        loop {
            let Ok(read) = self
                .read_transcript_body(ConversationReadTranscriptBodyParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    item_id: item.item_id.clone(),
                    revision: item.revision,
                    offset,
                })
                .await
            else {
                return if text.is_empty() {
                    item.body_preview.clone()
                } else {
                    text
                };
            };
            text.push_str(&read.text);
            match read.next_offset {
                // Past the bound, so the cut can say more was left out.
                Some(next) if text.chars().count() <= chars && next > offset => offset = next,
                _ => return text,
            }
        }
    }

    /// A tool call's input: the item's body, where Claude puts it, or its
    /// arguments, where the other agents do.
    async fn tool_input(
        &self,
        conversation: &ConversationID,
        item: &ConversationTranscriptItem,
        chars: usize,
    ) -> String {
        let body = self.item_text(conversation, item, chars).await;
        if !body.trim().is_empty() && body.trim() != "null" {
            return body;
        }
        match item.tool.as_ref().map(|tool| &tool.arguments) {
            Some(serde_json::Value::Null) | None => String::new(),
            Some(arguments) => arguments.to_string(),
        }
    }

    /// One tool call's input and a page of its saved text: a file change's
    /// contents, then its output, from `offset` bytes in. `tool_call_id` is
    /// the agent's call id, as the history names it, or the transcript
    /// item's id.
    pub(in crate::server) async fn tool_output(
        &self,
        conversation: &ConversationID,
        tool_call_id: &str,
        offset: u64,
    ) -> Result<String, CoreError> {
        let item = self
            .find_tool_call(conversation, tool_call_id)
            .await?
            .ok_or_else(|| {
                CoreError::new(
                    "toolCallNotFound",
                    format!(
                        "No tool call {tool_call_id} in this conversation. Use the call id \
                     get_session_messages shows."
                    ),
                )
            })?;
        let tool = item.tool.as_ref().ok_or_else(|| {
            CoreError::new(
                "toolCallNotFound",
                format!("{tool_call_id} is not a tool call"),
            )
        })?;
        let name = tool
            .title
            .as_deref()
            .or(tool.name.as_deref())
            .unwrap_or("Tool");
        let mut text = format!(
            "Tool call {tool_call_id}: {name} ({})",
            tool.status.as_deref().unwrap_or("unknown")
        );
        if let Some(code) = tool.exit_code {
            text.push_str(&format!(", exit code {code}"));
        }
        if offset == 0 {
            let input = self
                .tool_input(conversation, &item, TOOL_INPUT_FULL_CHARS)
                .await;
            if !input.is_empty() {
                text.push_str(&format!(
                    "\n\nInput:\n{}",
                    bounded(&input, TOOL_INPUT_FULL_CHARS)
                ));
            }
        }
        // A file change's saved contents, then the output: each payload's
        // parts read in turn as one text, `offset` bytes in.
        let mut parts: Vec<SavedPart> = Vec::new();
        let mut unsaved = Vec::new();
        let output = tool
            .payloads
            .iter()
            .find(|payload| payload.role == "output");
        let change = tool
            .payloads
            .iter()
            .find(|payload| payload.role == "fileChangeInput");
        for (payload, is_change) in [(change, true), (output, false)] {
            let Some(payload) = payload else { continue };
            let Some(payload_id) = payload.payload_id.clone() else {
                unsaved.push(format!(
                    "{} was not saved: {}.",
                    if is_change {
                        "Its file change"
                    } else {
                        "Its output"
                    },
                    unsaved_reason(payload.reason.as_deref().unwrap_or(&payload.availability))
                ));
                continue;
            };
            for (index, part) in payload.parts.iter().enumerate() {
                let heading = if is_change {
                    Some(match &part.path {
                        Some(path) => format!("{} for {path}", part.name),
                        None => part.name.clone(),
                    })
                } else {
                    // Output needs a heading only after a change's parts.
                    (index == 0 && change.is_some()).then(|| "Output".to_owned())
                };
                parts.push(SavedPart {
                    payload_id: payload_id.clone(),
                    revision: payload.revision,
                    index: index as u32,
                    byte_count: part.byte_count,
                    heading,
                });
            }
        }
        if output.is_none() {
            unsaved.push("The call has no saved output.".to_owned());
        }
        let total: u64 = parts.iter().map(|part| part.byte_count).sum();
        if total > 0 {
            let mut page = String::new();
            let mut read = 0;
            let mut start = 0;
            for part in &parts {
                let end = start + part.byte_count;
                if end > offset && read < OUTPUT_PAGE_BYTES {
                    let from = offset.saturating_sub(start);
                    let mut bytes = self
                        .read_saved_part(conversation, part, from, OUTPUT_PAGE_BYTES - read)
                        .await?;
                    let stopped_inside = from + (bytes.len() as u64) < part.byte_count;
                    // A page ends on a character, so the next one starts on one too.
                    if stopped_inside {
                        let end = complete_chars(&bytes);
                        if end > 0 {
                            bytes.truncate(end);
                        }
                    }
                    if let (0, Some(heading)) = (from, &part.heading) {
                        page.push_str(&format!("\n--- {heading} ---\n"));
                    }
                    read += bytes.len() as u64;
                    page.push_str(&String::from_utf8_lossy(&bytes));
                    if stopped_inside {
                        break;
                    }
                }
                start = end;
            }
            let read_to = offset + read;
            let label = if change.is_some() {
                "Changes and output"
            } else {
                "Output"
            };
            text.push_str(&format!(
                "\n\n{label}, bytes {offset}–{read_to} of {total}:\n{}",
                page.trim_start_matches('\n')
            ));
            if output.is_some_and(|output| output.upstream_truncated == Some(true)) {
                text.push_str("\n(The agent cut this output before Ottam saved it.)");
            }
            if read_to < total {
                text.push_str(&format!(
                    "\n\nMore: call get_session_toolcall_output again with offset={read_to}."
                ));
            }
        }
        for note in unsaved {
            text.push_str("\n\n");
            text.push_str(&note);
        }
        Ok(text)
    }

    /// Up to `budget` bytes of one saved part, `from` bytes in.
    async fn read_saved_part(
        &self,
        conversation: &ConversationID,
        part: &SavedPart,
        from: u64,
        budget: u64,
    ) -> Result<Vec<u8>, CoreError> {
        use base64::Engine;
        let mut bytes = Vec::new();
        let mut at = from;
        while at < part.byte_count && (bytes.len() as u64) < budget {
            let want = (budget - bytes.len() as u64).min(u32::MAX as u64);
            let read = self
                .read_transcript_payload(ConversationReadTranscriptPayloadParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    payload_id: part.payload_id.clone(),
                    revision: part.revision,
                    part: part.index,
                    offset: at,
                    length: Some(want as u32),
                })
                .await?;
            let data = base64::engine::general_purpose::STANDARD
                .decode(read.data.as_bytes())
                .unwrap_or_default();
            if data.is_empty() {
                break;
            }
            at += data.len() as u64;
            bytes.extend(data);
        }
        Ok(bytes)
    }

    async fn find_tool_call(
        &self,
        conversation: &ConversationID,
        tool_call_id: &str,
    ) -> Result<Option<ConversationTranscriptItem>, CoreError> {
        let mut cursor = None;
        let mut scanned = 0;
        loop {
            let page = self
                .list_transcript(ConversationListTranscriptParams {
                    workspace_id: self.workspace_id.clone(),
                    conversation_id: conversation.clone(),
                    limit: Some(RECORDS_PER_READ),
                    cursor,
                    subagent_thread_id: None,
                })
                .await?;
            scanned += page.items.len();
            if let Some(item) = page.items.into_iter().find(|item| {
                item.kind == TranscriptItemKind::Tool
                    && (item.item_id == tool_call_id
                        || item
                            .tool
                            .as_ref()
                            .and_then(|tool| tool.provider_tool_call_id.as_deref())
                            == Some(tool_call_id))
            }) {
                return Ok(Some(item));
            }
            match page.next_cursor {
                Some(next) if scanned < RECORDS_SCANNED => cursor = Some(next),
                _ => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_never_ends_inside_a_character() {
        let text = "ab—é😀".as_bytes();
        for cut in 0..=text.len() {
            let end = complete_chars(&text[..cut]);
            assert!(std::str::from_utf8(&text[..end]).is_ok(), "cut at {cut}");
            assert!(cut - end < 4, "drops only the split character");
        }
        assert_eq!(complete_chars(text), text.len());
    }

    #[test]
    fn a_cut_entry_names_the_read_that_shows_the_rest() {
        let long = "x".repeat(ENTRY_CHARS + 5);
        let cut = entry_body(&long, ENTRY_CHARS, 41);
        assert!(cut.contains("(5 more characters)"), "{cut}");
        assert!(cut.ends_with("before=42 and limit=1.)"), "{cut}");
        let whole = entry_body(&long, SINGLE_ENTRY_CHARS, 41);
        assert_eq!(whole, long);
        assert_eq!(entry_body("short", ENTRY_CHARS, 41), "short");
    }

    #[test]
    fn unsaved_output_says_why_in_words() {
        assert_eq!(
            unsaved_reason("notCaptured"),
            "the agent did not send it to Ottam"
        );
        assert_eq!(unsaved_reason("somethingNew"), "somethingNew");
    }
}
