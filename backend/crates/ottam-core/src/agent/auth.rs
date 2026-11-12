//! Sign-in orchestration for agent adapters.
//!
//! ACP describes two shapes of login and Ottam offers one flow over both.
//! Which shape an agent uses is read off the advertised method's *variant*, not
//! off the agent's id, so an adapter that changes its mind keeps working:
//!
//! - [`AgentAuthDelivery::AdapterBrowser`] — the adapter runs the exchange
//!   itself (`authenticate`), opening a browser and answering when it is done.
//!   Codex works this way.
//! - [`AgentAuthDelivery::ClientTerminal`] — the adapter hands over a command
//!   line and expects the client to run it. Claude works this way. The command
//!   also opens a browser in the ordinary case, so its terminal stays hidden;
//!   it is revealed only when the login needs to be seen.
//!
//! Because both mechanisms end in a browser, both look identical to the user:
//! press "Sign in", finish in the browser, come back signed in.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use ottam_protocol::{
    AgentAuthDelivery, AgentAuthPhase, AgentAuthSessionID, AgentAuthTerminal, AgentID,
    AgentSignInEventParams, AgentSignInParams, AgentSignInResult, TerminalID, TerminalSessionID,
    WorkspaceID,
};
use tokio::sync::oneshot;
// Tokio's clock, not the system's, so the reveal and lifetime timers advance
// under `tokio::time::pause()` and the flow can be tested without waiting.
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::registry::AgentRegistry;
use super::types::{
    AgentDefinition, HarnessAdapter, HarnessAuthMethod, HarnessAuthMethodKind, HarnessError,
    HarnessErrorKind, SignInProof, TerminalLoginReveal,
};
use crate::core_error::CoreError;

/// Starting a terminal fails the way the terminal layer fails, not the way an
/// adapter does, so this cannot reuse `HarnessFuture`.
pub(crate) type AuthTerminalFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthTerminalSession, CoreError>> + Send + 'a>>;

/// Methods core will never choose on its own. They authenticate from a secret
/// this flow has no way to obtain — offering the user a button that opens
/// nothing would be worse than not offering one.
///
/// An explicit `methodID` still reaches the adapter: the client asked for it by
/// name, and the adapter's own refusal is a better message than ours.
const SECRET_ONLY_METHOD_IDS: &[&str] = &["api-key", "apikey"];

/// Preferred method ids, best first. These are the subscription logins — the
/// ones a person signing in to Ottam almost always wants — for the agents
/// shipped today. An agent whose ids are not listed falls through to the
/// structural ordering below, so this list is a nicety, not a requirement.
const PREFERRED_METHOD_IDS: &[&str] = &["claude-ai-login", "chat-gpt", "claude-login"];

/// How long a login may run, and how patient the reveal is.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AuthTimingPolicy {
    /// Silence this long reveals the terminal. Output is the only evidence core
    /// has, and it cannot distinguish "waiting for the browser" from "waiting
    /// for someone to type" — so this errs toward showing a healthy flow rather
    /// than hiding a stuck one. An over-eager reveal corrects itself: the login
    /// finishes, `succeeded` arrives, and the client closes what it opened.
    pub reveal_after_silence: Duration,
    /// How often the output counter is sampled.
    pub poll_interval: Duration,
    /// A login that has run this long is abandoned and reported failed. Without
    /// it a wedged command would hold a PTY until the app quits.
    pub lifetime_cap: Duration,
}

impl Default for AuthTimingPolicy {
    fn default() -> Self {
        Self {
            reveal_after_silence: Duration::from_secs(45),
            poll_interval: Duration::from_secs(1),
            lifetime_cap: Duration::from_secs(300),
        }
    }
}

/// Where a login terminal runs. The login writes to the CLI's own config
/// directory, so this only decides a working directory — but the two cases
/// resolve differently enough (a DB lookup versus a path on disk) that the host
/// has to branch on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthTerminalRoot {
    /// The caller named a workspace; run in its own files.
    Workspace(WorkspaceID),
    /// No workspace — run in the agent's isolated home, which always exists.
    AgentHome(std::path::PathBuf),
}

/// A terminal to start for a sign-in, with everything already resolved.
pub(crate) struct AuthTerminalRequest {
    pub root: AuthTerminalRoot,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Answered when the login command exits. Core's own signal — the
    /// `terminal.exited` notification goes to a client that never asked for
    /// this terminal.
    pub exit_observer: oneshot::Sender<Option<i32>>,
}

/// A started sign-in terminal, addressed exactly like any other terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthTerminalSession {
    pub session_id: TerminalSessionID,
    pub terminal_id: TerminalID,
    pub rows: u16,
    pub cols: u16,
}

impl From<&AuthTerminalSession> for AgentAuthTerminal {
    fn from(session: &AuthTerminalSession) -> Self {
        Self {
            session_id: session.session_id.clone(),
            terminal_id: session.terminal_id.clone(),
            rows: session.rows,
            cols: session.cols,
        }
    }
}

/// The terminal capability a sign-in needs.
///
/// Behind a trait so this module does not reach into the terminal layer, and so
/// the flow — method choice, reveal timing, restart on success — can be tested
/// without a PTY.
pub(crate) trait AuthTerminalHost: Send + Sync {
    fn spawn(&self, request: AuthTerminalRequest) -> AuthTerminalFuture<'_>;
    /// Bytes the session has produced, or `None` if it is no longer registered.
    fn output_bytes(&self, session_id: &TerminalSessionID) -> Option<u64>;
    fn close(&self, session_id: &TerminalSessionID);
}

/// Where `agent.signInEvent` notifications go. A plain callback so tests can
/// collect them without a JSONL loop.
pub(crate) type AuthEventSink = Arc<dyn Fn(AgentSignInEventParams) + Send + Sync>;

/// Called once per sign-in that ends in `Succeeded`, with the agent that
/// signed in. The server hangs cache invalidation off it (the model-catalog
/// probe that failed while signed out is retried right away).
pub(crate) type SignedInSink = Arc<dyn Fn(&AgentID) + Send + Sync>;

struct LiveSignIn {
    auth_session_id: AgentAuthSessionID,
    delivery: AgentAuthDelivery,
    terminal: Option<AgentAuthTerminal>,
    cancel: CancellationToken,
    /// Fired by `agent.signInFinish`: the user says the login is complete.
    /// Only the terminal supervisor listens; for an adapter-browser login the
    /// token simply never matters.
    finish: CancellationToken,
}

/// Shared with the supervisor tasks, which outlive the request that started
/// them and have to clear their own entry when they finish.
type LiveSignIns = Arc<StdMutex<HashMap<AgentID, LiveSignIn>>>;

/// Holds the same per-agent boundary used to create sign-ins. While this is
/// alive no new sign-in can start, so uninstall can safely shut down and remove
/// the adapter/home after confirming there is no existing flow.
#[derive(Debug)]
pub(crate) struct AgentSignInRemovalGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// Drop a finished session, but only if it is still the current one — a cancel
/// followed immediately by a fresh sign-in must not have the old supervisor
/// tear down the new attempt.
fn retire(live: &LiveSignIns, agent_id: &AgentID, auth_session_id: &AgentAuthSessionID) {
    let mut live = live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if live
        .get(agent_id)
        .is_some_and(|session| session.auth_session_id == *auth_session_id)
    {
        live.remove(agent_id);
    }
}

/// One live sign-in per agent, the flow behind it, and the plumbing both
/// mechanisms need.
pub(crate) struct AgentSignInService {
    registry: Arc<AgentRegistry>,
    terminals: Arc<dyn AuthTerminalHost>,
    events: AuthEventSink,
    signed_in: Option<SignedInSink>,
    policy: AuthTimingPolicy,
    live: LiveSignIns,
    /// Serializes creation for one agent without making unrelated agents wait
    /// for each other's runtime probes or terminal startup.
    start_locks: StdMutex<HashMap<AgentID, Arc<tokio::sync::Mutex<()>>>>,
}

impl AgentSignInService {
    pub(crate) fn new(
        registry: Arc<AgentRegistry>,
        terminals: Arc<dyn AuthTerminalHost>,
        events: AuthEventSink,
        signed_in: Option<SignedInSink>,
    ) -> Self {
        Self::with_policy(
            registry,
            terminals,
            events,
            signed_in,
            AuthTimingPolicy::default(),
        )
    }

    pub(crate) fn with_policy(
        registry: Arc<AgentRegistry>,
        terminals: Arc<dyn AuthTerminalHost>,
        events: AuthEventSink,
        signed_in: Option<SignedInSink>,
        policy: AuthTimingPolicy,
    ) -> Self {
        Self {
            registry,
            terminals,
            events,
            signed_in,
            policy,
            live: Arc::new(StdMutex::new(HashMap::new())),
            start_locks: StdMutex::new(HashMap::new()),
        }
    }

