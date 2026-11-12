//! The configured MCP servers: one global list, each server on for every
//! workspace unless a workspace turned it off. Plain config goes in SQLite;
//! values marked secret go in the [`SecretStore`] and never come back out
//! through the protocol.

use super::oauth::{OAuthCache, StoredOAuth};
use super::secrets::SecretStore;
use crate::core_error::CoreError;
use crate::storage::models::WorkspaceId;
use crate::storage::repositories::{McpServerRecord, McpServerRepository, WorkspaceRepository};
use crate::storage::Database;
use ottam_protocol::{
    McpConfigValue, McpServerConfig, McpServerID, McpServerState, McpServerStatus,
    McpServerSummary, McpServerTransport, WorkspaceID,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_NAME_LEN: usize = 64;

#[derive(Clone)]
pub(crate) struct McpServers {
    database: Database,
    secrets: Arc<dyn SecretStore>,
    oauth: OAuthCache,
}

impl McpServers {
    pub(crate) fn new(database: Database, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            database,
            secrets,
            oauth: OAuthCache::default(),
        }
    }

    /// A remote server's OAuth credentials, beside its secrets.
    pub(crate) fn oauth(&self, server_id: &McpServerID) -> StoredOAuth {
        StoredOAuth::new(Arc::clone(&self.secrets), self.oauth.clone(), server_id)
    }

    pub(crate) async fn list(&self) -> Result<Vec<McpServerSummary>, CoreError> {
        let (records, disabled) = self
            .database
            .run("mcp.list", |connection| {
                Ok((
                    McpServerRepository::list(connection)?,
                    McpServerRepository::disabled_workspaces(connection)?,
                ))
            })
            .await?;
        let mut disabled_by_server = BTreeMap::<String, Vec<WorkspaceID>>::new();
        for (server_id, workspace_id) in disabled {
            disabled_by_server
                .entry(server_id)
                .or_default()
                .push(WorkspaceID(workspace_id));
        }
        records
            .into_iter()
            .map(|record| {
                let disabled = disabled_by_server.remove(&record.id).unwrap_or_default();
                summary(record, disabled)
            })
            .collect()
    }

    pub(crate) async fn add(&self, config: McpServerConfig) -> Result<McpServerSummary, CoreError> {
        validate(&config)?;
        if let Some(value) = values_of(&config.transport)
            .1
            .iter()
            .find(|value| value.secret && value.value.is_none())
        {
            return Err(invalid(format!("{} needs a value", value.name)));
        }
        let id = uuid::Uuid::now_v7().to_string();
        let secrets = secrets_to_write(&id, &config.transport);
        self.write_secrets(secrets.clone()).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let record = McpServerRecord {
            id: id.clone(),
            name: config.name.clone(),
            transport_json: stored_transport_json(&config.transport)?,
            created_at: now,
            updated_at: now,
        };
        let inserted = {
            let record = record.clone();
            self.database
                .run("mcp.add", move |connection| {
                    McpServerRepository::insert(connection, &record)
                })
                .await
        };
        if let Err(error) = inserted {
            self.delete_secrets(secrets.into_keys().collect()).await;
            return Err(error.into());
        }
        summary(record, Vec::new())
    }

    pub(crate) async fn update(
        &self,
        server_id: &McpServerID,
        mut config: McpServerConfig,
    ) -> Result<McpServerSummary, CoreError> {
        let existing = self.require(server_id).await?;
        let previous = decode_transport(&existing.transport_json)?;
        keep_unchanged_secrets(&previous, &mut config.transport)?;
        validate(&config)?;
        let id = existing.id.clone();
        let written = secret_accounts(&id, &config.transport);
        let transport_json = stored_transport_json(&config.transport)?;
        let writes = secrets_to_write(&id, &config.transport);
        // A failed update leaves the server as it was, its secrets included.
        let before = self.read_secrets(writes.keys().cloned().collect()).await?;
        if let Err(error) = self.write_secrets(writes).await {
            self.restore_secrets(before).await;
            return Err(error);
        }
        let now = chrono::Utc::now().timestamp_millis();
        let updated = {
            let (id, name, transport_json) =
                (id.clone(), config.name.clone(), transport_json.clone());
            self.database
                .run("mcp.update", move |connection| {
                    McpServerRepository::update(connection, &id, &name, &transport_json, now)
                })
                .await
        };
        match updated {
            Ok(true) => {}
            failed => {
                self.restore_secrets(before).await;
                return Err(match failed {
                    Err(error) => error.into(),
                    Ok(_) => not_found(),
                });
            }
        }
        let stale = secret_accounts(&id, &previous)
            .difference(&written)
            .cloned()
            .collect();
        self.delete_secrets(stale).await;
        let disabled = self.disabled_for(&id).await?;
        summary(
            McpServerRecord {
                name: config.name,
                transport_json,
                updated_at: now,
                ..existing
            },
            disabled,
        )
    }

    pub(crate) async fn remove(&self, server_id: &McpServerID) -> Result<(), CoreError> {
        let existing = self.require(server_id).await?;
        let transport = decode_transport(&existing.transport_json)?;
        let id = existing.id.clone();
        self.database
            .run("mcp.remove", move |connection| {
                McpServerRepository::delete(connection, &id)
            })
            .await?;
        self.delete_secrets(secret_accounts(&existing.id, &transport))
            .await;
        Ok(())
    }

    pub(crate) async fn set_workspace_enabled(
        &self,
        server_id: &McpServerID,
        workspace_id: &WorkspaceID,
        enabled: bool,
    ) -> Result<McpServerSummary, CoreError> {
        let existing = self.require(server_id).await?;
        let (id, workspace) = (existing.id.clone(), workspace_id.0.clone());
        let now = chrono::Utc::now().timestamp_millis();
        self.database
            .run("mcp.setWorkspaceEnabled", move |connection| {
                McpServerRepository::set_workspace_enabled(
                    connection, &id, &workspace, enabled, now,
                )
            })
            .await
            .map_err(|error| match error.kind {
                // The only foreign key the row can miss besides the server.
                "storageConstraint" => {
                    CoreError::new("workspaceNotFound", "No workspace has this id")
                }
                _ => error.into(),
            })?;
        let disabled = self.disabled_for(&existing.id).await?;
        summary(existing, disabled)
    }

    /// The server as its upstream connection needs it, secrets read back
    /// from the store.
    pub(crate) async fn resolve(
        &self,
        server_id: &McpServerID,
    ) -> Result<ResolvedServer, CoreError> {
        let record = self.require(server_id).await?;
        let transport = decode_transport(&record.transport_json)?;
        let (section, values) = values_of(&transport);
        let (server, secrets) = (record.id.clone(), Arc::clone(&self.secrets));
        let wanted: Vec<McpConfigValue> = values.to_vec();
        let section = section.to_owned();
        let values = blocking(move || {
            wanted
                .into_iter()
                .map(|value| {
                    let resolved = if value.secret {
                        secrets.get(&account(&server, &section, &value.name))?
                    } else {
                        value.value
                    };
                    let resolved = resolved.ok_or_else(|| {
                        CoreError::new(
                            "mcpSecretMissing",
                            format!("{} has no stored value; enter it again", value.name),
                        )
                    })?;
                    Ok((value.name, resolved))
                })
                .collect::<Result<Vec<_>, CoreError>>()
        })
        .await?;
        let transport = match transport {
            McpServerTransport::Stdio { command, args, .. } => ResolvedTransport::Stdio {
                command,
                args,
                env: values,
            },
            McpServerTransport::Http { url, .. } => ResolvedTransport::Http {
                url,
                headers: values,
            },
        };
        Ok(ResolvedServer {
            server_id: McpServerID(record.id),
            name: record.name,
            transport,
        })
    }

    /// The project directories of a workspace, in its order: a local
    /// server's roots, the first its working directory.
    pub(crate) async fn workspace_roots(
        &self,
        workspace_id: &WorkspaceID,
    ) -> Result<Vec<PathBuf>, CoreError> {
        let workspace_id = WorkspaceId::try_from(workspace_id.0.as_str())?;
        let detail = self
            .database
            .run("mcp.workspaceRoots", move |connection| {
                WorkspaceRepository::detail(connection, &workspace_id)
            })
            .await?;
        Ok(detail
            .projects
            .into_iter()
            .filter(|member| member.project.archived_at.is_none())
            .filter(|member| member.source.archived_at.is_none())
            .map(|member| PathBuf::from(member.source.canonical_path))
            .collect())
    }

    /// Whether the server runs as a local process, read without its secrets.
    #[cfg(test)]
    pub(crate) async fn is_local(&self, server_id: &McpServerID) -> Result<bool, CoreError> {
        let record = self.require(server_id).await?;
        Ok(matches!(
            decode_transport(&record.transport_json)?,
            McpServerTransport::Stdio { .. }
        ))
    }

    async fn require(&self, server_id: &McpServerID) -> Result<McpServerRecord, CoreError> {
        let id = server_id.0.clone();
        self.database
            .run("mcp.get", move |connection| {
                McpServerRepository::get(connection, &id)
            })
            .await?
            .ok_or_else(not_found)
    }

    async fn disabled_for(&self, server_id: &str) -> Result<Vec<WorkspaceID>, CoreError> {
        let server_id = server_id.to_owned();
        let disabled = self
            .database
            .run("mcp.disabledWorkspaces", |connection| {
                McpServerRepository::disabled_workspaces(connection)
            })
            .await?;
        Ok(disabled
            .into_iter()
            .filter(|(server, _)| *server == server_id)
            .map(|(_, workspace)| WorkspaceID(workspace))
            .collect())
    }

    async fn write_secrets(&self, values: BTreeMap<String, String>) -> Result<(), CoreError> {
        if values.is_empty() {
            return Ok(());
        }
        let secrets = Arc::clone(&self.secrets);
        blocking(move || {
            for (account, value) in values {
                secrets.set(&account, &value)?;
            }
            Ok(())
        })
        .await
    }

    /// The stored value of each account, `None` where it has none.
    async fn read_secrets(
        &self,
        accounts: BTreeSet<String>,
    ) -> Result<BTreeMap<String, Option<String>>, CoreError> {
        if accounts.is_empty() {
            return Ok(BTreeMap::new());
        }
        let secrets = Arc::clone(&self.secrets);
        blocking(move || {
            accounts
                .into_iter()
                .map(|account| {
                    let value = secrets.get(&account)?;
                    Ok((account, value))
                })
                .collect()
        })
        .await
    }

    /// Put each account back as [`Self::read_secrets`] found it. Best
    /// effort, like [`Self::delete_secrets`].
    async fn restore_secrets(&self, values: BTreeMap<String, Option<String>>) {
        if values.is_empty() {
            return;
        }
        let secrets = Arc::clone(&self.secrets);
        let restored = blocking(move || {
            for (account, value) in values {
                match value {
                    Some(value) => secrets.set(&account, &value)?,
                    None => secrets.delete(&account)?,
                }
            }
            Ok(())
        })
        .await;
        if let Err(error) = restored {
            tracing::warn!(kind = error.kind, "could not restore an MCP server secret");
        }
    }

    /// Best effort: a secret left behind is orphaned, never exposed.
    async fn delete_secrets(&self, accounts: BTreeSet<String>) {
        if accounts.is_empty() {
            return;
        }
        let secrets = Arc::clone(&self.secrets);
        let deleted = blocking(move || {
            for account in accounts {
                secrets.delete(&account)?;
            }
            Ok(())
        })
        .await;
        if let Err(error) = deleted {
            tracing::warn!(kind = error.kind, "could not delete an MCP server secret");
        }
    }
}

