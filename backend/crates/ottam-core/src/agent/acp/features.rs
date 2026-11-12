//! The ACP surface as three layers: what the pinned SDK defines, what the
//! agent advertised, and what Ottam's client implements.
//!
//! Rows were swept from `agent-client-protocol-schema` 1.4 (`src/v1`) and
//! re-swept at 1.9 (`agent-client-protocol` 2.2): the method-name constants,
//! the `SessionUpdate` variants, the agent and client request/notification
//! enums, the capability structs, and the vendor `_meta` namespaces
//! `crate::agent::metadata` already knows. Re-run that sweep when the
//! `agent-client-protocol` pin moves. `initialize` itself is not a row:
//! there is no ACP client without it.
//!
//! Adapter releases can outrun the schema pin. When they do, the surface
//! gets a row anyway, so the matrix says why core cannot opt in yet rather
//! than omitting it.
//!
//! [`CLIENT_FEATURES`] is the single source of truth for what
//! `initialize_request()` advertises: the request is built from it, so a
//! later spec flips a row here and the wire follows. Nothing infers support
//! from which code paths happen to consume an update.

use agent_client_protocol::schema::v1::{AuthMethod, InitializeResponse};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One ACP surface: a capability flag, a method, an update kind, or an
/// extension. The wire name doubles as the catalog key, so it is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AcpFeature {
    // Agent capabilities: flags in `initialize.agentCapabilities`.
    #[serde(rename = "loadSession")]
    LoadSession,
    #[serde(rename = "prompt.image")]
    PromptImage,
    #[serde(rename = "prompt.audio")]
    PromptAudio,
    #[serde(rename = "prompt.embeddedContext")]
    PromptEmbeddedContext,
    #[serde(rename = "mcp.http")]
    McpHttp,
    #[serde(rename = "mcp.sse")]
    McpSse,
    /// Client-hosted MCP over ACP (`mcp/connect`, `mcp/disconnect`,
    /// `mcp/message`, `tools/list`).
    #[serde(rename = "mcp.acp")]
    McpAcp,
    #[serde(rename = "session.list")]
    SessionList,
    #[serde(rename = "session.delete")]
    SessionDelete,
    #[serde(rename = "session.fork")]
    SessionFork,
    #[serde(rename = "session.resume")]
    SessionResume,
    #[serde(rename = "session.close")]
    SessionClose,
    #[serde(rename = "session.additionalDirectories")]
    SessionAdditionalDirectories,
    #[serde(rename = "auth.logout")]
    AuthLogout,
    /// `providers/list`, `providers/set`, `providers/disable`.
    #[serde(rename = "providers")]
    Providers,

    // Agent methods the client calls that have no capability flag.
    #[serde(rename = "authenticate")]
    Authenticate,
    #[serde(rename = "session/new")]
    SessionNew,
    #[serde(rename = "session/prompt")]
    SessionPrompt,
    #[serde(rename = "session/cancel")]
    SessionCancel,
    #[serde(rename = "session/set_mode")]
    SessionSetMode,
    #[serde(rename = "session/set_config_option")]
    SessionSetConfigOption,
    /// Next-edit suggestions (`nes/start`, `nes/suggest`, `nes/accept`, …).
    #[serde(rename = "nes/*")]
    Nes,
    /// `document/didOpen`, `didChange`, `didClose`, `didSave`, `didFocus`.
    #[serde(rename = "document/*")]
    DocumentSync,

    // Session update kinds the agent sends.
    #[serde(rename = "agent_message_chunk")]
    AgentMessageChunk,
    #[serde(rename = "agent_thought_chunk")]
    AgentThoughtChunk,
    #[serde(rename = "user_message_chunk")]
    UserMessageChunk,
    #[serde(rename = "tool_call")]
    ToolCall,
    #[serde(rename = "tool_call_update")]
    ToolCallUpdate,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "usage_update")]
    UsageUpdate,
    #[serde(rename = "config_option_update")]
    ConfigOptionUpdate,
    #[serde(rename = "current_mode_update")]
    CurrentModeUpdate,
    #[serde(rename = "available_commands_update")]
    AvailableCommandsUpdate,
    #[serde(rename = "session_info_update")]
    SessionInfoUpdate,
    /// Token usage on the `session/prompt` response at end of turn.
    #[serde(rename = "prompt.usage")]
    PromptUsage,
    /// The programmatic tool name as a top-level `name` on the first
    /// `tool_call` (ACP tool-call-name RFD). Both adapters send it
    /// unconditionally.
    #[serde(rename = "tool_call.name")]
    ToolCallName,
    /// A subagent session's announcement and state, on its parent's session
    /// (ACP Subagent Sessions RFD, schema 1.10).
    #[serde(rename = "subagent_update")]
    SubagentUpdate,
    /// A message one session sends another, such as a parent's assignment
    /// to its subagent (same RFD).
    #[serde(rename = "session_message")]
    SessionMessage,

    // Client requests the agent makes.
    #[serde(rename = "session/request_permission")]
    RequestPermission,
    #[serde(rename = "fs.readTextFile")]
    FsReadTextFile,
    #[serde(rename = "fs.writeTextFile")]
    FsWriteTextFile,
    /// The whole `terminal/*` family; ACP has one flag for all five methods.
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "elicitation.form")]
    ElicitationForm,
    #[serde(rename = "elicitation.url")]
    ElicitationUrl,

    // Client capability flags with no request of their own.
    #[serde(rename = "session.configOptions")]
    SessionConfigOptions,
    /// `plan_update` / `plan_removed` session updates.
    #[serde(rename = "plan.operations")]
    PlanOperations,
    /// `compaction_update` / `compaction_summary_chunk` session updates,
    /// sent only when the client advertises `session.compaction`. Without
    /// it both adapters keep their text-chunk fallback.
    #[serde(rename = "session.compaction")]
    SessionCompaction,
    /// `notice` session updates (severity, title, description), sent only
    /// when the client advertises `session.notices`. Without it Claude
    /// folds notices into bold agent text and Codex uses text or AIR
    /// `sessionFailure` records.
    #[serde(rename = "session.notices")]
    SessionNotices,
    /// Subagent sessions: `subagent_update` announcements, each child's own
    /// stream, and `session_message` between sessions. Adapters send these
    /// only to a client that advertises `subagents`.
    #[serde(rename = "subagents")]
    Subagents,

    // Extensions outside the schema: adapter `_meta` conventions and AIR.
    /// Terminal auth methods, plus the `terminal-auth` `_meta` flag that
    /// makes `claude-agent-acp` attach an explicit login command.
    #[serde(rename = "auth.terminal")]
    AuthTerminal,
    /// `_meta.terminal_output: true` on `initialize`, read by both adapters
    /// to stream tool-terminal output on tool-call updates.
    #[serde(rename = "terminal_output")]
    TerminalOutputMeta,
    #[serde(rename = "_auth/status_update")]
    AuthStatusUpdate,
    #[serde(rename = "_session/steering")]
    SessionSteering,
    #[serde(rename = "jetbrains.air.sessionFailure")]
    AirSessionFailure,
    #[serde(rename = "jetbrains.air.asyncTasks")]
    AirAsyncTasks,
    #[serde(rename = "jetbrains.air.nativeSubagentSessions")]
    AirNativeSubagentSessions,
    /// The changed-file list on request from `session/prompt`.
    #[serde(rename = "jetbrains.air.agentFileChangeReport")]
    AirAgentFileChangeReport,
    /// An exact Git patch in a `diff` block instead of file text.
    #[serde(rename = "jetbrains.air.diffPatch")]
    AirDiffPatch,
    /// No display copy of readable input (prompts, plans) in `content`; the
    /// client renders `rawInput` itself.
    #[serde(rename = "jetbrains.air.rawInputRendering")]
    AirRawInputRendering,
    /// Concrete model and effort recommendations in config options.
    #[serde(rename = "jetbrains.air.recommendedValue")]
    AirRecommendedValue,
    /// Claude's plan as a file path instead of text.
    #[serde(rename = "jetbrains.air.planFile")]
    AirPlanFile,
    /// Codex's plan stream as appended text instead of snapshots.
    #[serde(rename = "jetbrains.air.planContentDelta")]
    AirPlanContentDelta,
    /// Codex's message phase (`commentary` / `final_answer`) on message
    /// chunks, formerly `_meta.codex.phase`.
    #[serde(rename = "jetbrains.air.phase")]
    AirPhase,
    /// `subagent: true` on the tool call that runs a subagent while native
    /// subagent sessions are off.
    #[serde(rename = "jetbrains.air.subagent")]
    AirSubagent,
    #[serde(rename = "_meta.codex.threadStatus")]
    CodexThreadStatus,
    #[serde(rename = "_meta.codex.error")]
    CodexError,
    #[serde(rename = "_meta.claudeCode.parentToolUseId")]
    ClaudeParentToolUseId,
    #[serde(rename = "_meta.claudeCode.toolName")]
    ClaudeToolName,
    #[serde(rename = "_meta._claude/rateLimit")]
    ClaudeRateLimit,
    /// `_meta.terminal_info` / `terminal_output` / `terminal_exit` on tool
    /// call updates.
    #[serde(rename = "_meta.terminal_*")]
    TerminalMeta,
}