    /// Begin a sign-in, or re-join the one already running for this agent.
    ///
    /// Returning the live session rather than starting a second one is what
    /// lets the failed-run card and the engine picker both offer the button
    /// without either having to know about the other.
    pub(crate) async fn sign_in(
        &self,
        params: AgentSignInParams,
    ) -> Result<AgentSignInResult, HarnessError> {
        if let Some(existing) = self.resume_session(&params.agent_id) {
            return Ok(existing);
        }

        let agent_id = super::types::AgentId(params.agent_id.0.clone());
        // Validate before retaining a lock entry, so invalid wire ids cannot
        // grow the per-agent lock table. An existing CLI home signs in too: the
        // login is the CLI's own, run with that home's variables, so it writes
        // exactly what a terminal login would. Signing out stays with the CLI.
        self.registry.definition(&agent_id)?;
        let start_lock = self.start_lock(&params.agent_id);
        let _start_guard = start_lock.lock().await;
        // The first caller may have installed the live session while this one
        // waited for the per-agent creation lock.
        if let Some(existing) = self.resume_session(&params.agent_id) {
            return Ok(existing);
        }

        let adapter = self.registry.get_or_start(&agent_id).await?;
        let status = adapter.status().borrow().clone();
        let method = select_auth_method(
            &status.auth_methods,
            params.method_id.as_deref(),
            adapter.definition().sign_in_proof,
        )?;

        let delivery = delivery_for(&method);
        let auth_session_id = AgentAuthSessionID(format!("auth_{}", uuid::Uuid::now_v7()));
        let cancel = CancellationToken::new();
        let finish = CancellationToken::new();

        match delivery {
            AgentAuthDelivery::ClientTerminal => {
                let command = terminal_command(adapter.definition(), &method)?;
                let (exit_tx, exit_rx) = oneshot::channel();
                let session = self
                    .terminals
                    .spawn(AuthTerminalRequest {
                        root: self.terminal_root(&agent_id, params.workspace_id.clone()),
                        program: command.program,
                        args: command.args,
                        env: command.env,
                        exit_observer: exit_tx,
                    })
                    .await
                    .map_err(|error| {
                        HarnessError::new(
                            HarnessErrorKind::Internal,
                            format!("failed to start the sign-in terminal: {error}"),
                        )
                    })?;
                self.register(
                    &params.agent_id,
                    &auth_session_id,
                    delivery,
                    Some(AgentAuthTerminal::from(&session)),
                    cancel.clone(),
                    finish.clone(),
                );
                tokio::spawn(supervise_terminal_login(
                    TerminalLoginContext {
                        live: Arc::clone(&self.live),
                        registry: Arc::clone(&self.registry),
                        terminals: Arc::clone(&self.terminals),
                        events: Arc::clone(&self.events),
                        signed_in: self.signed_in.clone(),
                        policy: self.policy,
                        reveal: adapter.definition().terminal_login_reveal,
                        exit_proves_login: adapter.definition().sign_in_proof
                            == SignInProof::Session,
                        agent_id: params.agent_id.clone(),
                        auth_session_id: auth_session_id.clone(),
                        session,
                        cancel,
                        finish,
                    },
                    exit_rx,
                ));
            }
            AgentAuthDelivery::AdapterBrowser => {
                self.register(
                    &params.agent_id,
                    &auth_session_id,
                    delivery,
                    None,
                    cancel.clone(),
                    finish,
                );
                tokio::spawn(supervise_adapter_login(AdapterLoginContext {
                    live: Arc::clone(&self.live),
                    events: Arc::clone(&self.events),
                    signed_in: self.signed_in.clone(),
                    policy: self.policy,
                    agent_id: params.agent_id.clone(),
                    auth_session_id: auth_session_id.clone(),
                    method_id: method.method_id.clone(),
                    adapter,
                    cancel,
                }));
            }
        }

        Ok(AgentSignInResult {
            agent_id: params.agent_id,
            auth_session_id,
            delivery,
            phase: AgentAuthPhase::Starting,
        })
    }

    fn start_lock(&self, agent_id: &AgentID) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .start_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            locks
                .entry(agent_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Fence uninstall against sign-in creation. An existing flow is refused
    /// rather than cancelled so the user does not lose an authentication
    /// interaction as a side effect of pressing Remove.
    pub(crate) async fn guard_removal(
        &self,
        agent_id: &AgentID,
    ) -> Result<AgentSignInRemovalGuard, HarnessError> {
        let guard = self.start_lock(agent_id).lock_owned().await;
        if self.existing_session(agent_id).is_some() {
            return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                format!(
                    "{}'s sign-in is in progress; cancel or finish it first",
                    agent_id.0
                ),
            ));
        }
        Ok(AgentSignInRemovalGuard { _guard: guard })
    }

    /// Where the login terminal runs. A named workspace wins; without one the
    /// agent's isolated home is the natural root, since that is where the login
    /// writes anyway. Registries with no home policy — the test ones — fall
    /// back to the app home, which is only ever a working directory.
    fn terminal_root(
        &self,
        agent_id: &super::types::AgentId,
        workspace_id: Option<WorkspaceID>,
    ) -> AuthTerminalRoot {
        match workspace_id {
            Some(workspace_id) => AuthTerminalRoot::Workspace(workspace_id),
            None => AuthTerminalRoot::AgentHome(
                self.registry
                    .agent_home(agent_id)
                    .or_else(|| crate::storage::ottam_home().ok())
                    .unwrap_or_else(std::env::temp_dir),
            ),
        }
    }

    /// Abandon a live sign-in. Unknown or already-finished sessions succeed —
    /// the client cancelling something that just completed is ordinary.
    pub(crate) fn cancel(&self, agent_id: &AgentID, auth_session_id: &AgentAuthSessionID) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let matches = live
            .get(agent_id)
            .is_some_and(|session| session.auth_session_id == *auth_session_id);
        if matches {
            if let Some(session) = live.remove(agent_id) {
                session.cancel.cancel();
            }
        }
    }

    /// The user says the login is complete. Fires the live session's finish
    /// token and leaves the entry in place — the supervisor closes the
    /// terminal, restarts the adapter, and retires the entry itself, exactly
    /// as it does when the login command exits. Unknown or stale sessions are
    /// a silent no-op, mirroring [`Self::cancel`].
    pub(crate) fn finish(&self, agent_id: &AgentID, auth_session_id: &AgentAuthSessionID) {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = live
            .get(agent_id)
            .filter(|session| session.auth_session_id == *auth_session_id)
        {
            session.finish.cancel();
        }
    }

    /// Whether a sign-in is running for this agent. A browser login runs
    /// inside the adapter, so nothing else may restart it meanwhile.
    pub(crate) fn is_signing_in(&self, agent_id: &AgentID) -> bool {
        self.existing_session(agent_id).is_some()
    }

    fn existing_session(&self, agent_id: &AgentID) -> Option<AgentSignInResult> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        live.get(agent_id).map(|session| AgentSignInResult {
            agent_id: agent_id.clone(),
            auth_session_id: session.auth_session_id.clone(),
            delivery: session.delivery,
            phase: AgentAuthPhase::AwaitingUser,
        })
    }

    /// A new caller may have missed the original reveal, or dismissed its
    /// sheet. Reattach it to the existing terminal instead of silently joining.
    fn resume_session(&self, agent_id: &AgentID) -> Option<AgentSignInResult> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = live.get(agent_id)?;
        let result = AgentSignInResult {
            agent_id: agent_id.clone(),
            auth_session_id: session.auth_session_id.clone(),
            delivery: session.delivery,
            phase: AgentAuthPhase::AwaitingUser,
        };
        let event = session
            .terminal
            .clone()
            .map(|terminal| AgentSignInEventParams {
                agent_id: agent_id.clone(),
                auth_session_id: session.auth_session_id.clone(),
                delivery: session.delivery,
                phase: AgentAuthPhase::AwaitingUser,
                reveal_terminal: true,
                terminal: Some(terminal),
                error: None,
            });
        drop(live);
        if let Some(event) = event {
            (self.events)(event);
        }
        Some(result)
    }

    fn register(
        &self,
        agent_id: &AgentID,
        auth_session_id: &AgentAuthSessionID,
        delivery: AgentAuthDelivery,
        terminal: Option<AgentAuthTerminal>,
        cancel: CancellationToken,
        finish: CancellationToken,
    ) {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                agent_id.clone(),
                LiveSignIn {
                    auth_session_id: auth_session_id.clone(),
                    delivery,
                    terminal,
                    cancel,
                    finish,
                },
            );
    }
}

/// The command a terminal login runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedAuthCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Choose the method to sign in with.
///
/// `requested` is honoured whenever the agent advertises it, including methods
/// core would not have picked itself. Without one, the ranking below applies.
pub(crate) fn select_auth_method(
    methods: &[HarnessAuthMethod],
    requested: Option<&str>,
    proof: SignInProof,
) -> Result<HarnessAuthMethod, HarnessError> {
    if let Some(requested) = requested {
        return methods
            .iter()
            .find(|method| method.method_id == requested)
            .cloned()
            .ok_or_else(|| {
                HarnessError::new(
                    HarnessErrorKind::InvalidAuthMethod,
                    format!("the agent does not offer a {requested:?} sign-in"),
                )
            });
    }

    methods
        .iter()
        .filter(|method| is_selectable(method))
        .min_by_key(|method| method_rank(methods, method, proof))
        .cloned()
        .ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::InvalidAuthMethod,
                "the agent offers no sign-in Ottam can run",
            )
        })
}

