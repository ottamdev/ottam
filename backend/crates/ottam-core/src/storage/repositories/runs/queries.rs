use super::RunRepository;
use crate::storage::error::StorageError;
use crate::storage::models::{
    AgentSession, AgentSessionRow, ConversationId, ConversationRun, ConversationRunInput,
    ConversationRunInputRow, ConversationRunRow,
};
use crate::storage::repositories::pagination::{assemble_page, Page, PageRequest};
use crate::storage::schema::{agent_sessions, conversation_run_inputs, conversation_runs};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

impl RunRepository {
    // ------------------------------------------------------------------
    // Paged lists
    // ------------------------------------------------------------------

    pub fn list_runs(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        request: &PageRequest<'_>,
    ) -> Result<Page<ConversationRun>, StorageError> {
        let mut query = conversation_runs::table
            .filter(conversation_runs::conversation_id.eq(conversation_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            query = query.filter(conversation_runs::id.lt(cursor.to_string()));
        }
        let rows = query
            .order(conversation_runs::id.desc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(ConversationRunRow::as_select())
            .load::<ConversationRunRow>(connection)?;
        let page = assemble_page(
            rows,
            request,
            |row| {
                128 + row.error_json.as_deref().map_or(0, str::len)
                    + row.request_options_json.as_deref().map_or(0, str::len)
            },
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

    pub fn list_inputs(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        request: &PageRequest<'_>,
    ) -> Result<Page<ConversationRunInput>, StorageError> {
        let mut query = conversation_run_inputs::table
            .filter(conversation_run_inputs::conversation_id.eq(conversation_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            query = query.filter(conversation_run_inputs::id.lt(cursor.to_string()));
        }
        let rows = query
            .order(conversation_run_inputs::id.desc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(ConversationRunInputRow::as_select())
            .load::<ConversationRunInputRow>(connection)?;
        let page = assemble_page(
            rows,
            request,
            |row| 192 + row.failure_reason.as_deref().map_or(0, str::len),
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

    pub fn list_sessions(
        connection: &mut SqliteConnection,
        conversation_id: &ConversationId,
        request: &PageRequest<'_>,
    ) -> Result<Page<AgentSession>, StorageError> {
        let mut query = agent_sessions::table
            .filter(agent_sessions::conversation_id.eq(conversation_id.to_string()))
            .into_boxed();
        if let Some(cursor) = request.cursor {
            query = query.filter(agent_sessions::id.lt(cursor.to_string()));
        }
        let rows = query
            .order(agent_sessions::id.desc())
            .limit(request.clamped_limit() as i64 + 1)
            .select(AgentSessionRow::as_select())
            .load::<AgentSessionRow>(connection)?;
        let page = assemble_page(rows, request, |_| 160, |row| row.id.clone());
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
