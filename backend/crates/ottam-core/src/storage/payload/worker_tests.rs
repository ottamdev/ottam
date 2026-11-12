use super::*;
use std::time::{Duration, Instant};
use worker::{testing::*, AdmissionError, Worker};

pub(super) fn metadata(id: &str, revision: u64, run: &str) -> Revision {
    Revision {
        upstream_truncated: None,
        payload_id: id.into(),
        revision,
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: id.into(),
        run_id: run.into(),
        role: "output".into(),
        parts: vec![Part {
            name: "Output".into(),
            format: "text/plain".into(),
            source_path: None,
            unavailable_reason: None,
        }],
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "worker progress deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn payload_worker_idle_wait_has_no_quota_deadline_and_shutdown_interrupts() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    until(|| stats(&worker).last_wait.is_some());
    assert_eq!(stats(&worker).last_wait, Some(Duration::MAX));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(stats(&worker).quota_refreshes, 0);
    tokio::time::timeout(Duration::from_secs(2), worker.shutdown())
        .await
        .unwrap()
        .unwrap();
}

fn rows(repo: &mut Repository) -> i64 {
    chunks::table
        .count()
        .get_result(&mut repo.connection)
        .unwrap()
}

#[tokio::test]
async fn payload_worker_timer_completion_and_mixed_parts() {
    let home = tempfile::tempdir().unwrap();
    let (worker, clock) = start(home.path());
    let mut descriptor = metadata("mixed", 1, "run");
    descriptor.parts.push(Part {
        name: "Image".into(),
        format: "image/png".into(),
        source_path: Some("image.png".into()),
        unavailable_reason: None,
    });
    descriptor.parts.push(Part {
        name: "Missing".into(),
        format: "unavailable".into(),
        source_path: None,
        unavailable_reason: Some("notCaptured".into()),
    });
    let id = worker.begin(descriptor).unwrap();
    let text = "hello 日本語 ✓".as_bytes();
    for byte in text {
        worker.enqueue(id, 0, vec![*byte]).unwrap();
    }
    let binary = tests::fixture(PAGE + 1, true);
    worker.enqueue(id, 1, binary.clone()).unwrap();
    assert_eq!(
        worker.enqueue(id, 2, vec![1]),
        Err(AdmissionError::InvalidPart)
    );
    until(|| jobs(&worker) == 0);
    let mut repo = Repository::open_for_reads(home.path()).unwrap();
    assert_eq!(
        rows(&mut repo),
        0,
        "short batch waits for age or completion"
    );
    advance(&worker, &clock, 249);
    assert_eq!(rows(&mut repo), 0);
    advance(&worker, &clock, 1);
    until(|| rows(&mut repo) == 3);
    let part: i64 = parts::table
        .select(parts::id)
        .first(&mut repo.connection)
        .unwrap();
    assert!(repo.read(part, 0, PAGE).is_err(), "staging is not readable");
    let receipt = worker.complete(id).unwrap().await.unwrap().unwrap();
    assert_eq!(used(&worker), 0);
    assert_eq!(repo.read(receipt.part_ids[0], 0, PAGE).unwrap(), text);
    assert_eq!(
        repo.read(receipt.part_ids[1], 0, PAGE).unwrap(),
        binary[..PAGE]
    );
    assert_eq!(
        repo.read(receipt.part_ids[1], PAGE as u64, PAGE).unwrap(),
        binary[PAGE..]
    );
    assert_eq!(repo.read(receipt.part_ids[2], 0, PAGE).unwrap(), b"");
    assert_eq!(
        worker.enqueue(id, 0, vec![0]),
        Err(AdmissionError::UnknownOutput)
    );
    let stats = stats(&worker);
    worker.shutdown().await.unwrap();
    assert!(stats.max_batch_chunks <= BATCH);
    assert!(stats.max_batch_raw_bytes <= BATCH * PAGE);
}

#[tokio::test]
async fn payload_worker_pressure_counts_capacity_and_always_drains() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    let (entered, release) = gate(&worker);
    let first = worker.begin(metadata("first", 1, "run")).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = worker.begin(metadata("second", 1, "run")).unwrap();
    worker.enqueue(first, 0, vec![b'x'; CAPTURE_LIMIT]).unwrap();
    let mut oversized_capacity = Vec::with_capacity(16 * 1024 * 1024);
    oversized_capacity.push(1);
    assert_eq!(
        worker.enqueue(second, 0, oversized_capacity),
        Err(AdmissionError::QueuePressure)
    );
    worker
        .enqueue(second, 0, vec![b'y'; 8 * 1024 * 1024])
        .unwrap();
    assert_eq!(
        worker.enqueue(first, 0, vec![1]),
        Err(AdmissionError::CaptureLimit)
    );
    let first_done = worker.complete(first).unwrap();
    let second_done = worker.complete(second).unwrap();
    assert!(used(&worker) <= 32 * 1024 * 1024);
    release.send(()).unwrap();
    assert_eq!(
        first_done.await.unwrap().unwrap().capture_reason.as_deref(),
        Some("captureLimit")
    );
    let lossy = second_done.await.unwrap().unwrap();
    assert_eq!(lossy.capture_reason.as_deref(), Some("queueFull"));
    let mut repository = Repository::open_for_reads(home.path()).unwrap();
    let loss: (i32, Option<String>) = revisions::table
        .find(lossy.revision_id)
        .select((revisions::locally_truncated, revisions::capture_reason))
        .first(&mut repository.connection)
        .unwrap();
    assert_eq!(loss, (1, Some("queueFull".into())));
    assert_eq!(used(&worker), 0);
    let stats = stats(&worker);
    worker.shutdown().await.unwrap();
    assert_eq!(stats.max_batch_chunks, BATCH);
    assert_eq!(stats.max_batch_raw_bytes, BATCH * PAGE);
}

