use super::*;
use ottam_protocol::{TranscriptPayload, TranscriptPayloadPart};
use diesel::sql_types::Text;

fn stored_payload() -> (tempfile::TempDir, RevisionKey, Vec<u8>) {
    let home = tempfile::tempdir().unwrap();
    let mut main =
        SqliteConnection::establish(home.path().join("ottam.sqlite").to_str().unwrap())
            .unwrap();
    // Main is current, so only the payload side can have anything pending.
    diesel_migrations::MigrationHarness::run_pending_migrations(
        &mut main,
        crate::storage::database::MIGRATIONS,
    )
    .unwrap();
    main.batch_execute("INSERT INTO workspaces VALUES ('workspace', 'Workspace', 0, 0, 0, NULL); INSERT INTO conversations (id, workspace_id, working_root_kind, title, created_at, updated_at) VALUES ('conversation', 'workspace', 'workspaceFiles', 'Conversation', 0, 0); INSERT INTO transcript_items (id, conversation_id, position, revision, kind, completion, created_at, updated_at) VALUES ('item', 'conversation', 0, 1, 'tool', 'complete', 0, 0);").unwrap();
    let key = RevisionKey {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: "item".into(),
        payload_id: "payload".into(),
        revision: 1,
    };
    let bytes = tests::fixture(PAGE + 177, false);
    let descriptor = TranscriptPayload {
        role: "output".into(),
        availability: "available".into(),
        reason: None,
        payload_id: Some(key.payload_id.clone()),
        byte_count: Some(bytes.len() as u64),
        upstream_truncated: Some(false),
        locally_truncated: false,
        revision: 1,
        parts: vec![TranscriptPayloadPart {
            name: "Output".into(),
            format: "text/plain".into(),
            path: Some("source.log".into()),
            byte_count: bytes.len() as u64,
            digest: Some(format!("{:x}", Sha256::digest(&bytes))),
            chunk_digests: Vec::new(),
        }],
    };
    diesel::sql_query("INSERT INTO transcript_payloads VALUES ('payload', 1, 'item', ?)")
        .bind::<Text, _>(serde_json::to_string(&descriptor).unwrap())
        .execute(&mut main)
        .unwrap();
    diesel::sql_query("INSERT INTO transcript_tools (item_id, metadata_json) VALUES ('item', ?)")
        .bind::<Text, _>(serde_json::json!({"payloads": [descriptor]}).to_string())
        .execute(&mut main)
        .unwrap();
    let worker = worker::Worker::open_blocking(home.path().to_owned()).unwrap();
    let mut metadata = worker_tests::metadata(&key.payload_id, key.revision, "run");
    metadata.item_id = key.item_id.clone();
    metadata.parts[0].source_path = Some("source.log".into());
    let output = worker.begin(metadata).unwrap();
    worker.enqueue(output, 0, bytes.clone()).unwrap();
    worker
        .complete(output)
        .unwrap()
        .blocking_recv()
        .unwrap()
        .unwrap();
    (home, key, bytes)
}

#[derive(QueryableByName)]
struct Json {
    #[diesel(sql_type = Text)]
    value: String,
}

#[test]
fn payload_restart_reclaims_abandoned_and_orphan_revisions_without_body_validation() {
    let home = tempfile::tempdir().unwrap();
    let database = crate::storage::Database::open(home.path().to_owned()).unwrap();
    let worker = database.payload_worker();
    let id = worker
        .begin(worker_tests::metadata("orphan", 1, "run"))
        .unwrap();
    worker.enqueue(id, 0, b"orphan".to_vec()).unwrap();
    worker
        .complete(id)
        .unwrap()
        .blocking_recv()
        .unwrap()
        .unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    repo.create("abandoned", 1).unwrap();
    repo.connection
        .batch_execute("UPDATE payload_chunks SET raw_sha256 = zeroblob(32);")
        .unwrap();
    drop(repo);
    drop(database);
    let _database = crate::storage::Database::open(home.path().to_owned()).unwrap();
    assert!(maintenance::Maintenance::open(home.path())
        .unwrap()
        .candidates(0)
        .unwrap()
        .is_empty());
}

