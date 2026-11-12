use super::*;

fn key(payload: &str) -> RevisionKey {
    RevisionKey {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: payload.into(),
        payload_id: payload.into(),
        revision: 1,
    }
}

#[test]
fn payload_authorized_snapshot_survives_deletion_and_other_writes() {
    let home = tempfile::tempdir().unwrap();
    let bytes = fixture(150 * 1024, false);
    let (revision, _) = capture(home.path(), "snapshot", 1, &bytes);
    let mut reader = Reader::open(home.path()).unwrap();
    let path = home.path().to_path_buf();
    reader.0.after_read_lookup = Some(Box::new(move || {
        let mut writer = Repository::open(&path).unwrap();
        writer.delete(revision).unwrap();
        writer.create("another-output", 1).unwrap();
    }));
    assert_eq!(
        reader
            .read_sealed(&key("snapshot"), 0, bytes.len() as u64, 64000, PAGE)
            .unwrap(),
        bytes[64000..64000 + PAGE]
    );
    assert!(reader
        .read_sealed(&key("snapshot"), 0, bytes.len() as u64, 0, PAGE)
        .is_err());
    assert!(reader.0.metrics.read_buffers <= 3 * PAGE + 64);
}

#[test]
fn payload_manifest_and_pages_reject_wrong_identity_staging_lengths_and_corruption() {
    let home = tempfile::tempdir().unwrap();
    let (_, part) = capture(home.path(), "owned", 1, b"exact bytes");
    let mut reader = Reader::open(home.path()).unwrap();
    let correct = key("owned");
    for field in 0..5 {
        let mut wrong = correct.clone();
        match field {
            0 => wrong.workspace_id.push('x'),
            1 => wrong.conversation_id.push('x'),
            2 => wrong.item_id.push('x'),
            3 => wrong.payload_id.push('x'),
            _ => wrong.revision += 1,
        }
        assert!(reader.manifest(&wrong).is_err());
        assert!(reader.read_sealed(&wrong, 0, 11, 0, PAGE).is_err());
    }
    assert!(reader.read_sealed(&correct, 1, 11, 0, PAGE).is_err());
    assert!(reader.read_sealed(&correct, 0, 10, 0, PAGE).is_err());
    let mut repo = Repository::open(home.path()).unwrap();
    repo.create("staged", 1).unwrap();
    assert!(reader.manifest(&key("staged")).is_err());
    assert!(reader.read_sealed(&key("staged"), 0, 0, 0, PAGE).is_err());
    diesel::update(chunks::table.filter(chunks::part_id.eq(part)))
        .set(chunks::raw_sha256.eq(vec![0_u8; 32]))
        .execute(&mut repo.connection)
        .unwrap();
    // Metadata loading does not scan bodies; corruption is reported on the requested page.
    assert!(reader.manifest(&correct).is_ok());
    assert!(reader.read_sealed(&correct, 0, 11, 0, PAGE).is_err());
}

pub(super) fn fixture(size: usize, binary: bool) -> Vec<u8> {
    let mut state = 0x7a1234b9_u64;
    let text = "cargo build: compiling workspace ✓ 日本語\n".as_bytes();
    (0..size)
        .map(|i| {
            if binary {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            } else {
                text[i % text.len()]
            }
        })
        .collect()
}

pub(super) fn capture(home: &std::path::Path, id: &str, revision: u64, bytes: &[u8]) -> (i64, i64) {
    let (worker, _) = worker::testing::start(home);
    let output = worker
        .begin(worker_tests::metadata(id, revision, "run"))
        .unwrap();
    worker.enqueue(output, 0, bytes.to_vec()).unwrap();
    let receipt = worker
        .complete(output)
        .unwrap()
        .blocking_recv()
        .unwrap()
        .unwrap();
    worker::testing::shutdown(worker);
    (receipt.revision_id, receipt.part_ids[0])
}

fn raw_chunk(part: i64, index: i64, offset: i64, bytes: &[u8]) -> Chunk {
    Chunk {
        part_id: part,
        chunk_index: index,
        raw_offset: offset,
        raw_length: bytes.len() as i64,
        codec: "raw-v1".into(),
        raw_sha256: Sha256::digest(bytes).to_vec(),
        encoded_bytes: bytes.to_vec(),
    }
}