fn is_selectable(method: &HarnessAuthMethod) -> bool {
    !SECRET_ONLY_METHOD_IDS.contains(&method.method_id.as_str())
}

/// Lower sorts first: a known subscription login, then an adapter-run method
/// (fewer moving parts than a terminal we have to supervise), then whatever the
/// agent advertised first. An agent signed in by its models
/// ([`SignInProof::ModelSelector`]) takes the terminal first: oh-my-pi's
/// adapter-run method only accepts whatever credentials already exist, so it
/// can't add a provider.
fn method_rank(
    methods: &[HarnessAuthMethod],
    method: &HarnessAuthMethod,
    proof: SignInProof,
) -> (usize, usize, usize) {
    let preference = PREFERRED_METHOD_IDS
        .iter()
        .position(|id| *id == method.method_id)
        .unwrap_or(PREFERRED_METHOD_IDS.len());
    let structural = match (method.kind, proof) {
        (HarnessAuthMethodKind::Agent, SignInProof::Session)
        | (HarnessAuthMethodKind::Terminal, SignInProof::ModelSelector) => 0,
        _ => 1,
    };
    let advertised = methods
        .iter()
        .position(|candidate| candidate.method_id == method.method_id)
        .unwrap_or(usize::MAX);
    (preference, structural, advertised)
}

fn delivery_for(method: &HarnessAuthMethod) -> AgentAuthDelivery {
    match method.kind {
        HarnessAuthMethodKind::Terminal => AgentAuthDelivery::ClientTerminal,
        HarnessAuthMethodKind::Agent => AgentAuthDelivery::AdapterBrowser,
    }
}

/// Build the command line for a terminal method.
///
/// The adapter may name its own interpreter and script absolutely, in which case
/// that is used verbatim — it is the adapter's own resolved path, and re-running
/// the launch command would resolve through `npx` a second time. Otherwise the
/// method's arguments are appended to the agent's launch command, which is what
/// "run me again, like this" means.
pub(crate) fn terminal_command(
    definition: &AgentDefinition,
    method: &HarnessAuthMethod,
) -> Result<ResolvedAuthCommand, HarnessError> {
    let command = method.terminal_command.as_ref().ok_or_else(|| {
        HarnessError::new(
            HarnessErrorKind::InvalidAuthMethod,
            format!(
                "the agent's {:?} sign-in advertised no command to run",
                method.method_id
            ),
        )
    })?;

    let (program, args) = match (definition.terminal_login_args, command.program.as_ref()) {
        // The definition knows a better login than the one advertised; it
        // runs the same resolved executable.
        (Some(login), _) => (
            definition.launch.command.clone(),
            login.iter().map(|arg| arg.to_string()).collect(),
        ),
        // A method naming the adapter's own binary (a login that runs the
        // adapter again) must go through the launch spec instead: that
        // binary exists only via npx or Ottam's install, never on PATH.
        // A binary's launch arguments select its ACP mode (`opencode acp`);
        // the method's arguments name a different subcommand of the same
        // executable, so only the resolved path carries over.
        (None, Some(program))
            if adapter_names_itself(definition, program) && definition.runs_binary() =>
        {
            (definition.launch.command.clone(), command.args.clone())
        }
        (None, Some(program)) if adapter_names_itself(definition, program) => {
            let mut args = definition.launch.arguments.clone();
            args.extend(command.args.iter().cloned());
            (definition.launch.command.clone(), args)
        }
        (None, Some(program)) => (program.clone(), command.args.clone()),
        (None, None) => {
            let mut args = definition.launch.arguments.clone();
            args.extend(command.args.iter().cloned());
            (definition.launch.command.clone(), args)
        }
    };

    // The launch environment first, so a method that names the same variable
    // deliberately overrides it.
    let mut env = definition.launch.environment.clone();
    env.extend(
        command
            .env
            .iter()
            .filter(|(name, _)| {
                name != "HOME"
                    && !definition
                        .home_variables
                        .iter()
                        .any(|variable| variable.name == name)
            })
            .cloned(),
    );
    env.retain(|(name, _)| name != "HOME");

    Ok(ResolvedAuthCommand { program, args, env })
}

/// Whether a method's program is the adapter's own binary: the npm package's
/// last name segment (`@agentclientprotocol/claude-agent-acp` →
/// `claude-agent-acp`) or a binary adapter's command (`opencode`).
fn adapter_names_itself(definition: &AgentDefinition, program: &str) -> bool {
    definition
        .adapter
        .as_ref()
        .is_some_and(|package| package.program() == program)
}

struct TerminalLoginContext {
    live: LiveSignIns,
    registry: Arc<AgentRegistry>,
    terminals: Arc<dyn AuthTerminalHost>,
    events: AuthEventSink,
    signed_in: Option<SignedInSink>,
    policy: AuthTimingPolicy,
    reveal: TerminalLoginReveal,
    /// Whether a clean exit completes the login. Under
    /// [`SignInProof::ModelSelector`] it does not: `omp login` exits 0 having
    /// stored nothing, so the terminal stays up with its answer until Done.
    exit_proves_login: bool,
    agent_id: AgentID,
    auth_session_id: AgentAuthSessionID,
    session: AuthTerminalSession,
    cancel: CancellationToken,
    finish: CancellationToken,
}

struct AdapterLoginContext {
    live: LiveSignIns,
    events: AuthEventSink,
    signed_in: Option<SignedInSink>,
    policy: AuthTimingPolicy,
    agent_id: AgentID,
    auth_session_id: AgentAuthSessionID,
    method_id: String,
    adapter: Arc<dyn HarnessAdapter>,
    cancel: CancellationToken,
}

/// Emit one `agent.signInEvent`.
struct EventEmitter {
    events: AuthEventSink,
    agent_id: AgentID,
    auth_session_id: AgentAuthSessionID,
    delivery: AgentAuthDelivery,
    terminal: Option<AgentAuthTerminal>,
    /// Latched: once core has asked for the terminal, every later event asks
    /// again, so a client that missed one still ends up showing it.
    revealed: bool,
}

impl EventEmitter {
    fn emit(&mut self, phase: AgentAuthPhase, reveal: bool, error: Option<String>) {
        self.revealed = self.revealed || reveal;
        (self.events)(AgentSignInEventParams {
            agent_id: self.agent_id.clone(),
            auth_session_id: self.auth_session_id.clone(),
            delivery: self.delivery,
            phase,
            reveal_terminal: self.revealed,
            terminal: self.terminal.clone(),
            error,
        });
    }
}

async fn supervise_terminal_login(
    context: TerminalLoginContext,
    mut exit_rx: oneshot::Receiver<Option<i32>>,
) {
    let TerminalLoginContext {
        live,
        registry,
        terminals,
        events,
        signed_in,
        policy,
        reveal,
        exit_proves_login,
        agent_id,
        auth_session_id,
        session,
        cancel,
        finish,
    } = context;
    let mut emitter = EventEmitter {
        events,
        agent_id: agent_id.clone(),
        auth_session_id: auth_session_id.clone(),
        delivery: AgentAuthDelivery::ClientTerminal,
        terminal: Some(AgentAuthTerminal::from(&session)),
        revealed: false,
    };
    // An Immediate login IS the interaction, so the terminal shows from the
    // first frame — and the latched `revealed` flag then keeps the stuck
    // detectors below from ever firing a second ask.
    emitter.emit(
        AgentAuthPhase::AwaitingUser,
        reveal == TerminalLoginReveal::Immediate,
        None,
    );

    let started = Instant::now();
    let mut last_output_at = Instant::now();
    let mut last_bytes = terminals.output_bytes(&session.session_id).unwrap_or(0);
    // Set once a clean exit has to wait for Done; the receiver is spent then.
    let mut exited_cleanly = false;

    let outcome = loop {
        tokio::select! {
            exit = &mut exit_rx, if !exited_cleanly => {
                match exit {
                    // The user reads what the login printed, then says.
                    Ok(Some(0)) if !exit_proves_login => {
                        exited_cleanly = true;
                        emitter.emit(AgentAuthPhase::NeedsInput, true, None);
                    }
                    Ok(status) => break TerminalOutcome::Exited(status),
                    // The terminal layer dropped the observer without reporting,
                    // which only happens if the session was torn down under us.
                    Err(_) => break TerminalOutcome::Exited(None),
                }
            }
            () = cancel.cancelled() => break TerminalOutcome::Cancelled,
            () = finish.cancelled() => break TerminalOutcome::Finished,
            () = tokio::time::sleep(policy.poll_interval) => {
                let bytes = terminals.output_bytes(&session.session_id).unwrap_or(last_bytes);
                if bytes != last_bytes {
                    last_bytes = bytes;
                    last_output_at = Instant::now();
                }
                // A hidden login (claude's browser bounce) is revealed once it
                // has been silent long enough to look stuck — the fallback for
                // the WhenStuck policy; Immediate logins were revealed above.
                if !emitter.revealed
                    && last_output_at.elapsed() >= policy.reveal_after_silence
                {
                    emitter.emit(AgentAuthPhase::NeedsInput, true, None);
                }
                if started.elapsed() >= policy.lifetime_cap {
                    break TerminalOutcome::TimedOut;
                }
            }
        }
    };

    retire(&live, &agent_id, &auth_session_id);
    match outcome {
        // A clean exit and the user pressing Done both mean "the login is
        // complete": the credentials landed in the CLI's own config while this
        // adapter process and its SDK subprocess kept holding the signed-out
        // state. Without the restart the sign-in reports success and the very
        // next prompt fails exactly as before.
        TerminalOutcome::Exited(Some(0)) | TerminalOutcome::Finished => {
            terminals.close(&session.session_id);
            let agent = super::types::AgentId(agent_id.0.clone());
            match registry.restart(&agent).await {
                Ok(_) => {
                    if let Some(signed_in) = &signed_in {
                        signed_in(&agent_id);
                    }
                    emitter.emit(AgentAuthPhase::Succeeded, false, None);
                }
                Err(error) => emitter.emit(
                    AgentAuthPhase::Failed,
                    false,
                    Some(format!("signed in, but the agent did not restart: {error}")),
                ),
            }
        }
        TerminalOutcome::Exited(status) => {
            // Keep the terminal registered: it holds the only account of what
            // went wrong, and the client is about to show it.
            let detail = match status {
                Some(code) => format!("the sign-in command exited with status {code}"),
                None => "the sign-in command exited without a status".to_string(),
            };
            emitter.emit(AgentAuthPhase::Failed, true, Some(detail));
        }
        TerminalOutcome::TimedOut => {
            terminals.close(&session.session_id);
            emitter.emit(
                AgentAuthPhase::Failed,
                true,
                Some("the sign-in did not finish in time".to_string()),
            );
        }
        TerminalOutcome::Cancelled => {
            terminals.close(&session.session_id);
            emitter.emit(AgentAuthPhase::Cancelled, false, None);
        }
    }
}

