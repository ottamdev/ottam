//! Eviction retains immutable manifests. Only explicitly classified tool outputs
//! enter this table; role="output" alone also includes durable assistant media.
use super::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Default)]
pub(super) struct Accounting {
    pub cached_bytes: u64,
    eviction: Option<((i64, i64), u64)>,
}
type Accounts = Mutex<std::collections::HashMap<PathBuf, Weak<Mutex<Accounting>>>>;
static ACCOUNTS: OnceLock<Accounts> = OnceLock::new();

pub(super) fn accounting(
    home: &std::path::Path,
    connection: &mut SqliteConnection,
) -> Result<Arc<Mutex<Accounting>>> {
    let mut accounts = ACCOUNTS.get_or_init(Default::default).lock().unwrap();
    accounts.retain(|_, value| value.strong_count() > 0);
    if let Some(account) = accounts.get(home).and_then(Weak::upgrade) {
        return Ok(account);
    }
    let cached_bytes = authoritative_usage(connection)?;
    let account = Arc::new(Mutex::new(Accounting {
        cached_bytes,
        eviction: None,
    }));
    accounts.insert(home.to_owned(), Arc::downgrade(&account));
    Ok(account)
}

fn authoritative_usage(connection: &mut SqliteConnection) -> Result<u64> {
    Ok(diesel::sql_query("SELECT COALESCE(SUM(p.committed_encoded_bytes), 0) AS value FROM payload_cache c JOIN payload_parts p ON p.revision_id = c.revision_id WHERE c.evicted = 0")
        .get_result::<Number>(connection)?.value.try_into()?)
}

pub(super) fn revision_bytes(connection: &mut SqliteConnection, id: i64) -> Result<u64> {
    Ok(diesel::sql_query("SELECT COALESCE(SUM(p.committed_encoded_bytes), 0) AS value FROM payload_cache c JOIN payload_parts p ON p.revision_id = c.revision_id WHERE c.revision_id = ? AND c.evicted = 0")
        .bind::<BigInt, _>(id).get_result::<Number>(connection)?.value.try_into()?)
}

pub(crate) struct Cache(Repository, PathBuf);

#[derive(Clone, Debug, QueryableByName)]
pub(crate) struct Recovery {
    #[diesel(sql_type = Nullable<Text>)]
    pub agent_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub session_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub tool_call_id: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub cwd: Option<String>,
}

#[derive(QueryableByName)]
struct Number {
    #[diesel(sql_type = BigInt)]
    value: i64,
}

#[derive(QueryableByName)]
struct EvictionIndex {
    #[diesel(sql_type = BigInt)]
    value: i64,
    #[diesel(sql_type = BigInt)]
    accessed: i64,
}

impl Cache {
    pub(crate) fn open(home: &std::path::Path) -> Result<Self> {
        Ok(Self(Repository::open(home)?, home.to_owned()))
    }

    pub(crate) fn classify(&mut self, key: &RevisionKey, recovery: &Recovery) -> Result<()> {
        let mut accounting = self.0.accounting.lock().unwrap();
        // Mixed results stay durable as a whole: eviction works on immutable
        // revisions, and must never discard an image embedded in tool output.
        let has_image = parts::table
            .inner_join(revisions::table)
            .filter(revisions::payload_id.eq(&key.payload_id))
            .filter(revisions::revision.eq(integer(key.revision)?))
            .filter(parts::format.like("image/%"))
            .count()
            .get_result::<i64>(&mut self.0.connection)?
            != 0;
        if has_image {
            return Ok(());
        }
        // Classification is a metadata event, never a body scan.
        let added = diesel::sql_query("SELECT COALESCE(SUM(p.committed_encoded_bytes), 0) AS value FROM payload_revisions r JOIN payload_parts p ON p.revision_id = r.id WHERE r.payload_id = ? AND r.revision = ? AND r.role = 'output' AND r.state = 'ready' AND NOT EXISTS (SELECT 1 FROM payload_cache c WHERE c.revision_id = r.id)")
            .bind::<Text, _>(&key.payload_id).bind::<BigInt, _>(integer(key.revision)?)
            .get_result::<Number>(&mut self.0.connection)?.value as u64;
        diesel::sql_query("INSERT INTO payload_cache (revision_id, accessed_at_ms, agent_id, session_id, tool_call_id, cwd) SELECT id, updated_at_ms, ?, ?, ?, ? FROM payload_revisions WHERE payload_id = ? AND revision = ? AND role = 'output' AND state = 'ready' ON CONFLICT(revision_id) DO UPDATE SET agent_id = COALESCE(payload_cache.agent_id, excluded.agent_id), session_id = COALESCE(payload_cache.session_id, excluded.session_id), tool_call_id = COALESCE(payload_cache.tool_call_id, excluded.tool_call_id), cwd = COALESCE(payload_cache.cwd, excluded.cwd) WHERE (payload_cache.agent_id IS NULL AND excluded.agent_id IS NOT NULL) OR (payload_cache.session_id IS NULL AND excluded.session_id IS NOT NULL) OR (payload_cache.tool_call_id IS NULL AND excluded.tool_call_id IS NOT NULL) OR (payload_cache.cwd IS NULL AND excluded.cwd IS NOT NULL)")
            .bind::<Nullable<Text>, _>(&recovery.agent_id)
            .bind::<Nullable<Text>, _>(&recovery.session_id)
            .bind::<Nullable<Text>, _>(&recovery.tool_call_id)
            .bind::<Nullable<Text>, _>(&recovery.cwd)
            .bind::<Text, _>(&key.payload_id)
            .bind::<BigInt, _>(integer(key.revision)?)
            .execute(&mut self.0.connection)?;
        accounting.cached_bytes = accounting.cached_bytes.saturating_add(added);
        Ok(())
    }

