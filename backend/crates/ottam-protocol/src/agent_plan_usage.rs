use crate::ids::AgentID;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const METHOD_AGENT_PLAN_USAGE: &str = "agent.planUsage";
pub const METHOD_AGENT_PLAN_USAGE_CHANGED: &str = "agent.planUsageChanged";

/// Read the agent's subscription usage windows: the five-hour and weekly
/// limits the vendor's own app shows. Answers from core's last-known snapshot
/// and starts a refresh when that snapshot is missing or stale; the refreshed
/// snapshot arrives on `agent.planUsageChanged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPlanUsageParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    /// Refresh now, however fresh the snapshot or long the failure backoff;
    /// a refresh already running is joined, not repeated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanUsageResult {
    /// Absent when the agent has no plan limits core can read (an API-key
    /// account, an agent without a usage source) or none have been read yet.
    pub usage: Option<AgentPlanUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentPlanUsageWindowKind {
    FiveHour,
    Weekly,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanUsageWindow {
    pub kind: AgentPlanUsageWindowKind,
    /// Short label for the window, such as `5h` or `wk`.
    pub label: String,
    /// Share of the window used, 0 to 100.
    pub used_percent: f64,
    /// Unix seconds when the window resets.
    pub resets_at: Option<i64>,
    /// The model a scoped window limits, such as `Fable`; absent for a
    /// window covering every model.
    pub model: Option<String>,
}

/// Where the latest windows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentPlanUsageSource {
    /// Reported by the agent during a turn.
    Session,
    /// Read by core's background usage probe.
    Probe,
}

/// The agent's last-known plan usage. Each refresh replaces the windows it
/// reports; a failed refresh keeps the last windows and sets `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanUsage {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub windows: Vec<AgentPlanUsageWindow>,
    /// The vendor's verdict when it gives one: `allowed`, `allowed_warning`,
    /// or `rejected`.
    pub limit_status: Option<String>,
    pub source: AgentPlanUsageSource,
    /// Unix milliseconds when the windows were read.
    pub observed_at: i64,
    /// Why the latest refresh failed, when it did.
    pub error: Option<String>,
}

/// The agent's snapshot changed. `usage` is absent when core dropped it: the
/// account signed out or no longer reports plan limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanUsageChangedParams {
    #[serde(rename = "agentID")]
    pub agent_id: AgentID,
    pub usage: Option<AgentPlanUsage>,
}
