use super::error::StorageError;
use diesel::connection::SimpleConnection;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Pool};
use diesel::sql_types::{BigInt, Text};
use diesel::sqlite::SqliteConnection;
use diesel::{Connection, QueryableByName, RunQueryDsl};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use fs2::FileExt;
use std::env;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
const POOL_SIZE: u32 = 4;

/// The newest migration this binary embeds.
///
/// Derived from the embedded bundle rather than hard-coded: a constant has to be
/// bumped in lockstep with every new migration directory, and forgetting makes
/// each database reject itself as "created by a newer Ottam version" on the
/// first reopen after upgrading.
fn latest_migration_version() -> String {
    diesel::migration::MigrationSource::<diesel::sqlite::Sqlite>::migrations(&MIGRATIONS)
        .ok()
        .and_then(|migrations| {
            migrations
                .iter()
                .map(|migration| migration.name().version().as_owned())
                .max()
        })
        .map(|version| version.to_string())
        .unwrap_or_default()
}

#[derive(QueryableByName)]
struct IntegerValue {
    #[diesel(sql_type = BigInt)]
    value: i64,
}

#[derive(QueryableByName)]
pub(crate) struct TextValue {
    #[diesel(sql_type = Text)]
    pub(crate) value: String,
}

#[derive(QueryableByName)]
struct CheckpointValue {
    #[diesel(sql_type = BigInt)]
    busy: i64,
    #[diesel(sql_type = BigInt)]
    log: i64,
    #[diesel(sql_type = BigInt)]
    checkpointed: i64,
}

#[derive(Debug)]
struct CoreOwnerLock {
    _file: File,
}

impl CoreOwnerLock {
    fn acquire(home: &Path) -> Result<Self, StorageError> {
        let path = home.join("core.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| {
                StorageError::new(
                    "coreLockOpenFailed",
                    "failed to open the Core ownership lock",
                )
            })?;
        file.try_lock_exclusive().map_err(|_| {
            StorageError::new(
                "coreAlreadyRunning",
                "another ottam-core already owns this OTTAM_HOME",
            )
        })?;
        tracing::info!(lock_path = %path.display(), "Core ownership lock acquired");
        Ok(Self { _file: file })
    }
}

#[derive(Debug)]
struct ConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for ConnectionCustomizer {
    fn on_acquire(&self, connection: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        connection
            .batch_execute(
                "PRAGMA foreign_keys = ON;\
                 PRAGMA synchronous = NORMAL;\
                 PRAGMA busy_timeout = 5000;",
            )
            .map_err(diesel::r2d2::Error::QueryError)
    }
}

pub(crate) struct PayloadCacheRead {
    database: Database,
    key: super::payload::RevisionKey,
}

impl Drop for PayloadCacheRead {
    fn drop(&mut self) {
        let mut reads = self.database.inner.payload_cache_reads.lock().unwrap();
        let count = reads.get_mut(&self.key).expect("cache read lease");
        *count -= 1;
        if *count == 0 {
            reads.remove(&self.key);
        }
        drop(reads);
        self.database.payload_worker().notify_changed();
    }
}

struct DatabaseInner {
    home: PathBuf,
    path: PathBuf,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    permits: Arc<Semaphore>,
    transcript_orders:
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<Option<i64>>>>>,
    payload_worker: super::payload::worker::Worker,
    payload_cache_gate: std::sync::Mutex<()>,
    payload_cache_scan: tokio::sync::Mutex<(String, i64)>,
    payload_cache_reads:
        std::sync::Mutex<std::collections::HashMap<super::payload::RevisionKey, usize>>,
    payload_leases: std::sync::Mutex<std::collections::HashSet<super::payload::RevisionKey>>,
    _owner_lock: CoreOwnerLock,
}

#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

impl Database {
    pub fn open_from_environment() -> Result<Self, StorageError> {
        Self::open(ottam_home()?)
    }

