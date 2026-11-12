//! Bearer tokens that tell the proxy which agent session made a call.
//!
//! A token is minted before `session/new` or `session/load`, since agents
//! connect while that request is still in flight, and bound to the session
//! id the reply names. It lives no longer than that session: a token whose
//! adapter restarted, or whose session the adapter dropped or marked dead,
//! is refused, and forgotten at the next sweep or the next time anyone
//! presents it. Nothing is persisted, so every token is void after core
//! restarts.
//!
//! A token reaches only the servers its entries name, and holds their
//! leases once its session is bound, so a server's idle period starts once
//! no live token names it. Until then the mint holds them, and a mint given
//! up lets them go.
//!
//! OpenCode keeps one connection per server and directory for every
//! session there, with the headers of the latest session it opened, so its
//! token is shared: one per adapter process and directory, live while any
//! of the sessions bound to it is, and calls name their session in
//! `_meta`.

use super::runtime::McpLease;
use crate::agent::{
    AgentId, HarnessAdapter, HarnessSessionId, HarnessSessionState, HarnessSessionSummary,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ottam_protocol::{ConversationID, McpServerID, WorkspaceID};
use serde_json::{Map, Value};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// The `_meta` key OpenCode stamps on each MCP call with its session.
const OPENCODE_SESSION_ID: &str = "ai.opencode/sessionID";

/// Who a token speaks for. Attached to every proxied request it authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpCaller {
    /// The workspace whose server processes a call reaches. For a shared
    /// token, the one its live sessions agree on, until
    /// [`McpCaller::attributed`] names the call's session.
    pub(crate) workspace_id: WorkspaceID,
    /// The conversation the token was minted for, until
    /// [`McpCaller::attributed`] names a shared token's session.
    pub(crate) conversation_id: ConversationID,
    pub(crate) agent_id: AgentId,
    /// The adapter generation the session belongs to; a restart voids it.
    pub(crate) generation: u64,
    /// `None` while the `session/new` that will name the session is in
    /// flight, and for a shared token until a call names its session.
    pub(crate) session_id: Option<HarnessSessionId>,
    /// Whether the token is shared by an adapter's sessions in a directory.
    pub(crate) shared: bool,
    /// For a shared token, its live sessions; empty otherwise.
    pub(crate) sessions: Vec<BoundSession>,
    /// The servers the token's entries name; it reaches no other.
    pub(crate) servers: Vec<McpServerID>,
}

/// A session bound to a token, with the chat and workspace it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundSession {
    pub(crate) session_id: HarnessSessionId,
    pub(crate) conversation_id: ConversationID,
    pub(crate) workspace_id: WorkspaceID,
}

impl McpCaller {
    /// The caller of one call: a shared token's session is the one the
    /// call's `_meta` names among those bound to it, or its only one.
    pub(crate) fn attributed(mut self, meta: Option<&Map<String, Value>>) -> Self {
        if self.session_id.is_some() {
            return self;
        }
        let named = meta
            .and_then(|meta| meta.get(OPENCODE_SESSION_ID))
            .and_then(Value::as_str)
            .and_then(|named| {
                self.sessions
                    .iter()
                    .find(|session| session.session_id.0 == named)
            });
        let session = match (named, self.sessions.as_slice()) {
            (Some(session), _) => Some(session.clone()),
            (None, [only]) => Some(only.clone()),
            (None, _) => None,
        };
        if let Some(session) = session {
            self.session_id = Some(session.session_id);
            self.conversation_id = session.conversation_id;
            self.workspace_id = session.workspace_id;
        }
        self
    }

    /// Whether the token's entries name `server_id`.
    pub(crate) fn reaches(&self, server_id: &McpServerID) -> bool {
        self.servers.contains(server_id)
    }
}

/// Which sessions one token speaks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenScope {
    /// One agent session.
    Session,
    /// Every session one adapter process opens in this directory.
    Directory(PathBuf),
}

struct Grant {
    caller: McpCaller,
    adapter: Weak<dyn HarnessAdapter>,
    scope: TokenScope,
    /// The sessions bound to the token.
    sessions: BTreeMap<HarnessSessionId, BoundSession>,
    /// Mints of this token not yet bound or given up, with the workspace
    /// and servers each was granted.
    pending: HashMap<u64, (WorkspaceID, Vec<McpServerID>)>,
    leases: HashMap<(McpServerID, WorkspaceID), McpLease>,
}

impl Grant {
    /// The adapter's sessions, while the process the token was minted on
    /// still runs.
    fn held(&self) -> Option<Vec<HarnessSessionSummary>> {
        let adapter = self.adapter.upgrade()?;
        let generation = adapter.status().borrow().generation;
        (generation == self.caller.generation).then(|| adapter.sessions())
    }

    /// The bound sessions the adapter still holds alive.
    fn live_sessions(&self, held: &[HarnessSessionSummary]) -> Vec<BoundSession> {
        self.sessions
            .values()
            .filter(|bound| {
                held.iter().any(|session| {
                    session.session_id == bound.session_id
                        && session.state != HarnessSessionState::Dead
                })
            })
            .cloned()
            .collect()
    }

