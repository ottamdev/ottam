use crate::agent::{
    compatibility, AccountKind, AcpFeature, AcpFeatureGroup, AdapterLifecycleState, AdapterSource,
    AdapterStatus, AgentDefinition, AgentModelOption, AgentRuntimeStatus as HarnessRuntimeStatus,
    ClientSupport, FeatureCompatibility, HarnessAuthMethodKind, HarnessAuthState, HarnessError,
    HarnessErrorKind, HarnessStopReason, HarnessUpdateKind, RuntimeAvailability, RuntimeSource,
    SdkStatus, SteeringCapability as HarnessSteeringCapability,
};
use crate::core_error::CoreError;
use ottam_protocol::{
    AgentAccountKind, AgentAdapterInstallState, AgentAdapterPackage, AgentAdapterSource,
    AgentAdapterStatus, AgentAuthMethod, AgentAuthMethodKind, AgentAuthState,
    AgentFeatureClientSupport, AgentFeatureCompatibility, AgentFeatureGroup, AgentFeatureSdkStatus,
    AgentFeatureVerdict, AgentID, AgentLifecycleState, AgentRuntimeAvailability,
    AgentRuntimeSource, AgentRuntimeStatus, AgentStopReason, AgentSummary, AgentUpdate,
    ProtocolError, ProtocolResponse, RequestID, SteeringCapability,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn core_error_from_harness(error: HarnessError) -> CoreError {
    let kind = harness_error_kind(error.kind);
    CoreError::new(kind, sanitize_transport_message(&error.message)).recoverable(true)
}

pub(super) fn harness_error_kind(kind: HarnessErrorKind) -> &'static str {
    match kind {
        HarnessErrorKind::AgentNotFound => "agentNotFound",
        HarnessErrorKind::AdapterNotInstalled => "adapterNotInstalled",
        HarnessErrorKind::RuntimeMissing => "runtimeMissing",
        HarnessErrorKind::RuntimeInvalid => "runtimeInvalid",
        HarnessErrorKind::RuntimeUnsupported => "runtimeUnsupported",
        HarnessErrorKind::InvalidAuthMethod => "invalidAuthMethod",
        HarnessErrorKind::NotRunning => "notRunning",
        HarnessErrorKind::AdapterDead => "adapterDead",
        HarnessErrorKind::AdapterExited => "adapterExited",
        HarnessErrorKind::SessionNotFound => "sessionNotFound",
        HarnessErrorKind::SessionBusy | HarnessErrorKind::RemoteSessionBusy => "sessionBusy",
        HarnessErrorKind::SteeringUnsupported => "steeringUnsupported",
        HarnessErrorKind::NoActivePrompt => "noActivePrompt",
        HarnessErrorKind::SelectionUnavailable => "selectionUnavailable",
        HarnessErrorKind::AuthRequired => "authRequired",
        HarnessErrorKind::AuthRefreshBusy => "authRefreshBusy",
        HarnessErrorKind::RequestCancelled => "requestCancelled",
        HarnessErrorKind::PromptExecution => "promptExecution",
        HarnessErrorKind::Protocol => "protocol",
        HarnessErrorKind::Internal => "internal",
    }
}

pub(super) fn sanitize_transport_message(message: &str) -> String {
    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    single_line.chars().take(240).collect()
}

pub(in crate::server) fn harness_error_response(id: RequestID, error: HarnessError) -> Value {
    serde_json::to_value(ProtocolResponse::<Value>::failure(
        id,
        ProtocolError::with_data(-32000, error.message.clone(), agent_error_data(&error)),
    ))
    .expect("agent error response serializes")
}