/// Which side of the protocol a feature lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpFeatureGroup {
    /// A flag in `initialize.agentCapabilities`.
    AgentCapability,
    /// A method the client calls on the agent.
    AgentMethod,
    /// A `session/update` kind or notification the agent sends.
    UpdateKind,
    /// A request the agent makes of the client.
    ClientRequest,
    /// A flag in `initialize.clientCapabilities` with no request behind it.
    ClientCapability,
    /// Outside the schema: AIR, adapter `_meta` conventions, vendor namespaces.
    Extension,
}

/// The SDK layer: whether the pinned `agent-client-protocol` defines the
/// surface, and if so whether core compiles it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkStatus {
    /// Part of ACP v1 as the SDK compiles it by default.
    Stable,
    /// Behind a cargo feature that `agent-client-protocol` forwards to the
    /// schema crate. `enabled` is whether core turns it on in `Cargo.toml`;
    /// a `Supported` row must.
    Unstable {
        feature: &'static str,
        enabled: bool,
    },
    /// Behind a cargo feature of `agent-client-protocol-schema` that the
    /// pinned `agent-client-protocol` does not forward (or that only a newer
    /// schema than the pinned one defines), so core cannot enable it without
    /// a direct schema dependency or an SDK bump.
    SchemaOnly { feature: &'static str },
    /// Not in the SDK: a convention of AIR or of specific adapters. `vendor`
    /// is the label; `adapters` names the adapter packages (by npm slug,
    /// e.g. `claude-agent-acp`) the row applies to, and is empty when any
    /// adapter may use it. Nothing on the wire says which `_meta`
    /// namespaces an agent speaks, so this list is the roster research.
    Extension {
        vendor: &'static str,
        adapters: &'static [&'static str],
    },
}