    pub(crate) fn usage(&mut self) -> Result<u64> {
        Ok(self.0.accounting.lock().unwrap().cached_bytes)
    }

    pub(crate) fn reconcile(&mut self) -> Result<u64> {
        let mut accounting = self.0.accounting.lock().unwrap();
        accounting.cached_bytes = authoritative_usage(&mut self.0.connection)?;
        Ok(accounting.cached_bytes)
    }

    pub(crate) fn limit_gb(&mut self) -> Result<u64> {
        Ok(
            diesel::sql_query("SELECT limit_gb AS value FROM payload_cache_settings WHERE id = 1")
                .get_result::<Number>(&mut self.0.connection)?
                .value
                .try_into()?,
        )
    }

    pub(crate) fn set_limit_gb(&mut self, gb: u64) -> Result<()> {
        if gb == 0 || gb > i64::MAX as u64 / 1_000_000_000 {
            return Err(invalid("cache limit must be a positive whole number of GB"));
        }
        diesel::sql_query("UPDATE payload_cache_settings SET limit_gb = ? WHERE id = 1")
            .bind::<BigInt, _>(integer(gb)?)
            .execute(&mut self.0.connection)?;
        self.0.accounting.lock().unwrap().eviction = None;
        Ok(())
    }

    pub(crate) fn recovery(&mut self, key: &RevisionKey) -> Result<Option<Recovery>> {
        Ok(diesel::sql_query("SELECT c.agent_id, c.session_id, c.tool_call_id, c.cwd FROM payload_cache c JOIN payload_revisions r ON r.id = c.revision_id WHERE r.payload_id = ? AND r.revision = ? AND c.evicted = 1")
            .bind::<Text, _>(&key.payload_id).bind::<BigInt, _>(integer(key.revision)?)
            .get_result(&mut self.0.connection).optional()?)
    }

    pub(crate) fn touch(&mut self, key: &RevisionKey) -> Result<()> {
        diesel::sql_query("UPDATE payload_cache SET accessed_at_ms = ? WHERE revision_id = (SELECT id FROM payload_revisions WHERE payload_id = ? AND revision = ?)")
            .bind::<BigInt, _>(chrono::Utc::now().timestamp_millis())
            .bind::<Text, _>(&key.payload_id).bind::<BigInt, _>(integer(key.revision)?)
            .execute(&mut self.0.connection)?;
        Ok(())
    }

