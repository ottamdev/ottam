use super::types::{
    AdapterPackage, AgentDefinition, AgentId, AgentLaunchSpec, AgentPresentation, ChildSessionTags,
    ClientPlugin, FormSkip, HomeSetting, HomeVariable, McpConnections, SignInProof,
    SubagentMessageTools, TerminalLoginReveal,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Installation identity is distinct from the instance used for a conversation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HarnessId(pub String);

pub type AgentInstanceId = AgentId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HomeLayout {
    Managed,
    Existing {
        path: PathBuf,
        /// None means explicitly remove an inherited value, not "use app env".
        environment: BTreeMap<String, Option<String>>,
    },
}

/// A CLI whose own setup an instance can use: the login-shell variables
/// that choose its homes, all recorded on the instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExistingSetup {
    pub title: &'static str,
    /// The CLI's command.
    pub cli: &'static str,
    pub variables: &'static [&'static str],
}

pub(crate) fn existing_setup(harness: &str) -> Option<ExistingSetup> {
    Some(match harness {
        "codex" => ExistingSetup {
            title: "Codex",
            cli: "codex",
            variables: &["CODEX_HOME"],
        },
        "claude" => ExistingSetup {
            title: "Claude Code",
            cli: "claude",
            variables: &["CLAUDE_CONFIG_DIR"],
        },
        // OpenCode keeps its database under the data home and its settings
        // under the config home; either has its own override. Inline config
        // is the setup's top layer, which Ottam's plugin joins.
        "opencode" => ExistingSetup {
            title: "OpenCode",
            cli: "opencode",
            variables: &[
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "OPENCODE_DB",
                "OPENCODE_CONFIG_DIR",
                "OPENCODE_CONFIG_CONTENT",
            ],
        },
        // omp's agent directory holds credentials, settings, and sessions.
        // Its named profiles and XDG layout (`omp config init-xdg`) aren't
        // detected: launches use the default profile.
        "omp" => ExistingSetup {
            title: "Oh-My-Pi",
            cli: "omp",
            variables: &["PI_CODING_AGENT_DIR"],
        },
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInstance {
    pub id: AgentInstanceId,
    pub harness_id: HarnessId,
    pub label: String,
    pub home: HomeLayout,
    pub claimed: bool,
}

impl AgentDefinition {
    /// The instance's label as clients show it. One that only differs in case
    /// from the product name (a profile seeded before a rename) reads as the
    /// product name.
    pub(crate) fn instance_label(&self) -> Option<String> {
        let product_name = self.presentation.product_name;
        self.instance.as_ref().map(|instance| {
            if instance.label.eq_ignore_ascii_case(product_name) {
                product_name.to_string()
            } else {
                instance.label.clone()
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct HarnessDefinition {
    pub id: HarnessId,
    pub presentation: AgentPresentation,
    pub terminal_login_reveal: TerminalLoginReveal,
    pub adapter: Option<AdapterPackage>,
    pub launch: AgentLaunchSpec,
    pub home_variables: &'static [HomeVariable],
    pub subagent_messages: SubagentMessageTools,
    pub child_sessions: Option<ChildSessionTags>,
    pub client_plugin: Option<ClientPlugin>,
    pub home_settings: &'static [HomeSetting],
    pub form_skip: FormSkip,
    pub sign_in_proof: SignInProof,
    pub terminal_login_args: Option<&'static [&'static str]>,
    pub mcp_connections: McpConnections,
}

impl HarnessDefinition {
    pub(crate) fn from_definition(definition: AgentDefinition) -> Self {
        Self {
            id: HarnessId(definition.agent_id.0),
            presentation: definition.presentation,
            terminal_login_reveal: definition.terminal_login_reveal,
            adapter: definition.adapter,
            launch: definition.launch,
            home_variables: definition.home_variables,
            subagent_messages: definition.subagent_messages,
            child_sessions: definition.child_sessions,
            client_plugin: definition.client_plugin,
            home_settings: definition.home_settings,
            form_skip: definition.form_skip,
            sign_in_proof: definition.sign_in_proof,
            terminal_login_args: definition.terminal_login_args,
            mcp_connections: definition.mcp_connections,
        }
    }

    pub(crate) fn resolve(&self, instance: AgentInstance) -> AgentDefinition {
        AgentDefinition {
            agent_id: instance.id.clone(),
            presentation: self.presentation.clone(),
            terminal_login_reveal: self.terminal_login_reveal,
            adapter: self.adapter.clone(),
            launch: self.launch.clone(),
            home_variables: self.home_variables,
            subagent_messages: self.subagent_messages,
            child_sessions: self.child_sessions,
            client_plugin: self.client_plugin,
            home_settings: self.home_settings,
            form_skip: self.form_skip,
            sign_in_proof: self.sign_in_proof,
            terminal_login_args: self.terminal_login_args,
            mcp_connections: self.mcp_connections,
            instance: Some(instance),
        }
    }

    pub(crate) fn template_definition(&self) -> AgentDefinition {
        AgentDefinition {
            agent_id: AgentId(self.id.0.clone()),
            presentation: self.presentation.clone(),
            terminal_login_reveal: self.terminal_login_reveal,
            adapter: self.adapter.clone(),
            launch: self.launch.clone(),
            home_variables: self.home_variables,
            subagent_messages: self.subagent_messages,
            child_sessions: self.child_sessions,
            client_plugin: self.client_plugin,
            home_settings: self.home_settings,
            form_skip: self.form_skip,
            sign_in_proof: self.sign_in_proof,
            terminal_login_args: self.terminal_login_args,
            mcp_connections: self.mcp_connections,
            instance: None,
        }
    }
}

impl AgentDefinition {
    pub fn harness_id(&self) -> &str {
        self.instance
            .as_ref()
            .map_or(&self.agent_id.0, |instance| &instance.harness_id.0)
    }

    pub fn existing_home(&self) -> Option<&Path> {
        match self.instance.as_ref().map(|instance| &instance.home) {
            Some(HomeLayout::Existing { path, .. }) => Some(path),
            _ => None,
        }
    }

    /// The agent's own mode that stops permission requests. Agents without
    /// one are the ones Ottam's "Always allow tool actions" covers.
    pub fn permission_bypass_mode(&self) -> Option<&'static str> {
        match self.harness_id() {
            super::registry::CLAUDE_AGENT_ID => Some("bypassPermissions"),
            super::registry::CODEX_AGENT_ID => Some("agent-full-access"),
            _ => None,
        }
    }

    /// The agent's most restrictive mode that still works, which a named
    /// agent's children start in unless it names another. Read from each
    /// agent's mode list at its pin (2026-10-27).
    pub fn read_only_mode(&self) -> Option<&'static str> {
        match self.harness_id() {
            super::registry::CLAUDE_AGENT_ID => Some("plan"),
            super::registry::CODEX_AGENT_ID => Some("read-only"),
            super::registry::OPENCODE_AGENT_ID | super::registry::OMP_AGENT_ID => Some("plan"),
            _ => None,
        }
    }

    /// The mode a child starts in when neither its parent's call nor its
    /// named agent names one.
    /// Oh-My-Pi's plan mode makes every turn end with a question or a plan
    /// proposal (`#enforcePlanModeDecisionAtSettle` at 18.12.6), so a task
    /// that needs no plan would wait on the user. Its children start in
    /// `default`, where bash, delete and move still ask but writes don't.
    pub fn delegated_mode(&self) -> Option<&'static str> {
        match self.harness_id() {
            super::registry::OMP_AGENT_ID => Some("default"),
            _ => self.read_only_mode(),
        }
    }

    /// The agent's modes that ask before acting, which a conversation that
    /// asks may hand a child it starts. Read from each agent's mode list in
    /// its pinned fixture (2026-10-31). OpenCode's build and OMP's default act on their
    /// own permission config, so only their plan mode is known to ask.
    pub fn asking_modes(&self) -> &'static [&'static str] {
        match self.harness_id() {
            super::registry::CLAUDE_AGENT_ID => &["default", "plan"],
            super::registry::CODEX_AGENT_ID => &["read-only"],
            super::registry::OPENCODE_AGENT_ID | super::registry::OMP_AGENT_ID => &["plan"],
            _ => &[],
        }
    }

    /// What the agent's prompt-answer token counts cover.
    pub fn turn_usage_scope(&self) -> ottam_protocol::ConversationRunUsageScope {
        use ottam_protocol::ConversationRunUsageScope::*;
        match self.harness_id() {
            // claude-agent-acp reports the main loop; subagents are only in
            // `_meta.quota.model_usage`.
            super::registry::CLAUDE_AGENT_ID => MainAgent,
            // codex-acp forwards Codex's `tokenUsage.last`.
            super::registry::CODEX_AGENT_ID => LastRequest,
            // OpenCode sums the root session's steps, not child sessions.
            super::registry::OPENCODE_AGENT_ID => MainAgent,
            _ => Turn,
        }
    }

    pub(crate) fn refresh_lock_path(&self) -> Option<PathBuf> {
        (self.harness_id() == "claude")
            .then(|| {
                self.existing_home()
                    .map(|path| path.join(super::refresh_lock::CLAUDE_REFRESH_LOCK))
            })
            .flatten()
    }

    /// Claude's refresh lock in the Ottam-owned home this launch points
    /// at. Unlike [`Self::refresh_lock_path`], it only delays this adapter's
    /// own teardown, never Quit.
    pub(crate) fn managed_refresh_lock(&self) -> Option<PathBuf> {
        if self.harness_id() != "claude" || self.existing_home().is_some() {
            return None;
        }
        self.launch
            .environment
            .iter()
            .find(|(name, _)| name == "CLAUDE_CONFIG_DIR")
            .map(|(_, home)| Path::new(home).join(super::refresh_lock::CLAUDE_REFRESH_LOCK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_differing_only_in_case_reads_as_the_product_name() {
        let labelled = |label: &str| {
            HarnessDefinition::from_definition(AgentDefinition::for_test("omp"))
                .resolve(AgentInstance {
                    id: AgentId::new("omp"),
                    harness_id: HarnessId("omp".into()),
                    label: label.into(),
                    home: HomeLayout::Managed,
                    claimed: false,
                })
                .instance_label()
        };
        assert_eq!(labelled("test agent").as_deref(), Some("Test Agent"));
        assert_eq!(labelled("Work").as_deref(), Some("Work"));
        assert_eq!(AgentDefinition::for_test("omp").instance_label(), None);
    }
}