/// A configured server with every value present. Its `Debug` names values
/// without showing them, since some are secrets.
#[derive(Clone)]
pub(crate) struct ResolvedServer {
    pub(crate) server_id: McpServerID,
    pub(crate) name: String,
    pub(crate) transport: ResolvedTransport,
}

#[derive(Clone)]
pub(crate) enum ResolvedTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

impl std::fmt::Debug for ResolvedServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, names): (&str, Vec<&str>) = match &self.transport {
            ResolvedTransport::Stdio { env, .. } => {
                ("stdio", env.iter().map(|(name, _)| name.as_str()).collect())
            }
            ResolvedTransport::Http { headers, .. } => (
                "http",
                headers.iter().map(|(name, _)| name.as_str()).collect(),
            ),
        };
        formatter
            .debug_struct("ResolvedServer")
            .field("server_id", &self.server_id.0)
            .field("name", &self.name)
            .field("kind", &kind)
            .field("values", &names)
            .finish()
    }
}

/// Headers the MCP client sets itself; a configured one would conflict.
const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "content-length",
    "content-type",
    "host",
    "last-event-id",
    "mcp-protocol-version",
    "mcp-session-id",
];

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| CoreError::new("secretStoreUnavailable", "the secret store stopped"))?
}

fn not_found() -> CoreError {
    CoreError::new("mcpServerNotFound", "No MCP server has this id")
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::new("invalidMcpServer", message).recoverable(true)
}