enum TerminalOutcome {
    Exited(Option<i32>),
    /// The user pressed Done: complete the login as if the command exited 0.
    Finished,
    TimedOut,
    Cancelled,
}

async fn supervise_adapter_login(context: AdapterLoginContext) {
    let AdapterLoginContext {
        live,
        events,
        signed_in,
        policy,
        agent_id,
        auth_session_id,
        method_id,
        adapter,
        cancel,
    } = context;
    let mut emitter = EventEmitter {
        events,
        agent_id: agent_id.clone(),
        auth_session_id: auth_session_id.clone(),
        delivery: AgentAuthDelivery::AdapterBrowser,
        terminal: None,
        revealed: false,
    };
    emitter.emit(AgentAuthPhase::AwaitingUser, false, None);

    let outcome = {
        let authenticate = adapter.authenticate(method_id.clone());
        tokio::select! {
            result = authenticate => match result {
                Ok(()) => AdapterOutcome::Succeeded,
                Err(error) => AdapterOutcome::Failed(error.message),
            },
            () = cancel.cancelled() => AdapterOutcome::Cancelled,
            () = tokio::time::sleep(policy.lifetime_cap) => AdapterOutcome::TimedOut,
        }
    };

    // ACP has no way to withdraw an `authenticate` that is already outstanding,
    // so this abandons our side of it. That is only safe because the adapter's
    // command loop no longer blocks on the request: the abandoned login stops
    // holding up everything else the adapter has to do.
    if matches!(
        outcome,
        AdapterOutcome::Cancelled | AdapterOutcome::TimedOut
    ) {
        let _ = adapter.cancel_authenticate(method_id).await;
    }

    retire(&live, &agent_id, &auth_session_id);
    match outcome {
        AdapterOutcome::Succeeded => {
            if let Some(signed_in) = &signed_in {
                signed_in(&agent_id);
            }
            emitter.emit(AgentAuthPhase::Succeeded, false, None);
        }
        AdapterOutcome::Failed(message) => {
            emitter.emit(AgentAuthPhase::Failed, false, Some(message))
        }
        AdapterOutcome::TimedOut => emitter.emit(
            AgentAuthPhase::Failed,
            false,
            Some("the sign-in did not finish in time".to_string()),
        ),
        AdapterOutcome::Cancelled => emitter.emit(AgentAuthPhase::Cancelled, false, None),
    }
}