/// `None` for core's own turn marker, which no client sees.
pub(super) fn map_update(update: HarnessUpdateKind) -> Option<AgentUpdate> {
    Some(match update {
        HarnessUpdateKind::SessionSignal(signal) => AgentUpdate::SessionSignal { signal },
        HarnessUpdateKind::UserMessageChunk(text) => AgentUpdate::UserMessageChunk { text },
        HarnessUpdateKind::RichMessageChunk {
            text,
            message_id,
            reasoning,
            metadata,
        } => AgentUpdate::RichMessageChunk {
            text,
            message_id,
            reasoning,
            metadata,
        },
        HarnessUpdateKind::AgentMessageChunk(text, message_id) => {
            AgentUpdate::AgentMessageChunk { text, message_id }
        }
        HarnessUpdateKind::AgentThoughtChunk(text, message_id) => {
            AgentUpdate::AgentThoughtChunk { text, message_id }
        }
        HarnessUpdateKind::AgentImageChunk(content, message_id) => AgentUpdate::AgentImageChunk {
            content,
            message_id,
        },
        HarnessUpdateKind::ToolCall(payload) => AgentUpdate::ToolCall { payload },
        HarnessUpdateKind::ToolCallUpdate(payload) => AgentUpdate::ToolCallUpdate { payload },
        HarnessUpdateKind::Plan(payload) => AgentUpdate::Plan { payload },
        HarnessUpdateKind::AsyncTaskSpawned(task) => AgentUpdate::AsyncTaskSpawned {
            async_task_id: task.async_task_id,
            provider_tool_call_id: task.tool_call_id,
            name: task.name,
            task_type: task.task_type,
            description: task.description,
            output_file_path: task.output_file_path,
            show_in_transcript: task.show_in_transcript,
            can_stop: task.can_stop,
        },
        HarnessUpdateKind::AsyncTaskProgress(task) => AgentUpdate::AsyncTaskProgress {
            async_task_id: task.async_task_id,
            provider_tool_call_id: task.tool_call_id,
            description: task.description,
            summary: task.summary,
            last_tool_name: task.last_tool_name,
            usage: task.usage,
            output_file_path: task.output_file_path,
        },
        HarnessUpdateKind::AsyncTaskStateUpdate(state, task) => AgentUpdate::AsyncTaskStateUpdate {
            async_task_id: task.async_task_id,
            state,
            provider_tool_call_id: task.tool_call_id,
            summary: task.summary,
            output_file_path: task.output_file_path,
        },
        HarnessUpdateKind::SubagentSpawned {
            child,
            name,
            task,
            prompt,
        } => AgentUpdate::SubagentSpawned {
            subagent_session_id: child,
            name,
            task,
            prompt,
        },
        HarnessUpdateKind::SubagentStateUpdate { child, state } => {
            AgentUpdate::SubagentStateUpdate {
                subagent_session_id: child,
                state,
            }
        }
        HarnessUpdateKind::Other { kind, payload } => AgentUpdate::Other {
            update_kind: kind,
            payload,
        },
        // Core records it on the run; clients read it from the run summary.
        HarnessUpdateKind::TurnUsage(_) | HarnessUpdateKind::PromptSettled => return None,
    })
}

pub(in crate::server) fn map_steering_capability(
    capability: HarnessSteeringCapability,
) -> SteeringCapability {
    match capability {
        HarnessSteeringCapability::Native => SteeringCapability::Native,
        HarnessSteeringCapability::InterruptThenPrompt => SteeringCapability::InterruptThenPrompt,
        HarnessSteeringCapability::Unsupported => SteeringCapability::Unsupported,
    }
}

/// The pinned package for the wire. `None` for agents launched some other way
/// (the in-process test agent).
pub(super) fn adapter_package(
    definition: &AgentDefinition,
    install_state: AgentAdapterInstallState,
) -> Option<AgentAdapterPackage> {
    definition
        .adapter
        .as_ref()
        .map(|package| AgentAdapterPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            source: match package.source {
                AdapterSource::Npm => AgentAdapterSource::Npm,
                AdapterSource::Binary(_) => AgentAdapterSource::Binary,
            },
            install_state,
            installed_version: None,
            update: None,
        })
}

/// The adapter's program name (`@agentclientprotocol/codex-acp` →
/// `codex-acp`; a binary's command), the key the extension rows of the
/// compatibility matrix are filtered on. `None` for an agent without an
/// adapter package.
pub(super) fn adapter_slug(definition: &AgentDefinition) -> Option<String> {
    definition
        .adapter
        .as_ref()
        .map(|package| package.program().to_string())
}