    pub fn open(home: PathBuf) -> Result<Self, StorageError> {
        if home.exists() && !home.is_dir() {
            return Err(StorageError::new(
                "ottamHomeInvalid",
                "OTTAM_HOME points to a file",
            ));
        }
        std::fs::create_dir_all(&home).map_err(|_| {
            StorageError::new(
                "ottamHomeCreateFailed",
                "failed to create OTTAM_HOME",
            )
        })?;
        std::fs::create_dir_all(home.join("cache")).map_err(|_| {
            StorageError::new(
                "ottamHomeCreateFailed",
                "failed to create the cache directory",
            )
        })?;
        std::fs::create_dir_all(home.join("logs")).map_err(|_| {
            StorageError::new(
                "ottamHomeCreateFailed",
                "failed to create the logs directory",
            )
        })?;

        let owner_lock = CoreOwnerLock::acquire(&home)?;
        let path = home.join("ottam.sqlite");
        let database_url = path.to_string_lossy().into_owned();
        let mut bootstrap = SqliteConnection::establish(&database_url).map_err(|error| {
            StorageError::classify(
                error.to_string(),
                "storageUnavailable",
                "failed to open the Ottam database",
            )
        })?;
        // The busy handler goes first: a previous owner's connections can
        // still be closing (the last one checkpoints the WAL), and without it
        // the first pragma or write fails at once with "database is locked".
        bootstrap
            .batch_execute(
                "PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;",
            )
            .map_err(|error| {
                StorageError::classify(
                    error.to_string(),
                    "storageUnavailable",
                    "failed to configure the database",
                )
            })?;
        let journal_mode =
            diesel::sql_query("SELECT journal_mode AS value FROM pragma_journal_mode")
                .get_result::<TextValue>(&mut bootstrap)
                .map_err(StorageError::from)?
                .value;
        if journal_mode != "wal" {
            return Err(StorageError::new(
                "storageUnavailable",
                "SQLite refused WAL journal mode",
            ));
        }
        reject_ahead_of_binary(&mut bootstrap)?;
        // VACUUM INTO includes WAL content and creates a consistent backup;
        // copying only ottam.sqlite can silently omit committed history.
        if bootstrap
            .has_pending_migration(MIGRATIONS)
            .map_err(|error| {
                StorageError::classify(
                    error.to_string(),
                    "storageMigrationFailed",
                    "Could not inspect pending migrations",
                )
            })?
            && diesel::sql_query(
                "SELECT count(*) AS value FROM sqlite_schema WHERE name = 'conversation_runs'",
            )
            .get_result::<IntegerValue>(&mut bootstrap)?
            .value
                > 0
        {
            super::payload::backup::create(&home, &mut bootstrap).map_err(|error| {
                StorageError::classify(
                    error.to_string(),
                    "storageBackupFailed",
                    "Could not verify the coordinated payload backup",
                )
            })?;
            let mut backup = home.join(format!(
                "ottam-before-{}.sqlite",
                latest_migration_version()
            ));
            if backup.exists() {
                backup = home.join(format!(
                    "ottam-before-{}-{}.sqlite",
                    latest_migration_version(),
                    uuid::Uuid::now_v7()
                ));
            }
            if !backup.exists() {
                diesel::sql_query("VACUUM INTO ?")
                    .bind::<Text, _>(backup.to_string_lossy().as_ref())
                    .execute(&mut bootstrap)?;
                let mut check = SqliteConnection::establish(backup.to_string_lossy().as_ref())
                    .map_err(|error| {
                        StorageError::classify(
                            error.to_string(),
                            "storageBackupFailed",
                            "Could not verify migration backup",
                        )
                    })?;
                let integrity = diesel::sql_query(
                    "SELECT integrity_check AS value FROM pragma_integrity_check",
                )
                .get_result::<TextValue>(&mut check)?
                .value;
                if integrity != "ok" {
                    return Err(StorageError::new(
                        "storageBackupFailed",
                        "Migration backup failed its integrity check",
                    ));
                }
            }
        }
        let migration_started = Instant::now();
        tracing::info!(database_path = %path.display(), "database migration started");
        bootstrap
            .run_pending_migrations(MIGRATIONS)
            .map_err(|error| {
                StorageError::classify(
                    error.to_string(),
                    "storageMigrationFailed",
                    "failed to apply embedded database migrations",
                )
            })?;
        tracing::info!(
            database_path = %path.display(),
            elapsed_ms = migration_started.elapsed().as_millis() as u64,
            "database migrations complete"
        );
        drop_columns_removed_from_baseline(&mut bootstrap).map_err(|error| {
            StorageError::classify(
                error.to_string(),
                "storageMigrationFailed",
                "failed to drop columns removed from the baseline",
            )
        })?;

        super::payload::maintenance::reconcile_startup(&home, &mut bootstrap).map_err(|error| {
            StorageError::classify(
                error.to_string(),
                "storageUnavailable",
                "Payload reconciliation failed",
            )
        })?;
        let payload_worker =
            super::payload::worker::Worker::open_blocking(home.clone()).map_err(|error| {
                StorageError::classify(
                    error.to_string(),
                    "storageUnavailable",
                    "Payload worker could not start",
                )
            })?;
        let checkpoint_started = Instant::now();
        bootstrap.batch_execute("DELETE FROM transcript_checkpoint_bodies WHERE NOT EXISTS (SELECT 1 FROM transcript_checkpoint_heads WHERE transcript_checkpoint_heads.item_id = transcript_checkpoint_bodies.item_id AND transcript_checkpoint_heads.revision = transcript_checkpoint_bodies.revision);")?;
        bootstrap.batch_execute("DELETE FROM transcript_checkpoint_bodies WHERE byte_offset >= (SELECT body_byte_count FROM transcript_items WHERE id = transcript_checkpoint_bodies.item_id);")?;
        let checkpoint = diesel::sql_query("PRAGMA wal_checkpoint(PASSIVE)")
            .get_result::<CheckpointValue>(&mut bootstrap)
            .map_err(StorageError::from)?;
        tracing::info!(
            database_path = %path.display(),
            elapsed_ms = checkpoint_started.elapsed().as_millis() as u64,
            busy = checkpoint.busy,
            wal_frames = checkpoint.log,
            checkpointed_frames = checkpoint.checkpointed,
            "database checkpoint complete"
        );
        let manager = ConnectionManager::<SqliteConnection>::new(database_url);
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .connection_timeout(Duration::from_secs(5))
            .connection_customizer(Box::new(ConnectionCustomizer))
            .build(manager)
            .map_err(|_| {
                StorageError::new(
                    "storagePoolUnavailable",
                    "failed to create the database pool",
                )
            })?;
        // Close the bootstrap connection only after the pool holds connections.
        // Closing the last WAL connection tears down the -shm index, and the
        // pool's concurrent first connections then race its recovery, hitting
        // SQLITE_BUSY and an r2d2 retry delay on nearly every startup.
        drop(bootstrap);

        Ok(Self {
            inner: Arc::new(DatabaseInner {
                home,
                path,
                pool,
                permits: Arc::new(Semaphore::new(POOL_SIZE as usize)),
                transcript_orders: std::sync::Mutex::new(std::collections::HashMap::new()),
                payload_worker,
                payload_cache_gate: std::sync::Mutex::new(()),
                payload_cache_scan: tokio::sync::Mutex::new((String::new(), -1)),
                payload_cache_reads: std::sync::Mutex::new(std::collections::HashMap::new()),
                payload_leases: std::sync::Mutex::new(std::collections::HashSet::new()),
                _owner_lock: owner_lock,
            }),
        })
    }

    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    pub(crate) fn payload_worker(&self) -> &super::payload::worker::Worker {
        &self.inner.payload_worker
    }

