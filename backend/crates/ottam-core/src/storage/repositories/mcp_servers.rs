use crate::storage::error::StorageError;
use crate::storage::schema::{mcp_servers, workspace_mcp_disabled};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

/// A stored MCP server. `transport_json` never holds a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = mcp_servers)]
pub struct McpServerRecord {
    pub id: String,
    pub name: String,
    pub transport_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// MCP servers core proxies, and the workspaces that turned each off.
pub struct McpServerRepository;

impl McpServerRepository {
    /// Every server, in the order they were added.
    pub fn list(connection: &mut SqliteConnection) -> Result<Vec<McpServerRecord>, StorageError> {
        Ok(mcp_servers::table
            .order((mcp_servers::created_at, mcp_servers::id))
            .select(McpServerRecord::as_select())
            .load(connection)?)
    }

    pub fn get(
        connection: &mut SqliteConnection,
        id: &str,
    ) -> Result<Option<McpServerRecord>, StorageError> {
        Ok(mcp_servers::table
            .find(id)
            .select(McpServerRecord::as_select())
            .first(connection)
            .optional()?)
    }

    /// Fails with `mcpServerNameTaken` when another server has the name.
    pub fn insert(
        connection: &mut SqliteConnection,
        record: &McpServerRecord,
    ) -> Result<(), StorageError> {
        diesel::insert_into(mcp_servers::table)
            .values(record)
            .execute(connection)
            .map_err(name_taken)?;
        Ok(())
    }

    /// Replaces a server's name and transport. False when it is gone.
    pub fn update(
        connection: &mut SqliteConnection,
        id: &str,
        name: &str,
        transport_json: &str,
        updated_at: i64,
    ) -> Result<bool, StorageError> {
        let updated = diesel::update(mcp_servers::table.find(id))
            .set((
                mcp_servers::name.eq(name),
                mcp_servers::transport_json.eq(transport_json),
                mcp_servers::updated_at.eq(updated_at),
            ))
            .execute(connection)
            .map_err(name_taken)?;
        Ok(updated > 0)
    }

    /// Removes a server and its workspace rows. False when it was gone.
    pub fn delete(connection: &mut SqliteConnection, id: &str) -> Result<bool, StorageError> {
        Ok(diesel::delete(mcp_servers::table.find(id)).execute(connection)? > 0)
    }

    /// Every (server, workspace) pair where the workspace turned it off.
    pub fn disabled_workspaces(
        connection: &mut SqliteConnection,
    ) -> Result<Vec<(String, String)>, StorageError> {
        Ok(workspace_mcp_disabled::table
            .order((
                workspace_mcp_disabled::server_id,
                workspace_mcp_disabled::disabled_at,
                workspace_mcp_disabled::workspace_id,
            ))
            .select((
                workspace_mcp_disabled::server_id,
                workspace_mcp_disabled::workspace_id,
            ))
            .load(connection)?)
    }

    pub fn set_workspace_enabled(
        connection: &mut SqliteConnection,
        server_id: &str,
        workspace_id: &str,
        enabled: bool,
        now: i64,
    ) -> Result<(), StorageError> {
        if enabled {
            diesel::delete(workspace_mcp_disabled::table.find((workspace_id, server_id)))
                .execute(connection)?;
        } else {
            diesel::insert_into(workspace_mcp_disabled::table)
                .values((
                    workspace_mcp_disabled::workspace_id.eq(workspace_id),
                    workspace_mcp_disabled::server_id.eq(server_id),
                    workspace_mcp_disabled::disabled_at.eq(now),
                ))
                .on_conflict_do_nothing()
                .execute(connection)?;
        }
        Ok(())
    }
}

fn name_taken(error: diesel::result::Error) -> StorageError {
    use diesel::result::{DatabaseErrorKind, Error};
    match error {
        Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => StorageError::new(
            "mcpServerNameTaken",
            "another MCP server already has this name",
        )
        .recoverable(true),
        other => other.into(),
    }
}