    /// Caller serializes protection acquisition with this pass. A batch is
    /// bounded even if every candidate is protected; later releases reschedule.
    pub(crate) fn evict(&mut self, protected: &HashSet<RevisionKey>, clear: bool) -> Result<bool> {
        let usage = self.reconcile()?;
        let limit = self.limit_gb()? * 1_000_000_000;
        let (mut after, target) = {
            let accounting = self.0.accounting.lock().unwrap();
            match accounting.eviction {
                Some(progress) if !clear || progress.1 == 0 => progress,
                _ if clear => ((i64::MIN, i64::MIN), 0),
                _ if usage > limit => ((i64::MIN, i64::MIN), limit / 10 * 9),
                _ => return Ok(false),
            }
        };
        let mut remaining = usage;
        // Revision row IDs may be reused after deletion. Hold the same lock as
        // all deletion paths from candidate selection through body eviction.
        let mut accounting = self.0.accounting.lock().unwrap();
        let ids = diesel::sql_query("SELECT c.revision_id AS value, c.accessed_at_ms AS accessed FROM payload_cache c JOIN payload_revisions r ON r.id = c.revision_id WHERE c.evicted = 0 AND r.state = 'ready' AND (c.accessed_at_ms, c.revision_id) > (?, ?) ORDER BY c.accessed_at_ms, c.revision_id LIMIT 128")
            .bind::<BigInt, _>(after.0).bind::<BigInt, _>(after.1)
            .load::<EvictionIndex>(&mut self.0.connection)?;
        let full_batch = ids.len() == 128;
        for id in ids {
            if remaining <= target {
                break;
            }
            after = (id.accessed, id.value);
            let (workspace_id, conversation_id, item_id, payload_id, revision) = revisions::table
                .find(id.value)
                .select((
                    revisions::workspace_id,
                    revisions::conversation_id,
                    revisions::item_id,
                    revisions::payload_id,
                    revisions::revision,
                ))
                .first::<(String, String, String, String, i64)>(&mut self.0.connection)?;
            let key = RevisionKey {
                workspace_id,
                conversation_id,
                item_id,
                payload_id,
                revision: revision.try_into()?,
            };
            if protected.contains(&key) {
                continue;
            }
            let bytes = self.0.connection.immediate_transaction::<_, Error, _>(|c| {
                let bytes = diesel::sql_query("SELECT COALESCE(SUM(committed_encoded_bytes), 0) AS value FROM payload_parts WHERE revision_id = ?")
                    .bind::<BigInt, _>(id.value).get_result::<Number>(c)?.value;
                diesel::sql_query("DELETE FROM payload_chunks WHERE part_id IN (SELECT id FROM payload_parts WHERE revision_id = ?)")
                    .bind::<BigInt, _>(id.value).execute(c)?;
                // Retain raw length, part count and digests for exact recovery.
                diesel::update(parts::table.filter(parts::revision_id.eq(id.value)))
                    .set(parts::committed_encoded_bytes.eq(0)).execute(c)?;
                diesel::sql_query("UPDATE payload_cache SET evicted = 1 WHERE revision_id = ?")
                    .bind::<BigInt, _>(id.value).execute(c)?;
                Ok(bytes as u64)
            })?;
            remaining = remaining.saturating_sub(bytes);
            accounting.cached_bytes = accounting.cached_bytes.saturating_sub(bytes);
        }
        let pending = full_batch && remaining > target;
        accounting.eviction = pending.then_some((after, target));
        Ok(pending)
    }