/// The client layer: what Ottam does with the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSupport {
    Supported,
    /// Handled in part; the label says which part.
    Partial(&'static str),
    Unsupported,
}

impl AcpFeature {
    pub const ALL: &'static [AcpFeature] = &[
        Self::LoadSession,
        Self::PromptImage,
        Self::PromptAudio,
        Self::PromptEmbeddedContext,
        Self::McpHttp,
        Self::McpSse,
        Self::McpAcp,
        Self::SessionList,
        Self::SessionDelete,
        Self::SessionFork,
        Self::SessionResume,
        Self::SessionClose,
        Self::SessionAdditionalDirectories,
        Self::AuthLogout,
        Self::Providers,
        Self::Authenticate,
        Self::SessionNew,
        Self::SessionPrompt,
        Self::SessionCancel,
        Self::SessionSetMode,
        Self::SessionSetConfigOption,
        Self::Nes,
        Self::DocumentSync,
        Self::AgentMessageChunk,
        Self::AgentThoughtChunk,
        Self::UserMessageChunk,
        Self::ToolCall,
        Self::ToolCallUpdate,
        Self::Plan,
        Self::UsageUpdate,
        Self::ConfigOptionUpdate,
        Self::CurrentModeUpdate,
        Self::AvailableCommandsUpdate,
        Self::SessionInfoUpdate,
        Self::PromptUsage,
        Self::ToolCallName,
        Self::SubagentUpdate,
        Self::SessionMessage,
        Self::RequestPermission,
        Self::FsReadTextFile,
        Self::FsWriteTextFile,
        Self::Terminal,
        Self::ElicitationForm,
        Self::ElicitationUrl,
        Self::SessionConfigOptions,
        Self::PlanOperations,
        Self::SessionCompaction,
        Self::SessionNotices,
        Self::Subagents,
        Self::AuthTerminal,
        Self::TerminalOutputMeta,
        Self::AuthStatusUpdate,
        Self::SessionSteering,
        Self::AirSessionFailure,
        Self::AirAsyncTasks,
        Self::AirNativeSubagentSessions,
        Self::AirAgentFileChangeReport,
        Self::AirDiffPatch,
        Self::AirRawInputRendering,
        Self::AirRecommendedValue,
        Self::AirPlanFile,
        Self::AirPlanContentDelta,
        Self::AirPhase,
        Self::AirSubagent,
        Self::CodexThreadStatus,
        Self::CodexError,
        Self::ClaudeParentToolUseId,
        Self::ClaudeToolName,
        Self::ClaudeRateLimit,
        Self::TerminalMeta,
    ];

    pub fn group(self) -> AcpFeatureGroup {
        use AcpFeatureGroup::*;
        match self {
            Self::LoadSession
            | Self::PromptImage
            | Self::PromptAudio
            | Self::PromptEmbeddedContext
            | Self::McpHttp
            | Self::McpSse
            | Self::McpAcp
            | Self::SessionList
            | Self::SessionDelete
            | Self::SessionFork
            | Self::SessionResume
            | Self::SessionClose
            | Self::SessionAdditionalDirectories
            | Self::AuthLogout
            | Self::Providers => AgentCapability,
            Self::Authenticate
            | Self::SessionNew
            | Self::SessionPrompt
            | Self::SessionCancel
            | Self::SessionSetMode
            | Self::SessionSetConfigOption
            | Self::Nes
            | Self::DocumentSync => AgentMethod,
            Self::AgentMessageChunk
            | Self::AgentThoughtChunk
            | Self::UserMessageChunk
            | Self::ToolCall
            | Self::ToolCallUpdate
            | Self::Plan
            | Self::UsageUpdate
            | Self::ConfigOptionUpdate
            | Self::CurrentModeUpdate
            | Self::AvailableCommandsUpdate
            | Self::SessionInfoUpdate
            | Self::PromptUsage
            | Self::ToolCallName
            | Self::SubagentUpdate
            | Self::SessionMessage => UpdateKind,
            Self::RequestPermission
            | Self::FsReadTextFile
            | Self::FsWriteTextFile
            | Self::Terminal
            | Self::ElicitationForm
            | Self::ElicitationUrl => ClientRequest,
            Self::SessionConfigOptions
            | Self::PlanOperations
            | Self::SessionCompaction
            | Self::SessionNotices
            | Self::Subagents => ClientCapability,
            Self::AuthTerminal
            | Self::TerminalOutputMeta
            | Self::AuthStatusUpdate
            | Self::SessionSteering
            | Self::AirSessionFailure
            | Self::AirAsyncTasks
            | Self::AirNativeSubagentSessions
            | Self::AirAgentFileChangeReport
            | Self::AirDiffPatch
            | Self::AirRawInputRendering
            | Self::AirRecommendedValue
            | Self::AirPlanFile
            | Self::AirPlanContentDelta
            | Self::AirPhase
            | Self::AirSubagent
            | Self::CodexThreadStatus
            | Self::CodexError
            | Self::ClaudeParentToolUseId
            | Self::ClaudeToolName
            | Self::ClaudeRateLimit
            | Self::TerminalMeta => Extension,
        }
    }

    /// The SDK layer. Feature names are cargo features of the schema crate;
    /// `enabled` mirrors `agent-client-protocol` in `ottam-core/Cargo.toml`.
    /// `agent-client-protocol` 2.2 (schema 1.9) forwards every unstable
    /// feature except `nes`. Terminal auth, elicitation, and tool-call names
    /// are stable there; `session.resume` and `session.close` were already
    /// stable, and only `fork` is gated.
    pub fn sdk_status(self) -> SdkStatus {
        use SdkStatus::*;
        match self {
            Self::McpAcp => Unstable {
                feature: "unstable_mcp_over_acp",
                enabled: false,
            },
            Self::SessionFork => Unstable {
                feature: "unstable_session_fork",
                enabled: true,
            },
            Self::PromptUsage => Unstable {
                feature: "unstable_end_turn_token_usage",
                enabled: true,
            },
            Self::Providers => Unstable {
                feature: "unstable_llm_providers",
                enabled: false,
            },
            Self::Nes | Self::DocumentSync => SchemaOnly {
                feature: "unstable_nes",
            },
            Self::PlanOperations => Unstable {
                feature: "unstable_plan_operations",
                enabled: false,
            },
            Self::SessionCompaction => Unstable {
                feature: "unstable_session_compaction",
                enabled: false,
            },
            Self::SessionNotices => Unstable {
                feature: "unstable_session_notices",
                enabled: false,
            },
            // Newer than the pinned schema; core decodes the raw frames
            // (`subagent_sessions`), as it does AIR's.
            Self::Subagents | Self::SubagentUpdate | Self::SessionMessage => SchemaOnly {
                feature: "unstable_subagents",
            },
            Self::TerminalOutputMeta
            | Self::AuthStatusUpdate
            | Self::SessionSteering
            | Self::TerminalMeta => Extension {
                vendor: "adapter",
                adapters: ACP_ADAPTERS,
            },
            Self::AirSessionFailure
            | Self::AirAsyncTasks
            | Self::AirNativeSubagentSessions
            | Self::AirAgentFileChangeReport
            | Self::AirDiffPatch
            | Self::AirRawInputRendering
            | Self::AirRecommendedValue
            | Self::AirSubagent => Extension {
                vendor: "jetbrains.air",
                adapters: &[],
            },
            Self::AirPlanFile => Extension {
                vendor: "jetbrains.air",
                adapters: &["claude-agent-acp"],
            },
            Self::AirPlanContentDelta | Self::AirPhase => Extension {
                vendor: "jetbrains.air",
                adapters: &["codex-acp"],
            },
            Self::CodexThreadStatus | Self::CodexError => Extension {
                vendor: "adapter",
                adapters: &["codex-acp"],
            },
            Self::ClaudeParentToolUseId | Self::ClaudeToolName | Self::ClaudeRateLimit => {
                Extension {
                    vendor: "adapter",
                    adapters: &["claude-agent-acp"],
                }
            }
            _ => Stable,
        }
    }

    /// Whether the row belongs in this agent's matrix. Only adapter-specific
    /// extensions are ever excluded: a `_meta.codex.*` row means nothing to
    /// Claude, and an agent with no adapter package (or none we know) gets
    /// only the shared and AIR rows.
    pub fn applies_to(self, adapter_slug: Option<&str>) -> bool {
        match self.sdk_status() {
            SdkStatus::Extension { adapters, .. } if !adapters.is_empty() => {
                adapter_slug.is_some_and(|slug| adapters.contains(&slug))
            }
            _ => true,
        }
    }

    /// The serde rename: the wire name and the catalog key.
    pub fn wire_name(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(name)) => name,
            _ => unreachable!("AcpFeature serializes as a string"),
        }
    }

    /// The name this feature takes in `_meta.jetbrains.air.capabilities`.
    pub(super) fn air_capability(self) -> Option<&'static str> {
        match self {
            Self::AirSessionFailure => Some("sessionFailure"),
            Self::AirAsyncTasks => Some("asyncTasks"),
            Self::AirNativeSubagentSessions => Some("nativeSubagentSessions"),
            Self::AirAgentFileChangeReport => Some("agentFileChangeReport"),
            Self::AirDiffPatch => Some("diffPatch"),
            Self::AirRawInputRendering => Some("rawInputRendering"),
            Self::AirRecommendedValue => Some("recommendedValue"),
            Self::AirPlanFile => Some("planFile"),
            Self::AirPlanContentDelta => Some("planContentDelta"),
            _ => None,
        }
    }
}

