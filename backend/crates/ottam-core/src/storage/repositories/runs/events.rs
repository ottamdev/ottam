use super::{ensure_json_cap, RunRepository};
use crate::storage::database::recorded_transaction;
use crate::storage::error::StorageError;
use crate::storage::models::{
    ConversationId, ConversationRunId, NewRunEventRow, RunEvent, RunEventId, RunEventKind,
    RunEventRow,
};
use crate::storage::repositories::pagination::{assemble_page, Page, PageRequest};
use crate::storage::schema::{conversation_runs, run_events};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

impl RunRepository {
    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    /// Bounded event-batch append: sequences continue from the run's
    /// `event_sequence_cursor` and the cursor advances in the same
    /// transaction, making it the recorder checkpoint.
    pub fn append_event_batch(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        events: &[(RunEventKind, String)],
        now: DateTime<Utc>,
    ) -> Result<Vec<RunEvent>, StorageError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        recorded_transaction(connection, "conversation.appendEvents", |connection| {
            Self::append_event_batch_in_transaction(connection, run_id, events, now)
        })
    }

    pub(crate) fn append_event_batch_in_transaction(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        events: &[(RunEventKind, String)],
        now: DateTime<Utc>,
    ) -> Result<Vec<RunEvent>, StorageError> {
        for (_, payload) in events {
            ensure_json_cap(Some(payload), "RunEvent payload")?;
        }
        let run = Self::get_run(connection, run_id)?;
        let run_id_value = run_id.to_string();
        let now_ms = now.timestamp_millis();
        let mut persisted = Vec::with_capacity(events.len());
        let mut sequence = run.event_sequence_cursor;
        for (kind, payload) in events {
            sequence += 1;
            let event_id = RunEventId::new_v7();
            let event_id_value = event_id.to_string();
            diesel::insert_into(run_events::table)
                .values(NewRunEventRow {
                    id: &event_id_value,
                    run_id: &run_id_value,
                    sequence,
                    kind: kind.as_storage_str(),
                    payload_json: payload,
                    created_at: now_ms,
                })
                .execute(connection)?;
            persisted.push(RunEvent {
                id: event_id,
                run_id: run.id,
                sequence,
                kind: *kind,
                payload_json: payload.clone(),
                created_at: now,
            });
        }
        diesel::update(conversation_runs::table.find(&run_id_value))
            .set(conversation_runs::event_sequence_cursor.eq(sequence))
            .execute(connection)?;
        Ok(persisted)
    }

    /// Events in ascending sequence order after the cursor.
    pub fn list_events(
        connection: &mut SqliteConnection,
        run_id: &ConversationRunId,
        request: &PageRequest<'_>,
    ) -> Result<Page<RunEvent>, StorageError> {
        let mut query = run_events::table
            .filter(run_events::run_id.eq(run_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            let after: i64 = cursor.parse().map_err(|_| {
                StorageError::new("storageConstraint", "the event cursor is invalid")
                    .recoverable(true)
            })?;
            query = query.filter(run_events::sequence.gt(after));
        }
        let rows = query
            .order(run_events::sequence.asc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(RunEventRow::as_select())
            .load::<RunEventRow>(connection)?;
        let page = assemble_page(
            rows,
            request,
            |row| row.payload_json.len(),
            |row| row.sequence.to_string(),
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

    /// Events across every run of a Conversation, ascending by event id
    /// (UUIDv7 — insertion order), with an event-id cursor.
    pub fn list_conversation_events(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        request: &PageRequest<'_>,
    ) -> Result<Page<RunEvent>, StorageError> {
        let mut query = run_events::table
            .inner_join(conversation_runs::table)
            .filter(conversation_runs::conversation_id.eq(conversation_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            query = query.filter(run_events::id.gt(cursor.to_string()));
        }
        let rows = query
            .order(run_events::id.asc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(RunEventRow::as_select())
            .load::<RunEventRow>(connection)?;
        let page = assemble_page(
            rows,
            request,
            |row| row.payload_json.len(),
            |row| row.id.clone(),
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
}
