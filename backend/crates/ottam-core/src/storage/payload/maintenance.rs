//! Bounded metadata maintenance. Callers serialize reference lookup with publication.
use super::*;

pub(crate) const REVISION_BATCH: i64 = 128;

#[derive(Debug)]
pub(crate) struct Candidate {
    pub id: i64,
    pub key: RevisionKey,
    pub ready: bool,
}

pub(crate) struct Maintenance(Repository, PathBuf);

#[derive(QueryableByName)]
pub(crate) struct Usage {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub logical_bytes: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub encoded_bytes: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub staged_logical_bytes: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub staged_encoded_bytes: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub reusable_pages: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub page_size: i64,
}

impl Maintenance {
    /// SQLite reports checkpoint progress even when the PRAGMA itself succeeds.
    pub(crate) fn pending(&mut self) -> Result<bool> {
        #[derive(QueryableByName)]
        struct Checkpoint {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            busy: i32,
            #[diesel(sql_type = diesel::sql_types::Integer)]
            log: i32,
            #[diesel(sql_type = diesel::sql_types::Integer)]
            checkpointed: i32,
        }
        let checkpoint = diesel::sql_query("PRAGMA wal_checkpoint(PASSIVE)")
            .get_result::<Checkpoint>(&mut self.0.connection)?;
        Ok(checkpoint.busy != 0
            || checkpoint.log > checkpoint.checkpointed
            || self.usage()?.reusable_pages > 0)
    }
    pub(crate) fn usage(&mut self) -> Result<Usage> {
        Ok(diesel::sql_query("SELECT COALESCE((SELECT SUM(logical_bytes) FROM payload_usage), 0) AS logical_bytes, COALESCE((SELECT SUM(encoded_bytes) FROM payload_usage), 0) AS encoded_bytes, COALESCE((SELECT SUM(p.committed_raw_bytes) FROM payload_revisions r JOIN payload_parts p ON p.revision_id = r.id WHERE r.state IN ('staging', 'interrupted')), 0) AS staged_logical_bytes, COALESCE((SELECT SUM(p.committed_encoded_bytes) FROM payload_revisions r JOIN payload_parts p ON p.revision_id = r.id WHERE r.state IN ('staging', 'interrupted')), 0) AS staged_encoded_bytes, (SELECT freelist_count FROM pragma_freelist_count) AS reusable_pages, (SELECT page_size FROM pragma_page_size) AS page_size")
            .get_result(&mut self.0.connection)?)
    }

    pub(crate) fn log_usage(&mut self, home: &std::path::Path) -> Result<()> {
        if tracing::enabled!(tracing::Level::DEBUG) {
            let usage = self.usage()?;
            tracing::debug!(
                logical_bytes = usage.logical_bytes,
                encoded_bytes = usage.encoded_bytes,
                staged_logical_bytes = usage.staged_logical_bytes,
                staged_encoded_bytes = usage.staged_encoded_bytes,
                reusable_bytes = usage.reusable_pages * usage.page_size,
                database_bytes = std::fs::metadata(home.join("payloads.sqlite"))?.len(),
                wal_bytes =
                    std::fs::metadata(home.join("payloads.sqlite-wal")).map_or(0, |m| m.len()),
                "payload storage usage"
            );
        }
        Ok(())
    }
    pub(crate) fn delete_conversation(
        &mut self,
        workspace: &str,
        conversation: &str,
    ) -> Result<()> {
        loop {
            let ids = revisions::table
                .filter(revisions::workspace_id.eq(workspace))
                .filter(revisions::conversation_id.eq(conversation))
                .order(revisions::id)
                .limit(REVISION_BATCH)
                .select((revisions::id, revisions::payload_id, revisions::revision))
                .load::<(i64, String, i64)>(&mut self.0.connection)?;
            if ids.is_empty() {
                return Ok(());
            }
            for (id, payload_id, revision) in ids {
                self.remove(&Candidate {
                    id,
                    key: RevisionKey {
                        workspace_id: workspace.into(),
                        conversation_id: conversation.into(),
                        item_id: String::new(),
                        payload_id,
                        revision: revision.try_into()?,
                    },
                    ready: false,
                })?;
            }
        }
    }
    pub(crate) fn open(home: &std::path::Path) -> Result<Self> {
        Ok(Self(Repository::open(home)?, home.to_owned()))
    }