/// What Ottam's client does with each surface. One row per
/// [`AcpFeature`]; `initialize_request()` reads the `Supported` rows.
pub const CLIENT_FEATURES: &[(AcpFeature, ClientSupport)] = {
    use AcpFeature::*;
    use ClientSupport::*;
    const METADATA: &str = "persisted as session metadata; not surfaced";
    &[
        (LoadSession, Supported),
        (PromptImage, Supported),
        (PromptAudio, Unsupported),
        (PromptEmbeddedContext, Unsupported),
        (McpHttp, Supported),
        (McpSse, Unsupported),
        (McpAcp, Unsupported),
        (SessionList, Unsupported),
        (SessionDelete, Supported),
        (SessionFork, Unsupported),
        (SessionResume, Unsupported),
        (SessionClose, Supported),
        (SessionAdditionalDirectories, Unsupported),
        (AuthLogout, Unsupported),
        (Providers, Unsupported),
        (Authenticate, Supported),
        (SessionNew, Supported),
        (SessionPrompt, Supported),
        (SessionCancel, Supported),
        (
            SessionSetMode,
            Partial("mode is set through session/set_config_option"),
        ),
        (SessionSetConfigOption, Supported),
        (Nes, Unsupported),
        (DocumentSync, Unsupported),
        (AgentMessageChunk, Supported),
        (AgentThoughtChunk, Supported),
        (UserMessageChunk, Supported),
        (ToolCall, Supported),
        (ToolCallUpdate, Supported),
        (Plan, Supported),
        (UsageUpdate, Partial("context gauge; cost not surfaced")),
        (ConfigOptionUpdate, Partial(METADATA)),
        (CurrentModeUpdate, Partial(METADATA)),
        (AvailableCommandsUpdate, Partial(METADATA)),
        (SessionInfoUpdate, Partial("title only")),
        (PromptUsage, Supported),
        (ToolCallName, Supported),
        (SubagentUpdate, Supported),
        (
            SessionMessage,
            Partial("a subagent's first incoming message becomes its prompt; others are not shown"),
        ),
        (RequestPermission, Supported),
        (FsReadTextFile, Unsupported),
        (FsWriteTextFile, Unsupported),
        (Terminal, Unsupported),
        (ElicitationForm, Supported),
        (ElicitationUrl, Unsupported),
        (SessionConfigOptions, Unsupported),
        (PlanOperations, Unsupported),
        (SessionCompaction, Unsupported),
        (SessionNotices, Unsupported),
        (Subagents, Supported),
        (AuthTerminal, Supported),
        (TerminalOutputMeta, Supported),
        (AuthStatusUpdate, Supported),
        (SessionSteering, Supported),
        (AirSessionFailure, Supported),
        (AirAsyncTasks, Supported),
        (AirNativeSubagentSessions, Supported),
        (AirAgentFileChangeReport, Unsupported),
        (AirDiffPatch, Unsupported),
        (AirRawInputRendering, Unsupported),
        (AirRecommendedValue, Unsupported),
        (AirPlanFile, Unsupported),
        (AirPlanContentDelta, Unsupported),
        (AirPhase, Supported),
        (
            AirSubagent,
            Partial(
                "legacy Claude control calls group their children; native sessions replace them",
            ),
        ),
        (CodexThreadStatus, Supported),
        (CodexError, Supported),
        (ClaudeParentToolUseId, Supported),
        (ClaudeToolName, Unsupported),
        (ClaudeRateLimit, Supported),
        (TerminalMeta, Supported),
    ]
};

