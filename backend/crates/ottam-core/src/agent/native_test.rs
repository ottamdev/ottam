use super::types::{
    AdapterLifecycleState, AdapterStatus, AgentDefinition, HarnessAdapter, HarnessAuthState,
    HarnessCapabilities, HarnessError, HarnessErrorKind, HarnessFuture, HarnessPrompt,
    HarnessSessionId, HarnessSessionState, HarnessSessionSummary, HarnessStopReason, HarnessUpdate,
    HarnessUpdateKind, HarnessUpdateReceiver, HarnessUpdateSender, SessionConfigRequest,
    SteeringCapability,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Deterministic, in-process adapter registered only by the explicit
/// `OTTAM_TEST_NATIVE_STEER_ENGINE` process-test gate.
pub(super) struct NativeTestAdapter {
    definition: AgentDefinition,
    status_tx: watch::Sender<AdapterStatus>,
    updates_tx: HarnessUpdateSender,
    sessions: Mutex<BTreeMap<HarnessSessionId, NativeTestSession>>,
    next_session: AtomicU64,
}

struct NativeTestSession {
    summary: HarnessSessionSummary,
    cancel: CancellationToken,
    sequence: u64,
}

impl NativeTestAdapter {
    pub(super) fn new(definition: AgentDefinition, generation: u64) -> Self {
        let mut status = AdapterStatus::stopped();
        status.generation = generation.saturating_sub(1);
        status.capabilities = HarnessCapabilities {
            auth_status: false,
            load_session: true,
            delete_session: true,
            steering: SteeringCapability::Native,
            prompt_image: true,
            ..Default::default()
        };
        let (status_tx, _) = watch::channel(status);
        let updates_tx = HarnessUpdateSender::default();
        Self {
            definition,
            status_tx,
            updates_tx,
            sessions: Mutex::new(BTreeMap::new()),
            next_session: AtomicU64::new(1),
        }
    }

    fn send_update(
        &self,
        session_id: &HarnessSessionId,
        update: HarnessUpdateKind,
    ) -> Result<(), HarnessError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = sessions.get_mut(session_id).ok_or_else(|| {
            HarnessError::new(HarnessErrorKind::SessionNotFound, "unknown test session")
        })?;
        session.sequence = session.sequence.saturating_add(1);
        let _ = self.updates_tx.send(HarnessUpdate {
            session_id: session_id.clone(),
            sequence: session.sequence,
            update,
        });
        Ok(())
    }
}

impl HarnessAdapter for NativeTestAdapter {
    fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    fn status(&self) -> watch::Receiver<AdapterStatus> {
        self.status_tx.subscribe()
    }

    fn subscribe_updates(&self) -> HarnessUpdateReceiver {
        self.updates_tx.subscribe()
    }

    fn sessions(&self) -> Vec<HarnessSessionSummary> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|session| session.summary.clone())
            .collect()
    }

    fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
        self.status_tx.send_modify(|status| {
            if status.lifecycle != AdapterLifecycleState::Ready {
                status.generation = status.generation.saturating_add(1);
            }
            status.lifecycle = AdapterLifecycleState::Ready;
            status.auth_state = HarnessAuthState::Ready;
            status.agent_name = Some("Ottam native steering test adapter".to_string());
        });
        let status = self.status_tx.borrow().clone();
        Box::pin(async move { Ok(status) })
    }

    fn shutdown(&self) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for session in sessions.values() {
                session.cancel.cancel();
            }
            drop(sessions);
            self.status_tx
                .send_modify(|status| status.lifecycle = AdapterLifecycleState::Stopped);
            Ok(())
        })
    }

    fn authenticate(&self, _method_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn new_session(
        &self,
        cwd: PathBuf,
        _config: SessionConfigRequest,
    ) -> HarnessFuture<'_, HarnessSessionId> {
        Box::pin(async move {
            let number = self.next_session.fetch_add(1, Ordering::SeqCst);
            let session_id = HarnessSessionId::new(format!("test-native-session-{number}"));
            self.sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    session_id.clone(),
                    NativeTestSession {
                        summary: HarnessSessionSummary {
                            session_id: session_id.clone(),
                            cwd,
                            state: HarnessSessionState::Active,
                            last_error: None,
                            current_mode_id: None,
                            current_model_id: None,
                            current_thought_level_id: None,
                            announced_commands: None,
                        },
                        cancel: CancellationToken::new(),
                        sequence: 0,
                    },
                );
            Ok(session_id)
        })
    }

    fn load_session(&self, session_id: HarnessSessionId, cwd: PathBuf) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = sessions.get_mut(&session_id).ok_or_else(|| {
                HarnessError::new(HarnessErrorKind::SessionNotFound, "unknown test session")
            })?;
            session.summary.cwd = cwd;
            Ok(())
        })
    }

    fn delete_session(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            if let Some(session) = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&session_id)
            {
                session.cancel.cancel();
            }
            Ok(())
        })
    }

    fn prompt(
        &self,
        session_id: HarnessSessionId,
        _prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason> {
        Box::pin(async move {
            let cancel = {
                let mut sessions = self
                    .sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let session = sessions.get_mut(&session_id).ok_or_else(|| {
                    HarnessError::new(HarnessErrorKind::SessionNotFound, "unknown test session")
                })?;
                session.summary.state = HarnessSessionState::Prompting;
                session.cancel = CancellationToken::new();
                session.cancel.clone()
            };
            self.send_update(
                &session_id,
                HarnessUpdateKind::AgentMessageChunk(
                    "Native test turn is running.".to_string(),
                    None,
                ),
            )?;
            cancel.cancelled().await;
            if let Some(session) = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get_mut(&session_id)
            {
                session.summary.state = HarnessSessionState::Active;
            }
            Ok(HarnessStopReason::Cancelled)
        })
    }

    fn steer(&self, session_id: HarnessSessionId, prompt: HarnessPrompt) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            let text = prompt.text;
            self.send_update(
                &session_id,
                HarnessUpdateKind::AgentMessageChunk(
                    format!("Native steer accepted: {text}"),
                    None,
                ),
            )
        })
    }

    fn cancel(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = sessions.get(&session_id).ok_or_else(|| {
                HarnessError::new(HarnessErrorKind::SessionNotFound, "unknown test session")
            })?;
            session.cancel.cancel();
            Ok(())
        })
    }
}