    /// Keyset pagination bounds metadata memory; this never selects chunk bodies.
    pub(crate) fn candidates(&mut self, after: i64) -> Result<Vec<Candidate>> {
        let rows = revisions::table
            .filter(revisions::id.gt(after))
            .order(revisions::id)
            .limit(REVISION_BATCH)
            .select((
                revisions::id,
                revisions::workspace_id,
                revisions::conversation_id,
                revisions::item_id,
                revisions::payload_id,
                revisions::revision,
                revisions::state,
            ))
            .load::<(i64, String, String, String, String, i64, String)>(&mut self.0.connection)?;
        rows.into_iter()
            .map(
                |(id, workspace_id, conversation_id, item_id, payload_id, revision, state)| {
                    Ok(Candidate {
                        id,
                        key: RevisionKey {
                            workspace_id,
                            conversation_id,
                            item_id,
                            payload_id,
                            revision: revision.try_into()?,
                        },
                        ready: state == "ready",
                    })
                },
            )
            .collect()
    }

    /// Delete at most one revision per transaction, so large conversations do not
    /// hold a writer lock for their entire lifetime. Readers retain their snapshots.
    pub(crate) fn remove(&mut self, candidate: &Candidate) -> Result<()> {
        let mut accounting = self.0.accounting.lock().unwrap();
        let bytes = super::cache::revision_bytes(&mut self.0.connection, candidate.id)?;
        let removed = diesel::delete(
            revisions::table
                .filter(revisions::id.eq(candidate.id))
                .filter(revisions::payload_id.eq(&candidate.key.payload_id))
                .filter(revisions::revision.eq(integer(candidate.key.revision)?)),
        )
        .execute(&mut self.0.connection)?;
        if removed != 0 {
            accounting.cached_bytes = accounting.cached_bytes.saturating_sub(bytes);
        }
        Ok(())
    }

    pub(crate) fn idle(&mut self) -> Result<()> {
        if fs2::available_space(&self.1)? < 2 * quota::MAINTENANCE_RESERVE {
            return self.pressure_idle();
        }
        self.0
            .connection
            .batch_execute("PRAGMA wal_checkpoint(PASSIVE); PRAGMA incremental_vacuum(128);")?;
        Ok(())
    }

    pub(super) fn pressure_idle(&mut self) -> Result<()> {
        // Pressure recovery must not wait behind a reader. Reclaim at most
        // 2048 pages; busy checkpoints are retried by later maintenance.
        self.0.connection.batch_execute("PRAGMA busy_timeout=0;")?;
        let result = self.0.connection.batch_execute("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA incremental_vacuum(2048); PRAGMA wal_checkpoint(TRUNCATE);");
        self.0
            .connection
            .batch_execute("PRAGMA busy_timeout=5000;")?;
        result?;
        Ok(())
    }
}

/// Run before opening the live writer, under Core ownership. A process restart
/// abandons staging captures even if their chunks survived.
pub(crate) fn reconcile_startup(home: &std::path::Path, main: &mut SqliteConnection) -> Result<()> {
    let mut maintenance = Maintenance::open(home)?;
    let mut after = 0;
    loop {
        let candidates = maintenance.candidates(after)?;
        if candidates.is_empty() {
            break;
        }
        for candidate in candidates {
            after = candidate.id;
            if !candidate.ready || !referenced(main, &candidate.key)? {
                maintenance.remove(&candidate)?;
            }
        }
    }
    maintenance.idle()?;
    Ok(())
}

pub(crate) fn referenced(main: &mut SqliteConnection, key: &RevisionKey) -> Result<bool> {
    #[derive(QueryableByName)]
    struct Found {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        value: bool,
    }
    Ok(diesel::sql_query("SELECT EXISTS(SELECT 1 FROM transcript_payloads p JOIN transcript_items i ON i.id = p.item_id JOIN conversations c ON c.id = i.conversation_id WHERE p.payload_id = ? AND p.revision = ? AND p.item_id = ? AND i.conversation_id = ? AND c.workspace_id = ?) AS value")
        .bind::<diesel::sql_types::Text, _>(&key.payload_id)
        .bind::<diesel::sql_types::BigInt, _>(integer(key.revision)?)
        .bind::<diesel::sql_types::Text, _>(&key.item_id)
        .bind::<diesel::sql_types::Text, _>(&key.conversation_id)
        .bind::<diesel::sql_types::Text, _>(&key.workspace_id)
        .get_result::<Found>(main)?.value)
}
