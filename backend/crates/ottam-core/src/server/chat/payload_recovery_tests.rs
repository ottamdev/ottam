use super::*;
use crate::agent::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::watch;
// Paused runtimes must not advance replay deadlines while another test's
// runtime owns the process-wide concurrency slots.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct FakeReplay {
    definition: AgentDefinition,
    updates: HarnessUpdateSender,
    loads: AtomicUsize,
    stops: AtomicUsize,
    starts: AtomicUsize,
    cancel_shutdown_once: AtomicBool,
    supported: bool,
    hang: bool,
    hang_shutdown: AtomicBool,
    delay_ms: std::sync::atomic::AtomicU64,
}
impl FakeReplay {
    fn new(supported: bool, hang: bool) -> Arc<Self> {
        Arc::new(Self {
            definition: AgentDefinition::for_test("fake"),
            updates: HarnessUpdateSender::default(),
            loads: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            starts: AtomicUsize::new(0),
            cancel_shutdown_once: AtomicBool::new(false),
            supported,
            hang,
            hang_shutdown: AtomicBool::new(false),
            delay_ms: std::sync::atomic::AtomicU64::new(20),
        })
    }
}
impl HarnessAdapter for FakeReplay {
    fn definition(&self) -> &AgentDefinition {
        &self.definition
    }
    fn status(&self) -> watch::Receiver<AdapterStatus> {
        unreachable!()
    }
    fn sessions(&self) -> Vec<HarnessSessionSummary> {
        unreachable!()
    }
    fn subscribe_updates(&self) -> HarnessUpdateReceiver {
        self.updates.subscribe()
    }
    fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let mut status = AdapterStatus::stopped();
        status.capabilities.load_session = self.supported;
        Box::pin(async { Ok(status) })
    }
    fn shutdown(&self) -> HarnessFuture<'_, ()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            if self.cancel_shutdown_once.swap(false, Ordering::SeqCst) {
                return Err(HarnessError::new(
                    HarnessErrorKind::RequestCancelled,
                    "concurrent quit cancelled",
                ));
            }
            if self
                .definition
                .refresh_lock_path()
                .is_some_and(|path| path.exists())
            {
                return Err(HarnessError::new(
                    HarnessErrorKind::AuthRefreshBusy,
                    "refreshing",
                ));
            }
            if self.hang_shutdown.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn load_session(&self, session_id: HarnessSessionId, cwd: PathBuf) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(session_id.0, "saved");
            assert_eq!(cwd, PathBuf::from("/workspace"));
            self.loads.fetch_add(1, Ordering::SeqCst);
            if self.hang {
                std::future::pending::<()>().await;
            }
            for (sequence, tool) in ["one", "two"].into_iter().enumerate() {
                self.updates.send(HarnessUpdate { session_id: session_id.clone(), sequence: sequence as u64,
                    update: HarnessUpdateKind::ToolCall(serde_json::json!({"toolCallId":tool,"status":"completed","rawOutput":"— café"})) });
            }
            tokio::time::sleep(Duration::from_millis(self.delay_ms.load(Ordering::SeqCst))).await;
            Ok(())
        })
    }
    fn authenticate(&self, _: String) -> HarnessFuture<'_, ()> {
        panic!("replay authenticated")
    }
    fn new_session(
        &self,
        _: PathBuf,
        _: SessionConfigRequest,
    ) -> HarnessFuture<'_, HarnessSessionId> {
        panic!("replay created a session")
    }
    fn delete_session(&self, _: HarnessSessionId) -> HarnessFuture<'_, ()> {
        panic!("replay deleted a session")
    }
    fn prompt(
        &self,
        _: HarnessSessionId,
        _: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason> {
        panic!("replay executed a prompt")
    }
    fn steer(&self, _: HarnessSessionId, _: HarnessPrompt) -> HarnessFuture<'_, ()> {
        panic!("replay steered")
    }
    fn cancel(&self, _: HarnessSessionId) -> HarnessFuture<'_, ()> {
        panic!("replay cancelled a live session")
    }
}
async fn replay(fake: Arc<FakeReplay>, home: PathBuf, tool: &str) -> Result<Vec<Vec<u8>>, String> {
    load_with(
        home,
        "fake".into(),
        "saved".into(),
        tool.into(),
        "/workspace".into(),
        move || async move {
            let initialized = fake.start().await.map_err(|e| e.to_string());
            Ok((fake as Arc<dyn HarnessAdapter>, initialized, None))
        },
    )
    .await
}