pub(super) fn map_definition(
    definition: AgentDefinition,
    runtime: Option<HarnessRuntimeStatus>,
    adapter: AdapterStatus,
    cached_auth_methods: Vec<AgentAuthMethod>,
    cached_advertised: Option<BTreeMap<AcpFeature, bool>>,
    cached_models: &[AgentModelOption],
    install_state: AgentAdapterInstallState,
) -> AgentSummary {
    let package = adapter_package(&definition, install_state);
    let slug = adapter_slug(&definition);
    let mut adapter = map_adapter_status(adapter, slug.as_deref());
    // Same rule as engine.list: a running adapter has its methods first-hand
    // and wins even when empty; the catalog's cache is what lets a cold pane
    // still offer a sign-in, and the same cache answers the agent column of
    // the compatibility matrix.
    if adapter.lifecycle != AgentLifecycleState::Ready {
        adapter.auth_methods = cached_auth_methods;
        adapter.compatibility = map_compatibility(cached_advertised.as_ref(), slug.as_deref());
    }
    let presentation = &definition.presentation;
    AgentSummary {
        can_set_layout: None,
        harness_id: Some(definition.harness_id().into()),
        label: definition.instance_label(),
        home_kind: Some(if definition.existing_home().is_some() {
            ottam_protocol::AgentHomeKind::Existing
        } else {
            ottam_protocol::AgentHomeKind::Managed
        }),
        agent_id: AgentID(definition.agent_id.0.clone()),
        display_name: presentation.product_name.to_string(),
        vendor_name: presentation.vendor_name.to_string(),
        vendor_home: presentation.vendor_home.map(str::to_string),
        brand: presentation.brand.map(str::to_string),
        account_label: presentation.account_label.to_string(),
        account_kind: match presentation.account_kind {
            AccountKind::Subscription => AgentAccountKind::Subscription,
            AccountKind::ApiKey => AgentAccountKind::ApiKey,
            AccountKind::MultiProvider { .. } => AgentAccountKind::MultiProvider,
        },
        model_providers: model_providers(presentation.account_kind, cached_models),
        runtime: map_runtime_status(runtime),
        adapter,
        adapter_package: package,
    }
}

/// The providers a multi-provider agent's models come from, minus the ones it
/// lists with no setup. No models seen yet is unknown, not "none".
fn model_providers(kind: AccountKind, models: &[AgentModelOption]) -> Option<Vec<String>> {
    let AccountKind::MultiProvider { keyless_providers } = kind else {
        return None;
    };
    if models.is_empty() {
        return None;
    }
    let providers: BTreeSet<&str> = models
        .iter()
        .filter_map(|model| model.model_id.split_once('/').map(|(provider, _)| provider))
        .filter(|provider| !keyless_providers.contains(provider))
        .collect();
    Some(providers.into_iter().map(str::to_string).collect())
}

pub(super) fn map_auth_method_kind(kind: HarnessAuthMethodKind) -> AgentAuthMethodKind {
    match kind {
        HarnessAuthMethodKind::Agent => AgentAuthMethodKind::Agent,
        HarnessAuthMethodKind::Terminal => AgentAuthMethodKind::Terminal,
    }
}

pub(super) fn map_adapter_status(
    status: AdapterStatus,
    adapter_slug: Option<&str>,
) -> AgentAdapterStatus {
    AgentAdapterStatus {
        lifecycle: match status.lifecycle {
            AdapterLifecycleState::Stopped => AgentLifecycleState::Stopped,
            AdapterLifecycleState::Starting => AgentLifecycleState::Starting,
            AdapterLifecycleState::Ready => AgentLifecycleState::Ready,
            AdapterLifecycleState::Stopping => AgentLifecycleState::Stopping,
            AdapterLifecycleState::Dead => AgentLifecycleState::Dead,
        },
        auth_state: match status.auth_state {
            HarnessAuthState::Unknown => AgentAuthState::Unknown,
            HarnessAuthState::Authenticating => AgentAuthState::Authenticating,
            HarnessAuthState::Ready => AgentAuthState::Ready,
            HarnessAuthState::Missing => AgentAuthState::Missing,
        },
        auth_methods: status
            .auth_methods
            .into_iter()
            .map(|method| AgentAuthMethod {
                method_id: method.method_id,
                name: method.name,
                description: method.description,
                kind: map_auth_method_kind(method.kind),
            })
            .collect(),
        agent_name: status.agent_name,
        agent_version: status.agent_version,
        steering_capability: map_steering_capability(status.capabilities.steering),
        supports_image_input: status.capabilities.prompt_image,
        compatibility: map_compatibility(Some(&status.capabilities.agent_advertises), adapter_slug),
        credentials_seen: status.credentials_seen,
        generation: status.generation,
        last_error: status.last_error,
    }
}

