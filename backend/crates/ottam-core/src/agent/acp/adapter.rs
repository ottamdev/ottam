use super::super::types::{
    AdapterLifecycleState, AdapterStatus, AgentDefinition, HarnessAdapter, HarnessError,
    HarnessErrorKind, HarnessFuture, HarnessPermissionBroker, HarnessPrompt, HarnessSessionId,
    HarnessSessionSummary, HarnessStopReason, HarnessUpdateReceiver, HarnessUpdateSender,
    McpServerEntry, ModelSwitch, SessionConfigRequest, SteeringCapability,
};
use super::actor::{
    AcpBackend, AdapterBackend, AdapterCommand, BackendContext, SharedState, COMMAND_CAPACITY,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

#[cfg(not(test))]
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(2);
#[cfg(test)]
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_millis(50);

pub(crate) struct AcpHarnessAdapter {
    definition: AgentDefinition,
    shared: Arc<SharedState>,
    backend: Arc<dyn AdapterBackend>,
    runtime: Mutex<RuntimeControl>,
    operation_lock: Mutex<()>,
    permission_broker: Arc<dyn HarnessPermissionBroker>,
}

struct RuntimeControl {
    command_tx: Option<mpsc::Sender<AdapterCommand>>,
    supervisor: Option<JoinHandle<()>>,
}

enum StartAction {
    // Boxed for variant-size parity: `AdapterStatus` carries the model and
    // select catalogs inline.
    Immediate(Box<Result<AdapterStatus, HarnessError>>),
    Initializing(oneshot::Receiver<Result<AdapterStatus, HarnessError>>),
    Existing(watch::Receiver<AdapterStatus>),
}

enum ShutdownAction {
    Complete,
    Existing(watch::Receiver<AdapterStatus>),
    Initiate {
        command_tx: Option<mpsc::Sender<AdapterCommand>>,
        previous: AdapterLifecycleState,
    },
}

struct SharedShutdownCancellation<'a> {
    adapter: &'a AcpHarnessAdapter,
    complete: bool,
    previous: AdapterLifecycleState,
    acknowledgement: Option<oneshot::Receiver<()>>,
}

impl Drop for SharedShutdownCancellation<'_> {
    fn drop(&mut self) {
        let committed = self.acknowledgement.as_mut().is_some_and(|reply| {
            !matches!(reply.try_recv(), Err(oneshot::error::TryRecvError::Empty))
        });
        if !self.complete
            && !committed
            && self.adapter.shared.status().lifecycle == AdapterLifecycleState::Stopping
        {
            self.adapter.shared.set_lifecycle(self.previous, None);
        }
    }
}

impl AcpHarnessAdapter {
    pub(crate) fn with_generation_and_permission_broker(
        definition: AgentDefinition,
        generation: u64,
        permission_broker: Arc<dyn HarnessPermissionBroker>,
    ) -> Self {
        Self::with_backend_and_permission_broker(
            definition,
            generation,
            Arc::new(AcpBackend),
            permission_broker,
        )
    }

    #[cfg(test)]
    pub(super) fn with_backend(
        definition: AgentDefinition,
        generation: u64,
        backend: Arc<dyn AdapterBackend>,
    ) -> Self {
        Self::with_backend_and_permission_broker(
            definition,
            generation,
            backend,
            Arc::new(super::super::types::CancelPermissionBroker),
        )
    }

    fn with_backend_and_permission_broker(
        definition: AgentDefinition,
        generation: u64,
        backend: Arc<dyn AdapterBackend>,
        permission_broker: Arc<dyn HarnessPermissionBroker>,
    ) -> Self {
        let mut status = AdapterStatus::stopped();
        status.generation = generation.saturating_sub(1);
        status.capabilities.steering = SteeringCapability::InterruptThenPrompt;
        let (status_tx, _) = watch::channel(status);
        let updates_tx = HarnessUpdateSender::default();
        let child_sessions = definition
            .child_sessions
            .map(super::child_sessions::ChildSessions::new);
        let sign_in_proof = definition.sign_in_proof;
        Self {
            definition,
            shared: Arc::new(SharedState {
                status_tx,
                updates_tx,
                sessions: Mutex::new(BTreeMap::new()),
                update_sequences: Mutex::new(BTreeMap::new()),
                announced_commands: Mutex::new(BTreeMap::new()),
                child_sessions,
                sign_in_proof,
            }),
            backend,
            runtime: Mutex::new(RuntimeControl {
                command_tx: None,
                supervisor: None,
            }),
            operation_lock: Mutex::new(()),
            permission_broker,
        }
    }

