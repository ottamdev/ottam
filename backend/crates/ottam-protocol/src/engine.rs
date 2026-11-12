use crate::agent::{AgentAuthMethod, AgentRuntimeStatus};
use crate::ids::{EngineID, WorkspaceID};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_ENGINE_LIST: &str = "engine.list";
pub const METHOD_ENGINE_CHANGED: &str = "engine.changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EngineAuthState {
    Ready,
    Missing,
    Expired,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EngineInstallState {
    Ready,
    Missing,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EngineCapability {
    RepoRead,
    FileEdit,
    Shell,
    Streaming,
    Cancel,
    /// The engine accepts image content in a prompt, as reported by the ACP
    /// agent's `promptCapabilities.image`. Individual models behind the engine
    /// may still refuse images; ACP has no way to say so before the prompt.
    ImageInput,
    /// A permission-mode change reaches a turn already running. Without it, a
    /// change acknowledged mid-turn applies from the agent's next turn.
    LiveModeChange,
    /// The engine has no bypass mode of its own, so a conversation's "Always
    /// allow tool actions" answers its permission requests.
    AlwaysAllowToolActions,
}

/// How an engine accepts user input while a run is active: injected into the
/// live turn (`native`), by interrupting the turn and prompting a follow-up
/// (`interruptThenPrompt`), or not at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SteeringCapability {
    Native,
    InterruptThenPrompt,
    #[default]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelOption {
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub display_name: String,
    /// This model's own thought levels, once a session has run on it. Agents
    /// change the levels (and drop them) per model. Absent means not observed
    /// yet: fall back to the engine-wide `thoughtLevels`. Empty means this
    /// model has no effort setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_levels: Option<Vec<EngineSelectOption>>,
    /// The level the agent runs this model at when no level is requested,
    /// observed right after switching to it. Meaningful only alongside
    /// `thoughtLevels`.
    #[serde(
        rename = "defaultThoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_thought_level_id: Option<String>,
}

/// One choice in an agent-advertised session select — a permission mode or a
/// thought (reasoning effort) level. The id is adapter-defined, exactly like a
/// model id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineSelectOption {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineSummary {
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
    pub display_name: String,
    /// The client's icon key, as `agent.list` sends it. Absent, or a key
    /// the client does not know, means a monogram.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    pub auth_state: EngineAuthState,
    pub install_state: EngineInstallState,
    #[serde(default)]
    pub capabilities: Vec<EngineCapability>,
    #[serde(default)]
    pub models: Vec<EngineModelOption>,
    /// The model the agent runs when no explicit model is requested, as
    /// observed on the most recent session/new (the model select's current
    /// value). None when the agent advertises no model options.
    #[serde(
        rename = "defaultModelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_model_id: Option<String>,
    /// Permission/collaboration modes the agent advertises on `session/new`
    /// (ACP `category: mode` config option). Empty when the agent has none or
    /// none has been observed yet.
    #[serde(default)]
    pub modes: Vec<EngineSelectOption>,
    /// The mode select's current value on the most recent `session/new` — what
    /// the agent runs when the client requests nothing.
    #[serde(
        rename = "defaultModeID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_mode_id: Option<String>,
    /// Thought/reasoning effort levels the agent advertises on `session/new`
    /// (ACP `category: thought_level` config option).
    #[serde(default)]
    pub thought_levels: Vec<EngineSelectOption>,
    #[serde(
        rename = "defaultThoughtLevelID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub default_thought_level_id: Option<String>,
    #[serde(default)]
    pub steering_capability: SteeringCapability,
    /// Sign-ins the agent advertises, for a client that wants to offer one
    /// before any adapter is running. Empty is not "no sign-in exists" — it is
    /// "none has been observed yet", which is the ordinary state on a cold
    /// launch. Never required: an older client's `EngineSummary` decodes fine
    /// without it.
    #[serde(default)]
    pub auth_methods: Vec<AgentAuthMethod>,
    /// Node/npx health behind a harness engine. `install_state` says only
    /// whether the engine is usable; this says why it is not, which is what a
    /// client needs to tell the user what to install. None for engines with no
    /// runtime of their own, and from a server predating this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<AgentRuntimeStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineListParams {
    #[serde(rename = "workspaceID")]
    pub workspace_id: WorkspaceID,
    /// Re-run Node discovery and version probes instead of using process-local
    /// caches. Intended for an explicit user action after installing or
    /// switching runtimes; ordinary polling omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_runtime: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineListResult {
    pub engines: Vec<EngineSummary>,
}

/// One engine's cached facts changed behind the client's back — a background
/// model-catalog probe landing, a live adapter advertising a model list the
/// cache did not have, an uninstall clearing one. Without it a client that
/// listed engines before the probe answered keeps a stale model, mode, and
/// image-support picture until it happens to re-list for some other reason.
///
/// Deliberately a hint rather than a payload: the summary a client wants is
/// workspace-scoped and carries live runtime probes, so it can only come from
/// `engine.list`. Emitted when cached content changes or an install, sign-in,
/// or removal completes, even without a cached catalog. Listing alone does not
/// emit it; clients can coalesce bursts and re-list without looping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineChangedParams {
    #[serde(rename = "engineID")]
    pub engine_id: EngineID,
}