/// The SDK and client columns are static, so every row this agent's adapter
/// can use is always present; only the agent column depends on whether the
/// agent was ever probed.
pub(super) fn map_compatibility(
    advertised: Option<&BTreeMap<AcpFeature, bool>>,
    adapter_slug: Option<&str>,
) -> Vec<AgentFeatureCompatibility> {
    compatibility(advertised, adapter_slug)
        .into_iter()
        .map(|row| map_feature_compatibility(row, adapter_slug))
        .collect()
}

fn map_feature_compatibility(
    row: FeatureCompatibility,
    adapter_slug: Option<&str>,
) -> AgentFeatureCompatibility {
    let (sdk_status, sdk_detail) = match row.sdk {
        SdkStatus::Stable => (AgentFeatureSdkStatus::Stable, None),
        SdkStatus::Unstable {
            feature,
            enabled: true,
        } => (AgentFeatureSdkStatus::UnstableOn, Some(feature)),
        SdkStatus::Unstable {
            feature,
            enabled: false,
        } => (AgentFeatureSdkStatus::UnstableOff, Some(feature)),
        SdkStatus::SchemaOnly { feature } => (AgentFeatureSdkStatus::NotInBuild, Some(feature)),
        // Adapter rows survived the filter, so the detail is this agent's
        // own adapter; AIR rows keep their vendor label.
        SdkStatus::Extension { vendor, adapters } => (
            AgentFeatureSdkStatus::Extension,
            Some(if adapters.is_empty() {
                vendor
            } else {
                adapter_slug.unwrap_or(vendor)
            }),
        ),
    };
    let (client_support, client_detail) = match row.client {
        ClientSupport::Supported => (AgentFeatureClientSupport::Supported, None),
        ClientSupport::Partial(detail) => (AgentFeatureClientSupport::Partial, Some(detail)),
        ClientSupport::Unsupported => (AgentFeatureClientSupport::Unsupported, None),
    };
    AgentFeatureCompatibility {
        feature: row.feature.wire_name(),
        group: match row.group {
            AcpFeatureGroup::AgentCapability => AgentFeatureGroup::AgentCapability,
            AcpFeatureGroup::AgentMethod => AgentFeatureGroup::AgentMethod,
            AcpFeatureGroup::UpdateKind => AgentFeatureGroup::UpdateKind,
            AcpFeatureGroup::ClientRequest => AgentFeatureGroup::ClientRequest,
            AcpFeatureGroup::ClientCapability => AgentFeatureGroup::ClientCapability,
            AcpFeatureGroup::Extension => AgentFeatureGroup::Extension,
        },
        sdk_status,
        sdk_detail: sdk_detail.map(str::to_string),
        agent_advertises: row.agent,
        client_support,
        client_detail: client_detail.map(str::to_string),
        verdict: verdict(row.agent, row.client),
    }
}

/// Derived on the wire, never stored. `Partial` counts as the client having
/// the feature: the row's `client_detail` says which part.
pub(super) fn verdict(agent: Option<bool>, client: ClientSupport) -> AgentFeatureVerdict {
    let client_has = client != ClientSupport::Unsupported;
    match (agent, client_has) {
        (None, _) => AgentFeatureVerdict::Unknown,
        (Some(true), true) => AgentFeatureVerdict::Works,
        (Some(true), false) => AgentFeatureVerdict::AgentOnly,
        (Some(false), true) => AgentFeatureVerdict::ClientOnly,
        (Some(false), false) => AgentFeatureVerdict::Neither,
    }
}

