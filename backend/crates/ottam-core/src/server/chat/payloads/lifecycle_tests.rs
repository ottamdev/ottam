use super::*;

#[tokio::test]
async fn payload_observer_sleeps_until_notification_or_maintenance_and_drops() {
    let home = tempfile::tempdir().unwrap();
    let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
    let service = PayloadService::new(database.clone(), Weak::new());
    service.start();
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.maintenance_runs.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    let before = service.refresh_runs.load(Ordering::Relaxed);
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(service.refresh_runs.load(Ordering::Relaxed), before);
    tokio::time::advance(Duration::from_secs(15 * 60)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        service.refresh_runs.load(Ordering::Relaxed),
        before,
        "caught-up payload storage scheduled an idle timer"
    );
    service.wake.notify_one();
    tokio::task::yield_now().await;
    assert!(service.refresh_runs.load(Ordering::Relaxed) > before);
    let weak = Arc::downgrade(&service);
    let observer = service.observer.lock().unwrap().as_ref().unwrap().clone();
    drop(service);
    assert!(
        weak.upgrade().is_none(),
        "idle observer retained its service"
    );
    tokio::task::yield_now().await;
    assert!(observer.is_finished());
}

async fn orphan_revision(database: &Database, id: usize) -> RevisionKey {
    let id = id.to_string();
    let key = RevisionKey {
        workspace_id: "workspace".into(),
        conversation_id: "conversation".into(),
        item_id: id.clone(),
        payload_id: id.clone(),
        revision: 1,
    };
    let output = database
        .payload_worker()
        .begin(payload::Revision {
            payload_id: key.payload_id.clone(),
            revision: key.revision,
            workspace_id: key.workspace_id.clone(),
            conversation_id: key.conversation_id.clone(),
            item_id: key.item_id.clone(),
            run_id: "run".into(),
            role: "output".into(),
            upstream_truncated: None,
            parts: Vec::new(),
        })
        .unwrap();
    database
        .payload_worker()
        .complete(output)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    key
}

async fn leased_orphan_revision(database: &Database, id: usize) -> RevisionKey {
    let key = orphan_revision(database, id).await;
    database
        .payload_leases()
        .lock()
        .unwrap()
        .insert(key.clone());
    key
}

#[tokio::test]
async fn payload_observer_revisits_changes_between_maintenance_batches() {
    let home = tempfile::tempdir().unwrap();
    let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
    let service = PayloadService::new(database.clone(), Weak::new());
    let old = leased_orphan_revision(&database, 0).await;
    for id in 1..=payload::maintenance::REVISION_BATCH as usize {
        leased_orphan_revision(&database, id).await;
    }
    service.start();
    async fn wait_for_pass(service: &PayloadService, count: u64) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while service.maintenance_runs.load(Ordering::Relaxed) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    wait_for_pass(&service, 1).await;
    database.payload_leases().lock().unwrap().remove(&old);
    database.payload_worker().notify_changed();
    for count in 2..=3 {
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        wait_for_pass(&service, count).await;
    }
    assert!(payload::maintenance::Maintenance::open(home.path())
        .unwrap()
        .candidates(0)
        .unwrap()
        .iter()
        .all(|c| c.key != old));
}