    fn begin_start(&self) -> StartAction {
        let _operation = self
            .operation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = self.shared.status().lifecycle;
        match previous {
            AdapterLifecycleState::Ready => {
                return StartAction::Immediate(Box::new(Ok(self.shared.status())));
            }
            AdapterLifecycleState::Dead => {
                return StartAction::Immediate(Box::new(Err(HarnessError::new(
                    HarnessErrorKind::AdapterDead,
                    format!(
                        "{} adapter is dead; restart it explicitly",
                        self.definition.agent_id
                    ),
                ))));
            }
            AdapterLifecycleState::Starting => {
                return StartAction::Existing(self.shared.status_tx.subscribe());
            }
            AdapterLifecycleState::Stopping => {
                return StartAction::Immediate(Box::new(Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!(
                        "{} adapter lifecycle transition is already in progress",
                        self.definition.agent_id
                    ),
                ))));
            }
            AdapterLifecycleState::Stopped => {}
        }

        self.shared.begin_start();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let context = BackendContext {
            definition: self.definition.clone(),
            command_rx,
            ready_tx: Arc::new(Mutex::new(Some(ready_tx))),
            shared: Arc::clone(&self.shared),
            permission_broker: Arc::clone(&self.permission_broker),
        };
        let backend = Arc::clone(&self.backend);
        let shared = Arc::clone(&self.shared);
        let supervisor = tokio::spawn(async move {
            match backend.run(context).await {
                Ok(()) if shared.status().lifecycle == AdapterLifecycleState::Stopping => {
                    shared.mark_stopped("adapter stopped");
                }
                Ok(()) => shared.mark_dead("adapter exited unexpectedly"),
                Err(_) if shared.status().lifecycle == AdapterLifecycleState::Stopping => {
                    shared.mark_stopped("adapter stopped");
                }
                Err(error) => shared.mark_dead(&error.message),
            }
        });
        let mut runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.command_tx = Some(command_tx);
        runtime.supervisor = Some(supervisor);
        StartAction::Initializing(ready_rx)
    }

    async fn finish_start(action: StartAction) -> Result<AdapterStatus, HarnessError> {
        match action {
            StartAction::Immediate(result) => *result,
            StartAction::Initializing(ready_rx) => match ready_rx.await {
                Ok(result) => result,
                Err(_) => Err(HarnessError::new(
                    HarnessErrorKind::AdapterExited,
                    "adapter exited before initialization",
                )),
            },
            StartAction::Existing(mut status) => loop {
                let current = status.borrow().clone();
                match current.lifecycle {
                    AdapterLifecycleState::Ready => return Ok(current),
                    AdapterLifecycleState::Dead => {
                        return Err(HarnessError::new(
                            HarnessErrorKind::AdapterDead,
                            current.last_error.unwrap_or_else(|| {
                                "adapter exited during initialization".to_string()
                            }),
                        ));
                    }
                    AdapterLifecycleState::Stopped | AdapterLifecycleState::Stopping => {
                        return Err(HarnessError::new(
                            HarnessErrorKind::RequestCancelled,
                            "adapter initialization was cancelled",
                        ));
                    }
                    AdapterLifecycleState::Starting => {}
                }
                status.changed().await.map_err(|_| {
                    HarnessError::new(
                        HarnessErrorKind::AdapterExited,
                        "adapter status channel closed during initialization",
                    )
                })?;
            },
        }
    }

    fn begin_shutdown(&self) -> ShutdownAction {
        let _operation = self
            .operation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = self.shared.status().lifecycle;
        match previous {
            AdapterLifecycleState::Stopped => ShutdownAction::Complete,
            AdapterLifecycleState::Stopping => {
                ShutdownAction::Existing(self.shared.status_tx.subscribe())
            }
            AdapterLifecycleState::Starting
            | AdapterLifecycleState::Ready
            | AdapterLifecycleState::Dead => {
                self.shared
                    .set_lifecycle(AdapterLifecycleState::Stopping, None);
                let command_tx = self
                    .runtime
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .command_tx
                    .clone();
                ShutdownAction::Initiate {
                    command_tx,
                    previous,
                }
            }
        }
    }

    async fn finish_existing_shutdown(
        mut status: watch::Receiver<AdapterStatus>,
    ) -> Result<(), HarnessError> {
        let wait = async {
            loop {
                if status.borrow().lifecycle == AdapterLifecycleState::Stopped {
                    return Ok(());
                }
                status.changed().await.map_err(|_| {
                    HarnessError::new(
                        HarnessErrorKind::AdapterExited,
                        "adapter status channel closed during shutdown",
                    )
                })?;
            }
        };
        tokio::time::timeout(SHUTDOWN_GRACE_PERIOD.saturating_mul(3), wait)
            .await
            .unwrap_or_else(|_| {
                Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    "concurrent adapter shutdown did not complete",
                ))
            })
    }

    async fn shutdown_inner(&self, action: ShutdownAction) -> Result<(), HarnessError> {
        if self.definition.refresh_lock_path().is_some() {
            return self.shutdown_shared_home(action).await;
        }
        let command_tx = match action {
            ShutdownAction::Complete => return Ok(()),
            ShutdownAction::Existing(status) => {
                return Self::finish_existing_shutdown(status).await;
            }
            ShutdownAction::Initiate { command_tx, .. } => command_tx,
        };

        let shutdown_acknowledged = if let Some(command_tx) = command_tx {
            let (reply_tx, reply_rx) = oneshot::channel();
            tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, async move {
                if command_tx
                    .send(AdapterCommand::Shutdown { reply: reply_tx })
                    .await
                    .is_err()
                {
                    return false;
                }
                reply_rx.await.is_ok()
            })
            .await
            .unwrap_or(false)
        } else {
            false
        };

        let supervisor = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime.command_tx = None;
            runtime.supervisor.take()
        };
        if let Some(mut supervisor) = supervisor {
            let should_abort = if shutdown_acknowledged {
                tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, &mut supervisor)
                    .await
                    .is_err()
            } else {
                true
            };
            if should_abort {
                super::super::refresh_lock::wait_for_release(
                    self.definition.refresh_lock_path().as_deref(),
                )
                .await;
                supervisor.abort();
                let _ = supervisor.await;
            }
        }
        self.shared.mark_stopped("adapter stopped");
        Ok(())
    }

    async fn shutdown_shared_home(&self, action: ShutdownAction) -> Result<(), HarnessError> {
        let (command_tx, previous) = match action {
            ShutdownAction::Complete => return Ok(()),
            ShutdownAction::Existing(mut status) => {
                while status.borrow().lifecycle == AdapterLifecycleState::Stopping {
                    status.changed().await.map_err(|_| {
                        HarnessError::new(
                            HarnessErrorKind::AdapterExited,
                            "adapter status closed during shutdown",
                        )
                    })?;
                }
                return if status.borrow().lifecycle == AdapterLifecycleState::Ready {
                    Err(HarnessError::new(
                        HarnessErrorKind::RequestCancelled,
                        "shutdown preparation cancelled",
                    ))
                } else {
                    Ok(())
                };
            }
            ShutdownAction::Initiate {
                command_tx,
                previous,
            } => (command_tx, previous),
        };
        let mut cancellation = SharedShutdownCancellation {
            adapter: self,
            complete: false,
            previous,
            acknowledgement: None,
        };
        if let Some(command_tx) = command_tx {
            if let Ok(permit) = command_tx.reserve().await {
                let (reply_tx, reply_rx) = oneshot::channel();
                cancellation.acknowledgement = Some(reply_rx);
                permit.send(AdapterCommand::Shutdown { reply: reply_tx });
                let _ = cancellation.acknowledgement.as_mut().unwrap().await;
            }
        }
        // Beyond the acknowledgement the actor has committed to closing.
        // Cancellation can abandon the caller, but must never invent Ready.
        cancellation.complete = true;
        // A dead supervisor cannot publish a new lifecycle transition. Wait
        // for its actual completion rather than a watch update that won't come.
        while self
            .runtime
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .supervisor
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let supervisor = {
            let mut runtime = self
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            runtime.command_tx = None;
            runtime.supervisor.take()
        };
        if let Some(supervisor) = supervisor {
            let _ = supervisor.await;
        }
        self.shared.mark_stopped("adapter stopped");
        Ok(())
    }

    pub(super) fn command_sender(&self) -> Result<mpsc::Sender<AdapterCommand>, HarnessError> {
        match self.shared.status().lifecycle {
            AdapterLifecycleState::Dead => Err(HarnessError::new(
                HarnessErrorKind::AdapterDead,
                format!("{} adapter is dead", self.definition.agent_id),
            )),
            AdapterLifecycleState::Ready => self
                .runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .command_tx
                .clone()
                .ok_or_else(|| {
                    HarnessError::new(
                        HarnessErrorKind::NotRunning,
                        "adapter command channel is closed",
                    )
                }),
            _ => Err(HarnessError::new(
                HarnessErrorKind::NotRunning,
                format!("{} adapter is not running", self.definition.agent_id),
            )),
        }
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, HarnessError>>) -> AdapterCommand,
    ) -> Result<T, HarnessError> {
        let sender = self.command_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        sender.send(build(reply_tx)).await.map_err(|_| {
            HarnessError::new(
                HarnessErrorKind::AdapterExited,
                "adapter command channel closed",
            )
        })?;
        reply_rx.await.map_err(|_| {
            HarnessError::new(
                HarnessErrorKind::AdapterExited,
                "adapter exited before replying",
            )
        })?
    }
}