    pub(crate) fn payload_cache_gate(&self) -> &std::sync::Mutex<()> {
        &self.inner.payload_cache_gate
    }

    pub(crate) fn payload_cache_scan(&self) -> &tokio::sync::Mutex<(String, i64)> {
        &self.inner.payload_cache_scan
    }

    pub(crate) fn payload_cache_read(&self, key: super::payload::RevisionKey) -> PayloadCacheRead {
        let _gate = self.payload_cache_gate().lock().unwrap();
        *self
            .inner
            .payload_cache_reads
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default() += 1;
        PayloadCacheRead {
            database: self.clone(),
            key,
        }
    }

    pub(crate) fn payload_cache_read_keys(&self) -> Vec<super::payload::RevisionKey> {
        self.inner
            .payload_cache_reads
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn payload_leases(
        &self,
    ) -> &std::sync::Mutex<std::collections::HashSet<super::payload::RevisionKey>> {
        &self.inner.payload_leases
    }

    /// The shared Core ordering boundary, keyed by Conversation rather than Run.
    /// Reserve positions before the caller's admission/checkpoint transaction.
    /// Failed transactions leave harmless holes; committed positions never move.
    pub(crate) async fn run_transcript<T, F>(
        &self,
        conversation: String,
        operation: &'static str,
        reserve: usize,
        action: F,
    ) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteConnection, i64) -> Result<T, StorageError> + Send + 'static,
    {
        let first = self
            .reserve_transcript_positions(&conversation, reserve)
            .await?;
        self.run(operation, move |connection| action(connection, first))
            .await
    }