    /// Validate the entire bounded result before atomically restoring any bytes.
    pub(crate) fn restore(&mut self, key: &RevisionKey, bodies: &[Vec<u8>]) -> Result<()> {
        if self.recovery(key)?.is_none() {
            return Err(invalid("payload is not evicted"));
        }
        let manifest = Reader::open_from_repository(&mut self.0, key)?;
        if bodies.len() != manifest.len()
            || bodies.iter().map(Vec::len).sum::<usize>() > CAPTURE_LIMIT
        {
            return Err(invalid("replayed payload shape differs"));
        }
        let mut batch = Vec::new();
        let mut compressor = zstd::bulk::Compressor::new(1)?;
        for ((id, length, digest), body) in manifest.iter().zip(bodies) {
            if body.len() as i64 != *length || Sha256::digest(body).as_slice() != digest.as_slice()
            {
                return Err(invalid("replayed payload checksum differs"));
            }
            for (index, bytes) in body.chunks(PAGE).enumerate() {
                let compressed = compressor.compress(bytes)?;
                let saving = bytes.len().saturating_sub(compressed.len());
                let (codec, encoded_bytes) = if saving >= 64 && saving * 10 >= bytes.len() {
                    ("zstd-v1", compressed)
                } else {
                    ("raw-v1", bytes.to_vec())
                };
                batch.push(Chunk {
                    part_id: *id,
                    chunk_index: index as i64,
                    raw_offset: (index * PAGE) as i64,
                    raw_length: bytes.len() as i64,
                    codec: codec.into(),
                    raw_sha256: Sha256::digest(bytes).to_vec(),
                    encoded_bytes,
                });
            }
        }
        let required = batch
            .iter()
            .map(|c| c.encoded_bytes.len() as u64)
            .sum::<u64>()
            * 3;
        if fs2::available_space(&self.1)? < quota::MAINTENANCE_RESERVE.saturating_add(required) {
            return Err(invalid("insufficient disk space to restore payload"));
        }
        let mut accounting = self.0.accounting.lock().unwrap();
        self.0.connection.immediate_transaction::<_, Error, _>(|c| {
            for batch in batch.chunks(BATCH) {
                diesel::insert_into(chunks::table).values(batch).execute(c)?;
            }
            for (id, _, _) in &manifest {
                let encoded: i64 = batch.iter().filter(|c| c.part_id == *id).map(|c| c.encoded_bytes.len() as i64).sum();
                let count = batch.iter().filter(|c| c.part_id == *id).count() as i64;
                diesel::update(parts::table.find(id)).set((parts::committed_encoded_bytes.eq(encoded), parts::chunk_count.eq(count))).execute(c)?;
            }
            diesel::sql_query("UPDATE payload_cache SET evicted = 0, accessed_at_ms = ? WHERE revision_id = (SELECT id FROM payload_revisions WHERE payload_id = ? AND revision = ?)")
                .bind::<BigInt, _>(chrono::Utc::now().timestamp_millis()).bind::<Text, _>(&key.payload_id).bind::<BigInt, _>(integer(key.revision)?).execute(c)?;
            Ok(())
        })?;
        accounting.cached_bytes += batch
            .iter()
            .map(|c| c.encoded_bytes.len() as u64)
            .sum::<u64>();
        Ok(())
    }
}