impl HarnessAdapter for AcpHarnessAdapter {
    fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    fn status(&self) -> watch::Receiver<AdapterStatus> {
        self.shared.status_tx.subscribe()
    }

    fn subscribe_updates(&self) -> HarnessUpdateReceiver {
        self.shared.updates_tx.subscribe()
    }

    fn sessions(&self) -> Vec<HarnessSessionSummary> {
        self.shared.sessions()
    }

    fn prompt_settled_at(&self, session_id: &HarnessSessionId) -> Option<u64> {
        self.shared.prompt_settled_at(session_id)
    }

    fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
        Box::pin(Self::finish_start(self.begin_start()))
    }

    fn shutdown(&self) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            if self.shared.status().lifecycle == AdapterLifecycleState::Stopped {
                return Ok(());
            }
            let finished = self
                .runtime
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .supervisor
                .as_ref()
                .is_none_or(|task| task.is_finished());
            if super::super::refresh_lock::is_refreshing(
                self.definition.refresh_lock_path().as_deref(),
            ) && !(self.shared.status().lifecycle == AdapterLifecycleState::Dead && finished)
            {
                return Err(HarnessError::new(
                    HarnessErrorKind::AuthRefreshBusy,
                    "Waiting for Claude to finish refreshing sign-in…",
                ));
            }
            // Stopping kills the CLI. Mid-refresh, that loses the rotated
            // token and leaves the lock behind, so let a refresh in Core's own
            // home finish first. Bounded, unlike a shared home's fence: an
            // ownerless lock here must not stall Quit or a probe forever.
            if let Some(lock) = self.definition.managed_refresh_lock() {
                if !super::super::refresh_lock::wait_for_release_within(
                    &lock,
                    super::super::refresh_lock::MANAGED_REFRESH_WAIT,
                )
                .await
                {
                    tracing::warn!(target: "ottam_core::agent",
                        agent_id = %self.definition.agent_id,
                        "stopping an adapter whose home is still refreshing sign-in");
                }
            }
            self.shutdown_inner(self.begin_shutdown()).await
        })
    }

    fn authenticate(&self, method_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(async move {
            if !self
                .shared
                .status()
                .auth_methods
                .iter()
                .any(|method| method.method_id == method_id)
            {
                return Err(HarnessError::new(
                    HarnessErrorKind::InvalidAuthMethod,
                    format!("unknown authentication method {method_id:?}"),
                ));
            }
            self.request(|reply| AdapterCommand::Authenticate { method_id, reply })
                .await
        })
    }

    fn cancel_authenticate(&self, method_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::CancelAuthenticate { method_id, reply }))
    }

    fn new_session(
        &self,
        cwd: PathBuf,
        config: SessionConfigRequest,
    ) -> HarnessFuture<'_, HarnessSessionId> {
        Box::pin(self.request(|reply| AdapterCommand::NewSession { cwd, config, reply }))
    }

    fn load_session(&self, session_id: HarnessSessionId, cwd: PathBuf) -> HarnessFuture<'_, ()> {
        self.load_session_with_mode(session_id, cwd, None)
    }

    fn load_session_with_mode(
        &self,
        session_id: HarnessSessionId,
        cwd: PathBuf,
        mode_id: Option<String>,
    ) -> HarnessFuture<'_, ()> {
        self.load_session_with_config(session_id, cwd, mode_id, Vec::new())
    }

    fn load_session_with_config(
        &self,
        session_id: HarnessSessionId,
        cwd: PathBuf,
        mode_id: Option<String>,
        mcp_servers: Vec<McpServerEntry>,
    ) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::LoadSession {
            session_id,
            cwd,
            mode_id,
            mcp_servers,
            reply,
        }))
    }

    fn apply_mode(&self, session_id: HarnessSessionId, mode_id: String) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::ApplyMode {
            session_id,
            mode_id,
            reply,
        }))
    }

    fn apply_model(&self, session_id: HarnessSessionId, switch: ModelSwitch) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::ApplyModel {
            session_id,
            switch,
            reply,
        }))
    }

    fn delete_session(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::DeleteSession { session_id, reply }))
    }

    fn prompt(
        &self,
        session_id: HarnessSessionId,
        prompt: HarnessPrompt,
    ) -> HarnessFuture<'_, HarnessStopReason> {
        Box::pin(self.request(|reply| AdapterCommand::Prompt {
            session_id,
            prompt,
            reply,
        }))
    }

    fn steer(&self, session_id: HarnessSessionId, prompt: HarnessPrompt) -> HarnessFuture<'_, ()> {
        // Read live rather than from the cached registry status: the capability
        // is only known once the adapter has shaken hands, and a run is always
        // active by the time anything steers.
        if self.status().borrow().capabilities.steering != SteeringCapability::Native {
            return Box::pin(async {
                Err(HarnessError::new(
                    HarnessErrorKind::SteeringUnsupported,
                    "this agent steers through interrupt-and-continue, not `_session/steering`",
                ))
            });
        }
        Box::pin(self.request(|reply| AdapterCommand::Steer {
            session_id,
            prompt,
            reply,
        }))
    }

    fn cancel(&self, session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
        Box::pin(self.request(|reply| AdapterCommand::Cancel { session_id, reply }))
    }

    fn cancel_out_of_turn(&self, session_id: &HarnessSessionId) -> HarnessFuture<'_, ()> {
        let session_id = session_id.clone();
        Box::pin(self.request(|reply| AdapterCommand::CancelOutOfTurn { session_id, reply }))
    }

    fn stop_background_task(
        &self,
        session_id: &HarnessSessionId,
        async_task_id: &str,
    ) -> HarnessFuture<'_, bool> {
        let session_id = session_id.clone();
        let async_task_id = async_task_id.to_string();
        Box::pin(self.request(|reply| AdapterCommand::StopAsyncTask {
            session_id,
            async_task_id,
            reply,
        }))
    }
}
