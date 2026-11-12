//! On-demand replay has no coordinator, publication sink, or execution requests.
use super::*;
use crate::agent::{AgentId, AgentRegistry, HarnessAdapter, HarnessSessionId, HarnessUpdateKind};
use std::{path::PathBuf, sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;

#[derive(Default)]
struct Capture {
    parts: Vec<Part>,
    complete: bool,
    error: Option<String>,
}
struct Replay {
    targets: Mutex<HashMap<String, Capture>>,
    started: AtomicBool,
    result: Mutex<Option<Result<(), String>>>,
    done: Notify,
}
type ReplayKey = (PathBuf, String, String, PathBuf);
static REPLAYS: OnceLock<Mutex<HashMap<ReplayKey, Weak<Replay>>>> = OnceLock::new();
static SLOTS: Semaphore = Semaphore::const_new(2);

pub(super) async fn load(
    registry: Arc<AgentRegistry>,
    home: PathBuf,
    agent: String,
    session: String,
    tool: String,
    cwd: PathBuf,
) -> Result<Vec<Vec<u8>>, String> {
    let factory_agent = agent.clone();
    load_with(home, agent, session, tool, cwd, move || async move {
        registry
            .create_replay_adapter(&AgentId(factory_agent))
            .await
            .map(|(adapter, initialized, admission)| {
                (
                    adapter,
                    initialized.map_err(|e| e.to_string()),
                    Some(admission),
                )
            })
            .map_err(|e| e.to_string())
    })
    .await
}

async fn load_with<
    F: std::future::Future<
            Output = Result<
                (
                    Arc<dyn HarnessAdapter>,
                    Result<crate::agent::AdapterStatus, String>,
                    Option<crate::agent::ReplayAdmission>,
                ),
                String,
            >,
        > + Send
        + 'static,
>(
    home: PathBuf,
    agent: String,
    session: String,
    tool: String,
    cwd: PathBuf,
    factory: impl FnOnce() -> F + Send + 'static,
) -> Result<Vec<Vec<u8>>, String> {
    // Also bound the caller if a replay task unexpectedly panics. The adapter
    // has a 60-second work deadline plus a bounded shutdown grace period.
    let completion_deadline = tokio::time::Instant::now() + Duration::from_secs(65);
    let key = (home, agent.clone(), session.clone(), cwd.clone());
    let (replay, leader) = loop {
        let selection = {
            let mut replays = REPLAYS.get_or_init(Default::default).lock().unwrap();
            replays.retain(|_, replay| replay.strong_count() > 0);
            let existing = replays.get(&key).and_then(Weak::upgrade);
            if let Some(replay) = &existing {
                let mut targets = replay.targets.lock().unwrap();
                if targets.contains_key(&tool) || !replay.started.load(Ordering::Acquire) {
                    targets.entry(tool.clone()).or_default();
                    break (replay.clone(), false);
                }
            }
            // Once replay starts, a newly requested tool might already have passed.
            // Wait for that session's pass to finish before starting another pass.
            if existing
                .as_ref()
                .is_some_and(|r| r.result.lock().unwrap().is_none())
            {
                existing
            } else {
                let replay = Arc::new(Replay {
                    targets: Mutex::new(HashMap::from([(tool.clone(), Capture::default())])),
                    started: AtomicBool::new(false),
                    result: Mutex::new(None),
                    done: Notify::new(),
                });
                replays.insert(key.clone(), Arc::downgrade(&replay));
                break (replay, true);
            }
        };
        let previous = selection.expect("running replay");
        let notified = previous.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if previous.result.lock().unwrap().is_none() {
            tokio::time::timeout_at(completion_deadline, notified)
                .await
                .map_err(|_| "Session replay did not complete".to_owned())?;
        }
    };
    // Waiting for an earlier session pass has its own bounded deadline. Once
    // admitted to a pass, allow its full work and shutdown budget afresh.
    let completion_deadline = tokio::time::Instant::now() + Duration::from_secs(65);
    if leader {
        let replay = replay.clone();
        tokio::spawn(async move {
            let result = async {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                let _slot = tokio::time::timeout_at(deadline, SLOTS.acquire()).await
                    .map_err(|_| "Session replay timed out".to_owned())?.map_err(|e| e.to_string())?;
                let (adapter, initialized, _admission) = factory().await?;
                let mut updates = adapter.subscribe_updates();
                let operation = async {
                    let status = initialized?;
                    if !status.capabilities.load_session { return Err("This harness does not support session replay".into()); }
                    {
                        let _targets = replay.targets.lock().unwrap();
                        replay.started.store(true, Ordering::Release);
                    }
                    let loading = adapter.load_session(HarnessSessionId::new(session.clone()), cwd);
                    tokio::pin!(loading);
                    loop {
                        tokio::select! {
                            result = &mut loading => {
                                result.map_err(|e| e.to_string())?;
                                while let Ok(update) = updates.try_recv() {
                                    if update.session_id.0 == session { collect(&replay, update.update); }
                                }
                                return Ok(());
                            }
                            update = updates.recv() => {
                                let update = update.ok_or_else(|| "Session replay lost its adapter".to_owned())?;
                                if update.session_id.0 == session { collect(&replay, update.update); }
                            }
                        }
                    }
                };
                let result = tokio::time::timeout_at(deadline, operation).await
                    .map_err(|_| "Session replay timed out".to_owned()).and_then(|r| r);
                if adapter.definition().refresh_lock_path().is_some() {
                    // Retain replay ownership through refresh and child exit.
                    // The registry tracks this adapter alongside chat adapters.
                    loop {
                        match adapter.shutdown().await {
                            Err(error) if matches!(error.kind, crate::agent::HarnessErrorKind::AuthRefreshBusy | crate::agent::HarnessErrorKind::RequestCancelled) => {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            Err(_) if matches!(adapter.status().borrow().lifecycle, crate::agent::AdapterLifecycleState::Starting | crate::agent::AdapterLifecycleState::Ready | crate::agent::AdapterLifecycleState::Stopping) => {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            _ => break,
                        }
                    }
                } else {
                    let _ = tokio::time::timeout(Duration::from_secs(5), adapter.shutdown()).await;
                }
                result
            }.await;
            *replay.result.lock().unwrap() = Some(result);
            replay.done.notify_waiters();
        });
    }
    loop {
        let notified = replay.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = replay.result.lock().unwrap().as_ref() {
            result.clone()?;
            let targets = replay.targets.lock().unwrap();
            let capture = &targets[&tool];
            if let Some(error) = &capture.error {
                return Err(error.clone());
            }
            if !capture.complete {
                return Err("The saved session did not contain this completed tool result".into());
            }
            return Ok(capture.parts.iter().map(|p| p.bytes.clone()).collect());
        }
        tokio::time::timeout_at(completion_deadline, notified)
            .await
            .map_err(|_| "Session replay did not complete".to_owned())?;
    }
}

fn collect(replay: &Replay, update: HarnessUpdateKind) {
    let mut payload = match update {
        HarnessUpdateKind::ToolCall(p) | HarnessUpdateKind::ToolCallUpdate(p) => p,
        _ => return,
    };
    let Some(id) = payload
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let mut targets = replay.targets.lock().unwrap();
    let other_bytes = targets
        .iter()
        .filter(|(tool, _)| **tool != id)
        .flat_map(|(_, capture)| &capture.parts)
        .map(|part| part.bytes.len())
        .sum::<usize>();
    let Some(capture) = targets.get_mut(&id) else {
        return;
    };
    if capture.error.is_some() {
        return;
    }
    for update in extract(&mut payload)
        .into_iter()
        .filter(|u| u.role == "output")
    {
        capture.complete |= update.publish;
        if let Some(parts) = update.parts {
            if update.append && !capture.parts.is_empty() {
                if parts.len() != capture.parts.len() {
                    capture.error = Some("Replayed result shape changed".into());
                    return;
                }
                for (destination, source) in capture.parts.iter_mut().zip(parts) {
                    if destination.bytes.len().saturating_add(source.bytes.len()) > LIMIT {
                        capture.error = Some("Replayed result exceeds capture limit".into());
                        return;
                    }
                    destination.bytes.extend(source.bytes);
                }
            } else {
                capture.parts = parts;
            }
            let bytes = capture.parts.iter().map(|p| p.bytes.len()).sum::<usize>();
            if bytes > LIMIT || bytes.saturating_add(other_bytes) > 2 * LIMIT {
                capture.parts.clear();
                capture.error = Some("Replayed result exceeds capture limit".into());
            }
        }
    }
}

#[cfg(test)]
#[path = "../payload_recovery_tests.rs"]
mod tests;
