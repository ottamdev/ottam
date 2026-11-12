//! Delegation: a conversation hands a task to a named agent, or to any
//! installed agent, through Ottam's `delegate` tool, and the child's
//! report comes back as a note. See `docs/architecture/backend/mcp.md`
//! § Delegation.
//!
//! A child is an ordinary conversation, linked to its parent and started
//! with a run core opens itself. When any run of a child ends, its parent
//! gets a note. Notes wait while the parent runs or another child works;
//! then core delivers them in a prompt it writes, as a run of the parent
//! that follows the notes' "Received message" rows. A user message
//! delivers them too, ahead of its own text, and so does Continue.
//!
//! The messages the two sides send each other are notes too: a child's
//! `message_parent` in its parent, and a parent's `message_child` in the
//! child, delivered the same way when that conversation is idle.

use super::{ConversationRunContext, ConversationRunCoordinator};
use crate::agent::{AgentDefinition, AgentId, ModelCatalogService};
use crate::core_error::CoreError;
use crate::mcp::{OttamTools, DelegateRequest, DelegationTargets, McpCaller};
use crate::server::chat::{
    ChatService, ChildLink, DelegatedChild, WaitingNote, HISTORY_DEFAULT_ENTRIES,
    HISTORY_MAX_ENTRIES,
};
use crate::storage::repositories::{NamedAgentRecord, NamedAgentRepository};
use crate::storage::Database;
use ottam_protocol::{
    ConversationContextUsage, ConversationContinueDelegationResult, ConversationDeleteChildren,
    ConversationDeleteResult, ConversationDetail, ConversationID, ConversationInitialMessage,
    ConversationListChangedParams, ConversationRunID, ConversationRunStatus,
    ConversationRunSummary, ConversationSendToParentResult, ConversationStartRunParams,
    DelegationNote, DelegationNoteKind, DelegationNoteState, DelegationStopCause, DelegationTask,
    EngineID, ProtocolNotification, RunOrigin, WorkingRootAvailability, WorkingRootSelection,
    WorkspaceID, DELEGATION_ACK_DIRECTIVE, METHOD_CONVERSATION_LIST_CHANGED,
};
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

/// Working children one conversation may have at once.
const MAX_WORKING_CHILDREN: usize = 4;
/// Automatic follow-ups in a row one child may cause since the user's last
/// message; past this its notes wait for the user.
const MAX_WAKE_UPS: u32 = 5;
/// Prompted runs remembered per conversation: enough that a run ending as
/// the next ones start still finds how it was started.
const PROMPTED_RUNS_KEPT: usize = 4;
/// How much of a child's final message its note card quotes.
const NOTE_EXCERPT_CHARS: usize = 400;
/// How much of it the parent's prompt carries; the history tools read the
/// rest.
const REPORT_PROMPT_CHARS: usize = 8_000;
/// How often a working child's card in its parent refreshes, at most.
const ACTIVITY_REFRESH: Duration = Duration::from_secs(1);
/// How long shutdown waits for stopped children's notes.
const RUN_END_GRACE: Duration = Duration::from_secs(2);
/// The longest task title kept.
const TITLE_CHARS: usize = 60;
/// How much of a child's latest report `list_children` quotes.
const LIST_EXCERPT_CHARS: usize = 200;

/// Who runs a delegated task: a named agent, or an installed agent ad hoc.
struct DelegationTarget {
    /// The child's agent as its parent and the sidebar name it: the named
    /// agent's name, or the installed agent's display name.
    name: String,
    definition: AgentDefinition,
    model_id: Option<String>,
    mode_id: Option<String>,
    /// Whether `mode_id` is the agent's starting mode for a child, which
    /// neither the call nor a named agent chose.
    default_mode: bool,
    /// A named agent's instructions, sent ahead of the task; empty ad hoc.
    instructions: String,
    /// The seed the child's tile is drawn from: a random one ad hoc, none
    /// for a named agent's child, which draws its agent's current tile.
    avatar_seed: Option<u32>,
    /// The named agent's id; none ad hoc.
    named_agent_id: Option<String>,
}

/// A seed for an ad-hoc child's tile, so two children of one agent look
/// different. Any value will do when the system has no randomness.
fn random_avatar_seed() -> u32 {
    let mut bytes = [0u8; 4];
    if getrandom::getrandom(&mut bytes).is_err() {
        bytes = uuid::Uuid::now_v7().as_bytes()[12..16]
            .try_into()
            .unwrap_or_default();
    }
    u32::from_le_bytes(bytes)
}

/// The id of the model or mode `wanted` names, by id or display name. An
/// agent whose choices core hasn't seen yet gets `wanted` as given.
fn pick_choice<'a>(
    wanted: &str,
    choices: impl Iterator<Item = (&'a str, &'a str)>,
    kind: &str,
    definition: &AgentDefinition,
) -> Result<String, String> {
    let choices: Vec<(&str, &str)> = choices.collect();
    if choices.is_empty() {
        return Ok(wanted.to_owned());
    }
    choices
        .iter()
        .find(|(id, name)| id.eq_ignore_ascii_case(wanted) || name.eq_ignore_ascii_case(wanted))
        .map(|(id, _)| (*id).to_owned())
        .ok_or_else(|| {
            let listed: Vec<String> = choices
                .iter()
                .map(|(id, name)| {
                    if id == name {
                        (*id).to_owned()
                    } else {
                        format!("{name} ({id})")
                    }
                })
                .collect();
            format!(
                "{} offers no {kind} called {wanted}. Its {kind}s are: {}.",
                crate::server::engine_display_name(definition),
                listed.join(", ")
            )
        })
}

/// A conversation's agent and its mode: its live session's, or else the
/// mode its latest run asked for.
fn current_mode(detail: &ConversationDetail) -> Option<(EngineID, Option<String>)> {
    let run = detail.latest_run_page.runs.first()?;
    let mode = detail
        .latest_agent_session_page
        .sessions
        .iter()
        .find(|session| {
            session.engine_id == run.engine_id
                && run.agent_session_id.as_ref() == Some(&session.agent_session_id)
        })
        .and_then(|session| session.current_mode_id.clone())
        .or_else(|| run.mode_id.clone());
    Some((run.engine_id.clone(), mode))
}

/// What delegation needs from the server: the chats of any workspace, and
/// the database named agents are kept in.
pub(in crate::server) struct DelegationHost {
    pub(in crate::server) chats: Arc<dyn Fn(&WorkspaceID) -> ChatService + Send + Sync>,
    pub(in crate::server) database: Database,
}

#[derive(Default)]
pub(in crate::server::agents) struct Delegation {
    host: OnceLock<DelegationHost>,
    /// The models and modes each agent offers, which an ad-hoc child's
    /// are checked against.
    catalog: OnceLock<Arc<ModelCatalogService>>,
    /// Per parent, one `delegate` at a time, so concurrent calls see each
    /// other's children against the cap.
    starting: super::ConversationLocks,
    /// Per parent, one delivery at a time: two children ending together
    /// start one follow-up, and a user's message and a delivery never both
    /// carry the same notes.
    delivering: super::ConversationLocks,
    /// Per parent, the automatic follow-ups in a row each child caused
    /// since the user's last message.
    wake_ups: StdMutex<HashMap<ConversationID, HashMap<ConversationID, u32>>>,
    /// Children's runs stopped because their parent's turn was: their notes
    /// wait for the user.
    stopped_with_parent: StdMutex<HashSet<ConversationRunID>>,
    /// Children's runs their parent's agent stopped with `stop_child`: their
    /// notes are only a record, since the parent asked.
    stopped_by_parent_agent: StdMutex<HashSet<ConversationRunID>>,
    /// Per conversation, its latest prompted runs, oldest first, and whether
    /// the user drove each: started it with their own words, carrying no
    /// message from a parent. A child's report of such a run, or of the
    /// autonomous runs after it, wakes no one. Recorded as a run is created,
    /// before it can end; an autonomous run never outlives the process.
    prompted_runs: StdMutex<HashMap<ConversationID, Vec<(ConversationRunID, bool)>>>,
    /// When each workspace was last told a child's activity changed, and
    /// whether a later notice is already due.
    activity_told: StdMutex<HashMap<WorkspaceID, (Instant, bool)>>,
    /// Run ends being turned into notes. Shutdown lets them finish, so a
    /// child stopped by quitting still leaves its note.
    run_ends: StdMutex<tokio::task::JoinSet<()>>,
}