    fn is_live(&self) -> bool {
        self.live_caller().is_some()
    }

    /// Who the token speaks for now, or `None` once no mint or live
    /// session holds it.
    fn live_caller(&self) -> Option<McpCaller> {
        let held = self.held()?;
        let sessions = self.live_sessions(&held);
        if self.pending.is_empty() && sessions.is_empty() {
            return None;
        }
        let mut caller = self.caller.clone();
        caller.servers = self.servers();
        if matches!(self.scope, TokenScope::Directory(_)) {
            caller.shared = true;
            if let [first, rest @ ..] = sessions.as_slice() {
                if rest
                    .iter()
                    .all(|session| session.workspace_id == first.workspace_id)
                {
                    caller.workspace_id = first.workspace_id.clone();
                }
            }
            caller.sessions = sessions;
        }
        Some(caller)
    }

    /// The servers its bound sessions and pending mints were granted.
    fn servers(&self) -> Vec<McpServerID> {
        let mut servers: Vec<McpServerID> = Vec::new();
        let granted = self.leases.keys().map(|(server_id, _)| server_id);
        let pending = self.pending.values().flat_map(|(_, servers)| servers);
        for server_id in granted.chain(pending) {
            if !servers.contains(server_id) {
                servers.push(server_id.clone());
            }
        }
        servers
    }

    /// Let go of the leases of workspaces none of a shared token's live
    /// sessions or pending mints are in.
    fn release_unused(&mut self) -> Vec<McpLease> {
        if !matches!(self.scope, TokenScope::Directory(_)) {
            return Vec::new();
        }
        let Some(held) = self.held() else {
            return Vec::new();
        };
        let mut used: Vec<WorkspaceID> = self
            .live_sessions(&held)
            .into_iter()
            .map(|session| session.workspace_id)
            .collect();
        used.extend(
            self.pending
                .values()
                .map(|(workspace_id, _)| workspace_id.clone()),
        );
        let unused: Vec<_> = self
            .leases
            .keys()
            .filter(|(_, workspace_id)| !used.contains(workspace_id))
            .cloned()
            .collect();
        unused
            .iter()
            .filter_map(|key| self.leases.remove(key))
            .collect()
    }
}

/// What a token is minted for.
pub(crate) struct GrantRequest {
    pub(crate) workspace_id: WorkspaceID,
    pub(crate) conversation_id: ConversationID,
    pub(crate) agent_id: AgentId,
    pub(crate) scope: TokenScope,
    /// One lease per server the session's entries name.
    pub(crate) leases: Vec<(McpServerID, McpLease)>,
}

#[derive(Clone, Default)]
pub(crate) struct McpTokens {
    grants: Arc<Mutex<HashMap<String, Grant>>>,
    mints: Arc<AtomicU64>,
}

impl McpTokens {
    /// A token for a session about to be opened or restored on `adapter`.
    /// Dropping the returned grant without binding it gives this mint up;
    /// a token no mint or session holds is revoked.
    pub(crate) fn mint(
        &self,
        adapter: &Arc<dyn HarnessAdapter>,
        request: GrantRequest,
    ) -> McpGrant {
        let weak = Arc::downgrade(adapter);
        let generation = adapter.status().borrow().generation;
        let mint = self.mints.fetch_add(1, Ordering::Relaxed);
        let servers: Vec<McpServerID> = request
            .leases
            .iter()
            .map(|(server_id, _)| server_id.clone())
            .collect();
        let mut grants = self.lock();
        let dead = take_dead(&mut grants);
        let shared = match &request.scope {
            TokenScope::Session => None,
            TokenScope::Directory(directory) => grants
                .iter()
                .find(|(_, grant)| {
                    grant.caller.generation == generation
                        && Weak::ptr_eq(&grant.adapter, &weak)
                        && grant.scope == TokenScope::Directory(directory.clone())
                })
                .map(|(token, _)| token.clone()),
        };
        let pending = (request.workspace_id.clone(), servers);
        let token = match shared {
            Some(token) => {
                let grant = grants.get_mut(&token).expect("found above");
                grant.pending.insert(mint, pending);
                token
            }
            None => {
                let token = random_token();
                let caller = McpCaller {
                    workspace_id: request.workspace_id.clone(),
                    conversation_id: request.conversation_id.clone(),
                    agent_id: request.agent_id,
                    generation,
                    session_id: None,
                    shared: false,
                    sessions: Vec::new(),
                    servers: Vec::new(),
                };
                grants.insert(
                    token.clone(),
                    Grant {
                        caller,
                        adapter: weak,
                        scope: request.scope,
                        sessions: BTreeMap::new(),
                        pending: HashMap::from([(mint, pending)]),
                        leases: HashMap::new(),
                    },
                );
                token
            }
        };
        drop(grants);
        drop(dead);
        McpGrant {
            tokens: self.clone(),
            token: Some(token),
            mint,
            conversation_id: request.conversation_id,
            workspace_id: request.workspace_id,
            leases: request.leases,
        }
    }