#[tokio::test]
async fn payload_worker_global_pressure_cancellation_and_fairness() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    let (entered, release) = gate(&worker);
    let first = worker.begin(metadata("large", 1, "a")).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.enqueue(first, 0, vec![0; CAPTURE_LIMIT]).unwrap();
    let tiny = worker.begin(metadata("tiny", 1, "b")).unwrap();
    worker.enqueue(tiny, 0, b"tiny".to_vec()).unwrap();
    let tiny_done = worker.complete(tiny).unwrap();
    let mut cancelled = Vec::new();
    for run in ["c", "d"] {
        let id = worker.begin(metadata(run, 1, run)).unwrap();
        worker.enqueue(id, 0, vec![0; CAPTURE_LIMIT]).unwrap();
        cancelled.push(id);
    }
    let last = worker.begin(metadata("last", 1, "e")).unwrap();
    assert_eq!(
        worker.enqueue(last, 0, vec![0; CAPTURE_LIMIT]),
        Err(AdmissionError::QueuePressure)
    );
    worker.cancel(last).unwrap();
    for id in cancelled {
        worker.cancel(id).unwrap();
    }
    release.send(()).unwrap();
    tiny_done.await.unwrap().unwrap();
    assert!(
        stats(&worker).chunks < 256,
        "small output gets a turn before large input drains"
    );
    let first_done = worker.complete(first).unwrap();
    first_done.await.unwrap().unwrap();
    until(|| used(&worker) == 0);
    worker.shutdown().await.unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    assert_eq!(
        revisions::table
            .count()
            .get_result::<i64>(&mut repo.connection)
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn payload_worker_completed_replacements_and_fragmented_input_reopen() {
    let home = tempfile::tempdir().unwrap();
    let worker = Worker::open(home.path().to_owned()).await.unwrap();
    let first = worker.begin(metadata("replace", 1, "run")).unwrap();
    worker
        .enqueue(first, 0, b"long predecessor".to_vec())
        .unwrap();
    let old = worker.complete(first).unwrap().await.unwrap().unwrap();
    let replacement = worker.begin(metadata("replace", 2, "run")).unwrap();
    worker.enqueue(replacement, 0, b"short".to_vec()).unwrap();
    let short = worker
        .complete(replacement)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let empty = worker.begin(metadata("replace", 3, "run")).unwrap();
    worker.complete(empty).unwrap().await.unwrap().unwrap();
    let unfinished = worker.begin(metadata("shutdown", 1, "run")).unwrap();
    let fragmented = tests::fixture(150 * 1024, false);
    for fragment in fragmented.chunks(64) {
        worker.enqueue(unfinished, 0, fragment.to_vec()).unwrap();
    }
    worker.complete(unfinished).unwrap().await.unwrap().unwrap();
    worker.shutdown().await.unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    assert_eq!(
        repo.read(old.part_ids[0], 0, PAGE).unwrap(),
        b"long predecessor"
    );
    assert_eq!(repo.read(short.part_ids[0], 0, PAGE).unwrap(), b"short");
    let empty: i64 = parts::table
        .inner_join(revisions::table)
        .filter(revisions::payload_id.eq("replace"))
        .filter(revisions::revision.eq(3_i64))
        .select(parts::id)
        .first(&mut repo.connection)
        .unwrap();
    assert!(repo.read(empty, 0, PAGE).unwrap().is_empty());
    let drained: i64 = parts::table
        .inner_join(revisions::table)
        .filter(revisions::payload_id.eq("shutdown"))
        .select(parts::id)
        .first(&mut repo.connection)
        .unwrap();
    for offset in (0..fragmented.len()).step_by(PAGE) {
        assert_eq!(
            repo.read(drained, offset as u64, PAGE).unwrap(),
            fragmented[offset..(offset + PAGE).min(fragmented.len())]
        );
    }
    repo.delete(old.revision_id).unwrap();
    assert!(repo.read(old.part_ids[0], 0, PAGE).is_err());
    assert_eq!(repo.read(short.part_ids[0], 0, PAGE).unwrap(), b"short");
}

#[tokio::test]
async fn payload_worker_cancellation_resolves_completion_and_releases_all_bytes() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    let (entered, release) = gate(&worker);
    let id = worker.begin(metadata("cancel", 1, "run")).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.enqueue(id, 0, vec![1; PAGE * 2]).unwrap();
    let completion = worker.complete(id).unwrap();
    worker.cancel(id).unwrap();
    release.send(()).unwrap();
    assert!(completion.await.unwrap().is_err());
    assert_eq!(used(&worker), 0);
    worker.shutdown().await.unwrap();
    let mut repo = Repository::open(home.path()).unwrap();
    assert_eq!(
        revisions::table
            .count()
            .get_result::<i64>(&mut repo.connection)
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn payload_worker_shutdown_abandons_staging_without_database_writes() {
    let home = tempfile::tempdir().unwrap();
    let (worker, clock) = start(home.path());
    let id = worker.begin(metadata("abandoned", 1, "run")).unwrap();
    worker.enqueue(id, 0, vec![1; PAGE]).unwrap();
    until(|| jobs(&worker) == 0);
    advance(&worker, &clock, 250);
    until(|| stats(&worker).chunks == 1);
    let mut repo = Repository::open(home.path()).unwrap();
    repo.connection.batch_execute("CREATE TRIGGER forbid_shutdown_write BEFORE UPDATE ON payload_revisions BEGIN SELECT RAISE(ABORT, 'no shutdown writes'); END;").unwrap();
    let (entered, release) = gate(&worker);
    worker.enqueue(id, 0, b"uncommitted tail".to_vec()).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let done = worker.complete(id).unwrap();
    stop(&worker);
    release.send(()).unwrap();
    assert!(done.await.unwrap().is_err());
    assert_eq!(used(&worker), 0);
    worker.shutdown().await.unwrap();
    let part: i64 = parts::table
        .select(parts::id)
        .first(&mut repo.connection)
        .unwrap();
    drop(repo);
    let mut repo = Repository::open(home.path()).unwrap();
    assert!(repo.read(part, 0, PAGE).is_err());
    assert_eq!(rows(&mut repo), 1);
    assert_eq!(
        revisions::table
            .select(revisions::state)
            .first::<String>(&mut repo.connection)
            .unwrap(),
        "staging"
    );
}

#[tokio::test]
async fn payload_worker_transaction_failures_recover_without_replaying_failed_captures() {
    for (phase, trigger) in [
        ("begin", "BEFORE INSERT ON payload_revisions"),
        ("chunks", "BEFORE INSERT ON payload_chunks"),
        (
            "seal",
            "BEFORE UPDATE OF state ON payload_revisions WHEN NEW.state = 'ready'",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (worker, _) = start(home.path());
        let saved = worker.begin(metadata("saved", 1, "saved-run")).unwrap();
        worker.enqueue(saved, 0, b"published".to_vec()).unwrap();
        let saved = worker.complete(saved).unwrap().await.unwrap().unwrap();
        let mut repo = Repository::open(home.path()).unwrap();
        repo.connection.batch_execute(&format!("CREATE TRIGGER fail_capture {trigger} BEGIN SELECT RAISE(ABORT, 'injected disk failure'); END;")).unwrap();
        let (entered, release) = gate(&worker);
        let first = worker.begin(metadata("failed", 1, "failed-run")).unwrap();
        entered.recv().unwrap();
        let second = worker
            .begin(metadata("also-failed", 1, "other-run"))
            .unwrap();
        worker.enqueue(first, 0, vec![2; PAGE]).unwrap();
        worker.enqueue(second, 0, vec![3; PAGE]).unwrap();
        let first_done = worker.complete(first).unwrap();
        let second_done = worker.complete(second).unwrap();
        release.send(()).unwrap();
        assert!(first_done.await.unwrap().is_err(), "{phase}");
        assert!(second_done.await.unwrap().is_err(), "{phase}");
        assert_eq!(used(&worker), 0);
        assert_eq!(jobs(&worker), 0);
        assert_eq!(repo.read(saved.part_ids[0], 0, PAGE).unwrap(), b"published");
        repo.connection
            .batch_execute("DROP TRIGGER fail_capture;")
            .unwrap();
        let mut recovered = None;
        until(|| {
            let mut next = metadata("recovered", 1, "new-run");
            next.conversation_id = "new-conversation".into();
            match worker.begin(next) {
                Ok(id) => {
                    recovered = Some(id);
                    true
                }
                Err(AdmissionError::StorageUnavailable) => false,
                other => panic!("worker should recover after {phase}: {other:?}"),
            }
        });
        let recovered = recovered.unwrap();
        worker.enqueue(recovered, 0, b"recovered".to_vec()).unwrap();
        let receipt = worker.complete(recovered).unwrap().await.unwrap().unwrap();
        assert_eq!(
            repo.read(receipt.part_ids[0], 0, PAGE).unwrap(),
            b"recovered"
        );
        let failed_states = revisions::table
            .filter(revisions::payload_id.eq_any(["failed", "also-failed"]))
            .select(revisions::state)
            .load::<String>(&mut repo.connection)
            .unwrap();
        assert!(
            failed_states.iter().all(|state| state == "staging"),
            "{phase}"
        );
        worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn payload_worker_reopen_failure_rejects_admission_and_allows_recovery_or_shutdown() {
    for restore in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let (worker, _) = start(home.path());
        let mut repo = Repository::open(home.path()).unwrap();
        let (entered, release) = gate(&worker);
        let id = worker.begin(metadata("failure", 1, "run")).unwrap();
        entered.recv().unwrap();
        worker.enqueue(id, 0, b"unacknowledged".to_vec()).unwrap();
        let done = worker.complete(id).unwrap();
        repo.connection
            .batch_execute("CREATE TRIGGER fail_payload_begin BEFORE INSERT ON payload_revisions BEGIN SELECT RAISE(ABORT, 'injected write failure'); END; ALTER TABLE payload_usage RENAME TO unavailable_usage;")
            .unwrap();
        release.send(()).unwrap();
        assert!(done.await.unwrap().is_err());
        assert_eq!(used(&worker), 0);
        assert_eq!(
            worker.begin(metadata("during-recovery", 1, "run")),
            Err(AdmissionError::StorageUnavailable)
        );
        if restore {
            repo.connection
                .batch_execute("ALTER TABLE unavailable_usage RENAME TO payload_usage; DROP TRIGGER fail_payload_begin;")
                .unwrap();
            let mut next = None;
            until(|| match worker.begin(metadata("recovered", 1, "run")) {
                Ok(id) => {
                    next = Some(id);
                    true
                }
                Err(AdmissionError::StorageUnavailable) => false,
                other => panic!("unexpected recovery admission: {other:?}"),
            });
            let id = next.unwrap();
            worker.enqueue(id, 0, b"healthy".to_vec()).unwrap();
            let receipt = worker.complete(id).unwrap().await.unwrap().unwrap();
            assert_eq!(repo.read(receipt.part_ids[0], 0, PAGE).unwrap(), b"healthy");
        }
        tokio::time::timeout(Duration::from_secs(2), worker.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn payload_worker_batches_outputs_and_does_not_reset_oldest_age() {
    let home = tempfile::tempdir().unwrap();
    let (worker, clock) = start(home.path());
    let first = worker.begin(metadata("first", 1, "run")).unwrap();
    let second = worker.begin(metadata("second", 1, "run")).unwrap();
    worker.enqueue(first, 0, b"old".to_vec()).unwrap();
    worker.enqueue(second, 0, b"other".to_vec()).unwrap();
    until(|| jobs(&worker) == 0);
    advance(&worker, &clock, 200);
    worker.enqueue(first, 0, b"new".to_vec()).unwrap();
    until(|| jobs(&worker) == 0);
    advance(&worker, &clock, 50);
    until(|| stats(&worker).chunks == 2);
    assert_eq!(
        stats(&worker).transactions,
        3,
        "two creates and one cross-output batch"
    );
    let first = worker.complete(first).unwrap().await.unwrap().unwrap();
    let second = worker.complete(second).unwrap().await.unwrap().unwrap();
    let mut reader = Reader::open(home.path()).unwrap();
    assert_eq!(reader.read(first.part_ids[0], 0, PAGE).unwrap(), b"oldnew");
    assert_eq!(reader.read(second.part_ids[0], 0, PAGE).unwrap(), b"other");
    until(|| stats(&worker).transactions == 5);
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn payload_worker_job_limit_does_not_block_completion() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    let (entered, release) = gate(&worker);
    let id = worker.begin(metadata("jobs", 1, "run")).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    for _ in 0..4096 {
        worker.enqueue(id, 0, vec![b'x']).unwrap();
    }
    assert_eq!(
        worker.enqueue(id, 0, vec![b'y']),
        Err(AdmissionError::QueuePressure)
    );
    let complete = worker.complete(id).unwrap();
    release.send(()).unwrap();
    let receipt = complete.await.unwrap().unwrap();
    assert_eq!(receipt.capture_reason.as_deref(), Some("queueFull"));
    assert_eq!(used(&worker), 0);
    let mut reader = Reader::open(home.path()).unwrap();
    assert_eq!(
        reader.read(receipt.part_ids[0], 0, PAGE).unwrap(),
        vec![b'x'; 4096]
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn payload_worker_cancel_after_finish_linearization_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let (worker, _) = start(home.path());
    let id = worker.begin(metadata("race", 1, "run")).unwrap();
    worker.enqueue(id, 0, b"complete".to_vec()).unwrap();
    until(|| jobs(&worker) == 0);
    let (entered, release) = gate(&worker);
    let complete = worker.complete(id).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(worker.cancel(id), Err(AdmissionError::Closed));
    release.send(()).unwrap();
    complete.await.unwrap().unwrap();
    assert_eq!(used(&worker), 0);
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn payload_worker_reader_remains_bounded_during_capture_writes() {
    concurrent_reads().await;
}

#[test]
fn payload_worker_is_initialized_with_main_database() {
    let home = tempfile::tempdir().unwrap();
    let database = crate::storage::Database::open(home.path().to_owned()).unwrap();
    assert!(home.path().join("ottam.sqlite").is_file());
    assert!(home.path().join("payloads.sqlite").exists());
    assert!(Reader::open(home.path()).is_ok());
    drop(database);
    let _reopened = crate::storage::Database::open(home.path().to_owned()).unwrap();
    assert!(home.path().join("payloads.sqlite").exists());
}

async fn concurrent_reads() {
    let home = tempfile::tempdir().unwrap();
    let worker = Worker::open(home.path().to_owned()).await.unwrap();
    let corpus = tests::fixture(2 * 1024 * 1024, false);
    let seed = worker.begin(metadata("read-source", 1, "seed")).unwrap();
    worker.enqueue(seed, 0, corpus.clone()).unwrap();
    let seed = worker.complete(seed).unwrap().await.unwrap().unwrap();
    let mut reader = Reader::open(home.path()).unwrap();
    for offset in (0..corpus.len()).step_by(PAGE) {
        assert_eq!(
            reader.read(seed.part_ids[0], offset as u64, PAGE).unwrap(),
            corpus[offset..offset + PAGE]
        );
    }
    let (entered, release) = gate(&worker);
    let active = worker.begin(metadata("active-write", 1, "writer")).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let input = tests::fixture(CAPTURE_LIMIT, true);
    worker.enqueue(active, 0, input.clone()).unwrap();
    let complete = worker.complete(active).unwrap();
    let before = stats(&worker).chunks;
    release.send(()).unwrap();
    for i in 0..512 {
        let offset = (i * 7919) % corpus.len();
        let page = reader.read(seed.part_ids[0], offset as u64, PAGE).unwrap();
        assert_eq!(page, corpus[offset..(offset + PAGE).min(corpus.len())]);
    }
    let during = stats(&worker).chunks - before;
    assert!(
        during > 0,
        "writes must make progress during the measured read phase"
    );
    let complete = complete.await.unwrap().unwrap();
    for offset in (0..input.len()).step_by(PAGE) {
        assert_eq!(
            reader
                .read(complete.part_ids[0], offset as u64, PAGE)
                .unwrap(),
            input[offset..offset + PAGE]
        );
    }
    assert!(reader.0.metrics.read_buffers <= 3 * PAGE + 64);
    assert_eq!(used(&worker), 0);
    worker.shutdown().await.unwrap();
}
