//! Normalized transcript persistence. Callers own assembly; this repository owns
//! atomic revisions and bounded body I/O, without depending on protocol types.
use crate::storage::error::StorageError;
use crate::storage::schema::{
    transcript_body_chunks as bodies, transcript_cursors as cursors, transcript_gaps as gaps,
    transcript_items as items, transcript_permissions as permissions, transcript_plans as plans,
    transcript_tools as tools,
};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::{HashMap, HashSet};

pub const BODY_PAGE_BYTES: usize = 64 * 1024;
pub const PREVIEW_BYTES: usize = 4096;
/// Most records one transcript page reads. The server's default and maximum
/// page size must match it, or live items fill pages past the stored cursor.
pub const PAGE_RECORDS: usize = 1000;

#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = items)]
pub struct TranscriptRow {
    pub id: String,
    pub conversation_id: String,
    pub run_id: Option<String>,
    pub position: i64,
    pub revision: i64,
    pub kind: String,
    pub completion: String,
    pub provider_message_id: Option<String>,
    pub adapter_metadata_json: String,
    pub body_byte_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub enum TranscriptDetail {
    None,
    Tool {
        provider_id: Option<String>,
        json: String,
    },
    Plan(String),
    Permission {
        request_id: String,
        json: String,
    },
    Gap {
        end_position: i64,
        dropped: i64,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct TranscriptRecord {
    pub committed_revision: i64,
    pub row: TranscriptRow,
    pub detail: TranscriptDetail,
    pub preview: String,
    pub run_status: Option<String>,
}

pub struct TranscriptRepository;

pub fn prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The whole characters of a byte prefix cut from valid UTF-8 text.
fn utf8_prefix(mut bytes: Vec<u8>) -> String {
    if let Err(error) = std::str::from_utf8(&bytes) {
        bytes.truncate(error.valid_up_to());
    }
    String::from_utf8(bytes).unwrap_or_default()
}

impl TranscriptRepository {
    /// Called only at the ordered Conversation boundary. One shared spine means
    /// admission, tools, and text cannot reserve or renumber each other's places.
    pub fn next_position(
        connection: &mut SqliteConnection,
        conversation: &str,
    ) -> Result<i64, StorageError> {
        let last: Option<i64> = items::table
            .filter(items::conversation_id.eq(conversation))
            .select(diesel::dsl::max(items::position))
            .first(connection)?;
        let last_gap: Option<i64> = gaps::table
            .inner_join(items::table)
            .filter(items::conversation_id.eq(conversation))
            .select(diesel::dsl::max(gaps::end_position))
            .first(connection)?;
        // SQLite json_each is needed because capture loss can extend beyond
        // every retained item. Restart must never reuse those discarded places.
        #[derive(QueryableByName)]
        struct CaptureEnd {
            #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
            value: Option<i64>,
        }
        let capture_end = diesel::sql_query("SELECT MAX(CAST(json_extract(g.value, '$.endPosition') AS INTEGER)) AS value FROM transcript_capture c JOIN conversation_runs r ON r.id = c.run_id JOIN json_each(c.gaps_json) g WHERE r.conversation_id = ? AND c.gaps_json != '[]'")
            .bind::<diesel::sql_types::Text, _>(conversation)
            .get_result::<CaptureEnd>(connection)?.value;
        last.max(last_gap)
            .max(capture_end)
            .unwrap_or(-1)
            .checked_add(1)
            .ok_or_else(|| StorageError::new("storageConstraint", "Transcript positions exhausted"))
    }

    pub fn admit_message(
        connection: &mut SqliteConnection,
        conversation: &str,
        id: &str,
        position: i64,
        byte_count: i64,
        now: i64,
    ) -> Result<(), StorageError> {
        // The live coordinator owns segment revisions. Admission commits only
        // the Message spine row; a checkpoint cannot race a second revision owner.
        let row = TranscriptRow {
            id: id.to_owned(),
            conversation_id: conversation.to_owned(),
            run_id: None,
            position,
            revision: 1,
            kind: "message".into(),
            completion: "complete".into(),
            provider_message_id: None,
            adapter_metadata_json: "{}".into(),
            body_byte_count: byte_count,
            created_at: now,
            updated_at: now,
        };
        diesel::insert_into(items::table)
            .values(&row)
            .execute(connection)?;
        Self::advance_sequence(connection, conversation)?;
        Ok(())
    }

    pub fn sequence(
        connection: &mut SqliteConnection,
        conversation: &str,
    ) -> Result<i64, StorageError> {
        Ok(cursors::table
            .find(conversation)
            .select(cursors::sequence)
            .first::<i64>(connection)
            .optional()?
            .unwrap_or(0))
    }

    pub fn advance_sequence(
        connection: &mut SqliteConnection,
        conversation: &str,
    ) -> Result<i64, StorageError> {
        diesel::insert_into(cursors::table)
            .values((
                cursors::conversation_id.eq(conversation),
                cursors::sequence.eq(1),
            ))
            .on_conflict(cursors::conversation_id)
            .do_update()
            .set(cursors::sequence.eq(cursors::sequence + 1))
            .execute(connection)?;
        Self::sequence(connection, conversation)
    }

    pub fn get(
        connection: &mut SqliteConnection,
        conversation: &str,
        id: &str,
    ) -> Result<TranscriptRecord, StorageError> {
        let row = items::table
            .find(id)
            .filter(items::conversation_id.eq(conversation))
            .select(TranscriptRow::as_select())
            .first::<TranscriptRow>(connection)
            .optional()?
            .ok_or_else(|| StorageError::not_found("TranscriptItem"))?;
        Self::record(connection, row)
    }

    /// A run's tool item by its provider tool call id; the first, as the live
    /// lookup takes it. `transcript_tools` has no index on the call id, so
    /// the run's own items bound the scan.
    pub fn run_tool(
        connection: &mut SqliteConnection,
        conversation: &str,
        run: &str,
        provider_tool_call_id: &str,
    ) -> Result<Option<TranscriptRecord>, StorageError> {
        let row = items::table
            .inner_join(tools::table.on(tools::item_id.eq(items::id)))
            .filter(items::conversation_id.eq(conversation))
            .filter(items::run_id.eq(run))
            .filter(items::kind.eq("tool"))
            .filter(tools::provider_tool_call_id.eq(provider_tool_call_id))
            .order(items::position.asc())
            .select(TranscriptRow::as_select())
            .first::<TranscriptRow>(connection)
            .optional()?;
        row.map(|row| Self::record(connection, row)).transpose()
    }

    pub fn latest(
        connection: &mut SqliteConnection,
        conversation: &str,
    ) -> Result<Option<TranscriptRow>, StorageError> {
        Ok(items::table
            .filter(items::conversation_id.eq(conversation))
            .order(items::position.desc())
            .select(TranscriptRow::as_select())
            .first(connection)
            .optional()?)
    }

    pub fn finish_run(
        connection: &mut SqliteConnection,
        run: &str,
        completion: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let conversations = items::table
            .filter(items::run_id.eq(run))
            .filter(items::completion.eq("open"))
            .select(items::conversation_id)
            .distinct()
            .load::<String>(connection)?;
        diesel::update(
            items::table
                .filter(items::run_id.eq(run))
                .filter(items::completion.eq("open")),
        )
        .set((
            items::completion.eq(completion),
            items::revision.eq(items::revision + 1),
            items::updated_at.eq(now),
        ))
        .execute(connection)?;
        // A question still pending when its run ends was never answered. The
        // live path settles it in memory; this is the run an earlier core left.
        diesel::sql_query(
            "UPDATE transcript_items SET revision = revision + 1, updated_at = ?1 \
             WHERE run_id = ?2 AND id IN (SELECT item_id FROM transcript_tools \
             WHERE json_extract(metadata_json, '$.question.state') = 'pending')",
        )
        .bind::<diesel::sql_types::BigInt, _>(now)
        .bind::<diesel::sql_types::Text, _>(run)
        .execute(connection)?;
        diesel::sql_query(
            "UPDATE transcript_tools \
             SET metadata_json = json_set(metadata_json, '$.question.state', 'notAnswered') \
             WHERE json_extract(metadata_json, '$.question.state') = 'pending' \
             AND item_id IN (SELECT id FROM transcript_items WHERE run_id = ?1)",
        )
        .bind::<diesel::sql_types::Text, _>(run)
        .execute(connection)?;
        // A durable boundary represents even an empty failed/cancelled Run.
        // It is allocated in the same ordered writer transaction as finalization.
        use crate::storage::schema::conversation_runs as runs;
        let conversation = runs::table
            .find(run)
            .select(runs::conversation_id)
            .first::<String>(connection)?;
        let boundary_exists = items::table
            .filter(items::run_id.eq(run))
            .filter(items::kind.eq("runBoundary"))
            .count()
            .get_result::<i64>(connection)?
            > 0;
        if !boundary_exists {
            let row = TranscriptRow {
                id: uuid::Uuid::now_v7().to_string(),
                conversation_id: conversation.clone(),
                run_id: Some(run.into()),
                position: Self::next_position(connection, &conversation)?,
                revision: 1,
                kind: "runBoundary".into(),
                completion: completion.into(),
                provider_message_id: None,
                adapter_metadata_json: "{}".into(),
                body_byte_count: 0,
                created_at: now,
                updated_at: now,
            };
            Self::save(connection, &row, &TranscriptDetail::None, "")?;
        } else {
            // The new boundary's save already advances the sequence for the
            // whole finalization batch. Only an existing boundary needs a
            // separate advance when this call settles additional open items.
            for conversation in conversations {
                Self::advance_sequence(connection, &conversation)?;
            }
        }
        Ok(())
    }

    /// Keep identity/position so clients can revise already-published rows, but
    /// erase adapter narration when a typed failure proves the agent never ran.
    pub fn suppress_pre_agent_output(
        connection: &mut SqliteConnection,
        run: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let rows = items::table
            .filter(items::run_id.eq(run))
            .filter(items::kind.eq_any(["assistantText", "reasoning"]))
            .select(TranscriptRow::as_select())
            .load::<TranscriptRow>(connection)?;
        for mut row in rows {
            row.revision += 1;
            row.completion = "suppressed".into();
            row.body_byte_count = 0;
            row.updated_at = now;
            Self::save_replacing_body(connection, &row, &TranscriptDetail::None, Some(""))?;
        }
        Ok(())
    }

    /// The surrounding writer transaction commits the header, metadata, body
    /// chunks, and sequence together. A repeated/stale revision is a no-op.
    pub fn save(
        connection: &mut SqliteConnection,
        row: &TranscriptRow,
        detail: &TranscriptDetail,
        append: &str,
    ) -> Result<bool, StorageError> {
        Self::save_body(connection, row, detail, append, false, false)
    }

    pub fn save_replacing_body(
        connection: &mut SqliteConnection,
        row: &TranscriptRow,
        detail: &TranscriptDetail,
        body: Option<&str>,
    ) -> Result<bool, StorageError> {
        Self::save_body(
            connection,
            row,
            detail,
            body.unwrap_or(""),
            body.is_some(),
            false,
        )
    }

    /// Publish an already-staged body. Caller wraps this and capture metadata in
    /// a transaction; readers continue to see the previous head until commit.
    pub fn publish_checkpoint(
        connection: &mut SqliteConnection,
        row: &TranscriptRow,
        detail: &TranscriptDetail,
        body_revision: i64,
    ) -> Result<bool, StorageError> {
        use crate::storage::schema::transcript_checkpoint_heads as heads;
        // Never expose a header whose staged body is incomplete. Length is
        // measured in bytes, including multibyte UTF-8 text.
        #[derive(QueryableByName)]
        struct StagedLength {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            bytes: i64,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            end: i64,
        }
        let staged = diesel::sql_query("SELECT COALESCE(SUM(length(CAST(body AS BLOB))), 0) AS bytes, COALESCE(MAX(byte_offset + length(CAST(body AS BLOB))), 0) AS end FROM transcript_checkpoint_bodies WHERE item_id = ? AND revision = ? AND byte_offset < ?")
            .bind::<diesel::sql_types::Text, _>(&row.id)
            .bind::<diesel::sql_types::BigInt, _>(body_revision)
            .bind::<diesel::sql_types::BigInt, _>(row.body_byte_count)
            .get_result::<StagedLength>(connection)?;
        if staged.bytes != row.body_byte_count || staged.end != row.body_byte_count {
            return Err(StorageError::new(
                "storageConstraint",
                "Checkpoint body is not fully staged",
            ));
        }
        if !Self::save_body(connection, row, detail, "", false, true)? {
            return Ok(false);
        }
        diesel::insert_into(heads::table)
            .values((
                heads::item_id.eq(&row.id),
                heads::revision.eq(body_revision),
            ))
            .on_conflict(heads::item_id)
            .do_update()
            .set(heads::revision.eq(body_revision))
            .execute(connection)?;
        Ok(true)
    }

    fn save_body(
        connection: &mut SqliteConnection,
        row: &TranscriptRow,
        detail: &TranscriptDetail,
        append: &str,
        replace: bool,
        checkpoint: bool,
    ) -> Result<bool, StorageError> {
        let existing = items::table
            .find(&row.id)
            .select((items::revision, items::body_byte_count))
            .first::<(i64, i64)>(connection)
            .optional()?;
        if existing.is_some_and(|(revision, _)| revision >= row.revision) {
            return Ok(false);
        }
        let offset = if replace {
            0
        } else {
            existing.map_or(0, |(_, count)| count)
        };
        if !checkpoint && row.body_byte_count != offset + append.len() as i64 {
            return Err(StorageError::new(
                "storageConstraint",
                "Transcript body revision does not match its append",
            ));
        }
        if let Some((_, _)) = existing {
            let changed = diesel::update(
                items::table
                    .find(&row.id)
                    .filter(items::conversation_id.eq(&row.conversation_id))
                    .filter(items::position.eq(row.position))
                    .filter(items::kind.eq(&row.kind)),
            )
            .set((
                items::revision.eq(row.revision),
                items::completion.eq(&row.completion),
                items::adapter_metadata_json.eq(&row.adapter_metadata_json),
                items::body_byte_count.eq(row.body_byte_count),
                items::updated_at.eq(row.updated_at),
            ))
            .execute(connection)?;
            if changed != 1 {
                return Err(StorageError::new(
                    "storageConstraint",
                    "Transcript identity is immutable",
                ));
            }
        } else {
            diesel::insert_into(items::table)
                .values(row)
                .execute(connection)?;
        }
        if replace {
            use crate::storage::schema::transcript_checkpoint_heads as heads;
            diesel::delete(heads::table.filter(heads::item_id.eq(&row.id))).execute(connection)?;
            diesel::delete(bodies::table.filter(bodies::item_id.eq(&row.id)))
                .execute(connection)?;
        }
        let mut remaining = append;
        let mut offset = offset;
        if !remaining.is_empty() {
            if let Some((start, mut tail)) = bodies::table
                .filter(bodies::item_id.eq(&row.id))
                .order(bodies::byte_offset.desc())
                .select((bodies::byte_offset, bodies::body))
                .first::<(i64, String)>(connection)
                .optional()?
            {
                let part = prefix(remaining, BODY_PAGE_BYTES - tail.len());
                if !part.is_empty() {
                    tail.push_str(part);
                    diesel::update(bodies::table.find((&row.id, start)))
                        .set(bodies::body.eq(tail))
                        .execute(connection)?;
                    offset += part.len() as i64;
                    remaining = &remaining[part.len()..];
                }
            }
        }
        while !remaining.is_empty() {
            let part = prefix(remaining, BODY_PAGE_BYTES);
            diesel::insert_into(bodies::table)
                .values((
                    bodies::item_id.eq(&row.id),
                    bodies::byte_offset.eq(offset),
                    bodies::body.eq(part),
                ))
                .execute(connection)?;
            offset += part.len() as i64;
            remaining = &remaining[part.len()..];
        }
        match detail {
            TranscriptDetail::None => {}
            TranscriptDetail::Tool { provider_id, json } => {
                // Payload bytes have already been sealed by the independent
                // writer. Only immutable references join this item transaction.
                diesel::sql_query("INSERT INTO transcript_payloads (payload_id, revision, item_id, descriptor) SELECT json_extract(value, '$.payloadID'), json_extract(value, '$.revision'), ?, value FROM json_each(?, '$.payloads') WHERE json_extract(value, '$.payloadID') IS NOT NULL AND json_extract(value, '$.availability') IN ('available', 'partial') ON CONFLICT(payload_id, revision) DO UPDATE SET descriptor = excluded.descriptor")
                    .bind::<diesel::sql_types::Text,_>(&row.id)
                    .bind::<diesel::sql_types::Text,_>(json)
                    .execute(connection)?;
                diesel::sql_query("DELETE FROM transcript_payloads WHERE item_id = ? AND EXISTS (SELECT 1 FROM transcript_payloads newer WHERE newer.payload_id = transcript_payloads.payload_id AND newer.revision > transcript_payloads.revision)")
                    .bind::<diesel::sql_types::Text,_>(&row.id).execute(connection)?;
                diesel::insert_into(tools::table)
                    .values((
                        tools::item_id.eq(&row.id),
                        tools::provider_tool_call_id.eq(provider_id),
                        tools::metadata_json.eq(json),
                    ))
                    .on_conflict(tools::item_id)
                    .do_update()
                    .set((
                        tools::provider_tool_call_id.eq(provider_id),
                        tools::metadata_json.eq(json),
                    ))
                    .execute(connection)?;
            }
            TranscriptDetail::Plan(json) => {
                diesel::insert_into(plans::table)
                    .values((plans::item_id.eq(&row.id), plans::entries_json.eq(json)))
                    .on_conflict(plans::item_id)
                    .do_update()
                    .set(plans::entries_json.eq(json))
                    .execute(connection)?;
            }
            TranscriptDetail::Permission { request_id, json } => {
                diesel::insert_into(permissions::table)
                    .values((
                        permissions::item_id.eq(&row.id),
                        permissions::request_id.eq(request_id),
                        permissions::decision_json.eq(json),
                    ))
                    .on_conflict(permissions::item_id)
                    .do_update()
                    .set(permissions::decision_json.eq(json))
                    .execute(connection)?;
            }
            TranscriptDetail::Gap {
                end_position,
                dropped,
                reason,
            } => {
                diesel::insert_into(gaps::table)
                    .values((
                        gaps::item_id.eq(&row.id),
                        gaps::end_position.eq(end_position),
                        gaps::dropped_updates.eq(dropped),
                        gaps::reason.eq(reason),
                    ))
                    .on_conflict(gaps::item_id)
                    .do_update()
                    .set((
                        gaps::end_position.eq(end_position),
                        gaps::dropped_updates.eq(dropped),
                        gaps::reason.eq(reason),
                    ))
                    .execute(connection)?;
            }
        }
        Self::advance_sequence(connection, &row.conversation_id)?;
        Ok(true)
    }

    /// Every subagent thread anchor in the conversation, as
    /// `(item id, thread id, parent thread id)`.
    pub fn thread_anchors(
        connection: &mut SqliteConnection,
        conversation: &str,
    ) -> Result<Vec<(String, String, Option<String>)>, StorageError> {
        use diesel::dsl::sql;
        use diesel::sql_types::{Nullable, Text};
        let thread = "json_extract(transcript_tools.metadata_json, '$.subagentThread.id')";
        Ok(tools::table
            .inner_join(items::table)
            .filter(items::conversation_id.eq(conversation))
            .filter(sql::<Nullable<Text>>(thread).is_not_null())
            .select((
                items::id,
                sql::<Text>(thread),
                sql::<Nullable<Text>>(
                    "json_extract(transcript_tools.metadata_json, '$.subagentThread.parentThreadID')",
                ),
            ))
            .load(connection)?)
    }

    /// Rows newest first. With `members`, only the rows whose stored
    /// `subagentThreadID` is one of `threads`, the parent's calls that
    /// messaged one of them, and the items in `items`.
    pub fn list(
        connection: &mut SqliteConnection,
        conversation: &str,
        before: Option<i64>,
        limit: usize,
        members: Option<(&[String], &[String])>,
    ) -> Result<(Vec<TranscriptRecord>, Option<String>), StorageError> {
        use diesel::dsl::sql;
        use diesel::sql_types::{Nullable, Text};
        let limit = limit.clamp(1, PAGE_RECORDS);
        let mut query = items::table
            .filter(items::conversation_id.eq(conversation))
            .filter(items::position.lt(before.unwrap_or(i64::MAX)))
            .into_boxed();
        if let Some((threads, ids)) = members {
            // Only this conversation's calls: the tools table has no
            // conversation of its own.
            let follow_ups = tools::table
                .filter(
                    sql::<diesel::sql_types::Bool>(
                        "transcript_tools.item_id IN (SELECT conversation_items.id FROM transcript_items AS conversation_items WHERE conversation_items.conversation_id = ",
                    )
                    .bind::<Text, _>(conversation.to_owned())
                    .sql(")"),
                )
                .filter(
                    sql::<Nullable<Text>>(
                        "json_extract(transcript_tools.metadata_json, '$.subagentFollowUp.threadID')",
                    )
                    .eq_any(threads.to_vec()),
                )
                .select(tools::item_id);
            query = query.filter(
                sql::<Nullable<Text>>(
                    "json_extract(transcript_items.adapter_metadata_json, '$.subagentThreadID')",
                )
                .eq_any(threads.to_vec())
                .or(items::id.eq_any(ids.to_vec()).nullable())
                .or(items::id.eq_any(follow_ups).nullable()),
            );
        }
        let rows = query
            .order(items::position.desc())
            .limit(limit as i64 + 1)
            .select(TranscriptRow::as_select())
            .load::<TranscriptRow>(connection)?;
        let next = if rows.len() > limit {
            rows.get(limit - 1).map(|row| row.position.to_string())
        } else {
            None
        };
        let records = Self::records(connection, rows.into_iter().take(limit).collect())?;
        Ok((records, next))
    }

    pub fn read_body(
        connection: &mut SqliteConnection,
        conversation: &str,
        id: &str,
        revision: i64,
        offset: i64,
        limit: usize,
    ) -> Result<(String, Option<i64>), StorageError> {
        let row = items::table
            .find(id)
            .filter(items::conversation_id.eq(conversation))
            .select((items::revision, items::body_byte_count, items::kind))
            .first::<(i64, i64, String)>(connection)?;
        let suppressed = Self::suppressed_by_run(connection, id)?;
        let effective_revision = row.0 + i64::from(suppressed);
        if effective_revision != revision {
            return Err(StorageError::new(
                "transcriptRevisionChanged",
                "Reload the item before reading its changed body",
            )
            .recoverable(true));
        }
        if suppressed {
            return Ok((String::new(), None));
        }
        if offset < 0 || offset > row.1 {
            return Err(StorageError::new(
                "storageConstraint",
                "Invalid transcript body offset",
            ));
        }
        if row.2 == "message" {
            use crate::storage::schema::conversation_messages;
            let body = conversation_messages::table
                .find(id)
                .select(conversation_messages::body)
                .first::<String>(connection)?;
            if !body.is_char_boundary(offset as usize) {
                return Err(StorageError::new(
                    "invalidParams",
                    "Body offset splits a UTF-8 character",
                ));
            }
            let part = prefix(&body[offset as usize..], limit.min(BODY_PAGE_BYTES));
            let next = offset + part.len() as i64;
            return Ok((part.into(), (next < row.1).then_some(next)));
        }
        use crate::storage::schema::{
            transcript_checkpoint_bodies as staged, transcript_checkpoint_heads as heads,
        };
        if let Some(body_revision) = heads::table
            .find(id)
            .select(heads::revision)
            .first::<i64>(connection)
            .optional()?
        {
            let start = staged::table
                .filter(staged::item_id.eq(id))
                .filter(staged::revision.eq(body_revision))
                .filter(staged::byte_offset.le(offset))
                .select(diesel::dsl::max(staged::byte_offset))
                .first::<Option<i64>>(connection)?
                .unwrap_or(offset);
            let chunks = staged::table
                .filter(staged::item_id.eq(id))
                .filter(staged::revision.eq(body_revision))
                .filter(staged::byte_offset.ge(start))
                .filter(
                    staged::byte_offset
                        .lt(offset.saturating_add(limit.min(BODY_PAGE_BYTES) as i64)),
                )
                .order(staged::byte_offset.asc())
                .limit(64)
                .select((staged::byte_offset, staged::body))
                .load::<(i64, String)>(connection)?;
            let mut text = String::new();
            for (at, chunk) in chunks {
                let skip = offset.saturating_sub(at).max(0) as usize;
                if !chunk.is_char_boundary(skip) {
                    return Err(StorageError::new(
                        "invalidParams",
                        "Body offset splits a UTF-8 character",
                    ));
                }
                let part = prefix(
                    &chunk[skip..],
                    limit.min(BODY_PAGE_BYTES).min((row.1 - offset) as usize) - text.len(),
                );
                text.push_str(part);
                if part.len() < chunk.len() - skip {
                    break;
                }
            }
            let end = offset + text.len() as i64;
            return Ok((text, (end < row.1).then_some(end)));
        }
        // Start with the chunk containing offset, then seek forward. LIMIT bounds
        // decoded bytes even when an adapter emitted one-byte transport chunks.
        let start = bodies::table
            .filter(bodies::item_id.eq(id))
            .filter(bodies::byte_offset.le(offset))
            .select(diesel::dsl::max(bodies::byte_offset))
            .first::<Option<i64>>(connection)?
            .unwrap_or(offset);
        let chunks = bodies::table
            .filter(bodies::item_id.eq(id))
            .filter(bodies::byte_offset.ge(start))
            .filter(
                bodies::byte_offset.lt(offset.saturating_add(limit.min(BODY_PAGE_BYTES) as i64)),
            )
            .order(bodies::byte_offset.asc())
            .limit(64)
            .select((bodies::byte_offset, bodies::body))
            .load::<(i64, String)>(connection)?;
        let mut text = String::new();
        let limit = limit.min(BODY_PAGE_BYTES);
        for (at, chunk) in chunks {
            let skip = offset.saturating_sub(at).max(0) as usize;
            if skip > chunk.len() {
                continue;
            }
            if !chunk.is_char_boundary(skip) {
                return Err(StorageError::new(
                    "storageConstraint",
                    "Body offset must be a UTF-8 boundary",
                ));
            }
            let part = prefix(&chunk[skip..], limit - text.len());
            text.push_str(part);
            if part.len() < chunk.len() - skip || text.len() == limit {
                break;
            }
        }
        let end = offset + text.len() as i64;
        Ok((text, (end < row.1).then_some(end)))
    }

    fn suppressed_by_run(
        connection: &mut SqliteConnection,
        id: &str,
    ) -> Result<bool, StorageError> {
        let Some((kind, completion, run)) = items::table
            .find(id)
            .select((items::kind, items::completion, items::run_id))
            .first::<(String, String, Option<String>)>(connection)
            .optional()?
        else {
            return Ok(false);
        };
        let suppressing = Self::suppressing_runs(connection, run.as_slice())?;
        Ok(masked(&kind, &completion, run.as_ref(), &suppressing))
    }

    /// The runs among `runs` whose typed failure proves the agent never ran.
    fn suppressing_runs(
        connection: &mut SqliteConnection,
        runs: &[String],
    ) -> Result<HashSet<String>, StorageError> {
        use crate::storage::schema::conversation_runs;
        use diesel::dsl::sql;
        use diesel::sql_types::{Nullable, Text};
        if runs.is_empty() {
            return Ok(HashSet::new());
        }
        Ok(conversation_runs::table
            .filter(conversation_runs::id.eq_any(runs))
            .filter(conversation_runs::status.eq("failed"))
            .filter(
                sql::<Nullable<Text>>("json_extract(conversation_runs.error_json, '$.kind')")
                    .eq_any(PRE_AGENT_FAILURES),
            )
            .select(conversation_runs::id)
            .load::<String>(connection)?
            .into_iter()
            .collect())
    }

    fn record(
        connection: &mut SqliteConnection,
        row: TranscriptRow,
    ) -> Result<TranscriptRecord, StorageError> {
        let mut records = Self::records(connection, vec![row])?;
        Ok(records.remove(0))
    }

    /// Records for rows read in one snapshot. A fixed number of set queries
    /// serves the whole page; previews follow `read_body`'s first page.
    fn records(
        connection: &mut SqliteConnection,
        rows: Vec<TranscriptRow>,
    ) -> Result<Vec<TranscriptRecord>, StorageError> {
        use crate::storage::schema::{
            conversation_messages as messages, conversation_runs as runs,
            transcript_checkpoint_bodies as staged, transcript_checkpoint_heads as heads,
        };
        let ids = |kinds: &[&str]| -> Vec<String> {
            rows.iter()
                .filter(|row| kinds.contains(&row.kind.as_str()))
                .map(|row| row.id.clone())
                .collect()
        };
        let run_ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.run_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        // Execution may commit before capture. Mask pre-agent narration even
        // across a crash or an older in-flight checkpoint publication.
        let suppressing = Self::suppressing_runs(connection, &run_ids)?;
        let run_status: HashMap<String, String> = runs::table
            .filter(runs::id.eq_any(&run_ids))
            .select((runs::id, runs::status))
            .load::<(String, String)>(connection)?
            .into_iter()
            .collect();
        let mut tool_details: HashMap<String, (Option<String>, String)> = tools::table
            .filter(tools::item_id.eq_any(ids(&["tool", "image"])))
            .select((
                tools::item_id,
                tools::provider_tool_call_id,
                tools::metadata_json,
            ))
            .load::<(String, Option<String>, String)>(connection)?
            .into_iter()
            .map(|(id, provider_id, json)| (id, (provider_id, json)))
            .collect();
        let mut plan_details: HashMap<String, String> = plans::table
            .filter(plans::item_id.eq_any(ids(&["plan"])))
            .select((plans::item_id, plans::entries_json))
            .load::<(String, String)>(connection)?
            .into_iter()
            .collect();
        let mut permission_details: HashMap<String, (String, String)> = permissions::table
            .filter(permissions::item_id.eq_any(ids(&["permission"])))
            .select((
                permissions::item_id,
                permissions::request_id,
                permissions::decision_json,
            ))
            .load::<(String, String, String)>(connection)?
            .into_iter()
            .map(|(id, request_id, json)| (id, (request_id, json)))
            .collect();
        let mut gap_details: HashMap<String, (i64, i64, String)> = gaps::table
            .filter(gaps::item_id.eq_any(ids(&["gap"])))
            .select((
                gaps::item_id,
                gaps::end_position,
                gaps::dropped_updates,
                gaps::reason,
            ))
            .load::<(String, i64, i64, String)>(connection)?
            .into_iter()
            .map(|(id, end_position, dropped, reason)| (id, (end_position, dropped, reason)))
            .collect();

        let suppressed: HashSet<String> = rows
            .iter()
            .filter(|row| {
                masked(
                    &row.kind,
                    &row.completion,
                    row.run_id.as_ref(),
                    &suppressing,
                )
            })
            .map(|row| row.id.clone())
            .collect();
        // Read only each message's preview bytes; bodies can be far larger.
        // SQLite gives NULL for an empty body's substring, as on the note of
        // a child stopped before it wrote anything.
        let mut message_bodies: HashMap<String, String> = messages::table
            .filter(messages::id.eq_any(ids(&["message"])))
            .select((
                messages::id,
                diesel::dsl::sql::<diesel::sql_types::Binary>(&format!(
                    "ifnull(substr(CAST(conversation_messages.body AS BLOB), 1, {}), X'')",
                    PREVIEW_BYTES.min(BODY_PAGE_BYTES)
                )),
            ))
            .load::<(String, Vec<u8>)>(connection)?
            .into_iter()
            .map(|(id, bytes)| (id, utf8_prefix(bytes)))
            .collect();
        let chunked: Vec<String> = rows
            .iter()
            .filter(|row| row.kind != "message" && !suppressed.contains(&row.id))
            .map(|row| row.id.clone())
            .collect();
        let head_revisions: HashMap<String, i64> = heads::table
            .filter(heads::item_id.eq_any(&chunked))
            .select((heads::item_id, heads::revision))
            .load::<(String, i64)>(connection)?
            .into_iter()
            .collect();
        let mut staged_chunks: HashMap<String, Vec<String>> = HashMap::new();
        for (id, revision, body) in staged::table
            .filter(staged::item_id.eq_any(head_revisions.keys()))
            .filter(staged::byte_offset.lt(PREVIEW_BYTES as i64))
            .order((staged::item_id.asc(), staged::byte_offset.asc()))
            .select((staged::item_id, staged::revision, staged::body))
            .load::<(String, i64, String)>(connection)?
        {
            if head_revisions.get(&id) == Some(&revision) {
                staged_chunks.entry(id).or_default().push(body);
            }
        }
        let unstaged: Vec<&String> = chunked
            .iter()
            .filter(|id| !head_revisions.contains_key(*id))
            .collect();
        let mut body_chunks: HashMap<String, Vec<String>> = HashMap::new();
        for (id, body) in bodies::table
            .filter(bodies::item_id.eq_any(unstaged))
            .filter(bodies::byte_offset.lt(PREVIEW_BYTES as i64))
            .order((bodies::item_id.asc(), bodies::byte_offset.asc()))
            .select((bodies::item_id, bodies::body))
            .load::<(String, String)>(connection)?
        {
            body_chunks.entry(id).or_default().push(body);
        }

        let missing = || StorageError::from(diesel::result::Error::NotFound);
        rows.into_iter()
            .map(|mut row| {
                let committed_revision = row.revision;
                let stored_bytes = row.body_byte_count;
                let is_suppressed = suppressed.contains(&row.id);
                if is_suppressed {
                    row.revision += 1;
                    row.completion = "suppressed".into();
                    row.body_byte_count = 0;
                }
                let detail = match row.kind.as_str() {
                    "tool" | "image" => {
                        let (provider_id, json) =
                            tool_details.remove(&row.id).ok_or_else(missing)?;
                        TranscriptDetail::Tool { provider_id, json }
                    }
                    "plan" => {
                        TranscriptDetail::Plan(plan_details.remove(&row.id).ok_or_else(missing)?)
                    }
                    "permission" => {
                        let (request_id, json) =
                            permission_details.remove(&row.id).ok_or_else(missing)?;
                        TranscriptDetail::Permission { request_id, json }
                    }
                    "gap" => {
                        let (end_position, dropped, reason) =
                            gap_details.remove(&row.id).ok_or_else(missing)?;
                        TranscriptDetail::Gap {
                            end_position,
                            dropped,
                            reason,
                        }
                    }
                    _ => TranscriptDetail::None,
                };
                let preview = if is_suppressed {
                    String::new()
                } else if row.kind == "message" {
                    message_bodies.remove(&row.id).ok_or_else(missing)?
                } else if head_revisions.contains_key(&row.id) {
                    // A checkpoint body's header count hides its uncommitted suffix.
                    let limit = PREVIEW_BYTES
                        .min(BODY_PAGE_BYTES)
                        .min(stored_bytes.max(0) as usize);
                    first_page(staged_chunks.remove(&row.id), limit, false)
                } else {
                    first_page(
                        body_chunks.remove(&row.id),
                        PREVIEW_BYTES.min(BODY_PAGE_BYTES),
                        true,
                    )
                };
                let run_status = match &row.run_id {
                    Some(id) => Some(run_status.get(id).cloned().ok_or_else(missing)?),
                    None => None,
                };
                Ok(TranscriptRecord {
                    committed_revision,
                    run_status,
                    row,
                    detail,
                    preview,
                })
            })
            .collect()
    }
}

/// Failed-run error kinds that prove the agent never took the turn.
const PRE_AGENT_FAILURES: [&str; 9] = [
    "authRequired",
    "authRefreshBusy",
    "invalidAuthMethod",
    "selectionUnavailable",
    "agentNotFound",
    "adapterNotInstalled",
    "runtimeMissing",
    "runtimeInvalid",
    "runtimeUnsupported",
];

/// Narration a pre-agent failure masks, though storage still holds it.
fn masked(
    kind: &str,
    completion: &str,
    run: Option<&String>,
    suppressing: &HashSet<String>,
) -> bool {
    matches!(kind, "assistantText" | "reasoning")
        && completion != "suppressed"
        && run.is_some_and(|run| suppressing.contains(run))
}

/// `read_body`'s page at offset 0 over chunks ordered by offset: at most 64
/// chunks, as its LIMIT bounds decoding even for one-byte transport chunks.
fn first_page(chunks: Option<Vec<String>>, limit: usize, stop_at_limit: bool) -> String {
    let mut text = String::new();
    for chunk in chunks.into_iter().flatten().take(64) {
        let part = prefix(&chunk, limit - text.len());
        text.push_str(part);
        if part.len() < chunk.len() || (stop_at_limit && text.len() == limit) {
            break;
        }
    }
    text
}