    /// The caller a presented token speaks for, or `None` for a token that
    /// was never minted, was revoked, or outlived its sessions.
    pub(crate) fn resolve(&self, token: &str) -> Option<McpCaller> {
        let mut grants = self.lock();
        let caller = grants.get(token)?.live_caller();
        if caller.is_none() {
            let dead = grants.remove(token);
            drop(grants);
            drop(dead);
        }
        caller
    }

    /// Forget every token whose sessions all ended, and let go of what
    /// shared tokens hold for workspaces they no longer serve.
    pub(crate) fn sweep(&self) {
        let mut grants = self.lock();
        let dead = take_dead(&mut grants);
        let unused: Vec<McpLease> = grants
            .values_mut()
            .flat_map(Grant::release_unused)
            .collect();
        drop(grants);
        drop(dead);
        drop(unused);
    }

    /// Revoke what a conversation's sessions hold, as when it is deleted. A
    /// shared token loses only that conversation's sessions.
    pub(crate) fn revoke_conversation(&self, conversation_id: &ConversationID) {
        let mut grants = self.lock();
        let mut revoked = Vec::new();
        grants.retain(|_, grant| {
            grant
                .sessions
                .retain(|_, bound| &bound.conversation_id != conversation_id);
            let keep = match grant.scope {
                TokenScope::Session => &grant.caller.conversation_id != conversation_id,
                TokenScope::Directory(_) => !grant.pending.is_empty() || !grant.sessions.is_empty(),
            };
            if keep {
                revoked.extend(grant.release_unused());
            } else {
                revoked.extend(std::mem::take(&mut grant.leases).into_values());
            }
            keep
        });
        drop(grants);
        drop(revoked);
    }

    pub(crate) fn revoke_all(&self) {
        let revoked = std::mem::take(&mut *self.lock());
        drop(revoked);
    }

    fn bind(
        &self,
        token: &str,
        mint: u64,
        session: BoundSession,
        leases: Vec<(McpServerID, McpLease)>,
    ) {
        let mut grants = self.lock();
        let mut surplus = Vec::new();
        match grants.get_mut(token) {
            Some(grant) => {
                grant.pending.remove(&mint);
                if grant.scope == TokenScope::Session {
                    grant.caller.session_id = Some(session.session_id.clone());
                }
                for (server_id, lease) in leases {
                    let key = (server_id, session.workspace_id.clone());
                    match grant.leases.entry(key) {
                        Entry::Occupied(_) => surplus.push(lease),
                        Entry::Vacant(entry) => {
                            entry.insert(lease);
                        }
                    }
                }
                grant.sessions.insert(session.session_id.clone(), session);
            }
            // Revoked meanwhile: the session gets nothing.
            None => surplus.extend(leases.into_iter().map(|(_, lease)| lease)),
        }
        drop(grants);
        drop(surplus);
    }

    fn give_up(&self, token: &str, mint: u64) {
        let mut grants = self.lock();
        let Some(grant) = grants.get_mut(token) else {
            return;
        };
        grant.pending.remove(&mint);
        if grant.pending.is_empty() && grant.sessions.is_empty() {
            let revoked = grants.remove(token);
            drop(grants);
            drop(revoked);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Grant>> {
        self.grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Remove every grant that is no longer live. The caller drops them, and
/// with them their leases, once it has let go of the lock.
fn take_dead(grants: &mut HashMap<String, Grant>) -> Vec<Grant> {
    let dead: Vec<String> = grants
        .iter()
        .filter(|(_, grant)| !grant.is_live())
        .map(|(token, _)| token.clone())
        .collect();
    dead.iter()
        .filter_map(|token| grants.remove(token))
        .collect()
}

/// One mint of a token, not yet tied to a session. [`McpGrant::bind`] ties
/// it to the session the agent opened or restored, handing the token its
/// leases; dropping it gives the mint up and lets them go, as when the
/// request failed.
pub(crate) struct McpGrant {
    tokens: McpTokens,
    token: Option<String>,
    mint: u64,
    conversation_id: ConversationID,
    workspace_id: WorkspaceID,
    leases: Vec<(McpServerID, McpLease)>,
}

impl McpGrant {
    /// The `Authorization` header value an agent sends with every call.
    pub(crate) fn authorization(&self) -> String {
        format!("Bearer {}", self.token.as_deref().unwrap_or_default())
    }

    pub(crate) fn bind(mut self, session_id: HarnessSessionId) {
        if let Some(token) = self.token.take() {
            let session = BoundSession {
                session_id,
                conversation_id: self.conversation_id.clone(),
                workspace_id: self.workspace_id.clone(),
            };
            let leases = std::mem::take(&mut self.leases);
            self.tokens.bind(&token, self.mint, session, leases);
        }
    }
}

impl Drop for McpGrant {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.tokens.give_up(&token, self.mint);
        }
    }
}

/// 256 random bits, base64url. Never starts with `!`, which Oh-My-Pi would
/// run as a shell command, since that is not in the base64url alphabet.
fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).expect("the OS random source is available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The token in an `Authorization: Bearer <token>` header value.
pub(crate) fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token.trim())
}