#[test]
fn payload_repository_roundtrips_boundaries_and_bounded_pages() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    for (case, size) in [0, 1, PAGE - 1, PAGE, PAGE + 1, 150 * 1024, 16 * 1024 * 1024]
        .into_iter()
        .enumerate()
    {
        for binary in [false, true] {
            let bytes = fixture(size, binary);
            let (_, part) = capture(home.path(), &format!("{case}-{binary}"), 1, &bytes);
            for offset in (0..size).step_by(49001).chain([size]) {
                let page = repo.read(part, offset as u64, PAGE * 2).unwrap();
                assert_eq!(page, bytes[offset..(offset + PAGE).min(size)]);
            }
            assert!(repo.read(part, size as u64 + 1, PAGE).is_err());
        }
    }
    // Byte buffers only; SQLite/codec allocations are measured as process RSS.
    assert!(repo.metrics.read_buffers <= 3 * PAGE + 64);
}

#[test]
fn payload_repository_short_flushes_reopen_and_replacement() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    let (first, part) = repo.create("one", 1).unwrap();
    let bytes = fixture(150 * 1024, false);
    for (index, fragment) in bytes.chunks(997).enumerate() {
        repo.append(&[raw_chunk(
            part,
            index as i64,
            (index * 997) as i64,
            fragment,
        )])
        .unwrap();
    }
    repo.seal(first, part, bytes.len() as u64, &Sha256::digest(&bytes))
        .unwrap();
    drop(repo);
    let mut repo = Repository::open(home.path()).unwrap();
    assert_eq!(
        repo.read(part, 61111, PAGE).unwrap(),
        bytes[61111..61111 + PAGE]
    );
    let (_, short) = capture(home.path(), "one", 2, b"short");
    let (_, empty) = capture(home.path(), "one", 3, b"");
    assert_eq!(repo.read(part, 0, PAGE).unwrap(), bytes[..PAGE]);
    assert_eq!(repo.read(short, 0, PAGE).unwrap(), b"short");
    assert!(repo.read(empty, 0, PAGE).unwrap().is_empty());
    repo.delete(first).unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
    assert_eq!(
        chunks::table
            .filter(chunks::part_id.eq(part))
            .count()
            .get_result::<i64>(&mut repo.connection)
            .unwrap(),
        0
    );
    assert_eq!(repo.read(short, 0, PAGE).unwrap(), b"short");
}

#[test]
fn payload_repository_duplicate_rejection_rollback_and_staging_reopen() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    let (revision, part) = repo.create("one", 1).unwrap();
    let chunk = raw_chunk(part, 0, 0, b"hello");
    repo.append(std::slice::from_ref(&chunk)).unwrap();
    assert!(repo.append(std::slice::from_ref(&chunk)).is_err());
    assert_eq!(repo.metrics.chunks, 1);
    let mut conflict = chunk.clone();
    conflict.encoded_bytes[0] ^= 1;
    assert!(repo.append(&[conflict]).is_err());
    let mut second = chunk.clone();
    second.chunk_index = 1;
    second.raw_offset = 5;
    let mut gap = second.clone();
    gap.chunk_index = 3;
    gap.raw_offset = 15;
    assert!(repo.append(&[second, gap]).is_err());
    assert_eq!(
        parts::table
            .find(part)
            .select(parts::committed_raw_bytes)
            .first::<i64>(&mut repo.connection)
            .unwrap(),
        5
    );
    assert_eq!(
        chunks::table
            .count()
            .get_result::<i64>(&mut repo.connection)
            .unwrap(),
        1
    );
    assert!(repo.append(&vec![chunk.clone(); BATCH + 1]).is_err());
    assert!(repo.create("overflow", u64::MAX).is_err());
    drop(repo);
    let mut repo = Repository::open(home.path()).unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
    repo.seal(revision, part, 5, &Sha256::digest(b"hello"))
        .unwrap();
    assert_eq!(repo.read(part, 0, PAGE).unwrap(), b"hello");
    assert!(repo.append(&[chunk]).is_err());
    assert!(repo.seal(1, part, 5, &[0; 32]).is_err());
    assert_eq!(repo.read(part, 0, PAGE).unwrap(), b"hello");
}

