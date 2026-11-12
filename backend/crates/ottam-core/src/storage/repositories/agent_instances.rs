use crate::agent::{AgentId, AgentInstance, HarnessId, HomeLayout};
use crate::storage::StorageError;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};

#[derive(QueryableByName)]
struct InstanceRow {
    #[diesel(sql_type = Text)]
    id: String,
    #[diesel(sql_type = Text)]
    harness_id: String,
    #[diesel(sql_type = Text)]
    label: String,
    #[diesel(sql_type = Text)]
    home_json: String,
    #[diesel(sql_type = Nullable<BigInt>)]
    claimed_at: Option<i64>,
}

impl InstanceRow {
    fn decode(self) -> Result<AgentInstance, StorageError> {
        let home = serde_json::from_str(&self.home_json)
            .map_err(|_| StorageError::new("invalidAgentHome", "invalid stored agent home"))?;
        validate_home(&self.harness_id, &home)?;
        Ok(AgentInstance {
            id: AgentId(self.id),
            harness_id: HarnessId(self.harness_id),
            label: self.label,
            home,
            claimed: self.claimed_at.is_some(),
        })
    }
}

pub struct AgentInstanceRepository;

impl AgentInstanceRepository {
    /// Removed attachments still identify CLI homes we must never purge.
    pub fn retained_cli_homes(
        connection: &mut SqliteConnection,
    ) -> Result<Vec<std::path::PathBuf>, StorageError> {
        #[derive(QueryableByName)]
        struct HomeRow {
            #[diesel(sql_type = Text)]
            home_json: String,
        }
        diesel::sql_query("SELECT home_json FROM agent_instances")
            .load::<HomeRow>(connection)?
            .into_iter()
            .map(|row| {
                serde_json::from_str::<HomeLayout>(&row.home_json)
                    .map_err(|_| StorageError::new("invalidAgentHome", "invalid stored agent home"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|homes| {
                homes
                    .into_iter()
                    .filter_map(|home| match home {
                        HomeLayout::Existing { path, .. } => Some(path),
                        HomeLayout::Managed => None,
                    })
                    .collect()
            })
    }

    pub fn is_deleted(connection: &mut SqliteConnection, id: &str) -> Result<bool, StorageError> {
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type = BigInt)]
            count: i64,
        }
        let count = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM agent_instances WHERE id=? AND deleted_at IS NOT NULL",
        )
        .bind::<Text, _>(id)
        .get_result::<Count>(connection)?;
        Ok(count.count > 0)
    }

    pub fn create_managed(
        connection: &mut SqliteConnection,
        harness: &str,
        label: &str,
    ) -> Result<AgentInstance, StorageError> {
        connection.immediate_transaction(|connection| {
            let label = Self::validate_label(connection, harness, label, None)?;
            let id = format!("{harness}.{}", uuid::Uuid::now_v7());
            diesel::sql_query("INSERT INTO agent_instances (id,harness_id,label,home_json,created_at) VALUES (?,?,?,?,?)")
                .bind::<Text,_>(&id).bind::<Text,_>(harness).bind::<Text,_>(&label)
                .bind::<Text,_>(encode(&HomeLayout::Managed)?).bind::<BigInt,_>(chrono::Utc::now().timestamp_millis()).execute(connection)?;
            Ok(AgentInstance { id: AgentId(id), harness_id: HarnessId(harness.into()), label, home: HomeLayout::Managed, claimed: false })
        })
    }

    fn validate_label(
        connection: &mut SqliteConnection,
        harness: &str,
        label: &str,
        except: Option<&str>,
    ) -> Result<String, StorageError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 80 || label.chars().any(char::is_control) {
            return Err(StorageError::new(
                "invalidAgentLabel",
                "Use a profile name of 1–80 characters.",
            ));
        }
        if Self::list(connection)?.iter().any(|instance| {
            instance.harness_id.0 == harness
                && Some(instance.id.0.as_str()) != except
                && instance.label.to_lowercase() == label.to_lowercase()
        }) {
            return Err(StorageError::new(
                "duplicateAgentLabel",
                "This provider already has a profile with that name.",
            ));
        }
        Ok(label.into())
    }

    pub fn rename(
        connection: &mut SqliteConnection,
        id: &str,
        label: &str,
    ) -> Result<AgentInstance, StorageError> {
        connection.immediate_transaction(|connection| {
            let mut instance = Self::list(connection)?
                .into_iter()
                .find(|instance| instance.id.0 == id)
                .ok_or_else(|| StorageError::not_found("agent instance"))?;
            instance.label =
                Self::validate_label(connection, &instance.harness_id.0, label, Some(id))?;
            diesel::sql_query(
                "UPDATE agent_instances SET label=? WHERE id=? AND deleted_at IS NULL",
            )
            .bind::<Text, _>(&instance.label)
            .bind::<Text, _>(id)
            .execute(connection)?;
            Ok(instance)
        })
    }

    pub fn check_idle(connection: &mut SqliteConnection, id: &str) -> Result<(), StorageError> {
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type = BigInt)]
            count: i64,
        }
        let count = diesel::sql_query("SELECT COUNT(*) AS count FROM conversation_runs WHERE engine_id=? AND status IN ('queued','running')").bind::<Text,_>(id).get_result::<Count>(connection)?;
        if count.count > 0 {
            return Err(StorageError::new(
                "agentInstanceBusy",
                "Stop this profile's active and queued work before deleting it.",
            ));
        }
        Ok(())
    }

    pub fn delete(connection: &mut SqliteConnection, id: &str) -> Result<(), StorageError> {
        connection.immediate_transaction(|connection| {
            Self::check_idle(connection, id)?;
            let changed = diesel::sql_query(
                "UPDATE agent_instances SET deleted_at=? WHERE id=? AND deleted_at IS NULL",
            )
            .bind::<BigInt, _>(chrono::Utc::now().timestamp_millis())
            .bind::<Text, _>(id)
            .execute(connection)?;
            if changed == 0 {
                return Err(StorageError::not_found("agent instance"));
            }
            Ok(())
        })
    }

    pub fn list(connection: &mut SqliteConnection) -> Result<Vec<AgentInstance>, StorageError> {
        diesel::sql_query("SELECT id, harness_id, label, home_json, claimed_at FROM agent_instances WHERE deleted_at IS NULL ORDER BY created_at, id")
            .load::<InstanceRow>(connection)?.into_iter().map(InstanceRow::decode).collect()
    }

    pub fn set_layout(
        connection: &mut SqliteConnection,
        id: &str,
        home: &HomeLayout,
    ) -> Result<AgentInstance, StorageError> {
        connection.immediate_transaction(|connection| {
            let instances = Self::list(connection)?;
            let current = instances.iter().find(|instance| instance.id.0 == id)
                .ok_or_else(|| StorageError::not_found("agent instance"))?;
            validate_home(&current.harness_id.0, home)?;
            if instances.iter().any(|instance| instance.id.0 != id && instance.harness_id == current.harness_id && same_existing_home(&current.harness_id.0, &instance.home, home)) {
                return Err(StorageError::new("agentHomeAlreadyAdded", "This CLI setup is already added. Use its existing instance."));
            }
            let home_json = encode(home)?;
            let changed = diesel::sql_query("UPDATE agent_instances SET home_json = ? WHERE id = ? AND claimed_at IS NULL AND deleted_at IS NULL")
                .bind::<Text, _>(home_json).bind::<Text, _>(id).execute(connection)?;
            if changed == 0 {
                return Err(StorageError::new("agentLayoutClaimed", "This setup already has conversations and must keep its home. Use its current instance or add a different supported setup."));
            }
            Self::list(connection)?.into_iter().find(|instance| instance.id.0 == id)
                .ok_or_else(|| StorageError::not_found("agent instance"))
        })
    }

    /// The same transaction serializes duplicate additions from different windows.
    pub fn add_existing(
        connection: &mut SqliteConnection,
        harness_id: &str,
        label: &str,
        home: &HomeLayout,
    ) -> Result<AgentInstance, StorageError> {
        validate_home(harness_id, home)?;
        connection.immediate_transaction(|connection| {
            if let Some(existing) = Self::list(connection)?.into_iter().find(|instance| {
                instance.harness_id.0 == harness_id && same_existing_home(harness_id, &instance.home, home)
            }) {
                return Ok(existing);
            }
            let mut label = label.trim().to_owned();
            let base = label.clone();
            let mut suffix = 2;
            loop {
                match Self::validate_label(connection, harness_id, &label, None) {
                    Ok(valid) => { label = valid; break; }
                    Err(error) if error.kind == "duplicateAgentLabel" => {
                        let ending = format!(" ({suffix})");
                        label = format!("{}{ending}", base.chars().take(80 - ending.chars().count()).collect::<String>());
                        suffix += 1;
                    }
                    Err(error) => return Err(error),
                }
            }
            let id = format!("{harness_id}.{}", uuid::Uuid::now_v7());
            diesel::sql_query("INSERT INTO agent_instances (id, harness_id, label, home_json, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind::<Text, _>(&id).bind::<Text, _>(harness_id).bind::<Text, _>(&label)
                .bind::<Text, _>(encode(home)?).bind::<BigInt, _>(chrono::Utc::now().timestamp_millis())
                .execute(connection)?;
            Ok(AgentInstance { id: AgentId(id), harness_id: HarnessId(harness_id.into()), label, home: home.clone(), claimed: false })
        })
    }
}