fn summary(
    record: McpServerRecord,
    disabled_workspace_ids: Vec<WorkspaceID>,
) -> Result<McpServerSummary, CoreError> {
    Ok(McpServerSummary {
        server_id: McpServerID(record.id),
        config: McpServerConfig {
            name: record.name,
            transport: decode_transport(&record.transport_json)?,
        },
        disabled_workspace_ids,
        state: McpServerState {
            status: McpServerStatus::NotStarted,
            error: None,
        },
        signed_in: false,
    })
}

fn decode_transport(json: &str) -> Result<McpServerTransport, CoreError> {
    serde_json::from_str(json).map_err(|_| {
        CoreError::new(
            "invalidStoredMcpServer",
            "a stored MCP server is unreadable",
        )
    })
}

fn values_of(transport: &McpServerTransport) -> (&'static str, &[McpConfigValue]) {
    match transport {
        McpServerTransport::Stdio { env, .. } => ("env", env),
        McpServerTransport::Http { headers, .. } => ("header", headers),
    }
}

fn values_of_mut(transport: &mut McpServerTransport) -> &mut Vec<McpConfigValue> {
    match transport {
        McpServerTransport::Stdio { env, .. } => env,
        McpServerTransport::Http { headers, .. } => headers,
    }
}

/// The Keychain account each secret is stored under.
fn account(server_id: &str, section: &str, name: &str) -> String {
    format!("{server_id}/{section}/{name}")
}

