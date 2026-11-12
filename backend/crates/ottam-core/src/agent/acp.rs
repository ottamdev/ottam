mod actor;
mod adapter;
mod async_tasks;
mod child_sessions;
/// Shared with core's MCP client, whose upstream servers send the same forms.
pub(crate) mod elicitation;
mod features;
mod protocol;
mod session_config;
mod shared_transport;
mod steering;
mod subagent_sessions;
mod xd_devices;

#[cfg(test)]
mod tests;

pub(crate) use adapter::AcpHarnessAdapter;
pub(crate) use protocol::{launch_unset_variables, reword_refresh_lock_failure};
pub(crate) use xd_devices::DEVICE_CALL_STAMP;
pub use features::{
    compatibility, AcpFeature, AcpFeatureGroup, ClientSupport, FeatureCompatibility, SdkStatus,
    CLIENT_FEATURES,
};