enum AdapterOutcome {
    Succeeded,
    Failed(String),
    TimedOut,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentLaunchSpec, HarnessAuthTerminalCommand};

    fn method(id: &str, kind: HarnessAuthMethodKind) -> HarnessAuthMethod {
        HarnessAuthMethod {
            method_id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind,
            terminal_command: match kind {
                HarnessAuthMethodKind::Terminal => Some(HarnessAuthTerminalCommand {
                    program: None,
                    args: vec!["--cli".to_string(), "auth".to_string()],
                    env: Vec::new(),
                    label: None,
                }),
                _ => None,
            },
        }
    }

    // The two shapes actually observed on the wire, so the ranking is exercised
    // against reality rather than against invented ids.
    fn claude_methods() -> Vec<HarnessAuthMethod> {
        vec![
            method("console-login", HarnessAuthMethodKind::Terminal),
            method("claude-ai-login", HarnessAuthMethodKind::Terminal),
        ]
    }

    fn codex_methods() -> Vec<HarnessAuthMethod> {
        vec![
            method("api-key", HarnessAuthMethodKind::Agent),
            method("chat-gpt", HarnessAuthMethodKind::Agent),
        ]
    }

    #[test]
    fn the_subscription_login_wins_over_the_console_login() {
        let chosen =
            select_auth_method(&claude_methods(), None, SignInProof::Session).expect("a method");
        assert_eq!(chosen.method_id, "claude-ai-login");
        assert_eq!(delivery_for(&chosen), AgentAuthDelivery::ClientTerminal);
    }

    #[test]
    fn a_method_needing_a_secret_is_never_chosen_for_the_user() {
        let chosen =
            select_auth_method(&codex_methods(), None, SignInProof::Session).expect("a method");
        assert_eq!(chosen.method_id, "chat-gpt");
        assert_eq!(delivery_for(&chosen), AgentAuthDelivery::AdapterBrowser);

        let only_secrets = vec![
            method("api-key", HarnessAuthMethodKind::Agent),
            method("apikey", HarnessAuthMethodKind::Agent),
        ];
        let error = select_auth_method(&only_secrets, None, SignInProof::Session)
            .expect_err("nothing runnable");
        assert_eq!(error.kind, HarnessErrorKind::InvalidAuthMethod);
    }

    #[test]
    fn an_unranked_agent_still_gets_a_method() {
        let unknown = vec![
            method("mystery-terminal", HarnessAuthMethodKind::Terminal),
            method("mystery-browser", HarnessAuthMethodKind::Agent),
        ];
        // Adapter-run is fewer moving parts than a terminal we have to supervise.
        let chosen = select_auth_method(&unknown, None, SignInProof::Session).expect("a method");
        assert_eq!(chosen.method_id, "mystery-browser");
    }

    /// oh-my-pi's adapter-run method accepts whatever credentials exist, so
    /// an agent signed in by its models opens the terminal instead.
    #[test]
    fn model_selector_agents_sign_in_through_the_terminal() {
        let methods = vec![
            HarnessAuthMethod {
                method_id: "agent".into(),
                name: "Use existing local credentials".into(),
                description: None,
                kind: HarnessAuthMethodKind::Agent,
                terminal_command: None,
            },
            HarnessAuthMethod {
                method_id: "terminal".into(),
                name: "Set up omp in terminal".into(),
                description: None,
                kind: HarnessAuthMethodKind::Terminal,
                terminal_command: None,
            },
        ];
        let chosen =
            select_auth_method(&methods, None, SignInProof::ModelSelector).expect("a method");
        assert_eq!(chosen.method_id, "terminal");
        let chosen = select_auth_method(&methods, None, SignInProof::Session).expect("a method");
        assert_eq!(chosen.method_id, "agent");
    }

    #[test]
    fn an_explicit_method_is_honoured_even_when_core_would_not_pick_it() {
        // The client asked for it by name; the adapter's own refusal is a better
        // message than one invented here.
        let chosen = select_auth_method(&codex_methods(), Some("api-key"), SignInProof::Session)
            .expect("a method");
        assert_eq!(chosen.method_id, "api-key");

        let error = select_auth_method(&codex_methods(), Some("not-offered"), SignInProof::Session)
            .expect_err("no such method");
        assert_eq!(error.kind, HarnessErrorKind::InvalidAuthMethod);
    }

    fn definition() -> AgentDefinition {
        AgentDefinition {
            terminal_login_reveal: TerminalLoginReveal::WhenStuck,
            launch: AgentLaunchSpec {
                command: "npx".to_string(),
                arguments: vec!["-y".to_string(), "claude-agent-acp".to_string()],
                environment: vec![("SHARED".to_string(), "launch".to_string())],
                minimum_node_major: 20,
            },
            adapter: Some(crate::agent::AdapterPackage {
                name: "claude-agent-acp".to_string(),
                version: "0.81.0".to_string(),
                source: crate::agent::AdapterSource::Npm,
            }),
            ..AgentDefinition::for_test("claude")
        }
    }

    #[test]
    fn an_adapter_that_named_its_own_interpreter_is_run_verbatim() {
        // Re-running the launch command would resolve through `npx` a second
        // time; the adapter already did that work and told us the answer.
        let mut chosen = method("claude-ai-login", HarnessAuthMethodKind::Terminal);
        chosen.terminal_command = Some(HarnessAuthTerminalCommand {
            program: Some("/opt/node".to_string()),
            args: vec!["/pkg/acp.js".to_string(), "--cli".to_string()],
            env: vec![("SHARED".to_string(), "method".to_string())],
            label: Some("Log in".to_string()),
        });
        let command = terminal_command(&definition(), &chosen).expect("a command");
        assert_eq!(command.program, "/opt/node");
        assert_eq!(command.args, vec!["/pkg/acp.js", "--cli"]);
        // The launch environment first, so a method naming the same variable
        // deliberately overrides it.
        assert_eq!(
            command.env,
            vec![
                ("SHARED".to_string(), "launch".to_string()),
                ("SHARED".to_string(), "method".to_string()),
            ]
        );
    }

    #[test]
    fn without_an_explicit_program_the_launch_command_is_re_run_with_the_extra_arguments() {
        let chosen = method("claude-ai-login", HarnessAuthMethodKind::Terminal);
        let command = terminal_command(&definition(), &chosen).expect("a command");
        assert_eq!(command.program, "npx");
        assert_eq!(
            command.args,
            vec!["-y", "claude-agent-acp", "--cli", "auth"]
        );
    }

    /// An adapter whose login says "run `example-acp`" names its own binary,
    /// which exists only through npx and is never on PATH. That
    /// self-reference must be routed through the launch spec (pinned version,
    /// isolated-home env included) instead of being spawned verbatim and dying
    /// on PATH lookup.
    #[test]
    fn a_method_naming_the_adapters_own_binary_reuses_the_launch_spec() {
        let mut definition = definition();
        definition.launch.arguments = vec!["-y".to_string(), "example-acp@1.0.0".to_string()];
        definition.adapter = Some(crate::agent::AdapterPackage {
            name: "example-acp".to_string(),
            version: "1.0.0".to_string(),
            source: crate::agent::AdapterSource::Npm,
        });
        let mut chosen = method("example_terminal_login", HarnessAuthMethodKind::Terminal);
        chosen.terminal_command = Some(HarnessAuthTerminalCommand {
            program: Some("example-acp".to_string()),
            args: vec!["login".to_string()],
            env: Vec::new(),
            label: Some("Log in".to_string()),
        });
        let command = terminal_command(&definition, &chosen).expect("a command");
        assert_eq!(command.program, "npx");
        assert_eq!(command.args, vec!["-y", "example-acp@1.0.0", "login"]);

        // A scoped package's binary is its last path segment.
        definition.launch.arguments = vec![
            "-y".to_string(),
            "@agentclientprotocol/claude-agent-acp@0.81.0".to_string(),
        ];
        definition.adapter = Some(crate::agent::AdapterPackage {
            name: "@agentclientprotocol/claude-agent-acp".to_string(),
            version: "0.81.0".to_string(),
            source: crate::agent::AdapterSource::Npm,
        });
        chosen.terminal_command.as_mut().expect("command").program =
            Some("claude-agent-acp".to_string());
        let command = terminal_command(&definition, &chosen).expect("a command");
        assert_eq!(command.program, "npx");
        assert_eq!(
            command.args,
            vec![
                "-y",
                "@agentclientprotocol/claude-agent-acp@0.81.0",
                "login"
            ]
        );
    }

    /// A binary adapter's launch arguments select its ACP mode, so a login
    /// that names the binary runs the installed executable with only the
    /// method's arguments, while a bare terminal method re-runs the launch.
    #[test]
    fn a_binary_adapter_login_runs_the_installed_executable() {
        let mut definition = definition();
        definition.launch.command = "/agents/opencode/adapter/2.2.5/bin/opencode".to_string();
        definition.launch.arguments = vec!["acp".to_string()];
        definition.adapter = Some(crate::agent::AdapterPackage {
            name: "@opencode/cli".to_string(),
            version: "2.2.5".to_string(),
            source: crate::agent::AdapterSource::Binary(crate::agent::BinaryAdapter {
                command: "opencode".to_string(),
                artifacts: Vec::new(),
            }),
        });
        let mut chosen = method("opencode-login", HarnessAuthMethodKind::Terminal);
        chosen.terminal_command = Some(HarnessAuthTerminalCommand {
            program: Some("opencode".to_string()),
            args: vec!["auth".to_string(), "login".to_string()],
            env: Vec::new(),
            label: Some("OpenCode Login".to_string()),
        });
        let command = terminal_command(&definition, &chosen).expect("a command");
        assert_eq!(command.program, definition.launch.command);
        assert_eq!(command.args, vec!["auth", "login"]);

        chosen.terminal_command = Some(HarnessAuthTerminalCommand {
            program: None,
            args: vec!["--login".to_string()],
            env: Vec::new(),
            label: None,
        });
        let command = terminal_command(&definition, &chosen).expect("a command");
        assert_eq!(command.program, definition.launch.command);
        assert_eq!(command.args, vec!["acp", "--login"]);
    }

    /// omp advertises its plain TUI; the definition's `omp login` replaces it
    /// and keeps the launch environment.
    #[test]
    fn a_definition_login_replaces_the_advertised_one() {
        let mut definition = definition();
        definition.launch.command = "/agents/omp/adapter/18.16.0/bin/omp".to_string();
        definition.launch.arguments = vec!["acp".to_string()];
        definition.terminal_login_args = Some(&["login"]);
        let mut chosen = method("terminal", HarnessAuthMethodKind::Terminal);
        chosen.terminal_command = Some(HarnessAuthTerminalCommand {
            program: None,
            args: vec!["--acp-terminal-auth".to_string()],
            env: Vec::new(),
            label: None,
        });
        let command = terminal_command(&definition, &chosen).expect("a command");
        assert_eq!(command.program, definition.launch.command);
        assert_eq!(command.args, vec!["login"]);
        assert!(command
            .env
            .contains(&("SHARED".to_string(), "launch".to_string())));
    }

    #[test]
    fn a_terminal_method_with_nothing_to_run_is_rejected() {
        let mut chosen = method("claude-ai-login", HarnessAuthMethodKind::Terminal);
        chosen.terminal_command = None;
        let error = terminal_command(&definition(), &chosen).expect_err("nothing to run");
        assert_eq!(error.kind, HarnessErrorKind::InvalidAuthMethod);
    }

    // ---- Flow scaffolding -------------------------------------------------
    //
    // A fake adapter and a fake terminal, so the parts worth testing — when the
    // terminal is revealed, whether the adapter is restarted, what a cancel
    // leaves behind — are exercised without a PTY or a real agent process.

    use crate::agent::{
        AdapterFactory, AdapterStatus, HarnessFuture, HarnessPrompt, HarnessSessionId,
        HarnessSessionSummary, HarnessStopReason, HarnessUpdateReceiver, HarnessUpdateSender,
        RuntimeProbe,
    };
    use crate::agent::{AgentRuntimeStatus, HarnessAuthState};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{mpsc, watch};

    struct AlwaysReadyProbe;

    impl RuntimeProbe for AlwaysReadyProbe {
        fn probe(&self, _definition: &AgentDefinition) -> AgentRuntimeStatus {
            AgentRuntimeStatus::ready("v22.0.0", "10.0.0")
        }
    }

    struct FakeAuthAdapter {
        definition: AgentDefinition,
        status_tx: watch::Sender<AdapterStatus>,
        updates_tx: HarnessUpdateSender,
        authenticated: StdMutex<Vec<String>>,
        cancelled_authentications: StdMutex<Vec<String>>,
        /// Set for the adapter-run flow: `authenticate` parks until it is
        /// released, the way a real browser login does.
        block_authenticate: AtomicBool,
        released: CancellationToken,
        fail_authenticate: AtomicBool,
    }

    impl FakeAuthAdapter {
        fn new(
            definition: AgentDefinition,
            generation: u64,
            methods: Vec<HarnessAuthMethod>,
        ) -> Self {
            let mut status = AdapterStatus::stopped();
            status.generation = generation;
            status.auth_methods = methods;
            status.auth_state = HarnessAuthState::Missing;
            let (status_tx, _) = watch::channel(status);
            let updates_tx = HarnessUpdateSender::default();
            Self {
                definition,
                status_tx,
                updates_tx,
                authenticated: StdMutex::new(Vec::new()),
                cancelled_authentications: StdMutex::new(Vec::new()),
                block_authenticate: AtomicBool::new(false),
                released: CancellationToken::new(),
                fail_authenticate: AtomicBool::new(false),
            }
        }
    }

    impl HarnessAdapter for FakeAuthAdapter {
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
            Vec::new()
        }

        fn start(&self) -> HarnessFuture<'_, AdapterStatus> {
            Box::pin(async move { Ok(self.status_tx.borrow().clone()) })
        }

        fn shutdown(&self) -> HarnessFuture<'_, ()> {
            Box::pin(std::future::ready(Ok(())))
        }

        fn authenticate(&self, method_id: String) -> HarnessFuture<'_, ()> {
            self.authenticated
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(method_id);
            Box::pin(async move {
                if self.block_authenticate.load(Ordering::SeqCst) {
                    self.released.cancelled().await;
                }
                if self.fail_authenticate.load(Ordering::SeqCst) {
                    return Err(HarnessError::new(
                        HarnessErrorKind::AuthRequired,
                        "the browser login was declined",
                    ));
                }
                Ok(())
            })
        }

        fn cancel_authenticate(&self, method_id: String) -> HarnessFuture<'_, ()> {
            self.cancelled_authentications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(method_id);
            Box::pin(std::future::ready(Ok(())))
        }

        fn new_session(
            &self,
            _cwd: PathBuf,
            _config: crate::agent::SessionConfigRequest,
        ) -> HarnessFuture<'_, HarnessSessionId> {
            unimplemented!("sign-in never opens a session")
        }

        fn load_session(
            &self,
            _session_id: HarnessSessionId,
            _cwd: PathBuf,
        ) -> HarnessFuture<'_, ()> {
            unimplemented!("sign-in never loads a session")
        }

        fn delete_session(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
            unimplemented!("sign-in never deletes a session")
        }

        fn prompt(
            &self,
            _session_id: HarnessSessionId,
            _prompt: HarnessPrompt,
        ) -> HarnessFuture<'_, HarnessStopReason> {
            unimplemented!("sign-in never prompts")
        }

        fn steer(
            &self,
            _session_id: HarnessSessionId,
            _prompt: HarnessPrompt,
        ) -> HarnessFuture<'_, ()> {
            unimplemented!("sign-in never steers")
        }

        fn cancel(&self, _session_id: HarnessSessionId) -> HarnessFuture<'_, ()> {
            unimplemented!("sign-in never cancels a turn")
        }
    }

    struct FakeAuthFactory {
        methods: Vec<HarnessAuthMethod>,
        created: StdMutex<Vec<Arc<FakeAuthAdapter>>>,
    }

    impl FakeAuthFactory {
        fn new(methods: Vec<HarnessAuthMethod>) -> Arc<Self> {
            Arc::new(Self {
                methods,
                created: StdMutex::new(Vec::new()),
            })
        }

        fn adapters(&self) -> Vec<Arc<FakeAuthAdapter>> {
            self.created
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl AdapterFactory for FakeAuthFactory {
        fn create(&self, definition: AgentDefinition, generation: u64) -> Arc<dyn HarnessAdapter> {
            let adapter = Arc::new(FakeAuthAdapter::new(
                definition,
                generation,
                self.methods.clone(),
            ));
            self.created
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Arc::clone(&adapter));
            adapter
        }
    }

    #[derive(Default)]
    struct FakeTerminalState {
        spawned: Vec<(String, Vec<String>)>,
        roots: Vec<AuthTerminalRoot>,
        exit: Option<oneshot::Sender<Option<i32>>>,
        bytes: u64,
        closed: Vec<TerminalSessionID>,
        registered: bool,
    }

    #[derive(Default)]
    struct FakeTerminals {
        state: StdMutex<FakeTerminalState>,
    }

    impl FakeTerminals {
        fn session_id() -> TerminalSessionID {
            TerminalSessionID("terminal_local_auth".to_string())
        }

        fn with<T>(&self, edit: impl FnOnce(&mut FakeTerminalState) -> T) -> T {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            edit(&mut state)
        }

        /// Report the login command's exit, the way the terminal layer does.
        fn exit(&self, status: Option<i32>) {
            let sender = self.with(|state| state.exit.take());
            let _ = sender.expect("a terminal was spawned").send(status);
        }

        fn write_output(&self, bytes: u64) {
            self.with(|state| state.bytes += bytes);
        }

        fn closed(&self) -> Vec<TerminalSessionID> {
            self.with(|state| state.closed.clone())
        }

        fn roots(&self) -> Vec<AuthTerminalRoot> {
            self.with(|state| state.roots.clone())
        }
    }

    impl AuthTerminalHost for FakeTerminals {
        fn spawn(&self, request: AuthTerminalRequest) -> AuthTerminalFuture<'_> {
            Box::pin(async move {
                // Keep a scheduling point in the fake so concurrent start
                // tests exercise the service's creation boundary.
                tokio::task::yield_now().await;
                self.with(|state| {
                    state.spawned.push((request.program, request.args));
                    state.roots.push(request.root);
                    state.exit = Some(request.exit_observer);
                    state.registered = true;
                });
                Ok(AuthTerminalSession {
                    session_id: Self::session_id(),
                    terminal_id: TerminalID("terminal-auth".to_string()),
                    rows: 24,
                    cols: 80,
                })
            })
        }

        fn output_bytes(&self, _session_id: &TerminalSessionID) -> Option<u64> {
            self.with(|state| state.registered.then_some(state.bytes))
        }

        fn close(&self, session_id: &TerminalSessionID) {
            self.with(|state| {
                state.closed.push(session_id.clone());
                state.registered = false;
            });
        }
    }

    struct Harness {
        service: Arc<AgentSignInService>,
        factory: Arc<FakeAuthFactory>,
        terminals: Arc<FakeTerminals>,
        events: mpsc::UnboundedReceiver<AgentSignInEventParams>,
        /// Agents the signed-in sink was called for, in order.
        signed_in: Arc<StdMutex<Vec<AgentID>>>,
    }

    impl Harness {
        fn new(methods: Vec<HarnessAuthMethod>, policy: AuthTimingPolicy) -> Self {
            Self::with_reveal(methods, policy, TerminalLoginReveal::WhenStuck)
        }

        fn with_reveal(
            methods: Vec<HarnessAuthMethod>,
            policy: AuthTimingPolicy,
            reveal: TerminalLoginReveal,
        ) -> Self {
            Self::with_provider(methods, policy, reveal, "claude")
        }

        fn with_provider(
            methods: Vec<HarnessAuthMethod>,
            policy: AuthTimingPolicy,
            reveal: TerminalLoginReveal,
            provider: &str,
        ) -> Self {
            let mut definition = definition();
            definition.agent_id = super::super::AgentId::new(provider);
            definition.terminal_login_reveal = reveal;
            Self::with_definition(methods, policy, definition)
        }

        fn with_definition(
            methods: Vec<HarnessAuthMethod>,
            policy: AuthTimingPolicy,
            definition: AgentDefinition,
        ) -> Self {
            let factory = FakeAuthFactory::new(methods);
            let registry = AgentRegistry::with_components(
                vec![definition],
                Arc::new(AlwaysReadyProbe),
                Arc::clone(&factory) as Arc<dyn AdapterFactory>,
            );
            let terminals = Arc::new(FakeTerminals::default());
            let (events_tx, events) = mpsc::unbounded_channel();
            let sink: AuthEventSink = Arc::new(move |params| {
                let _ = events_tx.send(params);
            });
            let signed_in = Arc::new(StdMutex::new(Vec::new()));
            let recorder = Arc::clone(&signed_in);
            let signed_in_sink: SignedInSink = Arc::new(move |agent_id: &AgentID| {
                recorder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(agent_id.clone());
            });
            let service = Arc::new(AgentSignInService::with_policy(
                Arc::new(registry),
                Arc::clone(&terminals) as Arc<dyn AuthTerminalHost>,
                sink,
                Some(signed_in_sink),
                policy,
            ));
            service.registry.set_replay_sign_in_service(&service);
            Self {
                service,
                factory,
                terminals,
                events,
                signed_in,
            }
        }

        fn signed_in_agents(&self) -> Vec<AgentID> {
            self.signed_in
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        async fn sign_in(&self) -> Result<AgentSignInResult, HarnessError> {
            self.sign_in_with(Some(WorkspaceID("workspace-1".to_string())))
                .await
        }

        async fn sign_in_with(
            &self,
            workspace_id: Option<WorkspaceID>,
        ) -> Result<AgentSignInResult, HarnessError> {
            self.service
                .sign_in(AgentSignInParams {
                    agent_id: AgentID(self.service.registry.definitions()[0].agent_id.0.clone()),
                    workspace_id,
                    method_id: None,
                })
                .await
        }

        /// Wait for the next event. The supervisors run as tasks, so a test that
        /// polled without yielding would race them.
        ///
        /// The bound is generous because it is spent on Tokio's clock, which
        /// auto-advances: a test with nothing pending reaches it instantly, and
        /// one waiting on the lifetime cap is not cut off early.
        async fn next_event(&mut self) -> AgentSignInEventParams {
            tokio::time::timeout(Duration::from_secs(600), self.events.recv())
                .await
                .expect("an event within the test's virtual clock")
                .expect("the sink is still open")
        }
    }

    fn brisk_policy() -> AuthTimingPolicy {
        AuthTimingPolicy {
            reveal_after_silence: Duration::from_secs(20),
            poll_interval: Duration::from_millis(50),
            lifetime_cap: Duration::from_secs(120),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_login_without_a_workspace_runs_in_the_agents_own_home() {
        // Settings has no workspace, and a fresh install may have none at all.
        // The login writes to the CLI's config directory either way, so the
        // agent's own home is a working directory that always exists.
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in_with(None).await.expect("a sign-in");
        assert_eq!(started.delivery, AgentAuthDelivery::ClientTerminal);

        let roots = harness.terminals.roots();
        assert!(
            matches!(roots.as_slice(), [AuthTerminalRoot::AgentHome(_)]),
            "expected an agent-home root, got {roots:?}"
        );

        let awaiting = harness.next_event().await;
        assert_eq!(awaiting.phase, AgentAuthPhase::AwaitingUser);
        harness.terminals.exit(Some(0));
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Succeeded);
    }

    #[tokio::test]
    async fn a_login_with_a_workspace_still_runs_in_it() {
        let harness = Harness::new(claude_methods(), brisk_policy());
        harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.terminals.roots(),
            vec![AuthTerminalRoot::Workspace(WorkspaceID(
                "workspace-1".to_string()
            ))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_existing_cli_home_signs_in_through_its_own_login() {
        // The CLI's own login, run in the CLI's home, writes what a terminal
        // login would — so the user's setup signs in like any other.
        let home = tempfile::tempdir().unwrap();
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let agent_id = harness.service.registry.definitions()[0].agent_id.clone();
        harness
            .service
            .registry
            .publish_instance(crate::agent::AgentInstance {
                id: agent_id.clone(),
                harness_id: crate::agent::HarnessId(agent_id.0.clone()),
                label: "Your setup".into(),
                home: crate::agent::HomeLayout::Existing {
                    path: home.path().into(),
                    environment: std::collections::BTreeMap::from([(
                        "CLAUDE_CONFIG_DIR".into(),
                        None,
                    )]),
                },
                claimed: false,
            });
        assert!(harness
            .service
            .registry
            .definition(&agent_id)
            .unwrap()
            .existing_home()
            .is_some());
        let started = harness.sign_in_with(None).await.expect("a sign-in");
        assert_eq!(started.delivery, AgentAuthDelivery::ClientTerminal);
        assert_eq!(
            harness.terminals.roots(),
            vec![AuthTerminalRoot::AgentHome(home.path().into())]
        );

        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );
        harness.terminals.exit(Some(0));
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Succeeded);
    }

    #[tokio::test(start_paused = true)]
    async fn an_auth_recheck_spares_a_browser_sign_in_running_in_the_adapter() {
        // Returning to the app rechecks an existing home's auth. Restarting
        // the adapter then would kill the login's callback server.
        let home = tempfile::tempdir().unwrap();
        let mut harness = Harness::with_provider(
            codex_methods(),
            brisk_policy(),
            TerminalLoginReveal::WhenStuck,
            "codex",
        );
        let registry = Arc::clone(&harness.service.registry);
        let agent_id = registry.definitions()[0].agent_id.clone();
        registry.publish_instance(crate::agent::AgentInstance {
            id: agent_id.clone(),
            harness_id: crate::agent::HarnessId(agent_id.0.clone()),
            label: "Your setup".into(),
            home: crate::agent::HomeLayout::Existing {
                path: home.path().into(),
                environment: std::collections::BTreeMap::new(),
            },
            claimed: false,
        });
        registry.get_or_start(&agent_id).await.unwrap();
        harness.factory.adapters()[0]
            .block_authenticate
            .store(true, Ordering::SeqCst);
        let started = harness.sign_in_with(None).await.expect("a sign-in");
        assert_eq!(started.delivery, AgentAuthDelivery::AdapterBrowser);
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        registry.recheck_existing_auth(&agent_id).await.unwrap();
        assert_eq!(
            harness.factory.adapters().len(),
            1,
            "the adapter was restarted"
        );

        harness
            .service
            .cancel(&started.agent_id, &started.auth_session_id);
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Cancelled);
        registry.recheck_existing_auth(&agent_id).await.unwrap();
        assert_eq!(
            harness.factory.adapters().len(),
            2,
            "no sign-in holds it now"
        );
    }

    #[tokio::test]
    async fn a_terminal_login_that_exits_cleanly_restarts_the_agent() {
        // Without the restart the credentials sit in the CLI's config while the
        // running adapter keeps the signed-out state it started with, so the
        // sign-in reports success and the very next prompt fails identically.
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(started.delivery, AgentAuthDelivery::ClientTerminal);
        assert_eq!(started.phase, AgentAuthPhase::Starting);

        let awaiting = harness.next_event().await;
        assert_eq!(awaiting.phase, AgentAuthPhase::AwaitingUser);
        assert!(!awaiting.reveal_terminal, "a healthy login stays hidden");
        assert!(
            awaiting.terminal.is_some(),
            "the terminal is addressable from the first event"
        );

        harness.terminals.exit(Some(0));
        let finished = harness.next_event().await;
        assert_eq!(finished.phase, AgentAuthPhase::Succeeded);
        assert!(!finished.reveal_terminal);
        assert_eq!(
            harness.terminals.closed(),
            vec![FakeTerminals::session_id()]
        );
        assert_eq!(
            harness.factory.adapters().len(),
            2,
            "the adapter is replaced, not reused"
        );
        assert_eq!(
            harness.signed_in_agents(),
            vec![AgentID("claude".to_string())],
            "success tells the sink, so caches keyed on auth can invalidate"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_terminal_login_that_fails_is_revealed_and_left_readable() {
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness.terminals.exit(Some(1));
        let failed = harness.next_event().await;
        assert_eq!(failed.phase, AgentAuthPhase::Failed);
        assert!(
            failed.reveal_terminal,
            "the user has to see what went wrong"
        );
        assert_eq!(
            failed.terminal.expect("a terminal").session_id,
            FakeTerminals::session_id()
        );
        // The scrollback is the only account of the failure, so the terminal
        // outlives the flow that started it.
        assert!(harness.terminals.closed().is_empty());
        assert_eq!(
            harness.factory.adapters().len(),
            1,
            "a failed login must not restart the agent"
        );
        assert!(harness.signed_in_agents().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_login_that_goes_quiet_is_revealed_before_it_is_abandoned() {
        // Nothing distinguishes "waiting for the browser" from "waiting for
        // someone to type" but the passage of time, so silence reveals.
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );
        harness.terminals.write_output(64);

        let revealed = harness.next_event().await;
        assert_eq!(revealed.phase, AgentAuthPhase::NeedsInput);
        assert!(revealed.reveal_terminal);

        let abandoned = harness.next_event().await;
        assert_eq!(abandoned.phase, AgentAuthPhase::Failed);
        assert!(
            abandoned.reveal_terminal,
            "the reveal latches, so a client that missed the first ask still shows it"
        );
        assert_eq!(
            harness.terminals.closed(),
            vec![FakeTerminals::session_id()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_immediate_login_shows_its_terminal_from_the_first_frame() {
        // An interactive login IS the terminal interaction — there is nothing
        // for the user to do until they can see it, so hiding it behind the
        // stuck detectors would just make them wait.
        let mut harness = Harness::with_reveal(
            claude_methods(),
            brisk_policy(),
            TerminalLoginReveal::Immediate,
        );
        harness.sign_in().await.expect("a sign-in");

        let awaiting = harness.next_event().await;
        assert_eq!(awaiting.phase, AgentAuthPhase::AwaitingUser);
        assert!(awaiting.reveal_terminal, "the terminal is the sign-in");
        assert!(awaiting.terminal.is_some());

        // The latch also means the stuck detectors never fire a second ask:
        // silence long past the grace window produces no further event.
        harness.terminals.write_output(16);
        tokio::time::sleep(brisk_policy().reveal_after_silence * 3).await;
        harness.terminals.exit(Some(0));
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Succeeded);
    }

    #[tokio::test(start_paused = true)]
    async fn pressing_done_completes_the_login_like_a_clean_exit() {
        // A full-screen login picker may never exit on its own; the user is
        // the one who knows the login is complete. Done closes the terminal
        // and restarts the adapter so the fresh credentials are actually
        // picked up.
        let mut harness = Harness::with_reveal(
            claude_methods(),
            brisk_policy(),
            TerminalLoginReveal::Immediate,
        );
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness
            .service
            .finish(&started.agent_id, &started.auth_session_id);
        let finished = harness.next_event().await;
        assert_eq!(finished.phase, AgentAuthPhase::Succeeded);
        assert_eq!(
            harness.terminals.closed(),
            vec![FakeTerminals::session_id()]
        );
        assert_eq!(
            harness.factory.adapters().len(),
            2,
            "the adapter is replaced, not reused"
        );

        // The flow retired itself, so the next press starts a fresh one.
        let next = harness.sign_in().await.expect("a new sign-in");
        assert_ne!(next.auth_session_id, started.auth_session_id);
        assert_eq!(
            harness.signed_in_agents(),
            vec![AgentID("claude".to_string())],
            "Done counts as a sign-in for auth-keyed caches too"
        );
    }

    /// `omp login` exits 0 whether or not it stored a credential, so a clean
    /// exit leaves its answer on screen and Done completes the login.
    #[tokio::test(start_paused = true)]
    async fn a_clean_exit_waits_for_done_when_models_prove_sign_in() {
        let mut definition = definition();
        definition.terminal_login_reveal = TerminalLoginReveal::Immediate;
        definition.sign_in_proof = SignInProof::ModelSelector;
        let mut harness = Harness::with_definition(claude_methods(), brisk_policy(), definition);
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness.terminals.exit(Some(0));
        let exited = harness.next_event().await;
        assert_eq!(exited.phase, AgentAuthPhase::NeedsInput);
        assert!(exited.reveal_terminal);
        assert!(
            harness.terminals.closed().is_empty(),
            "the answer stays readable"
        );
        assert_eq!(harness.factory.adapters().len(), 1, "no restart yet");

        harness
            .service
            .finish(&started.agent_id, &started.auth_session_id);
        let finished = harness.next_event().await;
        assert_eq!(finished.phase, AgentAuthPhase::Succeeded);
        assert_eq!(
            harness.terminals.closed(),
            vec![FakeTerminals::session_id()]
        );
        assert_eq!(harness.factory.adapters().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn finishing_a_stale_session_changes_nothing() {
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness.service.finish(
            &started.agent_id,
            &AgentAuthSessionID("auth_from_last_week".to_string()),
        );
        // The live login is untouched: it still completes through its exit.
        harness.terminals.exit(Some(0));
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Succeeded);
        assert_eq!(harness.factory.adapters().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn finishing_after_a_cancel_is_a_no_op() {
        // Stop then Done in quick succession: the cancel removed the entry,
        // so the finish finds nothing and the flow ends exactly once.
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness
            .service
            .cancel(&started.agent_id, &started.auth_session_id);
        harness
            .service
            .finish(&started.agent_id, &started.auth_session_id);

        let ended = harness.next_event().await;
        assert_eq!(ended.phase, AgentAuthPhase::Cancelled);
        assert_eq!(
            harness.factory.adapters().len(),
            1,
            "a cancelled login must not restart the agent"
        );
        assert!(harness.signed_in_agents().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_sign_in_joins_the_one_already_running() {
        // The failed-run card and the engine picker can both fire; neither
        // should start a second login.
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let first = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        let second = harness.sign_in().await.expect("the same sign-in");
        assert_eq!(second.auth_session_id, first.auth_session_id);
        assert_eq!(second.phase, AgentAuthPhase::AwaitingUser);
        let resumed = harness.next_event().await;
        assert_eq!(resumed.auth_session_id, first.auth_session_id);
        assert!(resumed.reveal_terminal);
        assert_eq!(
            resumed.terminal.unwrap().session_id,
            FakeTerminals::session_id()
        );
        assert_eq!(
            harness.terminals.with(|state| state.spawned.len()),
            1,
            "only one login terminal"
        );

        // And once it finishes, the next press starts a fresh one.
        harness.terminals.exit(Some(0));
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Succeeded);
        let third = harness.sign_in().await.expect("a new sign-in");
        assert_ne!(third.auth_session_id, first.auth_session_id);
    }

    #[tokio::test(start_paused = true)]
    async fn a_workspace_reopens_the_terminal_started_in_settings() {
        let mut harness = Harness::with_reveal(
            claude_methods(),
            brisk_policy(),
            TerminalLoginReveal::Immediate,
        );
        let first = harness.sign_in_with(None).await.expect("settings login");
        let original = harness.next_event().await;
        assert!(original.reveal_terminal);

        // The workspace controller did not own the first event. Its request
        // must receive the same terminal, even after the sheet was dismissed.
        for _ in 0..2 {
            let joined = harness.sign_in().await.expect("resume from workspace");
            let replay = harness.next_event().await;
            assert_eq!(joined.auth_session_id, first.auth_session_id);
            assert_eq!(replay.auth_session_id, first.auth_session_id);
            assert!(replay.reveal_terminal);
            assert_eq!(replay.terminal, original.terminal);
        }
        assert_eq!(harness.terminals.with(|state| state.spawned.len()), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn removal_is_refused_while_a_sign_in_is_live() {
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        let error = harness
            .service
            .guard_removal(&started.agent_id)
            .await
            .expect_err("live sign-in blocks removal");
        assert!(error.message.contains("sign-in is in progress"));

        harness
            .service
            .cancel(&started.agent_id, &started.auth_session_id);
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Cancelled);
        let guard = harness
            .service
            .guard_removal(&started.agent_id)
            .await
            .expect("completed sign-in releases removal");
        drop(guard);
    }

    #[tokio::test(start_paused = true)]
    async fn replay_preserves_browser_sign_in_and_fences_new_auth_until_cleanup() {
        let mut harness = Harness::with_provider(
            codex_methods(),
            brisk_policy(),
            TerminalLoginReveal::WhenStuck,
            "codex",
        );
        harness
            .service
            .registry
            .get_or_start(&super::super::AgentId::new("codex"))
            .await
            .unwrap();
        harness.factory.adapters()[0]
            .block_authenticate
            .store(true, Ordering::SeqCst);
        let started = harness.sign_in().await.unwrap();
        assert_eq!(started.delivery, AgentAuthDelivery::AdapterBrowser);
        let _ = harness.next_event().await;
        let id = super::super::AgentId::new(&started.agent_id.0);
        let error = harness
            .service
            .registry
            .prepare_replay(&id)
            .await
            .unwrap_err();
        assert!(error.message.contains("sign-in is in progress"));
        assert!(harness
            .service
            .existing_session(&started.agent_id)
            .is_some());
        harness
            .service
            .cancel(&started.agent_id, &started.auth_session_id);
        assert_eq!(harness.next_event().await.phase, AgentAuthPhase::Cancelled);
        let admission = harness.service.registry.prepare_replay(&id).await.unwrap();
        let mut new_login = Box::pin(harness.sign_in());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut new_login)
                .await
                .is_err()
        );
        drop(admission);
        let next = new_login.await.unwrap();
        assert_ne!(next.auth_session_id, started.auth_session_id);
        harness
            .service
            .cancel(&next.agent_id, &next.auth_session_id);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_sign_ins_create_only_one_login() {
        let harness = Harness::new(claude_methods(), brisk_policy());

        let (first, second) = tokio::join!(harness.sign_in(), harness.sign_in());
        let first = first.expect("the first sign-in");
        let second = second.expect("the joined sign-in");

        assert_eq!(second.auth_session_id, first.auth_session_id);
        assert_eq!(
            harness.terminals.with(|state| state.spawned.len()),
            1,
            "concurrent callers must not spawn duplicate login commands"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_terminal_login_kills_the_terminal() {
        let mut harness = Harness::new(claude_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness
            .service
            .cancel(&AgentID("claude".to_string()), &started.auth_session_id);
        let cancelled = harness.next_event().await;
        assert_eq!(cancelled.phase, AgentAuthPhase::Cancelled);
        assert_eq!(
            harness.terminals.closed(),
            vec![FakeTerminals::session_id()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_adapter_run_login_reports_what_the_adapter_reports() {
        let mut harness = Harness::new(codex_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        assert_eq!(started.delivery, AgentAuthDelivery::AdapterBrowser);
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        let finished = harness.next_event().await;
        assert_eq!(finished.phase, AgentAuthPhase::Succeeded);
        assert!(finished.terminal.is_none(), "no terminal is involved");
        let adapters = harness.factory.adapters();
        assert_eq!(
            adapters[0]
                .authenticated
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            ["chat-gpt"]
        );
        assert_eq!(
            adapters.len(),
            1,
            "the adapter authenticated in-process, so it has nothing to restart for"
        );
        assert!(harness.terminals.with(|state| state.spawned.is_empty()));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_an_adapter_run_login_withdraws_it_from_the_adapter() {
        let mut harness = Harness::new(codex_methods(), brisk_policy());
        let started = harness.sign_in().await.expect("a sign-in");
        harness.factory.adapters()[0]
            .block_authenticate
            .store(true, Ordering::SeqCst);
        assert_eq!(
            harness.next_event().await.phase,
            AgentAuthPhase::AwaitingUser
        );

        harness
            .service
            .cancel(&AgentID("claude".to_string()), &started.auth_session_id);
        let cancelled = harness.next_event().await;
        assert_eq!(cancelled.phase, AgentAuthPhase::Cancelled);
        assert_eq!(
            harness.factory.adapters()[0]
                .cancelled_authentications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            ["chat-gpt"]
        );
    }
}