    /// Initialization may read SQLite, but subsequent reservations hold only a
    /// short memory lock. Admission never holds this lock while committing.
    pub(crate) async fn reserve_transcript_positions(
        &self,
        conversation: &str,
        reserve: usize,
    ) -> Result<i64, StorageError> {
        let order = {
            let mut orders = self
                .inner
                .transcript_orders
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            orders
                .entry(conversation.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                .clone()
        };
        let mut order = order.lock().await;
        if order.is_none() {
            let conversation = conversation.to_owned();
            *order = Some(
                self.run("transcript.initializeOrder", move |connection| {
                    crate::storage::repositories::transcript::TranscriptRepository::next_position(
                        connection,
                        &conversation,
                    )
                })
                .await?,
            );
        }
        let first = order.expect("initialized order");
        *order = Some(first.checked_add(reserve as i64).ok_or_else(|| {
            StorageError::new("storageConstraint", "Transcript positions exhausted")
        })?);
        Ok(first)
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub async fn run<T, F>(&self, operation: &'static str, action: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteConnection) -> Result<T, StorageError> + Send + 'static,
    {
        let queued_at = Instant::now();
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|_| {
                StorageError::new("storageUnavailable", "the database gateway is closed")
            })?;
        let queue_ms = queued_at.elapsed().as_millis() as u64;
        let pool = self.inner.pool.clone();
        let result = tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let mut connection = pool.get().map_err(|_| {
                tracing::warn!(operation, queue_ms, "database pool checkout timed out");
                StorageError::new(
                    "storagePoolUnavailable",
                    "timed out waiting for a database connection",
                )
                .recoverable(true)
            })?;
            let result = action(&mut connection);
            tracing::debug!(
                operation,
                queue_ms,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "database operation complete"
            );
            // Clients only see "the database operation failed"; the log keeps
            // the cause, such as a column a migration never added.
            if let Err(error) = &result {
                if error.is_unexpected() {
                    tracing::warn!(
                        operation,
                        error_kind = error.kind,
                        source = error.source_detail().unwrap_or("none"),
                        "database operation failed"
                    );
                }
            }
            drop(permit);
            result
        })
        .await
        .map_err(|_| StorageError::new("storageUnavailable", "database task failed"))?;
        result
    }
}

pub fn recorded_transaction<T, F>(
    connection: &mut SqliteConnection,
    operation: &'static str,
    action: F,
) -> Result<T, StorageError>
where
    F: FnOnce(&mut SqliteConnection) -> Result<T, StorageError>,
{
    let transaction_started = Instant::now();
    connection
        .batch_execute("BEGIN IMMEDIATE")
        .map_err(StorageError::from)?;
    match action(connection) {
        Ok(value) => {
            let commit_started = Instant::now();
            if let Err(error) = connection.batch_execute("COMMIT") {
                let _ = connection.batch_execute("ROLLBACK");
                return Err(StorageError::from(error));
            }
            tracing::debug!(
                operation,
                commit_ms = commit_started.elapsed().as_millis() as u64,
                transaction_ms = transaction_started.elapsed().as_millis() as u64,
                "database transaction committed"
            );
            Ok(value)
        }
        Err(error) => {
            let rollback_started = Instant::now();
            let rollback_result = connection.batch_execute("ROLLBACK");
            tracing::warn!(
                operation,
                error_kind = error.kind,
                rollback_ms = rollback_started.elapsed().as_millis() as u64,
                transaction_ms = transaction_started.elapsed().as_millis() as u64,
                rollback_failed = rollback_result.is_err(),
                "database transaction rolled back"
            );
            Err(error)
        }
    }
}