pub fn client_support(feature: AcpFeature) -> ClientSupport {
    CLIENT_FEATURES
        .iter()
        .find(|(candidate, _)| *candidate == feature)
        .map(|(_, support)| *support)
        .unwrap_or(ClientSupport::Unsupported)
}

/// One feature across the three layers, for a given agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureCompatibility {
    pub feature: AcpFeature,
    pub group: AcpFeatureGroup,
    pub sdk: SdkStatus,
    /// `None` when the row is not judgeable from `initialize`, or when the
    /// agent has never been probed.
    pub agent: Option<bool>,
    pub client: ClientSupport,
}

/// The adapter packages whose shared `_meta` conventions core implements.
const ACP_ADAPTERS: &[&str] = &["claude-agent-acp", "codex-acp"];

/// Join the table with what one agent advertised. `None` for `advertised`
/// means the agent was never probed, so every row's agent side is `None`.
/// `adapter_slug` is the agent's adapter package slug and drops the
/// extension rows that belong to other adapters.
pub fn compatibility(
    advertised: Option<&BTreeMap<AcpFeature, bool>>,
    adapter_slug: Option<&str>,
) -> Vec<FeatureCompatibility> {
    AcpFeature::ALL
        .iter()
        .filter(|feature| feature.applies_to(adapter_slug))
        .map(|feature| FeatureCompatibility {
            feature: *feature,
            group: feature.group(),
            sdk: feature.sdk_status(),
            agent: advertised.and_then(|map| map.get(feature).copied()),
            client: client_support(*feature),
        })
        .collect()
}