/// `None` is a probe still running when the list answered.
pub(in crate::server) fn map_runtime_status(
    status: Option<HarnessRuntimeStatus>,
) -> AgentRuntimeStatus {
    let Some(status) = status else {
        return AgentRuntimeStatus {
            availability: AgentRuntimeAvailability::Checking,
            source: None,
            component: None,
            found_major: None,
            minimum_major: None,
            node_version: None,
            npx_version: None,
            managed_update: None,
        };
    };
    let (availability, component, found_major, minimum_major) = match status.availability {
        RuntimeAvailability::Ready => (AgentRuntimeAvailability::Ready, None, None, None),
        RuntimeAvailability::Missing { component } => (
            AgentRuntimeAvailability::Missing,
            Some(component),
            None,
            None,
        ),
        RuntimeAvailability::Invalid { component } => (
            AgentRuntimeAvailability::Invalid,
            Some(component),
            None,
            None,
        ),
        RuntimeAvailability::Unsupported { found, minimum } => (
            AgentRuntimeAvailability::Unsupported,
            Some("node".to_string()),
            Some(found),
            Some(minimum),
        ),
    };
    AgentRuntimeStatus {
        availability,
        source: status.source.map(|source| match source {
            RuntimeSource::System => AgentRuntimeSource::System,
            RuntimeSource::Managed => AgentRuntimeSource::Managed,
            RuntimeSource::Override => AgentRuntimeSource::Override,
        }),
        component,
        found_major,
        minimum_major,
        node_version: status.node_version,
        npx_version: status.npx_version,
        managed_update: None,
    }
}

pub(super) fn map_stop_reason(reason: HarnessStopReason) -> AgentStopReason {
    match reason {
        HarnessStopReason::EndTurn => AgentStopReason::EndTurn,
        HarnessStopReason::MaxTokens => AgentStopReason::MaxTokens,
        HarnessStopReason::MaxTurnRequests => AgentStopReason::MaxTurnRequests,
        HarnessStopReason::Refusal => AgentStopReason::Refusal,
        HarnessStopReason::Cancelled => AgentStopReason::Cancelled,
        HarnessStopReason::Other => AgentStopReason::Other,
    }
}

pub(super) fn agent_error_data(error: &HarnessError) -> Value {
    json!({
        "kind": match error.kind {
            HarnessErrorKind::AgentNotFound => "agentNotFound",
            HarnessErrorKind::AdapterNotInstalled => "adapterNotInstalled",
            HarnessErrorKind::RuntimeMissing => "runtimeMissing",
            HarnessErrorKind::RuntimeInvalid => "runtimeInvalid",
            HarnessErrorKind::RuntimeUnsupported => "runtimeUnsupported",
            HarnessErrorKind::InvalidAuthMethod => "invalidAuthMethod",
            HarnessErrorKind::NotRunning => "notRunning",
            HarnessErrorKind::AdapterDead => "adapterDead",
            HarnessErrorKind::AdapterExited => "adapterExited",
            HarnessErrorKind::SessionNotFound => "sessionNotFound",
            HarnessErrorKind::SessionBusy | HarnessErrorKind::RemoteSessionBusy => "sessionBusy",
            HarnessErrorKind::SteeringUnsupported => "steeringUnsupported",
            HarnessErrorKind::NoActivePrompt => "noActivePrompt",
            HarnessErrorKind::SelectionUnavailable => "selectionUnavailable",
            HarnessErrorKind::AuthRequired => "authRequired",
            HarnessErrorKind::AuthRefreshBusy => "authRefreshBusy",
            HarnessErrorKind::RequestCancelled => "requestCancelled",
            HarnessErrorKind::PromptExecution => "promptExecution",
            HarnessErrorKind::Protocol => "protocol",
            HarnessErrorKind::Internal => "internal",
        }
    })
}