/// Read several statements against a single snapshot.
///
/// SQLite wraps every bare statement in its own implicit transaction, so a
/// composed read taken outside one straddles concurrent commits: the pages of
/// `conversation.get` are separate `SELECT`s, and a run finalized between two of
/// them is reported completed next to a message page taken before its assistant
/// reply — a reply the writer put in the very same transaction as the status.
/// `BEGIN DEFERRED` takes the WAL read snapshot at the first statement and holds
/// it to the end, without ever blocking a writer.
pub fn read_snapshot<T, F>(connection: &mut SqliteConnection, action: F) -> Result<T, StorageError>
where
    F: FnOnce(&mut SqliteConnection) -> Result<T, StorageError>,
{
    connection
        .batch_execute("BEGIN DEFERRED")
        .map_err(StorageError::from)?;
    let result = action(connection);
    // A read holds nothing to commit, so the snapshot is simply released; a
    // rollback that fails cannot lose data the way a failed commit would.
    if let Err(error) = connection.batch_execute("ROLLBACK") {
        tracing::warn!(%error, "failed to release a database read snapshot");
    }
    result
}
/// Columns the baseline once created and no longer does. A home that ran the
/// earlier baseline keeps them, and Diesel never reruns a version it recorded,
/// so startup drops them here; `content_digest` is NOT NULL and would fail
/// every attachment insert. Neither column was ever read.
const COLUMNS_REMOVED_FROM_BASELINE: [(&str, &str); 2] = [
    ("mcp_servers", "oauth_json"),
    ("conversation_attachments", "content_digest"),
];

fn drop_columns_removed_from_baseline(
    connection: &mut SqliteConnection,
) -> Result<(), diesel::result::Error> {
    for (table, column) in COLUMNS_REMOVED_FROM_BASELINE {
        let present = diesel::sql_query(
            "SELECT COUNT(*) AS value FROM pragma_table_info(?) WHERE name = ?",
        )
        .bind::<Text, _>(table)
        .bind::<Text, _>(column)
        .get_result::<IntegerValue>(connection)?
        .value
            > 0;
        if present {
            connection.batch_execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"))?;
            tracing::info!(table, column, "dropped a column removed from the baseline");
        }
    }
    Ok(())
}

fn reject_ahead_of_binary(connection: &mut SqliteConnection) -> Result<(), StorageError> {
    let migration_table_exists = diesel::sql_query(
        "SELECT COUNT(*) AS value FROM sqlite_schema \
         WHERE type = 'table' AND name = '__diesel_schema_migrations'",
    )
    .get_result::<IntegerValue>(connection)
    .map_err(StorageError::from)?
    .value
        != 0;
    if !migration_table_exists {
        return Ok(());
    }

    let versions = diesel::sql_query(
        "SELECT version AS value FROM __diesel_schema_migrations ORDER BY version",
    )
    .load::<TextValue>(connection)
    .map_err(StorageError::from)?;
    let latest = latest_migration_version();
    if versions
        .iter()
        .any(|version| version.value.as_str() > latest.as_str())
    {
        return Err(StorageError::new(
            "storageMigrationFailed",
            "the database was created by a newer Ottam version",
        ));
    }
    Ok(())
}