#[tokio::test]
async fn payload_replay_coalesces_session_requests_without_execution_or_publication() {
    let _guard = TEST_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let fake = FakeReplay::new(true, false);
    let (one, two) = tokio::join!(
        replay(fake.clone(), home.path().to_owned(), "one"),
        replay(fake.clone(), home.path().to_owned(), "two")
    );
    assert_eq!(one.unwrap(), vec!["— café".as_bytes()]);
    assert_eq!(two.unwrap(), vec!["— café".as_bytes()]);
    assert_eq!(fake.loads.load(Ordering::SeqCst), 1);
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn payload_replay_retains_shared_adapter_after_caller_cancellation_until_refresh_release() {
    let _guard = TEST_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let lock = home.path().join(".oauth_refresh.lock");
    std::fs::create_dir(&lock).unwrap();
    let mut fake = FakeReplay::new(true, false);
    let id = fake.definition.agent_id.clone();
    Arc::get_mut(&mut fake).unwrap().definition.instance = Some(AgentInstance {
        id,
        harness_id: HarnessId("claude".into()),
        label: "Replay Claude".into(),
        home: HomeLayout::Existing {
            path: home.path().into(),
            environment: Default::default(),
        },
        claimed: true,
    });
    let owned = fake.clone();
    let path = home.path().to_owned();
    let caller = tokio::spawn(async move { replay(owned, path, "one").await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while fake.stops.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!caller.is_finished());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(
        Arc::strong_count(&fake) > 1,
        "cleanup retains the adapter after the caller leaves"
    );
    assert!(lock.exists());
    std::fs::remove_dir(lock).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while Arc::strong_count(&fake) > 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(fake.stops.load(Ordering::SeqCst) >= 2);
}

#[tokio::test(start_paused = true)]
async fn payload_replay_late_tool_gets_a_full_deadline_after_waiting_for_previous_pass() {
    let _guard = TEST_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let fake = FakeReplay::new(true, false);
    fake.delay_ms.store(55_000, Ordering::SeqCst);
    let started = tokio::time::Instant::now();
    let mut updates = fake.subscribe_updates();
    let (one, two) = tokio::join!(replay(fake.clone(), home.path().to_owned(), "one"), async {
        tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap();
        replay(fake.clone(), home.path().to_owned(), "two").await
    });
    assert_eq!(one.unwrap(), vec!["— café".as_bytes()]);
    assert_eq!(two.unwrap(), vec!["— café".as_bytes()]);
    assert_eq!(fake.loads.load(Ordering::SeqCst), 2);
    assert_eq!(fake.stops.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= Duration::from_secs(110));
}

#[tokio::test(start_paused = true)]
async fn payload_replay_bounds_adapter_shutdown() {
    let _guard = TEST_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let fake = FakeReplay::new(true, false);
    fake.hang_shutdown.store(true, Ordering::SeqCst);
    assert!(replay(fake.clone(), home.path().to_owned(), "one")
        .await
        .is_ok());
    assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn payload_replay_retries_cancelled_shared_shutdown_without_starting_again() {
    let _guard = TEST_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let mut fake = FakeReplay::new(true, false);
    let id = fake.definition.agent_id.clone();
    Arc::get_mut(&mut fake).unwrap().definition.instance = Some(AgentInstance {
        id,
        harness_id: HarnessId("claude".into()),
        label: "Replay Claude".into(),
        home: HomeLayout::Existing {
            path: home.path().into(),
            environment: Default::default(),
        },
        claimed: true,
    });
    fake.cancel_shutdown_once.store(true, Ordering::SeqCst);
    tokio::time::timeout(
        Duration::from_secs(1),
        replay(fake.clone(), home.path().into(), "one"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fake.stops.load(Ordering::SeqCst), 2);
    assert_eq!(
        fake.starts.load(Ordering::SeqCst),
        1,
        "replay must not start outside the registry gate"
    );
}

#[tokio::test(start_paused = true)]
async fn payload_replay_timeout_and_unsupported_load_always_shutdown() {
    let _guard = TEST_LOCK.lock().await;
    for (supported, hang) in [(false, false), (true, true)] {
        let home = tempfile::tempdir().unwrap();
        let fake = FakeReplay::new(supported, hang);
        let error = replay(fake.clone(), home.path().to_owned(), "one")
            .await
            .unwrap_err();
        assert!(error.contains(if hang {
            "timed out"
        } else {
            "does not support"
        }));
        assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
        assert_eq!(fake.loads.load(Ordering::SeqCst), usize::from(supported));
    }
}
#[test]
fn payload_replay_collects_only_requested_tool_and_preserves_unicode_deltas() {
    let replay = Replay {
        targets: Mutex::new(HashMap::from([("wanted".into(), Capture::default())])),
        started: AtomicBool::new(false),
        result: Mutex::new(None),
        done: Notify::new(),
    };
    collect(
        &replay,
        HarnessUpdateKind::ToolCall(
            serde_json::json!({"toolCallId":"other", "status":"completed", "rawOutput":"wrong"}),
        ),
    );
    for text in ["— ", "café"] {
        collect(
            &replay,
            HarnessUpdateKind::ToolCallUpdate(
                serde_json::json!({"toolCallId":"wanted", "_meta":{"terminal_output":{"data":text}}}),
            ),
        );
    }
    collect(
        &replay,
        HarnessUpdateKind::ToolCallUpdate(
            serde_json::json!({"toolCallId":"wanted", "status":"completed"}),
        ),
    );
    let targets = replay.targets.lock().unwrap();
    assert_eq!(targets.len(), 1);
    assert!(targets["wanted"].complete);
    assert_eq!(targets["wanted"].parts[0].bytes, "— café".as_bytes());
}