fn secret_accounts(server_id: &str, transport: &McpServerTransport) -> BTreeSet<String> {
    let (section, values) = values_of(transport);
    values
        .iter()
        .filter(|value| value.secret)
        .map(|value| account(server_id, section, &value.name))
        .collect()
}

/// Each secret given a new value, by account. A secret an update kept has
/// no value in the config, and its stored one stays where it is.
fn secrets_to_write(server_id: &str, transport: &McpServerTransport) -> BTreeMap<String, String> {
    let (section, values) = values_of(transport);
    values
        .iter()
        .filter(|value| value.secret)
        .filter_map(|value| {
            let secret = value.value.clone()?;
            Some((account(server_id, section, &value.name), secret))
        })
        .collect()
}

/// An update may leave a secret's value out to keep it, but only for a
/// secret of the same name the server already had.
fn keep_unchanged_secrets(
    previous: &McpServerTransport,
    next: &mut McpServerTransport,
) -> Result<(), CoreError> {
    let (previous_section, previous_values) = values_of(previous);
    let next_section = values_of(next).0;
    let kept = previous_values
        .iter()
        .filter(|value| value.secret && previous_section == next_section)
        .map(|value| value.name.as_str())
        .collect::<BTreeSet<_>>();
    for value in values_of_mut(next).iter() {
        if value.secret && value.value.is_none() && !kept.contains(value.name.as_str()) {
            return Err(invalid(format!("{} needs a value", value.name)));
        }
    }
    Ok(())
}

/// The transport as stored: secret values left out.
fn stored_transport_json(transport: &McpServerTransport) -> Result<String, CoreError> {
    let mut stored = transport.clone();
    for value in values_of_mut(&mut stored) {
        if value.secret {
            value.value = None;
        }
    }
    serde_json::to_string(&stored)
        .map_err(|_| CoreError::new("invalidMcpServer", "the server could not be stored"))
}

pub(super) fn validate(config: &McpServerConfig) -> Result<(), CoreError> {
    let name = &config.name;
    if name.is_empty()
        || name.len() > MAX_NAME_LEN
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(invalid(
            "A server name is 1 to 64 letters, digits, hyphens, or underscores",
        ));
    }
    if name.eq_ignore_ascii_case(super::ottam::OTTAM_ROUTE) {
        return Err(CoreError::new(
            "mcpServerNameReserved",
            "The name ottam is Ottam's own; choose another",
        )
        .recoverable(true));
    }
    match &config.transport {
        McpServerTransport::Stdio { command, env, .. } => {
            if command.trim().is_empty() {
                return Err(invalid("A local server needs a command"));
            }
            validate_values(env, "environment variable", |name| {
                !name.contains(['=', '\0'])
            })?;
        }
        McpServerTransport::Http { url, headers } => {
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| invalid("A remote server needs a valid URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(invalid("A remote server's URL starts with http or https"));
            }
            validate_values(headers, "header", |name| {
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_ok()
                    && !RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
            })?;
        }
    }
    Ok(())
}

fn validate_values(
    values: &[McpConfigValue],
    noun: &str,
    valid_name: impl Fn(&str) -> bool,
) -> Result<(), CoreError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.name.is_empty() || !valid_name(&value.name) {
            return Err(invalid(format!(
                "{:?} is not a valid {noun} name",
                value.name
            )));
        }
        if !seen.insert(value.name.to_ascii_lowercase()) {
            return Err(invalid(format!("{} is set twice", value.name)));
        }
        if !value.secret && value.value.is_none() {
            return Err(invalid(format!("{} needs a value", value.name)));
        }
    }
    Ok(())
}