#[test]
fn payload_active_reads_never_expose_uncommitted_or_restarted_staging_bytes() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    let (_, part) = repo.create("active", 1).unwrap();
    repo.append(&[Chunk {
        part_id: part,
        chunk_index: 0,
        raw_offset: 0,
        raw_length: 5,
        codec: "raw-v1".into(),
        raw_sha256: Sha256::digest(b"first").to_vec(),
        encoded_bytes: b"first".to_vec(),
    }])
    .unwrap();
    let key = RevisionKey {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: "active".into(),
        payload_id: "active".into(),
        revision: 1,
    };
    let mut reader = Reader::open(home.path()).unwrap();
    assert_eq!(reader.read_active(&key, 0, 5, 0, PAGE).unwrap(), b"first");
    assert!(reader.read_active(&key, 0, 6, 0, PAGE).is_err());
    assert!(reader.read_sealed(&key, 0, 5, 0, PAGE).is_err());
}

#[test]
fn payload_delete_refill_reuses_physical_capacity_and_releases_usage() {
    let home = tempfile::tempdir().unwrap();
    let bytes = tests::fixture(1024 * 1024, true);
    let mut sizes = Vec::new();
    for cycle in 0..12 {
        let worker = worker::Worker::open_blocking(home.path().to_owned()).unwrap();
        let id = worker
            .begin(worker_tests::metadata("refill", cycle + 1, "run"))
            .unwrap();
        worker.enqueue(id, 0, bytes.clone()).unwrap();
        worker
            .complete(id)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        let mut maintenance = maintenance::Maintenance::open(home.path()).unwrap();
        let candidates = maintenance.candidates(0).unwrap();
        assert_eq!(candidates.len(), 1);
        maintenance.remove(&candidates[0]).unwrap();
        maintenance.idle().unwrap();
        drop(worker);
        drop(maintenance);
        sizes.push(
            std::fs::metadata(home.path().join("payloads.sqlite"))
                .unwrap()
                .len(),
        );
        let mut repo = Repository::open(home.path()).unwrap();
        #[derive(QueryableByName)]
        struct Count {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            value: i64,
        }
        assert_eq!(
            diesel::sql_query("SELECT COUNT(*) AS value FROM payload_usage")
                .get_result::<Count>(&mut repo.connection)
                .unwrap()
                .value,
            0
        );
    }
    assert!(sizes[4..].iter().max().unwrap() - sizes[4..].iter().min().unwrap() <= 128 * 4096);
}