impl Delegation {
    fn wake_ups(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ConversationID, HashMap<ConversationID, u32>>> {
        self.wake_ups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn stopped_with_parent(&self) -> std::sync::MutexGuard<'_, HashSet<ConversationRunID>> {
        self.stopped_with_parent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn stopped_by_parent_agent(&self) -> std::sync::MutexGuard<'_, HashSet<ConversationRunID>> {
        self.stopped_by_parent_agent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn prompted_runs(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ConversationID, Vec<(ConversationRunID, bool)>>> {
        self.prompted_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The digest a child's summary carries, so the parent's tool call that
/// started it can find it: the first 16 hex digits of the SHA-256 of the
/// trimmed prompt.
pub(in crate::server) fn delegation_key(prompt: &str) -> String {
    let digest = Sha256::digest(prompt.trim().as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A task's short name: the caller's title, or the prompt's first line,
/// either cut to a sidebar row's width.
fn task_title(title: Option<&str>, prompt: &str) -> String {
    let title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| prompt.trim().lines().next().unwrap_or_default());
    excerpt(title, TITLE_CHARS)
}

fn excerpt(text: &str, chars: usize) -> String {
    let text = text.trim();
    let mut cut: String = text.chars().take(chars).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    cut
}

/// Token usage by agent and scope, as `get_conversation_info` lists it.
/// Agents count differently (a turn, the main agent, or the last request),
/// so a sum never crosses a scope. `more` says older runs are left out.
fn usage_lines(runs: &[ConversationRunSummary], more: bool) -> Vec<String> {
    #[derive(Default)]
    struct Group {
        engine: String,
        scope: String,
        turns: u64,
        input: u64,
        output: u64,
        total: u64,
        cached_read: u64,
        cached_write: u64,
    }
    let mut groups: Vec<Group> = Vec::new();
    for run in runs {
        let Some(usage) = &run.usage else {
            continue;
        };
        let scope = serde_json::to_value(usage.scope)
            .ok()
            .and_then(|scope| scope.as_str().map(str::to_owned))
            .unwrap_or_default();
        let engine = &run.engine_id.0;
        let index = match groups
            .iter()
            .position(|group| &group.engine == engine && group.scope == scope)
        {
            Some(index) => index,
            None => {
                groups.push(Group {
                    engine: engine.clone(),
                    scope,
                    ..Group::default()
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.turns += 1;
        group.input += usage.input_tokens;
        group.output += usage.output_tokens;
        group.total += usage.total_tokens;
        group.cached_read += usage.cached_read_tokens.unwrap_or(0);
        group.cached_write += usage.cached_write_tokens.unwrap_or(0);
    }
    if groups.is_empty() {
        return vec!["Token usage: no turn has reported any yet.".to_owned()];
    }
    let span = if more {
        format!("the latest {} turns", runs.len())
    } else {
        "every turn".to_owned()
    };
    let mut lines = vec![format!(
        "Token usage over {span}, as each agent reported it (scopes count differently, so \
         they are not added together):"
    )];
    for group in groups {
        let mut line = format!(
            "- {}, scope {}: {} turns, {} input, {} output",
            group.engine, group.scope, group.turns, group.input, group.output
        );
        // Cached prompt tokens are most of a total that dwarfs input plus
        // output, so name them.
        if group.cached_read > 0 || group.cached_write > 0 {
            line.push_str(&format!(
                ", {} cache read, {} cache write",
                group.cached_read, group.cached_write
            ));
        }
        line.push_str(&format!(", {} total tokens", group.total));
        lines.push(line);
    }
    lines
}

/// A child's state, as `list_children` and the status block name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildState {
    Working,
    WaitingOnPermission,
    Idle,
    Failed,
    Stopped,
}

impl ChildState {
    fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::WaitingOnPermission => "waiting on a permission",
            Self::Idle => "idle",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

fn note_state(status: &ConversationRunStatus) -> DelegationNoteState {
    match status {
        ConversationRunStatus::Failed => DelegationNoteState::Failed,
        ConversationRunStatus::Canceled | ConversationRunStatus::Interrupted => {
            DelegationNoteState::Stopped
        }
        _ => DelegationNoteState::Completed,
    }
}

impl ConversationRunCoordinator {
    /// Give delegation what it needs to reach chats and named agents.
    /// Without it, `delegate` fails and no notes are written.
    pub(in crate::server::agents) fn install_delegation(
        &self,
        host: DelegationHost,
        catalog: Arc<ModelCatalogService>,
    ) {
        let _ = self.delegation.host.set(host);
        let _ = self.delegation.catalog.set(catalog);
    }

    /// The tools the proxy's `ottam` path answers with.
    pub(in crate::server::agents) fn ottam_tools(&self) -> Arc<dyn OttamTools> {
        Arc::new(DelegationTools {
            coordinator: self.this.clone(),
        })
    }

    fn chat(&self, workspace_id: &WorkspaceID) -> Option<ChatService> {
        self.delegation
            .host
            .get()
            .map(|host| (host.chats)(workspace_id))
    }

    async fn named_agents(&self) -> Result<Vec<NamedAgentRecord>, CoreError> {
        let Some(host) = self.delegation.host.get() else {
            return Ok(Vec::new());
        };
        host.database
            .run("namedAgent.list", NamedAgentRepository::list)
            .await
            .map_err(CoreError::from)
    }

    /// The conversation's running run, if it has one.
    async fn active_run_id(&self, conversation_id: &ConversationID) -> Option<ConversationRunID> {
        self.active_runs()
            .await
            .values()
            .find(|run| run.context.conversation_id == *conversation_id)
            .map(|run| run.context.run_id.clone())
    }

    /// The conversation's running run's agent and session, if it has one.
    async fn working_run(
        &self,
        conversation_id: &ConversationID,
    ) -> Option<(AgentId, Option<String>)> {
        self.active_runs()
            .await
            .values()
            .find(|run| run.context.conversation_id == *conversation_id)
            .map(|run| {
                (
                    run.agent_id.clone(),
                    run.session_id.as_ref().map(|session| session.0.clone()),
                )
            })
    }

    async fn is_working(&self, conversation_id: &ConversationID) -> bool {
        self.working_run(conversation_id).await.is_some()
    }

    async fn child_state(
        &self,
        chat: &ChatService,
        child: &ConversationID,
    ) -> (ChildState, Option<ConversationRunSummary>) {
        if let Some((agent_id, session_id)) = self.working_run(child).await {
            let waiting = session_id
                .is_some_and(|session_id| self.permissions.has_pending(&agent_id, &session_id));
            let state = if waiting {
                ChildState::WaitingOnPermission
            } else {
                ChildState::Working
            };
            return (state, None);
        }
        let latest = chat
            .conversation_detail(child)
            .await
            .ok()
            .and_then(|detail| detail.latest_run_page.runs.into_iter().next());
        let state = match latest.as_ref().map(|run| &run.status) {
            Some(ConversationRunStatus::Failed) => ChildState::Failed,
            Some(ConversationRunStatus::Canceled | ConversationRunStatus::Interrupted) => {
                ChildState::Stopped
            }
            _ => ChildState::Idle,
        };
        (state, latest)
    }

    /// Whether the parent answers its own permission requests: Always
    /// allow on, or Claude or Codex in its bypass mode. A child copies it.
    async fn approves_automatically(&self, chat: &ChatService, parent: &ConversationID) -> bool {
        let Ok(detail) = chat.conversation_detail(parent).await else {
            return false;
        };
        if detail.conversation.always_allow_tool_actions {
            return true;
        }
        let Some((engine, mode)) = current_mode(&detail) else {
            return false;
        };
        self.registry
            .definition(&AgentId(engine.0))
            .ok()
            .and_then(|definition| definition.permission_bypass_mode())
            .is_some_and(|bypass| mode.as_deref() == Some(bypass))
    }

    /// Whether a parent that asks before acting may hand its child `mode`
    /// of `definition`: a mode that asks too, or the parent's own.
    async fn mode_asks(
        &self,
        chat: &ChatService,
        parent: &ConversationID,
        definition: &AgentDefinition,
        mode: &str,
    ) -> bool {
        if definition.asking_modes().contains(&mode) {
            return true;
        }
        let Ok(detail) = chat.conversation_detail(parent).await else {
            return false;
        };
        current_mode(&detail).is_some_and(|(engine, current)| {
            engine.0 == definition.agent_id.0 && current.as_deref() == Some(mode)
        })
    }

    /// A child recorded a tool call: its card's counts changed. Clients
    /// re-list at most once a second, and a change inside that second is
    /// told at its end; the run's end re-lists regardless.
    pub(super) fn child_activity_changed(&self, workspace_id: &WorkspaceID) {
        let now = Instant::now();
        let wait = {
            let mut told = self
                .delegation
                .activity_told
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match told.get_mut(workspace_id) {
                Some((_, true)) => return,
                Some((last, due)) if now.duration_since(*last) < ACTIVITY_REFRESH => {
                    *due = true;
                    Some(ACTIVITY_REFRESH - now.duration_since(*last))
                }
                _ => {
                    told.insert(workspace_id.clone(), (now, false));
                    None
                }
            }
        };
        let Some(wait) = wait else {
            self.notify_list_changed(workspace_id);
            return;
        };
        let Some(coordinator) = self.this.upgrade() else {
            return;
        };
        let workspace_id = workspace_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            coordinator
                .delegation
                .activity_told
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(workspace_id.clone(), (Instant::now(), false));
            coordinator.notify_list_changed(&workspace_id);
        });
    }

    fn notify_list_changed(&self, workspace_id: &WorkspaceID) {
        let notification = ProtocolNotification::new(
            METHOD_CONVERSATION_LIST_CHANGED,
            ConversationListChangedParams {
                workspace_id: workspace_id.clone(),
            },
        );
        if let Ok(value) = serde_json::to_value(notification) {
            let _ = self.outbound.notify(value);
        }
    }

    /// The conversation's working root, as a run needs it.
    async fn run_root(
        chat: &ChatService,
        conversation_id: &ConversationID,
    ) -> Result<(PathBuf, Option<ottam_protocol::ProjectID>), CoreError> {
        let root = chat.working_root(conversation_id).await?;
        if root.availability != WorkingRootAvailability::Available {
            return Err(CoreError::new(
                "workingRootUnavailable",
                "the Conversation's working root is unavailable",
            )
            .recoverable(true));
        }
        Ok((PathBuf::from(root.canonical_path), root.project_id))
    }

    async fn is_child(&self, caller: &McpCaller) -> bool {
        let Some(chat) = self.chat(&caller.workspace_id) else {
            return false;
        };
        matches!(chat.parent_of(&caller.conversation_id).await, Ok(Some(_)))
    }

    /// The installed agents, in the registry's order: those whose adapter
    /// needs no install.
    fn installed_agents(&self) -> Vec<AgentDefinition> {
        self.registry
            .definitions()
            .into_iter()
            .filter(|definition| !self.registry.adapter_needs_install(definition))
            .collect()
    }

    /// Who runs a `delegate` call's task, and how.
    async fn delegation_target(
        &self,
        chat: &ChatService,
        caller: &McpCaller,
        request: &DelegateRequest,
    ) -> Result<DelegationTarget, String> {
        if let Some(agent) = &request.agent {
            return self.named_target(agent).await;
        }
        let (definition, own) = match &request.engine {
            Some(engine) => {
                let installed = self.installed_agents();
                let listed = || {
                    installed
                        .iter()
                        .map(|definition| {
                            format!(
                                "{} ({})",
                                crate::server::engine_display_name(definition),
                                definition.agent_id.0
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                // An id names one agent; a display name may name several
                // setups of one product.
                let by_name: Vec<&AgentDefinition> = installed
                    .iter()
                    .filter(|definition| {
                        crate::server::engine_display_name(definition).eq_ignore_ascii_case(engine)
                    })
                    .collect();
                let found = installed
                    .iter()
                    .find(|definition| definition.agent_id.0.eq_ignore_ascii_case(engine))
                    .or(match by_name.as_slice() {
                        [only] => Some(*only),
                        _ => None,
                    });
                let Some(definition) = found.cloned() else {
                    return Err(if by_name.len() > 1 {
                        format!(
                            "More than one installed agent is called {engine}; pass its id. \
                             The installed agents are: {}.",
                            listed()
                        )
                    } else {
                        format!(
                            "No installed agent is called {engine}. The installed agents are: {}.",
                            listed()
                        )
                    });
                };
                let own = definition.agent_id == caller.agent_id;
                (definition, own)
            }
            None => (
                self.registry
                    .definition(&caller.agent_id)
                    .map_err(|_| "Ottam cannot find your agent to delegate to")?,
                true,
            ),
        };
        let catalog = self.delegation.catalog.get();
        let model_id = match &request.model {
            Some(model) => {
                let models = catalog
                    .map(|catalog| catalog.models_for(&definition.agent_id))
                    .unwrap_or_default();
                Some(pick_choice(
                    model,
                    models
                        .iter()
                        .map(|option| (option.model_id.as_str(), option.display_name.as_str())),
                    "model",
                    &definition,
                )?)
            }
            // The caller's own agent runs on its model, as a subagent does.
            None if own => self.latest_model(chat, &caller.conversation_id).await,
            None => None,
        };
        let mode_id = match &request.mode {
            Some(mode) => {
                let modes = catalog
                    .and_then(|catalog| catalog.session_selects_for(&definition.agent_id))
                    .map(|selects| selects.modes)
                    .unwrap_or_default();
                Some(pick_choice(
                    mode,
                    modes
                        .iter()
                        .map(|option| (option.id.as_str(), option.display_name.as_str())),
                    "mode",
                    &definition,
                )?)
            }
            None => definition.delegated_mode().map(str::to_owned),
        };
        Ok(DelegationTarget {
            name: crate::server::engine_display_name(&definition),
            model_id,
            mode_id,
            default_mode: request.mode.is_none(),
            instructions: String::new(),
            avatar_seed: Some(random_avatar_seed()),
            named_agent_id: None,
            definition,
        })
    }

    async fn named_target(&self, agent: &str) -> Result<DelegationTarget, String> {
        let agents = self.named_agents().await.map_err(|error| error.message)?;
        let Some(named) = agents
            .iter()
            .find(|named| named.name.eq_ignore_ascii_case(agent))
            .cloned()
        else {
            let names: Vec<&str> = agents.iter().map(|named| named.name.as_str()).collect();
            return Err(if names.is_empty() {
                "There are no named agents yet. Pass `engine` to delegate to an installed \
                 agent, or leave it out to delegate to your own."
                    .to_owned()
            } else {
                format!(
                    "There is no named agent called {agent}. The named agents are: {}.",
                    names.join(", ")
                )
            });
        };
        let definition = self
            .registry
            .definition(&AgentId(named.engine_id.clone()))
            .map_err(|_| {
                format!(
                    "{}'s agent was removed. The user can choose another for it in \
                     Settings › Agents › Named Agents.",
                    named.name
                )
            })?;
        let mode_id = named
            .mode_id
            .clone()
            .or_else(|| definition.delegated_mode().map(str::to_owned));
        Ok(DelegationTarget {
            default_mode: named.mode_id.is_none(),
            name: named.name,
            model_id: named.model_id,
            mode_id,
            instructions: named.instructions.trim().to_owned(),
            avatar_seed: None,
            named_agent_id: Some(named.id),
            definition,
        })
    }

    /// The model the conversation's latest run asked for.
    async fn latest_model(
        &self,
        chat: &ChatService,
        conversation: &ConversationID,
    ) -> Option<String> {
        chat.conversation_detail(conversation)
            .await
            .ok()?
            .latest_run_page
            .runs
            .into_iter()
            .next()?
            .model_id
    }

    async fn delegate(
        self: &Arc<Self>,
        caller: McpCaller,
        request: DelegateRequest,
    ) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot delegate right now")?;
        let parent = caller.conversation_id.clone();
        let _starting = super::lock_conversation(&self.delegation.starting, &parent).await;
        let target = self.delegation_target(&chat, &caller, &request).await?;
        let DelegateRequest {
            title,
            prompt,
            mode: request_mode,
            ..
        } = request;
        let definition = &target.definition;
        let working = self.working_children(&chat, &parent).await?;
        if working.len() >= MAX_WORKING_CHILDREN {
            return Err(format!(
                "{MAX_WORKING_CHILDREN} delegated agents are already working: {}. Wait \
                 for one to report back before delegating again.",
                working.join(", ")
            ));
        }
        let parent_approves = self.approves_automatically(&chat, &parent).await;
        let mut mode_id = target.mode_id.clone();
        // A child's starting mode may act without asking (Oh-My-Pi's
        // `default`); a parent that asks gets its read-only mode instead.
        let read_only_mode = definition.read_only_mode();
        if target.default_mode && !parent_approves && mode_id.as_deref() != read_only_mode {
            if let Some(mode) = mode_id.as_deref() {
                if !self.mode_asks(&chat, &parent, definition, mode).await {
                    mode_id = read_only_mode.map(str::to_owned);
                }
            }
        }
        // A child in its agent's read-only mode asks before it acts,
        // whatever its parent allows: Always allow would answer for it.
        let read_only =
            read_only_mode.is_some_and(|read_only| mode_id.as_deref() == Some(read_only));
        // An ad-hoc mode is the parent's choice, not the user's, so it may
        // not skip the questions the parent itself would be asked: a parent
        // that asks hands its child only a mode that asks too, or its own.
        let chosen = mode_id
            .as_deref()
            .filter(|_| request_mode.is_some() && !parent_approves);
        if let Some(mode) = chosen {
            if !self.mode_asks(&chat, &parent, definition, mode).await {
                let asking = match definition.asking_modes() {
                    [] => String::new(),
                    modes => format!(", or pick one that asks: {}", modes.join(", ")),
                };
                return Err(format!(
                    "This conversation asks before acting, so a delegated agent cannot run \
                     in {}'s {mode} mode, which may act without asking. Leave `mode` \
                     out{asking}.",
                    target.name
                ));
            }
        }
        let approves = !read_only && parent_approves;
        let root = chat
            .working_root(&parent)
            .await
            .map_err(|error| error.message)?;
        let selection = match (root.project_id.clone(), root.source_id.clone()) {
            (Some(project_id), Some(source_id)) => WorkingRootSelection::Project {
                project_id,
                source_id,
            },
            _ => WorkingRootSelection::WorkspaceFiles,
        };
        let body = if target.instructions.is_empty() {
            prompt.clone()
        } else {
            format!("{}\n\n{}", target.instructions, prompt)
        };
        let task = DelegationTask {
            title: task_title(title.as_deref(), &prompt),
            prompt: prompt.clone(),
            instructions: target.instructions.clone(),
            started_by_engine_id: Some(EngineID(caller.agent_id.0.clone())),
        };
        let created = chat
            .create_conversation(
                selection,
                Vec::new(),
                Some(format!("{}: {}", target.name, task.title)),
                Some(ConversationInitialMessage {
                    body,
                    idempotency_key: uuid::Uuid::now_v7().to_string(),
                    attachment_ids: Vec::new(),
                }),
            )
            .await
            .map_err(|error| error.message)?;
        let child = created.conversation.conversation.conversation_id.clone();
        let input = created
            .initial_input
            .ok_or("the delegated conversation has no first message")?;
        // The turn that called `delegate`, so clients can show the child
        // there even when the agent's transcript does not name the call.
        let from_run = self.active_run_id(&parent).await;
        chat.mark_delegated(
            &child,
            ChildLink {
                parent: parent.clone(),
                agent_name: target.name.clone(),
                task_title: task.title.clone(),
                delegation_key: delegation_key(&prompt),
                always_allow_tool_actions: approves,
                avatar_seed: target.avatar_seed,
                named_agent_id: target.named_agent_id.clone(),
                from_run,
            },
            Some((input.message_id.clone(), task)),
        )
        .await
        .map_err(|error| error.message)?;
        self.notify_list_changed(&caller.workspace_id);
        let started = match Self::run_root(&chat, &child).await {
            Ok((root, project_id)) => {
                self.start_run(
                    ConversationStartRunParams {
                        workspace_id: caller.workspace_id.clone(),
                        conversation_id: child.clone(),
                        engine_id: EngineID(definition.agent_id.0.clone()),
                        model_id: target.model_id.clone(),
                        mode_id,
                        thought_level_id: None,
                        run_input_id: input.run_input_id,
                    },
                    chat.clone(),
                    root,
                    project_id,
                    false,
                )
                .await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = started {
            // A failed call started no child, so the parent's card for it
            // must not claim this one, nor the next call with this prompt.
            if let Err(error) = chat.forget_delegation_key(&child).await {
                tracing::warn!(kind = error.kind, "a failed child kept its delegation key");
            }
            return Err(format!(
                "Ottam opened a conversation for {} ({}), but its agent could not \
                 start: {}",
                target.name, child.0, error.message
            ));
        }
        Ok(format!(
            "Delegated to {} in conversation {}. It is working now. Its report will \
             arrive in this conversation as a new message when it finishes, so do not \
             wait or poll for it: continue with other work, or end your turn.",
            target.name, child.0
        ))
    }

    async fn list_children(&self, caller: McpCaller) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot list children right now")?;
        let children = chat
            .children(&caller.conversation_id)
            .await
            .map_err(|error| error.message)?;
        if children.is_empty() {
            return Ok("This conversation has not delegated to any agent.".to_owned());
        }
        let mut lines = Vec::new();
        for child in &children {
            let (state, latest) = self.child_state(&chat, &child.conversation_id).await;
            let mut line = format!(
                "- {} ({}): {}",
                child.name,
                child.conversation_id.0,
                state.label()
            );
            if let Some(run) = latest {
                let report = chat
                    .final_report(&child.conversation_id, &run.run_id)
                    .await
                    .unwrap_or_default();
                if !report.trim().is_empty() {
                    // A stopped or failed turn's last words are not a report.
                    let label = match state {
                        ChildState::Idle => "Latest report",
                        _ => "Its last message before that",
                    };
                    line.push_str(&format!(
                        ". {label}: \"{}\"",
                        excerpt(&report, LIST_EXCERPT_CHARS)
                    ));
                }
            }
            lines.push(line);
        }
        Ok(lines.join("\n"))
    }

    /// The caller's own conversation: what it is, who is running it, the
    /// agents around it, and the tokens it used as the agents reported them.
    async fn conversation_info(&self, caller: McpCaller) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot describe this conversation right now")?;
        let me = &caller.conversation_id;
        let detail = chat
            .conversation_detail(me)
            .await
            .map_err(|error| error.message)?;
        let conversation = &detail.conversation;
        let mut lines = vec![
            format!("Conversation: {}", me.0),
            format!("Title: {}", conversation.title),
            format!("Workspace: {}", detail.workspace.display_name),
            format!(
                "Working folder: {} ({})",
                conversation.working_root.canonical_path, conversation.working_root.display_name
            ),
        ];
        let runs = &detail.latest_run_page.runs;
        let caller_session = caller.session_id.as_ref().map(|session| session.0.as_str());
        let own_run = runs.iter().find(|run| {
            run.agent_session_id.as_ref().map(|id| id.0.as_str()) == caller_session
                && caller_session.is_some()
        });
        let session = chat
            .active_agent_session(me, None)
            .await
            .ok()
            .flatten()
            .filter(|session| session.external_session_id.as_deref() == caller_session);
        let mut you = format!("You: {}", self.agent_label(&caller.agent_id));
        if let Some(run) = own_run {
            you.push_str(&format!(", engine {}", run.engine_id.0));
            if let Some(model) = &run.model_id {
                you.push_str(&format!(", model {model}"));
            }
        }
        if let Some(mode) = session
            .as_ref()
            .and_then(|session| session.current_mode_id.as_deref())
            .or_else(|| own_run.and_then(|run| run.mode_id.as_deref()))
        {
            you.push_str(&format!(", mode {mode}"));
        }
        if let Some(session) = caller_session {
            you.push_str(&format!(" (agent session {session})"));
        }
        lines.push(you);
        let mut earlier = Vec::new();
        for run in runs
            .iter()
            .filter(|run| run.agent_session_id.as_ref().map(|id| id.0.as_str()) != caller_session)
        {
            let agent = match &run.model_id {
                Some(model) => format!("{} ({model})", run.engine_id.0),
                None => run.engine_id.0.clone(),
            };
            if !earlier.contains(&agent) {
                earlier.push(agent);
            }
        }
        if !earlier.is_empty() {
            lines.push(format!(
                "Other sessions in this conversation ran on: {}",
                earlier.join(", ")
            ));
        }
        if let Ok(Some(parent)) = chat.parent_of(me).await {
            lines.push(format!(
                "Delegated by: conversation {} (get_session_messages reads it)",
                parent.0
            ));
        }
        if let Ok(children) = chat.children(me).await {
            if !children.is_empty() {
                lines.push(format!(
                    "Delegated agents: {} (list_children lists them)",
                    children.len()
                ));
            }
        }
        lines.push(String::new());
        lines.extend(usage_lines(
            runs,
            detail.latest_run_page.next_cursor.is_some(),
        ));
        if let Some(context) = session
            .as_ref()
            .and_then(|session| session.context_usage.as_deref())
            .and_then(|usage| serde_json::from_str::<ConversationContextUsage>(usage).ok())
        {
            lines.push(format!(
                "Context: {} of {} tokens used, as your agent last reported.",
                context.used, context.size
            ));
        }
        Ok(lines.join("\n"))
    }

    /// An agent instance's name as the user knows it: its label, or its
    /// product's name.
    fn agent_label(&self, agent_id: &AgentId) -> String {
        match self.registry.definition(agent_id) {
            Ok(definition) => definition.instance.as_ref().map_or_else(
                || definition.presentation.product_name.to_owned(),
                |instance| instance.label.clone(),
            ),
            Err(_) => agent_id.0.clone(),
        }
    }

    /// One of `parent`'s children, by conversation id, or by its task title
    /// or agent name when only one child has it.
    async fn resolve_child(
        chat: &ChatService,
        parent: &ConversationID,
        reference: &str,
    ) -> Result<DelegatedChild, String> {
        let children = chat.children(parent).await.map_err(|error| error.message)?;
        if children.is_empty() {
            return Err("This conversation has not delegated to any agent.".to_owned());
        }
        let reference = reference.trim();
        if let Some(child) = children
            .iter()
            .find(|child| child.conversation_id.0 == reference)
        {
            return Ok(child.clone());
        }
        let named: Vec<&DelegatedChild> = children
            .iter()
            .filter(|child| {
                child.task_title.eq_ignore_ascii_case(reference)
                    || child.name.eq_ignore_ascii_case(reference)
            })
            .collect();
        let listing = || {
            children
                .iter()
                .map(|child| {
                    format!(
                        "{} \"{}\" ({})",
                        child.name, child.task_title, child.conversation_id.0
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        match named.as_slice() {
            [only] => Ok((*only).clone()),
            [] => Err(format!(
                "No delegated agent of this conversation is {reference}. Its agents are: {}.",
                listing()
            )),
            _ => Err(format!(
                "More than one delegated agent is {reference}; name one by its conversation \
                 id: {}.",
                listing()
            )),
        }
    }

    /// `parent`'s working children, as "name (conversation)".
    async fn working_children(
        &self,
        chat: &ChatService,
        parent: &ConversationID,
    ) -> Result<Vec<String>, String> {
        let children = chat.children(parent).await.map_err(|error| error.message)?;
        let mut working = Vec::new();
        for child in &children {
            if self.is_working(&child.conversation_id).await {
                working.push(format!("{} ({})", child.name, child.conversation_id.0));
            }
        }
        Ok(working)
    }

    async fn message_child(
        self: &Arc<Self>,
        caller: McpCaller,
        child: String,
        message: String,
    ) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot send messages right now")?;
        let parent = caller.conversation_id.clone();
        let child = Self::resolve_child(&chat, &parent, &child).await?;
        // Waking an idle child counts against the cap as delegating does.
        let _starting = super::lock_conversation(&self.delegation.starting, &parent).await;
        if !self.is_working(&child.conversation_id).await {
            let working = self.working_children(&chat, &parent).await?;
            if working.len() >= MAX_WORKING_CHILDREN {
                return Err(format!(
                    "{MAX_WORKING_CHILDREN} delegated agents are already working: {}. Wait \
                     for one to report back before messaging {}, which is idle.",
                    working.join(", "),
                    child.name
                ));
            }
        }
        let note = DelegationNote {
            child_conversation_id: child.conversation_id.clone(),
            child_name: self.agent_label(&caller.agent_id),
            kind: DelegationNoteKind::ParentMessage,
            state: DelegationNoteState::Completed,
            excerpt: excerpt(&message, NOTE_EXCERPT_CHARS),
            waits_for_user: false,
            deferred: false,
            run_id: None,
            stop_cause: None,
        };
        chat.post_note(&child.conversation_id, &note, message, false)
            .await
            .map_err(|error| error.message)?;
        let delivered = self
            .try_deliver(&chat, &child.conversation_id)
            .await
            .map_err(|error| {
                format!(
                    "The message is in {}'s conversation ({}), but its agent could not \
                     start: {}",
                    child.name, child.conversation_id.0, error.message
                )
            })?;
        Ok(if delivered.is_some() {
            format!(
                "{} ({}) is reading your message now. Its reply arrives in this \
                 conversation as a new message when it finishes, so do not wait or poll \
                 for it.",
                child.name, child.conversation_id.0
            )
        } else {
            format!(
                "{} ({}) is working, so your message waits for the end of its turn. It \
                 reads it then, and its reply arrives here as a new message.",
                child.name, child.conversation_id.0
            )
        })
    }

    async fn message_parent(
        self: &Arc<Self>,
        caller: McpCaller,
        message: String,
    ) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot send messages right now")?;
        let child = caller.conversation_id.clone();
        let (parent, name) = chat
            .delegation_of(&child)
            .await
            .map_err(|error| error.message)?
            .ok_or("This conversation was not delegated, so it has no parent to message.")?;
        let capped = self.wake_ups_capped(&parent, &child);
        let note = DelegationNote {
            child_conversation_id: child.clone(),
            child_name: name,
            kind: DelegationNoteKind::ChildMessage,
            state: DelegationNoteState::Completed,
            excerpt: excerpt(&message, NOTE_EXCERPT_CHARS),
            waits_for_user: capped,
            deferred: false,
            run_id: None,
            stop_cause: None,
        };
        chat.post_note(&parent, &note, message, false)
            .await
            .map_err(|error| error.message)?;
        if capped {
            return Ok(
                "Your parent has been woken often enough without the user, so your \
                       message waits for the user's next message. Keep working on your task."
                    .to_owned(),
            );
        }
        let delivered = self
            .try_deliver(&chat, &parent)
            .await
            .map_err(|error| error.message)?;
        Ok(if delivered.is_some() {
            "Your parent is reading your message now. Keep working on your task; its \
             reply, if any, arrives as a new message after your turn."
                .to_owned()
        } else {
            "Your parent is busy, so your message waits for the end of its turn. Keep \
             working on your task."
                .to_owned()
        })
    }

    async fn stop_child(&self, caller: McpCaller, child: String) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot stop agents right now")?;
        let child = Self::resolve_child(&chat, &caller.conversation_id, &child).await?;
        let run_id = self.active_run_id(&child.conversation_id).await;
        let Some(run_id) = run_id else {
            let (state, _) = self.child_state(&chat, &child.conversation_id).await;
            return Ok(format!(
                "{} ({}) is not working; it is {}.",
                child.name,
                child.conversation_id.0,
                state.label()
            ));
        };
        self.delegation
            .stopped_by_parent_agent()
            .insert(run_id.clone());
        if let Err(error) = self
            .cancel_conversation_run(&chat, &child.conversation_id, &run_id)
            .await
        {
            self.delegation.stopped_by_parent_agent().remove(&run_id);
            return Err(format!(
                "{} could not be stopped: {}",
                child.name, error.message
            ));
        }
        Ok(format!(
            "Stopped {} ({}). Its conversation keeps what it did so far; message_child \
             starts it again.",
            child.name, child.conversation_id.0
        ))
    }

    /// The conversation a history tool reads: one of the caller's children,
    /// the caller's parent (a child's default), or the caller itself (the
    /// default for a conversation that was not delegated).
    async fn history_target(
        chat: &ChatService,
        caller: &McpCaller,
        conversation: Option<String>,
    ) -> Result<ConversationID, String> {
        let me = &caller.conversation_id;
        let parent = chat.parent_of(me).await.map_err(|error| error.message)?;
        match conversation.as_deref().map(str::trim) {
            None | Some("") => Ok(parent.unwrap_or_else(|| me.clone())),
            Some(reference) if reference == me.0 => Ok(me.clone()),
            Some(reference) if parent.as_ref().is_some_and(|parent| parent.0 == reference) => {
                Ok(ConversationID(reference.to_owned()))
            }
            Some(reference) => match Self::resolve_child(chat, me, reference).await {
                Ok(child) => Ok(child.conversation_id),
                Err(_) if parent.is_some() => Err(format!(
                    "You can read your parent's conversation ({}) and your own, not {reference}.",
                    parent.map(|parent| parent.0).unwrap_or_default()
                )),
                Err(error) => Err(error),
            },
        }
    }

    async fn session_messages(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        before: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot read history right now")?;
        let target = Self::history_target(&chat, &caller, conversation).await?;
        let limit = limit.map_or(HISTORY_DEFAULT_ENTRIES, |limit| {
            (limit as usize).clamp(1, HISTORY_MAX_ENTRIES)
        });
        chat.history_page(&target, before, limit)
            .await
            .map_err(|error| error.message)
    }

    async fn toolcall_output(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        tool_call_id: String,
        offset: Option<u64>,
    ) -> Result<String, String> {
        let chat = self
            .chat(&caller.workspace_id)
            .ok_or("Ottam cannot read history right now")?;
        let target = Self::history_target(&chat, &caller, conversation).await?;
        chat.tool_output(&target, &tool_call_id, offset.unwrap_or(0))
            .await
            .map_err(|error| error.message)
    }

    /// Whether `child` has caused its 5 automatic follow-ups in `parent`
    /// since the user last wrote there.
    fn wake_ups_capped(&self, parent: &ConversationID, child: &ConversationID) -> bool {
        self.delegation
            .wake_ups()
            .get(parent)
            .and_then(|counts| counts.get(child))
            .is_some_and(|count| *count >= MAX_WAKE_UPS)
    }

    /// The status block a core-written follow-up ends with.
    async fn status_block(&self, chat: &ChatService, parent: &ConversationID) -> String {
        let Ok(children) = chat.children(parent).await else {
            return String::new();
        };
        let mut states = Vec::new();
        for child in &children {
            let (state, _) = self.child_state(chat, &child.conversation_id).await;
            states.push(format!(
                "{} ({}): {}",
                child.name,
                child.conversation_id.0,
                state.label()
            ));
        }
        if states.is_empty() {
            String::new()
        } else {
            format!("\n\nYour delegated agents now: {}.", states.join("; "))
        }
    }

    /// What the notes are, for the line that opens the prompt carrying
    /// them. A child's notes are its parent's messages; a parent's are its
    /// children's reports and messages.
    fn notes_heading(notes: &[WaitingNote]) -> &'static str {
        if notes
            .iter()
            .all(|waiting| waiting.note.kind == DelegationNoteKind::ParentMessage)
        {
            "Messages from the agent that delegated this task to you"
        } else {
            "Reports and messages from agents you delegated to"
        }
    }

    fn reports(notes: &[WaitingNote]) -> String {
        notes
            .iter()
            .map(|waiting| {
                let note = &waiting.note;
                let body = waiting.report.trim();
                match note.kind {
                    DelegationNoteKind::Report => {}
                    DelegationNoteKind::ChildMessage => {
                        return format!(
                            "## {} sent a message while it works (conversation {})\n\n{}",
                            note.child_name,
                            note.child_conversation_id.0,
                            excerpt(body, REPORT_PROMPT_CHARS)
                        );
                    }
                    DelegationNoteKind::ParentMessage => {
                        return format!(
                            "## From {}\n\n{}\n\nYour final message reaches it when you \
                             finish; message_parent reaches it sooner.",
                            note.child_name,
                            excerpt(body, REPORT_PROMPT_CHARS)
                        );
                    }
                }
                let ended = match (note.state, note.stop_cause) {
                    (DelegationNoteState::Completed, _) => "finished",
                    (DelegationNoteState::Failed, _) => "failed",
                    (DelegationNoteState::Stopped, Some(DelegationStopCause::Quit)) => {
                        "was stopped when Ottam quit"
                    }
                    (DelegationNoteState::Stopped, Some(DelegationStopCause::ParentStopped)) => {
                        "was stopped when the user stopped your turn"
                    }
                    (DelegationNoteState::Stopped, Some(DelegationStopCause::User)) => {
                        "was stopped by the user in its own conversation"
                    }
                    (DelegationNoteState::Stopped, Some(DelegationStopCause::ParentAgent)) => {
                        "was stopped by you"
                    }
                    (DelegationNoteState::Stopped, None) => "was stopped",
                };
                let report = if body.is_empty() {
                    "(no final message)".to_owned()
                } else if body.chars().count() > REPORT_PROMPT_CHARS {
                    format!(
                        "{}\n\n(The report goes on. get_session_messages with conversation \
                         {} and limit=1 reads it whole.)",
                        excerpt(body, REPORT_PROMPT_CHARS),
                        note.child_conversation_id.0
                    )
                } else {
                    body.to_owned()
                };
                format!(
                    "## {} {ended} (conversation {})\n\n{report}",
                    note.child_name, note.child_conversation_id.0
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Starts a run of `params`. One the user started also delivers the
    /// conversation's waiting notes, ahead of their message, and resets its
    /// children's wake-up counts.
    pub(super) async fn start_run(
        self: &Arc<Self>,
        params: ConversationStartRunParams,
        chat: ChatService,
        working_root: PathBuf,
        project_id: Option<ottam_protocol::ProjectID>,
        from_user: bool,
    ) -> Result<ottam_protocol::ConversationStartRunResult, CoreError> {
        if !from_user {
            return self
                .start_conversation_run_with(params, chat, working_root, project_id, None, false)
                .await;
        }
        let conversation = params.conversation_id.clone();
        let workspace_id = params.workspace_id.clone();
        self.user_wrote(&conversation);
        // Held across the start, so a delivery waits for this run rather
        // than carrying the same notes in a run of its own.
        let _delivering =
            super::lock_conversation(&self.delegation.delivering, &conversation).await;
        let notes = chat.waiting_notes(&conversation).await.unwrap_or_default();
        let prefix = (!notes.is_empty()).then(|| {
            format!(
                "[Ottam] {}, since your last turn:\n\n{}\n\n\
                 [End of those. The user's message follows.]\n\n",
                Self::notes_heading(&notes),
                Self::reports(&notes)
            )
        });
        // A child's turn that carries its parent's message answers the
        // parent, whoever typed.
        let user_driven = !notes
            .iter()
            .any(|waiting| waiting.note.kind == DelegationNoteKind::ParentMessage);
        let started = self
            .start_conversation_run_with(
                params,
                chat.clone(),
                working_root,
                project_id,
                prefix,
                user_driven,
            )
            .await?;
        // Only a run that started carries them; a failed start leaves them
        // waiting.
        if !notes.is_empty() {
            if let Err(error) = chat.mark_notes_delivered(&notes).await {
                tracing::warn!(kind = error.kind, "waiting notes not marked delivered");
            }
            self.notify_if_deferred(&workspace_id, &notes);
        }
        Ok(started)
    }

    /// Records how a prompted run of `conversation` was started, as it is
    /// created and before it can end.
    pub(in crate::server::agents) fn record_prompted_run(
        &self,
        conversation: &ConversationID,
        run: &ConversationRunID,
        user_driven: bool,
    ) {
        let mut prompted = self.delegation.prompted_runs();
        let runs = prompted.entry(conversation.clone()).or_default();
        runs.push((run.clone(), user_driven));
        if runs.len() > PROMPTED_RUNS_KEPT {
            runs.remove(0);
        }
    }

    /// `conversation` was deleted: how its runs started no longer matters.
    pub(in crate::server::agents) fn forget_prompted_runs(&self, conversation: &ConversationID) {
        self.delegation.prompted_runs().remove(conversation);
    }

    /// Whether the user drove `run`: a prompted run as it was recorded, an
    /// autonomous one as the prompted run before it. Run ids order by time.
    pub(in crate::server::agents) fn user_drove(&self, conversation: &ConversationID, run: &ConversationRunSummary) -> bool {
        let prompted = self.delegation.prompted_runs();
        let Some(runs) = prompted.get(conversation) else {
            return false;
        };
        let started = match run.origin {
            RunOrigin::Autonomous => runs.iter().rev().find(|(id, _)| id.0 < run.run_id.0),
            RunOrigin::User => runs.iter().find(|(id, _)| *id == run.run_id),
        };
        started.is_some_and(|(_, user_driven)| *user_driven)
    }

    /// Deferred notes changed, so their children's Send to Parent changes.
    fn notify_if_deferred(&self, workspace_id: &WorkspaceID, notes: &[WaitingNote]) {
        if notes.iter().any(|waiting| waiting.note.deferred) {
            self.notify_list_changed(workspace_id);
        }
    }

    /// The user wrote in `conversation`: its children's wake-up counts
    /// start again.
    pub(super) fn user_wrote(&self, conversation: &ConversationID) {
        self.delegation.wake_ups().remove(conversation);
    }

    /// Runs after any run leaves `active_runs`: a child's run leaves a note
    /// in its parent, and a parent that is now idle gets its notes.
    pub(super) fn after_run(&self, context: &ConversationRunContext) {
        if self.delegation.host.get().is_none() {
            return;
        }
        let Some(coordinator) = self.this.upgrade() else {
            return;
        };
        let (workspace_id, conversation_id, run_id) = (
            context.workspace_id.clone(),
            context.conversation_id.clone(),
            context.run_id.clone(),
        );
        let mut run_ends = self
            .delegation
            .run_ends
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while run_ends.try_join_next().is_some() {}
        run_ends.spawn(async move {
            coordinator
                .handle_run_end(workspace_id, conversation_id, run_id)
                .await;
        });
    }

    /// At shutdown, after every run has stopped: lets the run ends write
    /// their notes, for at most `RUN_END_GRACE`.
    pub(super) async fn finish_run_ends(&self) {
        let mut run_ends = std::mem::take(
            &mut *self
                .delegation
                .run_ends
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        let finished = tokio::time::timeout(RUN_END_GRACE, async {
            while run_ends.join_next().await.is_some() {}
        })
        .await;
        if finished.is_err() {
            tracing::warn!("delegation notes were still being written at shutdown");
            run_ends.abort_all();
        }
    }

    async fn handle_run_end(
        self: Arc<Self>,
        workspace_id: WorkspaceID,
        conversation: ConversationID,
        run_id: ottam_protocol::ConversationRunID,
    ) {
        let Some(chat) = self.chat(&workspace_id) else {
            return;
        };
        let Ok(run) = chat.conversation_run(&conversation, &run_id).await else {
            return;
        };
        let status = run.status.clone();
        if let Ok(Some((parent, name))) = chat.delegation_of(&conversation).await {
            let report = chat
                .final_report(&conversation, &run_id)
                .await
                .unwrap_or_default();
            let state = note_state(&status);
            let stopped_with_parent = self.delegation.stopped_with_parent().remove(&run_id);
            let stopped_by_parent_agent = self.delegation.stopped_by_parent_agent().remove(&run_id);
            let stop_cause = (state == DelegationNoteState::Stopped).then(|| {
                if self.quitting() {
                    DelegationStopCause::Quit
                } else if stopped_with_parent {
                    DelegationStopCause::ParentStopped
                } else if stopped_by_parent_agent {
                    DelegationStopCause::ParentAgent
                } else {
                    DelegationStopCause::User
                }
            });
            // The user drove this turn and saw it end; the parent hears of
            // it on its next run. Waking no one, it counts against no cap.
            let user_drove = self.user_drove(&conversation, &run);
            let capped = !user_drove && self.wake_ups_capped(&parent, &conversation);
            let waits_for_user =
                stopped_with_parent || capped || stop_cause == Some(DelegationStopCause::Quit);
            // The parent's agent stopped it, so it needs no telling: the
            // note is only a record.
            let record_only = stop_cause == Some(DelegationStopCause::ParentAgent);
            let deferred = user_drove && !waits_for_user && !record_only;
            let note = DelegationNote {
                child_conversation_id: conversation.clone(),
                child_name: name,
                kind: DelegationNoteKind::Report,
                state,
                excerpt: if report.trim().is_empty() {
                    match state {
                        DelegationNoteState::Completed => "Finished with no final message.",
                        DelegationNoteState::Failed => "The turn failed.",
                        DelegationNoteState::Stopped => "The turn was stopped.",
                    }
                    .to_owned()
                } else {
                    excerpt(&report, NOTE_EXCERPT_CHARS)
                },
                waits_for_user,
                deferred,
                run_id: Some(run_id.clone()),
                stop_cause,
            };
            let body = excerpt(&report, REPORT_PROMPT_CHARS);
            let posted = chat.post_note(&parent, &note, body, record_only).await;
            // The child's card shows how its turn ended, and a deferred
            // report's child offers Send to Parent.
            self.notify_list_changed(&workspace_id);
            match posted {
                Ok(()) => self.deliver_if_idle(&chat, &parent).await,
                Err(error) => tracing::warn!(kind = error.kind, "a child's note was not written"),
            }
        }
        // A parent whose own turn just ended gets what waited meanwhile,
        // unless the user stopped that turn.
        if status != ConversationRunStatus::Canceled {
            self.deliver_if_idle(&chat, &conversation).await;
        }
    }

    /// Delivers the conversation's waiting notes in a follow-up, if it is
    /// idle; see [`Self::try_deliver`].
    async fn deliver_if_idle(self: &Arc<Self>, chat: &ChatService, parent: &ConversationID) {
        if let Err(error) = self.try_deliver(chat, parent).await {
            tracing::warn!(kind = error.kind, "waiting notes were not delivered");
        }
    }

    /// Delivers the conversation's waiting notes in a follow-up, if it is
    /// idle and none of its children is working; a child's message skips
    /// the wait for the other children. Returns the run that carries them,
    /// or `None` when they keep waiting.
    async fn try_deliver(
        self: &Arc<Self>,
        chat: &ChatService,
        parent: &ConversationID,
    ) -> Result<Option<ConversationRunSummary>, CoreError> {
        let _delivering = super::lock_conversation(&self.delegation.delivering, parent).await;
        // Quitting stops every run; a follow-up started now would only
        // fail and spend the notes it carries.
        if self.quitting() || self.is_working(parent).await {
            return Ok(None);
        }
        let notes = Self::wake_notes(chat, parent).await?;
        // Deferred reports ride along but wake no one.
        if notes.iter().all(|waiting| waiting.note.deferred) {
            return Ok(None);
        }
        let urgent = notes
            .iter()
            .any(|waiting| waiting.note.kind == DelegationNoteKind::ChildMessage);
        if !urgent {
            for child in &chat.children(parent).await? {
                if self.is_working(&child.conversation_id).await {
                    return Ok(None);
                }
            }
        }
        self.deliver(chat, parent, notes, true).await.map(Some)
    }

    /// The waiting notes a wake carries: all but those held for the user.
    async fn wake_notes(
        chat: &ChatService,
        parent: &ConversationID,
    ) -> Result<Vec<WaitingNote>, CoreError> {
        Ok(chat
            .waiting_notes(parent)
            .await?
            .into_iter()
            .filter(|waiting| !waiting.note.waits_for_user)
            .collect())
    }

    /// Posts the prompt that carries `notes` and starts the parent's run
    /// with it, the way its latest run was started.
    async fn deliver(
        self: &Arc<Self>,
        chat: &ChatService,
        parent: &ConversationID,
        notes: Vec<WaitingNote>,
        automatic: bool,
    ) -> Result<ConversationRunSummary, CoreError> {
        let detail = chat.conversation_detail(parent).await?;
        let latest = detail
            .latest_run_page
            .runs
            .first()
            .cloned()
            .ok_or_else(|| {
                CoreError::new("noRunToContinue", "the conversation has never run an agent")
            })?;
        let (root, project_id) = Self::run_root(chat, parent).await?;
        // A child must answer its parent; a parent may have nothing to say.
        let for_parent = notes
            .iter()
            .any(|waiting| waiting.note.kind != DelegationNoteKind::ParentMessage);
        let ack = if for_parent {
            format!(
                "\n\nIf none of this changes your work or needs the user, end your turn \
                 with only this line: {DELEGATION_ACK_DIRECTIVE}"
            )
        } else {
            String::new()
        };
        let body = format!(
            "[Ottam] {}:\n\n{}{}{ack}",
            Self::notes_heading(&notes),
            Self::reports(&notes),
            self.status_block(chat, parent).await
        );
        let input = chat.post_note_delivery(parent, body).await?;
        let started = self
            .start_run(
                ConversationStartRunParams {
                    workspace_id: detail.workspace.workspace_id.clone(),
                    conversation_id: parent.clone(),
                    engine_id: latest.engine_id.clone(),
                    model_id: latest.model_id.clone(),
                    mode_id: latest.mode_id.clone(),
                    thought_level_id: latest.thought_level_id.clone(),
                    run_input_id: input.run_input_id,
                },
                chat.clone(),
                root,
                project_id,
                false,
            )
            .await?;
        // Only once a run carries them: a failed start leaves them waiting
        // for the next delivery, a user message, or Continue.
        if let Err(error) = chat.mark_notes_delivered(&notes).await {
            tracing::warn!(kind = error.kind, "delivered notes not marked delivered");
        }
        self.notify_if_deferred(&detail.workspace.workspace_id, &notes);
        if automatic {
            let mut wake_ups = self.delegation.wake_ups();
            let counts = wake_ups.entry(parent.clone()).or_default();
            // One follow-up is one wake-up for each child in it, however
            // many of its notes it carries. Deferred reports woke no one.
            let children: std::collections::HashSet<_> = notes
                .iter()
                .filter(|waiting| !waiting.note.deferred)
                .map(|waiting| &waiting.note.child_conversation_id)
                .collect();
            for child in children {
                *counts.entry(child.clone()).or_default() += 1;
            }
        }
        Ok(started.run)
    }

    /// Continue on a note card: deliver every waiting note now, the held
    /// ones too.
    pub(in crate::server::agents) async fn continue_delegation(
        self: &Arc<Self>,
        chat: &ChatService,
        conversation: &ConversationID,
    ) -> Result<ConversationContinueDelegationResult, CoreError> {
        let _delivering = super::lock_conversation(&self.delegation.delivering, conversation).await;
        if self.is_working(conversation).await {
            return Err(CoreError::new(
                "conversationBusy",
                "the conversation is running; its notes arrive when the turn ends",
            )
            .recoverable(true));
        }
        self.user_wrote(conversation);
        let notes = chat.waiting_notes(conversation).await?;
        if notes.is_empty() {
            return Ok(ConversationContinueDelegationResult { run: None });
        }
        let run = self.deliver(chat, conversation, notes, false).await?;
        Ok(ConversationContinueDelegationResult { run: Some(run) })
    }

    /// Send to Parent in a child: its deferred reports stop being deferred
    /// and wake the parent now, with the other notes a wake would carry;
    /// notes held for the user stay for Continue. A busy parent refuses, as
    /// Continue does, and the reports stay deferred.
    pub(in crate::server::agents) async fn send_to_parent(
        self: &Arc<Self>,
        chat: &ChatService,
        workspace_id: &WorkspaceID,
        child: &ConversationID,
    ) -> Result<ConversationSendToParentResult, CoreError> {
        let (parent, _) = chat.delegation_of(child).await?.ok_or_else(|| {
            CoreError::new("notDelegated", "the conversation has no parent to send to")
        })?;
        let _delivering = super::lock_conversation(&self.delegation.delivering, &parent).await;
        if self.quitting() || self.is_working(&parent).await {
            return Err(CoreError::new(
                "conversationBusy",
                "the parent is working; send this report when its turn ends",
            )
            .recoverable(true));
        }
        let deferred: Vec<WaitingNote> = chat
            .waiting_notes(&parent)
            .await?
            .into_iter()
            .filter(|waiting| waiting.note.deferred && waiting.note.child_conversation_id == *child)
            .collect();
        if deferred.is_empty() {
            return Ok(ConversationSendToParentResult { run: None });
        }
        chat.undefer_notes(&deferred).await?;
        // The user asked, so this child's wake-ups start again.
        if let Some(counts) = self.delegation.wake_ups().get_mut(&parent) {
            counts.remove(child);
        }
        self.notify_list_changed(workspace_id);
        let notes = Self::wake_notes(chat, &parent).await?;
        let run = self.deliver(chat, &parent, notes, false).await?;
        Ok(ConversationSendToParentResult { run: Some(run) })
    }

    /// Deletes `conversation` and, as `children` says, its delegated
    /// children too. Kept children stop working and, with their parent
    /// gone, are top-level conversations.
    pub(in crate::server::agents) async fn delete_with_children(
        &self,
        chat: &ChatService,
        workspace_id: &WorkspaceID,
        conversation: &ConversationID,
        children: ConversationDeleteChildren,
    ) -> Result<ConversationDeleteResult, CoreError> {
        let delegated = chat.children(conversation).await?;
        let mut result = self.delete_conversation(chat, conversation).await?;
        for child in &delegated {
            match children {
                ConversationDeleteChildren::Delete => {
                    match self.delete_conversation(chat, &child.conversation_id).await {
                        Ok(deleted) => result
                            .harness_session_cleanup
                            .extend(deleted.harness_session_cleanup),
                        Err(error) => {
                            tracing::warn!(kind = error.kind, "a delegated child was not deleted");
                        }
                    }
                }
                ConversationDeleteChildren::Keep => {
                    let run_id = self.active_run_id(&child.conversation_id).await;
                    let Some(run_id) = run_id else { continue };
                    if let Err(error) = self
                        .cancel_conversation_run(chat, &child.conversation_id, &run_id)
                        .await
                    {
                        tracing::warn!(kind = error.kind, "a kept child was not stopped");
                    }
                }
            }
        }
        if !delegated.is_empty() {
            self.notify_list_changed(workspace_id);
        }
        Ok(result)
    }

    /// The user stopped `parent`'s turn: stop its working children too.
    /// Their notes wait for the user.
    pub(in crate::server::agents) async fn stop_children(
        &self,
        chat: &ChatService,
        parent: &ConversationID,
    ) {
        let Ok(children) = chat.children(parent).await else {
            return;
        };
        for child in children {
            let run_id = self.active_run_id(&child.conversation_id).await;
            let Some(run_id) = run_id else {
                continue;
            };
            // By run, so a run that ends on its own meanwhile marks no
            // later one.
            self.delegation.stopped_with_parent().insert(run_id.clone());
            if let Err(error) = self
                .cancel_conversation_run(chat, &child.conversation_id, &run_id)
                .await
            {
                self.delegation.stopped_with_parent().remove(&run_id);
                tracing::warn!(kind = error.kind, "a delegated child was not stopped");
            }
        }
    }
}

/// The `ottam` path's tools, answered by the coordinator.
struct DelegationTools {
    coordinator: Weak<ConversationRunCoordinator>,
}

impl OttamTools for DelegationTools {
    fn is_child(&self, caller: &McpCaller) -> BoxFuture<'_, bool> {
        let caller = caller.clone();
        Box::pin(async move {
            match self.coordinator.upgrade() {
                Some(coordinator) => coordinator.is_child(&caller).await,
                None => false,
            }
        })
    }

    fn delegation_targets(&self) -> BoxFuture<'_, DelegationTargets> {
        Box::pin(async move {
            let Some(coordinator) = self.coordinator.upgrade() else {
                return DelegationTargets::default();
            };
            DelegationTargets {
                named: coordinator
                    .named_agents()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|named| named.name)
                    .collect(),
                installed: coordinator
                    .installed_agents()
                    .iter()
                    .map(|definition| {
                        (
                            definition.agent_id.0.clone(),
                            crate::server::engine_display_name(definition),
                        )
                    })
                    .collect(),
            }
        })
    }

    fn delegate(
        &self,
        caller: McpCaller,
        request: DelegateRequest,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            let coordinator = self
                .coordinator
                .upgrade()
                .ok_or("Ottam is shutting down")?;
            coordinator.delegate(caller, request).await
        })
    }

    fn list_children(&self, caller: McpCaller) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move { self.coordinator()?.list_children(caller).await })
    }

    fn conversation_info(&self, caller: McpCaller) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move { self.coordinator()?.conversation_info(caller).await })
    }

    fn message_child(
        &self,
        caller: McpCaller,
        child: String,
        message: String,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.coordinator()?
                .message_child(caller, child, message)
                .await
        })
    }

    fn stop_child(
        &self,
        caller: McpCaller,
        child: String,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move { self.coordinator()?.stop_child(caller, child).await })
    }

    fn message_parent(
        &self,
        caller: McpCaller,
        message: String,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move { self.coordinator()?.message_parent(caller, message).await })
    }

    fn session_messages(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        before: Option<u64>,
        limit: Option<u64>,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.coordinator()?
                .session_messages(caller, conversation, before, limit)
                .await
        })
    }

    fn toolcall_output(
        &self,
        caller: McpCaller,
        conversation: Option<String>,
        tool_call_id: String,
        offset: Option<u64>,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.coordinator()?
                .toolcall_output(caller, conversation, tool_call_id, offset)
                .await
        })
    }
}

impl DelegationTools {
    fn coordinator(&self) -> Result<Arc<ConversationRunCoordinator>, String> {
        self.coordinator
            .upgrade()
            .ok_or_else(|| "Ottam is shutting down".to_owned())
    }
}
