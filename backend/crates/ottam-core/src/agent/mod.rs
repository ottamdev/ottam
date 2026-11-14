//! Shared ACP harness adapters and the core-owned agent registry.

mod acp;
mod adapter_install;
mod agent_install_service;
mod auth;
mod catalog_probe;
mod instances;
mod managed_node;
pub(crate) mod metadata;
mod model_catalog;
#[cfg(test_seams)]
mod native_test;
mod node_resolver;
mod plan_usage;
pub(crate) mod refresh_lock;
mod registry;
mod runtime_install;
pub(crate) mod setup_detection;
mod types;

pub(crate) use acp::elicitation;
pub(crate) use acp::{launch_unset_variables, reword_refresh_lock_failure};
pub(crate) use acp::DEVICE_CALL_STAMP;
pub use acp::{
    compatibility, AcpFeature, AcpFeatureGroup, ClientSupport, FeatureCompatibility, SdkStatus,
    CLIENT_FEATURES,
};
pub(crate) use agent_install_service::{
    AgentInstallEventSink, AgentInstallService, InstallOutcome,
};
pub use catalog_probe::ProbeCleanup;
pub(crate) use auth::{
    AgentSignInService, AuthEventSink, AuthTerminalFuture, AuthTerminalHost, AuthTerminalRequest,
    AuthTerminalRoot, AuthTerminalSession, SignedInSink,
};
pub(crate) use instances::{existing_setup, ExistingSetup};
pub use instances::{AgentInstance, AgentInstanceId, HarnessDefinition, HarnessId, HomeLayout};
pub(crate) use model_catalog::pinned_adapter_package;
pub(crate) use model_catalog::CatalogChangeSink;
pub use model_catalog::ModelCatalogService;
pub(crate) use plan_usage::{PlanUsageService, PlanUsageSink};
pub(crate) use registry::toolchain_path;
#[cfg(test)]
pub(crate) use registry::AdapterFactory;
pub(crate) use registry::ReplayAdmission;
pub(crate) use registry::CLAUDE_AGENT_ID;
pub use registry::{AgentRegistry, RuntimeProbe, SystemRuntimeProbe};
pub(crate) use runtime_install::{RuntimeInstallEventSink, RuntimeInstallService};
pub use types::*;