pub fn ottam_home() -> Result<PathBuf, StorageError> {
    if let Ok(configured) = env::var("OTTAM_HOME") {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let home = env::var("HOME")
        .map_err(|_| StorageError::new("homeUnset", "HOME is not set").recoverable(true))?;
    Ok(PathBuf::from(home).join(".ottam"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn wal_connection(path: &Path) -> SqliteConnection {
        let mut connection =
            SqliteConnection::establish(&path.to_string_lossy()).expect("opens the database");
        connection
            // The timeout first, as `Database::open` does: switching the
            // journal mode can meet a lock the last open is still releasing.
            .batch_execute("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;")
            .expect("configures WAL");
        connection
    }

    fn count(connection: &mut SqliteConnection) -> i64 {
        diesel::sql_query("SELECT COUNT(*) AS value FROM snapshot_probe")
            .get_result::<IntegerValue>(connection)
            .expect("counts rows")
            .value
    }

    /// A reopen waits out a write lock another connection still holds, as when
    /// the previous owner's last connection is closing, instead of failing.
    #[test]
    fn reopening_waits_for_a_held_write_lock() {
        let directory = TempDir::new().expect("creates a home");
        drop(Database::open(directory.path().to_owned()).expect("first open"));
        let mut holder = wal_connection(&directory.path().join("ottam.sqlite"));
        holder
            .batch_execute("BEGIN IMMEDIATE")
            .expect("takes the write lock");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            holder.batch_execute("COMMIT").expect("releases the lock");
        });
        Database::open(directory.path().to_owned()).expect("reopen waits for the lock");
        release.join().unwrap();
    }

    /// A home whose schema drifted from its migrations fails with a generic
    /// message; `run` logs the SQLite cause, which names the missing column.
    #[test]
    fn a_missing_column_is_unexpected_and_keeps_its_cause() {
        let directory = TempDir::new().expect("creates a database directory");
        let mut connection = wal_connection(&directory.path().join("drift.sqlite"));
        connection
            .batch_execute("CREATE TABLE snapshot_probe (id INTEGER PRIMARY KEY)")
            .expect("creates the probe table");
        let Err(error) = diesel::sql_query("SELECT missing_column AS value FROM snapshot_probe")
            .get_result::<IntegerValue>(&mut connection)
        else {
            panic!("the column does not exist");
        };
        let error = StorageError::from(error);
        assert_eq!(error.kind, "storageUnavailable");
        assert!(error.is_unexpected());
        assert_eq!(
            error.source_detail(),
            Some("no such column: missing_column")
        );
        assert!(!StorageError::not_found("record").is_unexpected());

        // Diesel's message about a value it could not read may quote it.
        let unreadable = StorageError::from(diesel::result::Error::DeserializationError(Box::new(
            std::io::Error::other("unexpected variant 'secret-value'"),
        )));
        assert!(unreadable.is_unexpected());
        assert!(!unreadable
            .source_detail()
            .is_some_and(|detail| detail.contains("secret-value")));
    }

    /// The property `conversation.get` leans on: statements inside one snapshot
    /// agree with each other even when a writer commits between them.
    #[test]
    fn read_snapshot_hides_a_commit_that_lands_mid_read() {
        let directory = TempDir::new().expect("creates a database directory");
        let path = directory.path().join("snapshot.sqlite");
        let mut reader = wal_connection(&path);
        reader
            .batch_execute("CREATE TABLE snapshot_probe (id INTEGER PRIMARY KEY)")
            .expect("creates the probe table");
        let mut writer = wal_connection(&path);

        let observed = read_snapshot(&mut reader, |reader| {
            let before = count(reader);
            writer
                .batch_execute("INSERT INTO snapshot_probe (id) VALUES (1)")
                .expect("writes during the read");
            Ok((before, count(reader)))
        })
        .expect("reads a snapshot");

        assert_eq!(observed, (0, 0), "a snapshot must not see a later commit");
        // The snapshot is released with the closure, so the same connection now
        // sees the row; a read transaction left open would pin the WAL forever.
        assert_eq!(count(&mut reader), 1);
    }

    /// Without the snapshot each statement takes its own, which is exactly the
    /// torn read the helper exists to prevent.
    #[test]
    fn bare_reads_straddle_a_commit() {
        let directory = TempDir::new().expect("creates a database directory");
        let path = directory.path().join("bare.sqlite");
        let mut reader = wal_connection(&path);
        reader
            .batch_execute("CREATE TABLE snapshot_probe (id INTEGER PRIMARY KEY)")
            .expect("creates the probe table");
        let mut writer = wal_connection(&path);

        let before = count(&mut reader);
        writer
            .batch_execute("INSERT INTO snapshot_probe (id) VALUES (1)")
            .expect("writes between the reads");
        let after = count(&mut reader);

        assert_eq!((before, after), (0, 1));
    }
}
