//! Private SQLite payload storage with bounded live capture.
pub(in crate::storage) mod backup;
pub(crate) mod cache;
pub(crate) mod maintenance;
mod manifest;
mod quota;
mod reader;
pub(crate) use manifest::{Manifest, RevisionKey};
#[cfg(test)]
mod cutover_tests;
mod schema;
#[cfg(test)]
mod tests;
pub(crate) mod worker;
#[cfg(test)]
mod worker_tests;

use diesel::{connection::SimpleConnection, prelude::*};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use schema::{payload_chunks as chunks, payload_parts as parts, payload_revisions as revisions};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const PAGE: usize = 65536;
const BATCH: usize = 16;
const CAPTURE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Revision {
    pub payload_id: String,
    pub revision: u64,
    pub workspace_id: String,
    pub conversation_id: String,
    pub item_id: String,
    pub run_id: String,
    pub role: String,
    pub upstream_truncated: Option<bool>,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug)]
pub(crate) struct Part {
    pub name: String,
    pub format: String,
    pub source_path: Option<String>,
    pub unavailable_reason: Option<String>,
}

impl Revision {
    fn validate(&self) -> Result<()> {
        if self.parts.capacity() > 128 || self.revision > i64::MAX as u64 {
            return Err(invalid("revision bounds"));
        }
        for value in [
            &self.payload_id,
            &self.workspace_id,
            &self.conversation_id,
            &self.item_id,
            &self.run_id,
            &self.role,
        ] {
            if value.is_empty() || value.capacity() > 4096 {
                return Err(invalid("revision identity bounds"));
            }
        }
        for part in &self.parts {
            if part.name.capacity() > 4096
                || part.format.capacity() > 4096
                || part
                    .source_path
                    .as_ref()
                    .is_some_and(|s| s.capacity() > 4096)
                || part
                    .unavailable_reason
                    .as_ref()
                    .is_some_and(|s| s.capacity() > 4096)
            {
                return Err(invalid("part metadata bounds"));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Receipt {
    pub revision_id: i64,
    pub part_ids: Vec<i64>,
    pub capture_reason: Option<String>,
}

struct FinalPart {
    id: i64,
    length: u64,
    digest: [u8; 32],
}

/// Use from a blocking context. Each caller owns its connection and decode buffer.
pub(crate) struct Reader(Repository);
impl Reader {
    pub(crate) fn open(home: &std::path::Path) -> Result<Self> {
        Ok(Self(Repository::open_for_reads(home)?))
    }
    #[cfg(test)]
    pub(crate) fn read(&mut self, part: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.0.read(part, offset, length)
    }
}

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/storage/payload/migrations");
pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;
pub(crate) type Result<T> = std::result::Result<T, Error>;
fn invalid(message: &str) -> Error {
    std::io::Error::other(message).into()
}
fn integer(value: u64) -> Result<i64> {
    Ok(value.try_into()?)
}

#[derive(Queryable, Selectable, Insertable, Debug, Clone)]
#[diesel(table_name = chunks)]
struct Chunk {
    part_id: i64,
    chunk_index: i64,
    raw_offset: i64,
    raw_length: i64,
    codec: String,
    raw_sha256: Vec<u8>,
    encoded_bytes: Vec<u8>,
}

#[cfg(test)]
#[derive(Default, Debug)]
struct Metrics {
    transactions: u64,
    chunks: u64,
    read_buffers: usize,
}

struct Repository {
    accounting: std::sync::Arc<std::sync::Mutex<cache::Accounting>>,
    connection: SqliteConnection,
    #[cfg(test)]
    after_read_lookup: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    metrics: Metrics,
    read_scratch: Vec<u8>,
    read_decoder: Option<zstd::bulk::Decompressor<'static>>,
}
impl Repository {
    fn open_for_reads(home: &std::path::Path) -> Result<Self> {
        let path = home.join("payloads.sqlite");
        if !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "payload database is not initialized",
            )
            .into());
        }
        let mut connection =
            SqliteConnection::establish(path.to_str().ok_or_else(|| invalid("path"))?)?;
        connection.batch_execute("PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA query_only=ON;")?;
        Ok(Self {
            accounting: cache::accounting(home, &mut connection)?,
            connection,
            #[cfg(test)]
            after_read_lookup: None,
            #[cfg(test)]
            metrics: Metrics::default(),
            read_scratch: Vec::new(),
            read_decoder: None,
        })
    }

    fn open(home: &std::path::Path) -> Result<Self> {
        let path = home.join("payloads.sqlite");
        let mut connection =
            SqliteConnection::establish(path.to_str().ok_or_else(|| invalid("path"))?)?;
        Self::configure_writer(&mut connection)?;
        connection.run_pending_migrations(MIGRATIONS)?;
        Ok(Self {
            accounting: cache::accounting(home, &mut connection)?,
            connection,
            #[cfg(test)]
            after_read_lookup: None,
            #[cfg(test)]
            metrics: Metrics::default(),
            read_scratch: Vec::new(),
            read_decoder: None,
        })
    }

    fn configure_writer(connection: &mut SqliteConnection) -> Result<()> {
        // A fresh cache/maintenance connection can overlap the payload worker.
        // Install the busy handler before any pragma that reads the WAL/header.
        connection.batch_execute("PRAGMA busy_timeout=5000;")?;
        connection.batch_execute("PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        #[derive(QueryableByName)]
        struct Mode {
            #[diesel(sql_type = diesel::sql_types::Text)]
            value: String,
        }
        let mode: Mode = diesel::sql_query("SELECT journal_mode AS value FROM pragma_journal_mode")
            .get_result(connection)?;
        if mode.value != "wal" {
            return Err(invalid(&format!(
                "payload storage requires WAL mode; SQLite selected {}",
                mode.value
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    fn create(&mut self, payload: &str, revision: u64) -> Result<(i64, i64)> {
        let receipt = self.create_revision(&Revision {
            payload_id: payload.into(),
            revision,
            workspace_id: "workspace".into(),
            conversation_id: "conversation".into(),
            item_id: payload.into(),
            run_id: "run".into(),
            role: "output".into(),
            upstream_truncated: None,
            parts: vec![Part {
                name: "Output".into(),
                format: "application/octet-stream".into(),
                source_path: None,
                unavailable_reason: None,
            }],
        })?;
        Ok((receipt.revision_id, receipt.part_ids[0]))
    }

    fn create_revision(&mut self, revision: &Revision) -> Result<Receipt> {
        revision.validate()?;
        let ids = self.connection.immediate_transaction::<_, Error, _>(|c| {
            diesel::insert_into(revisions::table)
                .values((
                    revisions::payload_id.eq(&revision.payload_id),
                    revisions::revision.eq(integer(revision.revision)?),
                    revisions::workspace_id.eq(&revision.workspace_id),
                    revisions::conversation_id.eq(&revision.conversation_id),
                    revisions::item_id.eq(&revision.item_id),
                    revisions::run_id.eq(&revision.run_id),
                    revisions::role.eq(&revision.role),
                    revisions::upstream_truncated.eq(revision.upstream_truncated.map(i32::from)),
                    revisions::state.eq("staging"),
                    revisions::created_at_ms.eq(chrono::Utc::now().timestamp_millis()),
                    revisions::updated_at_ms.eq(chrono::Utc::now().timestamp_millis()),
                ))
                .execute(c)?;
            let id: i64 = revisions::table
                .filter(revisions::payload_id.eq(&revision.payload_id))
                .filter(revisions::revision.eq(integer(revision.revision)?))
                .select(revisions::id)
                .first(c)?;
            let mut part_ids = Vec::with_capacity(revision.parts.len());
            for (index, part) in revision.parts.iter().enumerate() {
                diesel::insert_into(parts::table)
                    .values((
                        parts::revision_id.eq(id),
                        parts::part_index.eq(index as i32),
                        parts::name.eq(&part.name),
                        parts::format.eq(&part.format),
                        parts::source_path.eq(&part.source_path),
                        parts::unavailable_reason.eq(&part.unavailable_reason),
                    ))
                    .execute(c)?;
                let part = parts::table
                    .filter(parts::revision_id.eq(id))
                    .filter(parts::part_index.eq(index as i32))
                    .select(parts::id)
                    .first(c)?;
                part_ids.push(part);
            }
            Ok(Receipt {
                revision_id: id,
                part_ids,
                capture_reason: None,
            })
        })?;
        #[cfg(test)]
        {
            self.metrics.transactions += 1;
        }
        Ok(ids)
    }

    fn append(&mut self, batch: &[Chunk]) -> Result<()> {
        if batch.len() > BATCH
            || batch
                .iter()
                .map(|c| c.raw_length.max(0) as u64)
                .sum::<u64>()
                > (PAGE * BATCH) as u64
        {
            return Err(invalid("batch limit"));
        }
        for chunk in batch {
            if chunk.raw_length <= 0
                || chunk.raw_length > PAGE as i64
                || chunk.encoded_bytes.is_empty()
                || chunk.encoded_bytes.len() > PAGE
                || chunk.raw_sha256.len() != 32
                || !matches!(chunk.codec.as_str(), "raw-v1" | "zstd-v1")
            {
                return Err(invalid("invalid chunk bounds or codec"));
            }
        }
        self.connection.immediate_transaction::<_, Error, _>(|c| {
            // Load and advance each part once per batch. Chunk insertion and the
            // committed prefix still share this one atomic writer transaction.
            let ids: Vec<_> = batch.iter().map(|chunk| chunk.part_id).collect();
            let rows = parts::table
                .inner_join(revisions::table)
                .filter(parts::id.eq_any(&ids))
                .select((
                    parts::id,
                    revisions::state,
                    parts::chunk_count,
                    parts::committed_raw_bytes,
                    parts::unavailable_reason,
                ))
                .load::<(i64, String, i64, i64, Option<String>)>(c)?;
            let mut positions = std::collections::BTreeMap::new();
            for (id, state, count, bytes, unavailable) in rows {
                if state != "staging" || unavailable.is_some() {
                    return Err(invalid("sealed revision or unavailable part"));
                }
                positions.insert(id, (count, bytes, 0_i64));
            }
            for chunk in batch {
                let (count, bytes, encoded) = positions
                    .get_mut(&chunk.part_id)
                    .ok_or_else(|| invalid("payload part missing"))?;
                if chunk.chunk_index != *count || chunk.raw_offset != *bytes {
                    return Err(invalid("noncontiguous chunk"));
                }
                *count += 1;
                *bytes += chunk.raw_length;
                *encoded += chunk.encoded_bytes.len() as i64;
            }
            diesel::insert_into(chunks::table)
                .values(batch)
                .execute(c)?;
            for (id, (count, bytes, encoded)) in positions {
                diesel::update(parts::table.find(id))
                    .set((
                        parts::chunk_count.eq(count),
                        parts::committed_raw_bytes.eq(bytes),
                        parts::committed_encoded_bytes.eq(parts::committed_encoded_bytes + encoded),
                    ))
                    .execute(c)?;
            }
            Ok(())
        })?;
        #[cfg(test)]
        {
            self.metrics.transactions += 1;
            self.metrics.chunks += batch.len() as u64;
        }
        Ok(())
    }

    #[cfg(test)]
    fn seal(&mut self, revision: i64, part: i64, length: u64, digest: &[u8]) -> Result<()> {
        self.seal_revision(
            revision,
            &[FinalPart {
                id: part,
                length,
                digest: digest.try_into().map_err(|_| invalid("digest length"))?,
            }],
        )
    }

    #[cfg(test)]
    fn seal_revision(&mut self, revision: i64, manifests: &[FinalPart]) -> Result<()> {
        self.seal_with_loss(revision, manifests, None)
    }

    fn seal_with_loss(
        &mut self,
        revision: i64,
        manifests: &[FinalPart],
        reason: Option<&str>,
    ) -> Result<()> {
        self.connection.immediate_transaction::<_, Error, _>(|c| {
            let count: i64 = parts::table
                .filter(parts::revision_id.eq(revision))
                .count()
                .get_result(c)?;
            let unique: std::collections::HashSet<_> = manifests.iter().map(|p| p.id).collect();
            if count as usize != manifests.len()
                || unique.len() != manifests.len()
                || manifests
                    .iter()
                    .try_fold(0_u64, |total, p| total.checked_add(p.length))
                    .is_none_or(|total| total > CAPTURE_LIMIT as u64)
            {
                return Err(invalid("incomplete or oversized manifest"));
            }
            for manifest in manifests {
                let changed = diesel::update(
                    parts::table
                        .find(manifest.id)
                        .filter(parts::revision_id.eq(revision))
                        .filter(parts::committed_raw_bytes.eq(integer(manifest.length)?)),
                )
                .set(parts::final_sha256.eq(manifest.digest.as_slice()))
                .execute(c)?;
                if changed != 1 {
                    return Err(invalid("seal length or ownership mismatch"));
                }
            }
            let changed = diesel::update(
                revisions::table
                    .find(revision)
                    .filter(revisions::state.eq("staging")),
            )
            .set((
                revisions::state.eq("ready"),
                revisions::updated_at_ms.eq(chrono::Utc::now().timestamp_millis()),
                revisions::locally_truncated.eq(i32::from(reason.is_some())),
                revisions::capture_reason.eq(reason),
            ))
            .execute(c)?;
            if changed != 1 {
                return Err(invalid("already sealed"));
            }
            Ok(())
        })?;
        #[cfg(test)]
        {
            self.metrics.transactions += 1;
        }
        Ok(())
    }

    fn delete(&mut self, revision: i64) -> Result<()> {
        let mut accounting = self.accounting.lock().unwrap();
        let bytes = cache::revision_bytes(&mut self.connection, revision)?;
        diesel::delete(revisions::table.find(revision)).execute(&mut self.connection)?;
        accounting.cached_bytes = accounting.cached_bytes.saturating_sub(bytes);
        #[cfg(test)]
        {
            self.metrics.transactions += 1;
        }
        Ok(())
    }

    #[cfg(test)]
    fn read(&mut self, part: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.read_page(part, offset, length)
    }
}