#[test]
fn payload_coordinated_backup_restores_committed_wal_and_discards_staging() {
    let (home, key, bytes) = stored_payload();
    let database = crate::storage::Database::open(home.path().to_owned()).unwrap();
    // Retain connections so WAL content remains present after quiescing the worker.
    let mut main =
        SqliteConnection::establish(home.path().join("ottam.sqlite").to_str().unwrap())
            .unwrap();
    let mut payload = Repository::open(home.path()).unwrap();
    drop(database);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.path().join("core.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    main.batch_execute("UPDATE conversations SET title = 'Backed up WAL title';")
        .unwrap();
    payload.create("unfinished-backup", 1).unwrap();
    assert!(home.path().join("ottam.sqlite-wal").is_file());
    assert!(home.path().join("payloads.sqlite-wal").is_file());
    let snapshot = backup::create(home.path(), &mut main).unwrap();
    backup::verify_snapshot(&snapshot).unwrap();
    let restored = tempfile::tempdir().unwrap();
    for name in ["ottam.sqlite", "payloads.sqlite"] {
        std::fs::copy(snapshot.join(name), restored.path().join(name)).unwrap();
    }
    let _restored_database = crate::storage::Database::open(restored.path().to_owned()).unwrap();
    let mut restored_main =
        SqliteConnection::establish(restored.path().join("ottam.sqlite").to_str().unwrap())
            .unwrap();
    assert_eq!(
        diesel::sql_query("SELECT title AS value FROM conversations")
            .get_result::<Json>(&mut restored_main)
            .unwrap()
            .value,
        "Backed up WAL title"
    );
    assert_eq!(
        Reader::open(restored.path())
            .unwrap()
            .read_sealed(&key, 0, bytes.len() as u64, 0, PAGE)
            .unwrap(),
        bytes[..PAGE]
    );
    assert_eq!(
        maintenance::Maintenance::open(restored.path())
            .unwrap()
            .candidates(0)
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_file(snapshot.join("payloads.sqlite")).unwrap();
    assert!(backup::verify_snapshot(&snapshot).is_err());
    assert!(!snapshot.join("payloads.sqlite").exists());
    drop(lock);
}

#[test]
fn payload_full_replacement_with_held_reader_fits_maintenance_reserve() {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    let home = tempfile::tempdir().unwrap();
    let worker = worker::Worker::open_blocking(home.path().to_owned()).unwrap();
    let bytes = tests::fixture(CAPTURE_LIMIT, true);
    let output = worker
        .begin(worker_tests::metadata("replace", 1, "run"))
        .unwrap();
    worker.enqueue(output, 0, bytes.clone()).unwrap();
    worker
        .complete(output)
        .unwrap()
        .blocking_recv()
        .unwrap()
        .unwrap();
    let size = |path: &std::path::Path| -> u64 {
        ["payloads.sqlite", "payloads.sqlite-wal"]
            .iter()
            .map(|n| std::fs::metadata(path.join(n)).map_or(0, |m| m.len()))
            .sum()
    };
    let before = size(home.path());
    let peak = Arc::new(AtomicU64::new(0));
    let observed = peak.clone();
    let path = home.path().to_owned();
    let mut reader = Reader::open(home.path()).unwrap();
    reader.0.after_read_lookup = Some(Box::new(move || {
        let mut replacement = tests::fixture(CAPTURE_LIMIT, true);
        replacement[0] ^= 1;
        let output = worker
            .begin(worker_tests::metadata("replace", 2, "run"))
            .unwrap();
        worker.enqueue(output, 0, replacement).unwrap();
        worker
            .complete(output)
            .unwrap()
            .blocking_recv()
            .unwrap()
            .unwrap();
        let mut maintenance = maintenance::Maintenance::open(&path).unwrap();
        let predecessor = maintenance
            .candidates(0)
            .unwrap()
            .into_iter()
            .find(|c| c.key.revision == 1)
            .unwrap();
        maintenance.remove(&predecessor).unwrap();
        maintenance.pressure_idle().unwrap();
        observed.store(size(&path), Ordering::Relaxed);
    }));
    let key = RevisionKey {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: "replace".into(),
        payload_id: "replace".into(),
        revision: 1,
    };
    assert_eq!(
        reader
            .read_sealed(&key, 0, CAPTURE_LIMIT as u64, 0, PAGE)
            .unwrap(),
        bytes[..PAGE]
    );
    let growth = peak.load(Ordering::Relaxed).saturating_sub(before);
    assert!(
        growth < quota::MAINTENANCE_RESERVE,
        "observed replacement/WAL growth: {growth}"
    );
    if let Some(report) = std::env::var_os("PAYLOAD_RESERVE_REPORT") {
        std::fs::write(report,serde_json::json!({"before_bytes":before,"held_reader_peak_bytes":peak.load(Ordering::Relaxed),"growth_bytes":growth,"reserve_bytes":quota::MAINTENANCE_RESERVE}).to_string()).unwrap();
    }
}

/// The payload migrations a later binary embeds: the shipped set plus one.
struct NextPayloadSchema;
struct UpgradeProbe;

impl std::fmt::Display for UpgradeProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("00000000000002_upgrade_probe")
    }
}
impl diesel::migration::MigrationName for UpgradeProbe {
    fn version(&self) -> diesel::migration::MigrationVersion<'_> {
        "00000000000002".into()
    }
}
impl diesel::migration::MigrationMetadata for UpgradeProbe {}
impl diesel::migration::Migration<diesel::sqlite::Sqlite> for UpgradeProbe {
    fn run(
        &self,
        connection: &mut dyn diesel::connection::BoxableConnection<diesel::sqlite::Sqlite>,
    ) -> diesel::migration::Result<()> {
        connection.batch_execute("CREATE TABLE upgrade_probe (id INTEGER PRIMARY KEY);")?;
        Ok(())
    }
    fn revert(
        &self,
        connection: &mut dyn diesel::connection::BoxableConnection<diesel::sqlite::Sqlite>,
    ) -> diesel::migration::Result<()> {
        connection.batch_execute("DROP TABLE upgrade_probe;")?;
        Ok(())
    }
    fn metadata(&self) -> &dyn diesel::migration::MigrationMetadata {
        self
    }
    fn name(&self) -> &dyn diesel::migration::MigrationName {
        self
    }
}
impl diesel::migration::MigrationSource<diesel::sqlite::Sqlite> for NextPayloadSchema {
    fn migrations(
        &self,
    ) -> diesel::migration::Result<Vec<Box<dyn diesel::migration::Migration<diesel::sqlite::Sqlite>>>>
    {
        let mut migrations =
            diesel::migration::MigrationSource::<diesel::sqlite::Sqlite>::migrations(&MIGRATIONS)?;
        migrations.push(Box::new(UpgradeProbe));
        Ok(migrations)
    }
}