pub(super) fn client_supports(feature: AcpFeature) -> bool {
    client_support(feature) == ClientSupport::Supported
}

/// The AIR capability names `initialize_request()` advertises.
pub(super) fn air_capabilities() -> Vec<&'static str> {
    AcpFeature::ALL
        .iter()
        .filter(|feature| client_supports(**feature))
        .filter_map(|feature| feature.air_capability())
        .collect()
}

/// The agent layer, read off a decoded `initialize` response. Only surfaces
/// the response can answer get an entry: AIR capabilities come from the
/// agent's `_meta.jetbrains.air.capabilities`, while update kinds, client
/// requests, and vendor `_meta` keys are observed once a session runs and
/// stay absent here.
pub(super) fn advertised_features(
    initialization: &InitializeResponse,
) -> BTreeMap<AcpFeature, bool> {
    let capabilities = &initialization.agent_capabilities;
    let session = &capabilities.session_capabilities;
    let auth_status = capabilities
        .meta
        .as_ref()
        .is_some_and(|meta| meta.contains_key("authStatus"));
    let steering = super::steering::steering_capability(initialization.meta.as_ref())
        == crate::agent::SteeringCapability::Native;
    let terminal_auth = initialization
        .auth_methods
        .iter()
        .any(|method| matches!(method, AuthMethod::Terminal(_)));
    // An agent that declares no AIR block says nothing about AIR rows.
    let air = initialization
        .meta
        .as_ref()
        .and_then(|meta| meta.get("jetbrains"))
        .and_then(|jetbrains| jetbrains.get("air"))
        .filter(|air| air.get("version").and_then(serde_json::Value::as_u64) == Some(1))
        .map(|air| {
            air.get("capabilities")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        });
    let air_rows = air.into_iter().flat_map(|declared| {
        AcpFeature::ALL.iter().filter_map(move |feature| {
            let name = feature.air_capability()?;
            Some((*feature, declared.iter().any(|value| value == name)))
        })
    });
    [
        (AcpFeature::LoadSession, capabilities.load_session),
        (
            AcpFeature::PromptImage,
            capabilities.prompt_capabilities.image,
        ),
        (
            AcpFeature::PromptAudio,
            capabilities.prompt_capabilities.audio,
        ),
        (
            AcpFeature::PromptEmbeddedContext,
            capabilities.prompt_capabilities.embedded_context,
        ),
        (AcpFeature::McpHttp, capabilities.mcp_capabilities.http),
        (AcpFeature::McpSse, capabilities.mcp_capabilities.sse),
        (AcpFeature::SessionList, session.list.is_some()),
        (AcpFeature::SessionDelete, session.delete.is_some()),
        (AcpFeature::SessionFork, session.fork.is_some()),
        (AcpFeature::SessionResume, session.resume.is_some()),
        (AcpFeature::SessionClose, session.close.is_some()),
        (
            AcpFeature::SessionAdditionalDirectories,
            session.additional_directories.is_some(),
        ),
        (AcpFeature::AuthLogout, capabilities.auth.logout.is_some()),
        // `authenticate` is a baseline method; a non-empty `authMethods`
        // list says sign-in is needed, not that the method exists, so it
        // gets no entry.
        (AcpFeature::AuthTerminal, terminal_auth),
        (AcpFeature::AuthStatusUpdate, auth_status),
        (AcpFeature::SessionSteering, steering),
    ]
    .into_iter()
    .chain(air_rows)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn client_features_table_has_one_row_per_feature() {
        for feature in AcpFeature::ALL {
            let rows = CLIENT_FEATURES
                .iter()
                .filter(|(candidate, _)| candidate == feature)
                .count();
            assert_eq!(rows, 1, "{feature:?} needs exactly one row");
        }
        assert_eq!(CLIENT_FEATURES.len(), AcpFeature::ALL.len());
        // `ALL` is hand-maintained; a variant missing from it would also be
        // missing from every table walk.
        let mut names = std::collections::BTreeSet::new();
        for feature in AcpFeature::ALL {
            let name = serde_json::to_string(feature).unwrap();
            assert!(names.insert(name.clone()), "{name} listed twice");
        }
    }

    /// Rows newer than the pinned schema that core still supports, because
    /// it decodes their frames as raw JSON (`subagent_sessions`) instead of
    /// through the SDK's types.
    const RAW_DECODED: &[AcpFeature] = &[
        AcpFeature::Subagents,
        AcpFeature::SubagentUpdate,
        AcpFeature::SessionMessage,
    ];

    /// A `Supported` row whose SDK surface is compiled out would advertise
    /// something the client cannot even decode.
    #[test]
    fn client_features_supported_rows_are_compiled_in() {
        for (feature, support) in CLIENT_FEATURES {
            if *support != ClientSupport::Supported || RAW_DECODED.contains(feature) {
                continue;
            }
            assert!(
                !matches!(
                    feature.sdk_status(),
                    SdkStatus::Unstable { enabled: false, .. } | SdkStatus::SchemaOnly { .. }
                ),
                "{feature:?} is Supported but its SDK feature is off"
            );
        }
    }

    /// The table drives the request; this pins the serialized wire so a row
    /// flip that the builder silently ignores is caught.
    #[test]
    fn client_features_table_matches_initialize_request() {
        let request = serde_json::to_value(super::super::protocol::initialize_request()).unwrap();
        let capabilities = &request["clientCapabilities"];
        let expect = |feature: AcpFeature, pointer: &str| {
            let advertised = capabilities.pointer(pointer) == Some(&Value::Bool(true));
            assert_eq!(
                advertised,
                client_supports(feature),
                "{feature:?} at {pointer} disagrees with CLIENT_FEATURES"
            );
        };
        expect(AcpFeature::AuthTerminal, "/auth/terminal");
        expect(AcpFeature::AuthTerminal, "/_meta/terminal-auth");
        expect(AcpFeature::TerminalOutputMeta, "/_meta/terminal_output");
        expect(AcpFeature::FsReadTextFile, "/fs/readTextFile");
        expect(AcpFeature::FsWriteTextFile, "/fs/writeTextFile");
        expect(AcpFeature::Terminal, "/terminal");
        // `{}` advertises a mode; omitted means unsupported.
        assert_eq!(
            capabilities.pointer("/elicitation/form") == Some(&json!({})),
            client_supports(AcpFeature::ElicitationForm),
            "ElicitationForm disagrees with CLIENT_FEATURES"
        );
        assert_eq!(
            capabilities.pointer("/elicitation/url").is_some(),
            client_supports(AcpFeature::ElicitationUrl),
            "ElicitationUrl disagrees with CLIENT_FEATURES"
        );
        // Newer than the schema, so only the wire request carries it; the
        // rest of that request is the typed one.
        let mut wire = serde_json::to_value(super::super::protocol::initialize_wire()).unwrap();
        let subagents = wire["clientCapabilities"]
            .as_object_mut()
            .unwrap()
            .remove("subagents");
        assert_eq!(
            subagents == Some(json!({})),
            client_supports(AcpFeature::Subagents),
            "Subagents disagrees with CLIENT_FEATURES"
        );
        assert_eq!(wire, request);
        // Unsupported rows with their SDK feature compiled out (plan
        // operations, nes) stay off the wire.
        assert!(capabilities.get("plan").is_none());
        assert!(capabilities.get("nes").is_none());
        // Opt-in update families the adapters gate on these flags: absent,
        // so both keep their pre-RFD text fallbacks.
        assert!(capabilities.pointer("/session/compaction").is_none());
        assert!(capabilities.pointer("/session/notices").is_none());
        let air = capabilities
            .pointer("/_meta/jetbrains/air/capabilities")
            .and_then(Value::as_array)
            .expect("AIR capability list");
        for feature in AcpFeature::ALL {
            let Some(name) = feature.air_capability() else {
                continue;
            };
            assert_eq!(
                air.contains(&json!(name)),
                client_supports(*feature),
                "{feature:?} in AIR list disagrees with CLIENT_FEATURES"
            );
        }
    }

    /// The join has one row per feature and never invents an agent answer.
    #[test]
    fn client_features_compatibility_joins_the_advertised_map() {
        let unprobed = compatibility(None, Some("claude-agent-acp"));
        assert!(unprobed.iter().all(|row| row.agent.is_none()));
        let has =
            |rows: &[FeatureCompatibility], feature| rows.iter().any(|row| row.feature == feature);
        assert!(has(&unprobed, AcpFeature::ClaudeRateLimit));
        assert!(has(&unprobed, AcpFeature::SessionSteering));
        assert!(has(&unprobed, AcpFeature::AirAsyncTasks));
        assert!(
            !has(&unprobed, AcpFeature::AirPhase),
            "codex-only rows mean nothing to claude"
        );

        // No adapter package: only the shared and AIR extension rows stay,
        // and nothing outside the Extension group is ever dropped.
        let bare = compatibility(None, None);
        assert!(!has(&bare, AcpFeature::ClaudeRateLimit));
        assert!(!has(&bare, AcpFeature::SessionSteering));
        assert!(has(&bare, AcpFeature::AirAsyncTasks));
        assert_eq!(
            AcpFeature::ALL
                .iter()
                .filter(|feature| feature.group() != AcpFeatureGroup::Extension)
                .count(),
            bare.iter()
                .filter(|row| row.group != AcpFeatureGroup::Extension)
                .count()
        );

        let advertised: BTreeMap<AcpFeature, bool> = [
            (AcpFeature::LoadSession, true),
            (AcpFeature::SessionList, false),
        ]
        .into_iter()
        .collect();
        let rows = compatibility(Some(&advertised), Some("codex-acp"));
        let row = |feature| rows.iter().find(|row| row.feature == feature).unwrap();
        assert_eq!(row(AcpFeature::LoadSession).agent, Some(true));
        assert_eq!(row(AcpFeature::SessionList).agent, Some(false));
        assert_eq!(row(AcpFeature::UsageUpdate).agent, None);
        assert_eq!(
            row(AcpFeature::UsageUpdate).client,
            client_support(AcpFeature::UsageUpdate)
        );
        assert_eq!(
            row(AcpFeature::AirSessionFailure).group,
            AcpFeatureGroup::Extension
        );
    }

    #[test]
    fn client_features_feature_names_round_trip_as_map_keys() {
        let map: BTreeMap<AcpFeature, bool> = AcpFeature::ALL
            .iter()
            .map(|feature| (*feature, true))
            .collect();
        let json = serde_json::to_value(&map).unwrap();
        assert_eq!(json["prompt.image"], Value::Bool(true));
        let back: BTreeMap<AcpFeature, bool> = serde_json::from_value(json).unwrap();
        assert_eq!(back, map);
    }

    /// Claude-shaped `initialize`: the typed flags become entries, the
    /// extensions are read from `_meta`, and nothing is invented for the
    /// surfaces the response cannot answer.
    #[test]
    fn client_features_advertised_set_is_read_from_initialize() {
        let response: InitializeResponse = serde_json::from_value(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {"image": true, "embeddedContext": true},
                "sessionCapabilities": {"delete": {}},
                "_meta": {"authStatus": {}}
            },
            "authMethods": [{"id": "claude-login", "name": "Log in", "type": "terminal", "args": ["/login"]}],
            "_meta": {
                "steering": {"supported": true},
                "jetbrains": {"air": {"version": 1, "capabilities": ["sessionFailure", "planFile"]}}
            }
        }))
        .unwrap();
        let advertised = advertised_features(&response);
        for (feature, expected) in [
            (AcpFeature::LoadSession, true),
            (AcpFeature::PromptImage, true),
            (AcpFeature::PromptAudio, false),
            (AcpFeature::PromptEmbeddedContext, true),
            (AcpFeature::SessionDelete, true),
            (AcpFeature::SessionList, false),
            (AcpFeature::SessionFork, false),
            (AcpFeature::AuthTerminal, true),
            (AcpFeature::AuthStatusUpdate, true),
            (AcpFeature::SessionSteering, true),
            (AcpFeature::AirSessionFailure, true),
            (AcpFeature::AirPlanFile, true),
            (AcpFeature::AirAsyncTasks, false),
        ] {
            assert_eq!(advertised.get(&feature), Some(&expected), "{feature:?}");
        }
        for absent in [
            AcpFeature::Authenticate,
            AcpFeature::UsageUpdate,
            AcpFeature::RequestPermission,
            AcpFeature::AirPhase,
        ] {
            assert!(
                !advertised.contains_key(&absent),
                "{absent:?} is not judgeable"
            );
        }
        // Every entry is a row the table knows.
        for feature in advertised.keys() {
            assert!(AcpFeature::ALL.contains(feature));
        }
    }
}