impl Reader {
    fn open_from_repository(
        repo: &mut Repository,
        key: &RevisionKey,
    ) -> Result<Vec<(i64, i64, Vec<u8>)>> {
        Ok(parts::table
            .inner_join(revisions::table)
            .filter(revisions::payload_id.eq(&key.payload_id))
            .filter(revisions::revision.eq(integer(key.revision)?))
            .order(parts::part_index)
            .select((
                parts::id,
                parts::committed_raw_bytes,
                parts::final_sha256.assume_not_null(),
            ))
            .load(&mut repo.connection)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> RevisionKey {
        RevisionKey {
            workspace_id: "workspace".into(),
            conversation_id: "conversation".into(),
            item_id: id.into(),
            payload_id: id.into(),
            revision: 1,
        }
    }
    fn unknown() -> Recovery {
        Recovery {
            agent_id: None,
            session_id: None,
            tool_call_id: None,
            cwd: None,
        }
    }

    #[test]
    fn payload_cache_automatic_eviction_obeys_budget_and_recent_access() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(home.path()).unwrap();
        cache.set_limit_gb(1).unwrap();
        for index in 0..10 {
            budget_entry(&mut cache, home.path(), index);
        }
        assert_eq!(cache.reconcile().unwrap(), 1_000_000_000);
        assert!(!cache.evict(&HashSet::new(), false).unwrap());
        assert!(cache.recovery(&key("budget-0")).unwrap().is_none());
        budget_entry(&mut cache, home.path(), 10);
        cache.touch(&key("budget-0")).unwrap();
        assert!(!cache.evict(&HashSet::new(), false).unwrap());
        assert_eq!(cache.usage().unwrap(), 900_000_000);
        assert!(cache.recovery(&key("budget-0")).unwrap().is_none());
        for index in [1, 2] {
            assert!(cache
                .recovery(&key(&format!("budget-{index}")))
                .unwrap()
                .is_some());
        }
        assert!(cache.recovery(&key("budget-3")).unwrap().is_none());
        assert!(!cache.evict(&HashSet::new(), false).unwrap());
        assert_eq!(cache.reconcile().unwrap(), 900_000_000);
    }

    #[test]
    fn payload_cache_automatic_eviction_resumes_after_protection_release() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(home.path()).unwrap();
        cache.set_limit_gb(1).unwrap();
        let mut protected = HashSet::new();
        for index in 0..11 {
            budget_entry(&mut cache, home.path(), index);
            protected.insert(key(&format!("budget-{index}")));
        }
        assert!(!cache.evict(&protected, false).unwrap());
        assert_eq!(cache.usage().unwrap(), 1_100_000_000);
        protected.remove(&key("budget-0"));
        protected.remove(&key("budget-1"));
        assert!(!cache.evict(&protected, false).unwrap());
        assert_eq!(cache.usage().unwrap(), 900_000_000);
        assert!(cache.recovery(&key("budget-0")).unwrap().is_some());
        assert!(cache.recovery(&key("budget-1")).unwrap().is_some());
        for index in 2..11 {
            assert!(cache
                .recovery(&key(&format!("budget-{index}")))
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn payload_cache_automatic_eviction_keeps_target_across_batches() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(home.path()).unwrap();
        cache.set_limit_gb(1).unwrap();
        for index in 0..138 {
            budget_entry(&mut cache, home.path(), index);
        }
        assert!(cache.evict(&HashSet::new(), false).unwrap());
        assert_eq!(cache.usage().unwrap(), 1_000_000_000);
        // Getting below the trigger threshold must not abandon the 90% target.
        assert!(!cache.evict(&HashSet::new(), false).unwrap());
        assert_eq!(cache.usage().unwrap(), 900_000_000);
        assert!(cache.recovery(&key("budget-128")).unwrap().is_some());
        assert!(cache.recovery(&key("budget-129")).unwrap().is_none());
    }

    // Model large encoded-body accounting with small real SQLite bodies so
    // threshold tests do not write a gigabyte. Production eviction reads these
    // committed byte counters, deletes real chunks, and retains the manifests.
    fn budget_entry(cache: &mut Cache, home: &std::path::Path, index: i64) {
        let name = format!("budget-{index}");
        super::super::tests::capture(home, &name, 1, b"x");
        diesel::sql_query("UPDATE payload_parts SET committed_encoded_bytes = 100000000 WHERE revision_id IN (SELECT id FROM payload_revisions WHERE payload_id = ?)")
            .bind::<Text, _>(&name).execute(&mut cache.0.connection).unwrap();
        cache.classify(&key(&name), &unknown()).unwrap();
        diesel::sql_query("UPDATE payload_cache SET accessed_at_ms = ? WHERE revision_id IN (SELECT id FROM payload_revisions WHERE payload_id = ?)")
            .bind::<BigInt, _>(index).bind::<Text, _>(&name).execute(&mut cache.0.connection).unwrap();
    }

    #[test]
    fn payload_cache_eviction_keeps_manifests_and_durable_unclassified_content() {
        let home = tempfile::tempdir().unwrap();
        super::super::tests::capture(home.path(), "tool", 1, "— café".as_bytes());
        super::super::tests::capture(home.path(), "image", 1, b"durable");
        let mut cache = Cache::open(home.path()).unwrap();
        diesel::update(
            parts::table.filter(
                parts::revision_id.eq_any(
                    revisions::table
                        .filter(revisions::payload_id.eq("image"))
                        .select(revisions::id),
                ),
            ),
        )
        .set(parts::format.eq("image/png"))
        .execute(&mut cache.0.connection)
        .unwrap();
        cache.classify(&key("image"), &unknown()).unwrap();
        cache.classify(&key("tool"), &unknown()).unwrap();
        assert_eq!(cache.limit_gb().unwrap(), 5);
        assert!(cache.usage().unwrap() > 0);
        cache.evict(&HashSet::from([key("tool")]), true).unwrap();
        assert!(cache.recovery(&key("tool")).unwrap().is_none());
        cache.evict(&HashSet::new(), true).unwrap();
        assert_eq!(cache.usage().unwrap(), 0);
        assert!(cache.recovery(&key("tool")).unwrap().is_some());
        let mut reader = Reader::open(home.path()).unwrap();
        assert_eq!(
            reader.manifest(&key("tool")).unwrap().parts[0].committed_raw_bytes,
            9
        );
        assert_eq!(
            reader.read_sealed(&key("image"), 0, 7, 0, PAGE).unwrap(),
            b"durable"
        );
        assert!(cache
            .restore(&key("tool"), &[b"different".to_vec()])
            .is_err());
        assert_eq!(cache.usage().unwrap(), 0);
        cache
            .restore(&key("tool"), &["— café".as_bytes().to_vec()])
            .unwrap();
        assert_eq!(
            reader.read_sealed(&key("tool"), 0, 9, 0, PAGE).unwrap(),
            "— café".as_bytes()
        );
        let usage = cache.usage().unwrap();
        assert_eq!(cache.reconcile().unwrap(), usage);
        cache.set_limit_gb(12).unwrap();
        assert!(cache.set_limit_gb(0).is_err());
        assert!(cache.set_limit_gb(u64::MAX).is_err());
        drop(reader);
        drop(cache);
        let mut cache = Cache::open(home.path()).unwrap();
        assert_eq!(cache.limit_gb().unwrap(), 12);
        assert_eq!(cache.usage().unwrap(), usage);
    }

    #[test]
    fn payload_cache_restores_multiple_chunks_and_reconciles_deletion() {
        let home = tempfile::tempdir().unwrap();
        let bytes = super::super::tests::fixture(2 * PAGE + 17, false);
        let (id, _) = super::super::tests::capture(home.path(), "tool", 1, &bytes);
        let mut cache = Cache::open(home.path()).unwrap();
        cache.classify(&key("tool"), &unknown()).unwrap();
        cache.evict(&HashSet::new(), true).unwrap();
        cache
            .restore(&key("tool"), std::slice::from_ref(&bytes))
            .unwrap();
        let mut reader = Reader::open(home.path()).unwrap();
        assert_eq!(
            reader
                .read_sealed(&key("tool"), 0, bytes.len() as u64, PAGE as u64 - 10, PAGE)
                .unwrap(),
            bytes[PAGE - 10..2 * PAGE - 10]
        );
        let mut repo = Repository::open(home.path()).unwrap();
        repo.delete(id).unwrap();
        assert_eq!(cache.usage().unwrap(), 0);
        assert_eq!(cache.reconcile().unwrap(), 0);
    }

    #[test]
    fn payload_cache_eviction_batches_skip_protections_and_keep_lru_order() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(home.path()).unwrap();
        for index in 0..130 {
            let name = format!("tool-{index}");
            let (id, part) = cache.0.create(&name, 1).unwrap();
            cache
                .0
                .append(&[Chunk {
                    part_id: part,
                    chunk_index: 0,
                    raw_offset: 0,
                    raw_length: 1,
                    codec: "raw-v1".into(),
                    raw_sha256: Sha256::digest(b"x").to_vec(),
                    encoded_bytes: b"x".to_vec(),
                }])
                .unwrap();
            cache.0.seal(id, part, 1, &Sha256::digest(b"x")).unwrap();
            cache.classify(&key(&name), &unknown()).unwrap();
        }
        let protected = HashSet::from([key("tool-0")]);
        // Access times are milliseconds: a touch in the same one as the last
        // seal would tie, and the tie breaks by id, ahead of the rest.
        std::thread::sleep(std::time::Duration::from_millis(2));
        cache.touch(&key("tool-1")).unwrap();
        assert!(cache.evict(&protected, true).unwrap());
        assert!(cache.recovery(&key("tool-0")).unwrap().is_none());
        assert!(cache.recovery(&key("tool-1")).unwrap().is_none());
        assert!(!cache.evict(&protected, true).unwrap());
        assert_eq!(cache.usage().unwrap(), 1);
        assert!(cache.recovery(&key("tool-1")).unwrap().is_some());
        assert!(!cache.evict(&HashSet::new(), true).unwrap());
        assert_eq!(cache.usage().unwrap(), 0);
    }

    #[test]
    fn payload_cache_initial_schema_supports_results_without_recovery_identifiers() {
        let home = tempfile::tempdir().unwrap();
        {
            let mut repo = Repository::open(home.path()).unwrap();
            assert_eq!(repo.connection.applied_migrations().unwrap().len(), 1);
        }
        super::super::tests::capture(home.path(), "legacy", 1, b"legacy");
        let mut cache = Cache::open(home.path()).unwrap();
        assert_eq!(cache.limit_gb().unwrap(), 5);
        cache.classify(&key("legacy"), &unknown()).unwrap();
        cache.evict(&HashSet::new(), true).unwrap();
        assert!(cache.recovery(&key("legacy")).unwrap().is_some());
        assert!(Reader::open(home.path())
            .unwrap()
            .manifest(&key("legacy"))
            .is_ok());
    }
}