fn connect(path: &std::path::Path) -> SqliteConnection {
    let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    connection
        .batch_execute("PRAGMA busy_timeout=5000;")
        .unwrap();
    connection
}

fn applied_versions(connection: &mut SqliteConnection) -> Vec<String> {
    diesel::sql_query("SELECT version AS value FROM __diesel_schema_migrations ORDER BY version")
        .load::<Json>(connection)
        .unwrap()
        .into_iter()
        .map(|row| row.value)
        .collect()
}

fn verified_backups(home: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home.join("backups")) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("verified").is_file())
        .collect()
}

#[test]
fn payload_schema_newer_than_the_binary_is_refused_before_either_database_migrates() {
    let (home, _, _) = stored_payload();
    connect(&home.path().join("payloads.sqlite"))
        .batch_execute(
            "INSERT INTO __diesel_schema_migrations (version) VALUES ('99999999999999');",
        )
        .unwrap();
    // Main is pending too, so a payload check that ran late would migrate or
    // back up main before refusing.
    connect(&home.path().join("ottam.sqlite"))
        .batch_execute("DELETE FROM __diesel_schema_migrations;")
        .unwrap();

    let error = crate::storage::Database::open(home.path().to_owned())
        .err()
        .expect("a newer payload schema must refuse startup");
    assert_eq!(error.kind, "storageMigrationFailed");
    assert_eq!(error.message, crate::storage::database::NEWER_PAYLOAD);

    let error = Repository::open(home.path())
        .err()
        .expect("a newer payload schema must refuse the writer");
    let error = error
        .downcast_ref::<crate::storage::StorageError>()
        .expect("the writer's refusal stays typed");
    assert_eq!(error.kind, "storageMigrationFailed");

    assert!(verified_backups(home.path()).is_empty());
    assert!(applied_versions(&mut connect(&home.path().join("ottam.sqlite"))).is_empty());
    assert_eq!(
        applied_versions(&mut connect(&home.path().join("payloads.sqlite"))),
        ["00000000000001", "99999999999999"]
    );
}