#[test]
fn payload_repository_codec_fallback_and_corruption() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    for (id, data, expected) in [
        ("tiny", b"abc".to_vec(), "raw-v1"),
        ("binary", fixture(PAGE, true), "raw-v1"),
        ("text", fixture(PAGE, false), "zstd-v1"),
    ] {
        let (_, part) = capture(home.path(), id, 1, &data);
        let chunk = chunks::table
            .find((part, 0_i64))
            .select(Chunk::as_select())
            .first::<Chunk>(&mut repo.connection)
            .unwrap();
        assert_eq!(chunk.codec, expected);
        diesel::update(chunks::table.find((part, 0_i64)))
            .set(chunks::raw_sha256.eq(vec![0_u8; 32]))
            .execute(&mut repo.connection)
            .unwrap();
        assert!(repo.read(part, 0, PAGE).is_err());
        diesel::update(chunks::table.find((part, 0_i64)))
            .set((
                chunks::raw_sha256.eq(&chunk.raw_sha256),
                chunks::encoded_bytes.eq(vec![1_u8; PAGE * 8]),
            ))
            .execute(&mut repo.connection)
            .unwrap();
        assert!(repo.read(part, 0, PAGE).is_err());
        diesel::update(chunks::table.find((part, 0_i64)))
            .set((
                chunks::encoded_bytes.eq(&chunk.encoded_bytes),
                chunks::raw_length.eq(chunk.raw_length - 1),
            ))
            .execute(&mut repo.connection)
            .unwrap();
        assert!(repo.read(part, 0, PAGE).is_err());
    }
    let data = fixture(PAGE * 2, false);
    let (_, part) = capture(home.path(), "gap", 1, &data);
    diesel::delete(chunks::table.find((part, 1_i64)))
        .execute(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, (PAGE - 1) as u64, PAGE).is_err());
    assert_eq!(
        repo.read(part, 0, PAGE).unwrap(),
        data[..PAGE],
        "unrequested missing tail does not invalidate this page"
    );
    assert!(repo.read(part, PAGE as u64, PAGE).is_err());
    let (_, part) = capture(home.path(), "invalid-zstd", 1, &fixture(PAGE, false));
    diesel::update(chunks::table.find((part, 0_i64)))
        .set(chunks::encoded_bytes.eq(b"not a zstd frame".as_slice()))
        .execute(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
    let oversized = zstd::bulk::compress(&vec![0; PAGE * 2], 1).unwrap();
    diesel::update(chunks::table.find((part, 0_i64)))
        .set(chunks::encoded_bytes.eq(oversized))
        .execute(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
}

#[test]
fn payload_repository_cached_decoder_does_not_cache_integrity_or_content() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    let bytes = fixture(PAGE * 2, false);
    let (_, part) = capture(home.path(), "cached", 1, &bytes);
    assert_eq!(repo.read(part, 0, PAGE).unwrap(), bytes[..PAGE]);
    let encoded = chunks::table
        .find((part, 0_i64))
        .select(chunks::encoded_bytes)
        .first::<Vec<u8>>(&mut repo.connection)
        .unwrap();
    diesel::update(chunks::table.find((part, 0_i64)))
        .set(chunks::encoded_bytes.eq(b"bad frame".as_slice()))
        .execute(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
    diesel::update(chunks::table.find((part, 0_i64)))
        .set(chunks::encoded_bytes.eq(encoded))
        .execute(&mut repo.connection)
        .unwrap();
    assert_eq!(repo.read(part, 0, PAGE).unwrap(), bytes[..PAGE]);
    diesel::update(chunks::table.find((part, 1_i64)))
        .set(chunks::raw_offset.eq(PAGE as i64 + 1))
        .execute(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, (PAGE - 1) as u64, PAGE).is_err());
}