#[tokio::test]
async fn payload_maintenance_retries_failed_batch_before_advancing() {
    use diesel::{connection::SimpleConnection, Connection};
    let home = tempfile::tempdir().unwrap();
    let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
    let service = PayloadService::new(database.clone(), Weak::new());
    for id in 0..payload::maintenance::REVISION_BATCH as usize {
        orphan_revision(&database, id).await;
    }
    let mut connection =
        diesel::SqliteConnection::establish(home.path().join("payloads.sqlite").to_str().unwrap())
            .unwrap();
    connection.batch_execute("CREATE TRIGGER fail_cleanup BEFORE DELETE ON payload_revisions BEGIN SELECT RAISE(ABORT, 'injected cleanup failure'); END;").unwrap();
    let mut cursor = 0;
    assert!(service.maintain(&mut cursor).await);
    assert_eq!(cursor, 0, "failed cleanup must not advance past its batch");
    connection
        .batch_execute("DROP TRIGGER fail_cleanup")
        .unwrap();
    service.maintain(&mut cursor).await;
    assert!(payload::maintenance::Maintenance::open(home.path())
        .unwrap()
        .candidates(0)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn payload_maintenance_revisits_released_orphan_after_short_batch_with_new_revisions() {
    let home = tempfile::tempdir().unwrap();
    let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
    let service = PayloadService::new(database.clone(), Weak::new());
    let old = leased_orphan_revision(&database, 0).await;
    for id in 1..payload::maintenance::REVISION_BATCH as usize {
        leased_orphan_revision(&database, id).await;
    }

    let mut cursor = 0;
    service.maintain(&mut cursor).await;
    assert_ne!(cursor, 0);
    database.payload_leases().lock().unwrap().remove(&old);

    for id in payload::maintenance::REVISION_BATCH as usize
        ..(2 * payload::maintenance::REVISION_BATCH) as usize
    {
        leased_orphan_revision(&database, id).await;
    }
    service.maintain(&mut cursor).await;
    assert_ne!(cursor, 0);

    leased_orphan_revision(
        &database,
        (2 * payload::maintenance::REVISION_BATCH) as usize,
    )
    .await;
    service.maintain(&mut cursor).await;
    assert_eq!(cursor, 0);

    for id in (2 * payload::maintenance::REVISION_BATCH + 1) as usize
        ..(3 * payload::maintenance::REVISION_BATCH + 1) as usize
    {
        leased_orphan_revision(&database, id).await;
    }
    service.maintain(&mut cursor).await;

    assert!(payload::maintenance::Maintenance::open(home.path())
        .unwrap()
        .candidates(0)
        .unwrap()
        .into_iter()
        .all(|candidate| candidate.key != old));
}

#[tokio::test]
async fn payload_idle_worker_does_not_run_maintenance_on_each_wakeup() {
    let home = tempfile::tempdir().unwrap();
    let database = Database::open_test_home(home.path().to_path_buf()).unwrap();
    let service = PayloadService::new(database, Weak::new());
    service.start();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.maintenance_runs.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::pause();
    for _ in 0..5 {
        service.wake.notify_one();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(service.maintenance_runs.load(Ordering::Relaxed), 1);
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    tokio::time::resume();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.maintenance_runs.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.maintenance_runs.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn payload_deleted_conversation_releases_entries_but_rejects_late_submissions() {
    let home = tempfile::tempdir().unwrap();
    let service = PayloadService::new(
        Database::open_test_home(home.path().to_path_buf()).unwrap(),
        Weak::new(),
    );
    let workspace = uuid::Uuid::now_v7().to_string();
    let conversation = uuid::Uuid::now_v7().to_string();
    let run = uuid::Uuid::now_v7().to_string();
    let updates = || extract(&mut serde_json::json!({"status":"completed", "rawOutput":"bytes"}));
    assert!(!service
        .submit(&workspace, &conversation, &run, "tool", updates())
        .is_empty());
    service.fence(&conversation).await;
    service.forget_conversation(&conversation);
    {
        let state = service.state.lock().unwrap();
        assert!(state.entries.is_empty());
        assert!(state.run_bytes.is_empty());
        assert!(state.deleted.contains(&conversation));
    }
    assert!(service
        .submit(&workspace, &conversation, &run, "late", updates())
        .is_empty());
}

#[tokio::test]
async fn payload_image_over_aggregate_limit_is_unavailable_in_descriptor_and_manifest() {
    for preceding_text_bytes in [LIMIT, LIMIT - 1] {
        let home = tempfile::tempdir().unwrap();
        let service = PayloadService::new(
            Database::open_test_home(home.path().to_path_buf()).unwrap(),
            Weak::new(),
        );
        let workspace = uuid::Uuid::now_v7().to_string();
        let conversation = uuid::Uuid::now_v7().to_string();
        let run = uuid::Uuid::now_v7().to_string();
        let descriptors = service.submit(
            &workspace,
            &conversation,
            &run,
            "tool",
            vec![Update {
                role: "output".into(),
                parts: Some(vec![
                    Part::text("Output", "x".repeat(preceding_text_bytes), None),
                    Part {
                        name: "Image".into(),
                        format: "image/png".into(),
                        bytes: vec![1, 2],
                        path: None,
                        reason: None,
                    },
                ]),
                append: false,
                publish: true,
                upstream: Some(false),
            }],
        );
        let payload_id = descriptors[0].payload_id.clone().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while service.pending(&run) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let descriptor = service
            .live(&workspace, &conversation, &payload_id)
            .unwrap();
        assert_eq!(descriptor.parts[1].format, "unavailable");
        assert_eq!(descriptor.reason.as_deref(), Some("captureLimit"));
        assert!(descriptor.locally_truncated);

        let key = service
            .live_key(&workspace, &conversation, &payload_id)
            .unwrap();
        let manifest = Reader::open(home.path()).unwrap().manifest(&key).unwrap();
        assert_eq!(manifest.parts[1].format, "unavailable");
        assert_eq!(
            manifest.parts[1].unavailable_reason.as_deref(),
            Some("captureLimit")
        );
    }
}