#[test]
fn payload_upgrade_backs_up_the_pair_and_the_restored_pair_keeps_committed_history() {
    let (home, key, bytes) = stored_payload();
    drop(crate::storage::Database::open(home.path().to_owned()).unwrap());
    // Bootstrap holds the Core ownership lock while it backs up.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.path().join("core.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut main = connect(&home.path().join("ottam.sqlite"));
    main.batch_execute(
        "PRAGMA journal_mode=WAL; UPDATE conversations SET title = 'Committed before upgrade';",
    )
    .unwrap();
    assert!(home.path().join("ottam.sqlite-wal").is_file());

    crate::storage::database::prepare_upgrade(home.path(), &mut main, &NextPayloadSchema).unwrap();
    let backups = verified_backups(home.path());
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].join("verified")).unwrap(),
        "ottam.sqlite\npayloads.sqlite\n"
    );
    backup::verify_snapshot(&backups[0]).unwrap();
    // Main had nothing pending, so no main-only snapshot either.
    assert!(!std::fs::read_dir(home.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("ottam-before-")));

    let mut upgraded = Repository::open_with(home.path(), NextPayloadSchema).unwrap();
    assert_eq!(
        applied_versions(&mut upgraded.connection),
        ["00000000000001", "00000000000002"]
    );
    drop(upgraded);
    crate::storage::database::prepare_upgrade(home.path(), &mut main, &NextPayloadSchema).unwrap();
    assert_eq!(
        verified_backups(home.path()).len(),
        1,
        "nothing pending, no new backup"
    );
    drop(main);
    drop(lock);

    // No automatic downgrade: the shipped binary refuses the upgraded pair.
    let error = crate::storage::Database::open(home.path().to_owned())
        .err()
        .expect("the shipped binary must refuse the upgraded payload schema");
    assert_eq!(error.kind, "storageMigrationFailed");

    let restored = tempfile::tempdir().unwrap();
    for name in ["ottam.sqlite", "payloads.sqlite"] {
        std::fs::copy(backups[0].join(name), restored.path().join(name)).unwrap();
    }
    let _restored_database = crate::storage::Database::open(restored.path().to_owned()).unwrap();
    assert_eq!(
        diesel::sql_query("SELECT title AS value FROM conversations")
            .get_result::<Json>(&mut connect(&restored.path().join("ottam.sqlite")))
            .unwrap()
            .value,
        "Committed before upgrade"
    );
    assert_eq!(
        applied_versions(&mut connect(&restored.path().join("payloads.sqlite"))),
        ["00000000000001"]
    );
    assert_eq!(
        Reader::open(restored.path())
            .unwrap()
            .read_sealed(&key, 0, bytes.len() as u64, 0, PAGE)
            .unwrap(),
        bytes[..PAGE]
    );
}

/// Only payload migrations pending: startup still takes the coordinated
/// backup first, and the failing payload migration stays typed.
#[test]
fn payload_only_pending_migration_is_backed_up_and_fails_typed() {
    let (home, _, _) = stored_payload();
    drop(crate::storage::Database::open(home.path().to_owned()).unwrap());
    // The tables stay but the payload baseline reads as pending, so startup
    // backs up and then fails to create them again.
    connect(&home.path().join("payloads.sqlite"))
        .batch_execute("DELETE FROM __diesel_schema_migrations;")
        .unwrap();

    let error = crate::storage::Database::open(home.path().to_owned())
        .err()
        .expect("the payload migration must fail");
    assert_eq!(error.kind, "storageMigrationFailed");
    assert_eq!(error.message, "failed to apply embedded payload migrations");

    let backups = verified_backups(home.path());
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(backups[0].join("verified")).unwrap(),
        "ottam.sqlite\npayloads.sqlite\n"
    );
    backup::verify_snapshot(&backups[0]).unwrap();
    let mut payload = connect(&home.path().join("payloads.sqlite"));
    assert!(applied_versions(&mut payload).is_empty());
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        value: i64,
    }
    assert_eq!(
        diesel::sql_query("SELECT COUNT(*) AS value FROM payload_revisions")
            .get_result::<Count>(&mut payload)
            .unwrap()
            .value,
        1,
        "the failed migration leaves the payload database in place"
    );
}