#[test]
fn payload_repository_four_query_only_readers_share_initialized_store() {
    let home = tempfile::tempdir().unwrap();
    assert!(Repository::open_for_reads(home.path()).is_err());
    let mut writer = Repository::open(home.path()).unwrap();
    let bytes = fixture(150 * 1024, false);
    let (revision, part) = capture(home.path(), "concurrent", 1, &bytes);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for lane in 0..4 {
            let home = &home;
            let bytes = &bytes;
            handles.push(scope.spawn(move || {
                let mut reader = Repository::open_for_reads(home.path()).unwrap();
                for i in 0..16 {
                    let offset = (i * 7919 + lane * 997) % bytes.len();
                    assert_eq!(
                        reader.read(part, offset as u64, PAGE).unwrap(),
                        bytes[offset..(offset + PAGE).min(bytes.len())]
                    );
                }
                assert!(reader.delete(revision).is_err());
                assert!(reader.metrics.read_buffers <= 3 * PAGE + 64);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert_eq!(writer.read(part, 0, PAGE).unwrap(), bytes[..PAGE]);
}

#[test]
fn payload_repository_connection_settings_and_migration_redo() {
    let home = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    #[derive(QueryableByName)]
    struct Setting {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        value: i64,
    }
    for (name, expected) in [("synchronous", 1), ("foreign_keys", 1), ("auto_vacuum", 2)] {
        let row: Setting = diesel::sql_query(format!("SELECT {name} AS value FROM pragma_{name}"))
            .get_result(&mut repo.connection)
            .unwrap();
        assert_eq!(row.value, expected);
    }
    #[derive(QueryableByName)]
    struct Mode {
        #[diesel(sql_type = diesel::sql_types::Text)]
        value: String,
    }
    let mode: Mode = diesel::sql_query("SELECT journal_mode AS value FROM pragma_journal_mode")
        .get_result(&mut repo.connection)
        .unwrap();
    assert_eq!(mode.value, "wal");
    repo.connection.revert_last_migration(MIGRATIONS).unwrap();
    repo.connection.run_pending_migrations(MIGRATIONS).unwrap();
    capture(home.path(), "after-redo", 1, b"works");
}

#[test]
fn payload_repository_rejects_silent_wal_fallback() {
    // In-memory SQLite cannot enter WAL mode, but the pragma still succeeds.
    // Exercise that real fallback without depending on a particular filesystem.
    let mut connection = SqliteConnection::establish(":memory:").unwrap();
    connection
        .batch_execute("PRAGMA journal_mode=WAL;")
        .unwrap();
    let error = Repository::configure_writer(&mut connection).unwrap_err();
    assert_eq!(
        error.to_string(),
        "payload storage requires WAL mode; SQLite selected memory"
    );
}

#[test]
fn payload_writer_installs_busy_handler_before_lock_sensitive_configuration() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("payloads.sqlite");
    let mut writer = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    // An exclusive WAL owner makes the first header read on a new connection
    // contend deterministically, rather than relying on a worker startup race.
    writer.batch_execute("PRAGMA journal_mode=WAL; PRAGMA locking_mode=EXCLUSIVE; CREATE TABLE lock_probe (id INTEGER); BEGIN EXCLUSIVE;").unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let (released, finished) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        wait.recv().unwrap();
        writer.batch_execute("ROLLBACK").unwrap();
        drop(writer);
        released.send(()).unwrap();
    });
    let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    let mut release = Some(release);
    connection.set_instrumentation(move |event: diesel::connection::InstrumentationEvent<'_>| {
        if matches!(
            event,
            diesel::connection::InstrumentationEvent::FinishQuery { .. }
        ) {
            if let Some(release) = release.take() {
                // Release after the first configuration statement completes.
                // A busy handler can be installed while the owner holds its lock;
                // reading the header first instead fails with SQLITE_BUSY.
                release.send(()).unwrap();
                finished.recv().unwrap();
            }
        }
    });
    let result = Repository::configure_writer(&mut connection);
    owner.join().unwrap();
    result.expect("connection configuration must install the busy handler first");
}