fn encode(home: &HomeLayout) -> Result<String, StorageError> {
    serde_json::to_string(home)
        .map_err(|_| StorageError::new("invalidAgentHome", "cannot encode agent home"))
}

// Codex's explicit default override and unset override refer to the same home.
// Claude also uses the override to choose its global MCP metadata location.
fn same_existing_home(harness: &str, left: &HomeLayout, right: &HomeLayout) -> bool {
    match (left, right) {
        (
            HomeLayout::Existing {
                path: left,
                environment: left_env,
            },
            HomeLayout::Existing {
                path: right,
                environment: right_env,
            },
        ) => {
            // Claude's unset override also selects ~/.claude.json. An explicit
            // default path selects metadata inside the directory instead.
            // OpenCode's path is only its database's directory: its variables
            // pick the database file and the config directory.
            if matches!(harness, "claude" | "opencode") && left_env != right_env {
                return false;
            }
            left == right
                || matches!((left.canonicalize(), right.canonicalize()), (Ok(left), Ok(right)) if left == right)
        }
        _ => false,
    }
}

fn validate_home(harness: &str, home: &HomeLayout) -> Result<(), StorageError> {
    if let HomeLayout::Existing { path, environment } = home {
        let Some(setup) = crate::agent::existing_setup(harness) else {
            return Err(StorageError::new(
                "invalidAgentHome",
                "unsupported existing setup",
            ));
        };
        if !path.is_absolute()
            || environment.len() != setup.variables.len()
            || !setup
                .variables
                .iter()
                .all(|variable| environment.contains_key(*variable))
            || environment.iter().any(|(variable, value)| {
                value.as_deref().is_some_and(|value| {
                    value.is_empty()
                        // OpenCode resolves a relative database against its
                        // data home.
                        || (variable != "OPENCODE_DB" && !std::path::Path::new(value).is_absolute())
                })
            })
        {
            return Err(StorageError::new(
                "invalidAgentHome",
                "invalid CLI home environment",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;
    use diesel_migrations::MigrationHarness;
    use std::collections::BTreeMap;

    #[test]
    fn cli_attachments_allocate_unique_labels_and_reuse_identity() {
        let mut connection = database();
        let label = "Codex — Your setup";
        let managed =
            AgentInstanceRepository::create_managed(&mut connection, "codex", label).unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = AgentInstanceRepository::add_existing(
            &mut connection,
            "codex",
            label,
            &existing(a.path(), None),
        )
        .unwrap();
        let second = AgentInstanceRepository::add_existing(
            &mut connection,
            "codex",
            label,
            &existing(b.path(), None),
        )
        .unwrap();
        assert_eq!(first.label, format!("{label} (2)"));
        assert_eq!(second.label, format!("{label} (3)"));
        let repeated = AgentInstanceRepository::add_existing(
            &mut connection,
            "codex",
            label,
            &existing(a.path(), None),
        )
        .unwrap();
        assert_eq!(repeated, first);
        assert_ne!(managed.id, first.id);
        AgentInstanceRepository::delete(&mut connection, &first.id.0).unwrap();
        let retained = AgentInstanceRepository::retained_cli_homes(&mut connection).unwrap();
        assert!(retained.contains(&a.path().to_path_buf()));
        assert!(retained.contains(&b.path().to_path_buf()));
    }

    #[test]
    fn second_instance_rename_delete_preserves_identity_and_siblings() {
        let mut connection = database();
        let first =
            AgentInstanceRepository::create_managed(&mut connection, "codex", "Personal").unwrap();
        let second =
            AgentInstanceRepository::create_managed(&mut connection, "codex", "Work").unwrap();
        assert_ne!(first.id, second.id);
        assert!(
            AgentInstanceRepository::create_managed(&mut connection, "codex", " work ").is_err()
        );
        assert!(AgentInstanceRepository::create_managed(&mut connection, "claude", "Work").is_ok());
        let renamed =
            AgentInstanceRepository::rename(&mut connection, &second.id.0, "Office").unwrap();
        assert_eq!(renamed.id, second.id);
        assert_eq!(renamed.home, second.home);
        AgentInstanceRepository::delete(&mut connection, &second.id.0).unwrap();
        let replacement =
            AgentInstanceRepository::create_managed(&mut connection, "codex", "Office").unwrap();
        assert_ne!(replacement.id, second.id);
        assert!(AgentInstanceRepository::list(&mut connection)
            .unwrap()
            .iter()
            .any(|instance| instance.id == first.id));
        assert!(AgentInstanceRepository::rename(&mut connection, &second.id.0, "Back").is_err());
    }

    #[test]
    fn instance_delete_seed_preserves_history_and_rejects_late_runs() {
        let mut connection = database();
        connection.batch_execute("INSERT INTO conversation_runs (id,workspace_id,conversation_id,engine_id,status,queued_at,origin) VALUES ('r','w','c','codex','queued',42,'user');").unwrap();
        assert!(AgentInstanceRepository::delete(&mut connection, "codex").is_err());
        connection
            .batch_execute("UPDATE conversation_runs SET status='completed' WHERE id='r';")
            .unwrap();
        AgentInstanceRepository::delete(&mut connection, "codex").unwrap();
        connection
            .run_pending_migrations(crate::storage::database::MIGRATIONS)
            .unwrap();
        assert!(!AgentInstanceRepository::list(&mut connection)
            .unwrap()
            .iter()
            .any(|instance| instance.id.0 == "codex"));
        assert!(connection.batch_execute("INSERT INTO conversation_runs (id,workspace_id,conversation_id,engine_id,status,queued_at,origin) VALUES ('late','w','c','codex','queued',43,'user');").is_err());
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type = BigInt)]
            count: i64,
        }
        let count: Count = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM conversation_runs WHERE id='r' AND engine_id='codex'",
        )
        .get_result(&mut connection)
        .unwrap();
        assert_eq!(count.count, 1);
    }

    fn database() -> SqliteConnection {
        let mut connection = SqliteConnection::establish(":memory:").unwrap();
        connection
            .run_pending_migrations(crate::storage::database::MIGRATIONS)
            .unwrap();
        connection.batch_execute("INSERT INTO workspaces VALUES ('w','Workspace',0,0,0,NULL); INSERT INTO conversations (id,workspace_id,working_root_kind,title,created_at,updated_at) VALUES ('c','w','workspaceFiles','Conversation',0,0);").unwrap();
        connection
    }

    fn existing(path: &std::path::Path, value: Option<String>) -> HomeLayout {
        HomeLayout::Existing {
            path: path.into(),
            environment: BTreeMap::from([("CODEX_HOME".into(), value)]),
        }
    }

    #[test]
    fn existing_home_environment_cannot_override_home_or_hide_claude_metadata_choice() {
        let mut connection = database();
        let home = tempfile::tempdir().unwrap();
        let invalid = HomeLayout::Existing {
            path: home.path().into(),
            environment: BTreeMap::from([(
                "HOME".into(),
                Some(home.path().to_string_lossy().into()),
            )]),
        };
        assert!(AgentInstanceRepository::add_existing(
            &mut connection,
            "claude",
            "Invalid",
            &invalid
        )
        .is_err());
        let layout = |value| HomeLayout::Existing {
            path: home.path().into(),
            environment: BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), value)]),
        };
        let unset = AgentInstanceRepository::add_existing(
            &mut connection,
            "claude",
            "Default Claude",
            &layout(None),
        )
        .unwrap();
        let explicit = AgentInstanceRepository::add_existing(
            &mut connection,
            "claude",
            "Custom Claude",
            &layout(Some(home.path().to_string_lossy().into())),
        )
        .unwrap();
        assert_ne!(unset.id, explicit.id);
        let duplicate = AgentInstanceRepository::add_existing(
            &mut connection,
            "claude",
            "Duplicate",
            &layout(None),
        )
        .unwrap();
        assert_eq!(unset.id, duplicate.id);
    }

    #[test]
    fn agent_instances_preserve_seeds_and_reuse_same_home() {
        let mut connection = database();
        let seeds = AgentInstanceRepository::list(&mut connection).unwrap();
        let mut harnesses: Vec<_> = seeds
            .iter()
            .map(|instance| instance.harness_id.0.as_str())
            .collect();
        harnesses.sort_unstable();
        assert_eq!(harnesses, ["claude", "codex", "omp", "opencode"]);
        assert!(seeds
            .iter()
            .all(|instance| instance.home == HomeLayout::Managed && !instance.claimed));
        let home = tempfile::tempdir().unwrap();
        let first = AgentInstanceRepository::add_existing(
            &mut connection,
            "codex",
            "My Codex",
            &existing(home.path(), None),
        )
        .unwrap();
        let duplicate = AgentInstanceRepository::add_existing(
            &mut connection,
            "codex",
            "Other label",
            &existing(home.path(), Some(home.path().to_string_lossy().into())),
        )
        .unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(
            AgentInstanceRepository::list(&mut connection)
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn agent_instances_claim_runs_and_sessions_before_layout_can_change() {
        let mut connection = database();
        connection.batch_execute("INSERT INTO conversation_runs (id,workspace_id,conversation_id,engine_id,status,queued_at,origin) VALUES ('r','w','c','codex','queued',42,'user');").unwrap();
        let home = tempfile::tempdir().unwrap();
        assert!(AgentInstanceRepository::set_layout(
            &mut connection,
            "codex",
            &existing(home.path(), None)
        )
        .is_err());
        connection.batch_execute("INSERT INTO agent_sessions (id,workspace_id,conversation_id,engine_id,source_scope_fingerprint,status,created_at,last_used_at) VALUES ('s','w','c','claude','scope','active',43,43);").unwrap();
        assert!(AgentInstanceRepository::set_layout(
            &mut connection,
            "claude",
            &existing(home.path(), None)
        )
        .is_err());
        let instances = AgentInstanceRepository::list(&mut connection).unwrap();
        assert!(instances
            .iter()
            .filter(|instance| !matches!(instance.harness_id.0.as_str(), "opencode" | "omp"))
            .all(|instance| instance.claimed));
    }

    #[test]
    fn concurrent_conversation_claim_prevents_layout_change_atomically() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("claims.sqlite");
        let url = path.to_string_lossy().into_owned();
        let mut connection = SqliteConnection::establish(&url).unwrap();
        connection
            .run_pending_migrations(crate::storage::database::MIGRATIONS)
            .unwrap();
        connection.batch_execute("PRAGMA busy_timeout=3000; INSERT INTO workspaces VALUES ('w','Workspace',0,0,0,NULL); INSERT INTO conversations (id,workspace_id,working_root_kind,title,created_at,updated_at) VALUES ('c','w','workspaceFiles','Conversation',0,0);").unwrap();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut writer = SqliteConnection::establish(&url).unwrap();
            writer.batch_execute("PRAGMA busy_timeout=3000;").unwrap();
            writer.immediate_transaction::<_, diesel::result::Error, _>(|writer| {
                writer.batch_execute("INSERT INTO conversation_runs (id,workspace_id,conversation_id,engine_id,status,queued_at,origin) VALUES ('r','w','c','codex','queued',42,'user');")?;
                claimed_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(100));
                Ok(())
            }).unwrap();
        });
        claimed_rx.recv().unwrap();
        let result = AgentInstanceRepository::set_layout(
            &mut connection,
            "codex",
            &existing(root.path(), None),
        );
        writer.join().unwrap();
        assert!(result.is_err());
        let codex = AgentInstanceRepository::list(&mut connection)
            .unwrap()
            .into_iter()
            .find(|instance| instance.id.0 == "codex")
            .unwrap();
        assert!(codex.claimed);
        assert_eq!(codex.home, HomeLayout::Managed);
    }
}
