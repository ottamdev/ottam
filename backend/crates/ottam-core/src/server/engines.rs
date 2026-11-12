use super::agents::map_steering_capability;
use ottam_protocol::{
    AgentAuthMethod, AgentAuthMethodKind, EngineAuthState, EngineCapability, EngineID,
    EngineInstallState, EngineModelOption, EngineSelectOption, EngineSummary,
};
use std::collections::BTreeMap;

/// What the app calls an agent. The same product name agent.list ships, so
/// two windows never name the same agent differently. A label that already
/// names the product (an existing setup's "Claude Code — Your setup") is not
/// prefixed again.
pub(crate) fn engine_display_name(definition: &AgentDefinition) -> String {
    let product_name = definition.presentation.product_name;
    match definition.instance_label() {
        Some(label)
            if label == product_name || label.starts_with(&format!("{product_name} — ")) =>
        {
            label
        }
        Some(label) => format!("{product_name} — {label}"),
        None => product_name.to_string(),
    }
}

// One argument per independent input; bundling them into a struct would just
// move the same list somewhere else.
#[allow(clippy::too_many_arguments)]
pub(super) fn harness_engine(
    definition: AgentDefinition,
    runtime: Option<AgentRuntimeStatus>,
    adapter: AdapterStatus,
    models: Vec<AgentModelOption>,
    default_model_id: Option<String>,
    cached_selects: Option<AgentSessionSelects>,
    cached_model_selects: BTreeMap<String, AgentModelSelects>,
    cached_prompt_image: bool,
    cached_auth_methods: Vec<AgentAuthMethod>,
    adapter_needs_install: bool,
) -> EngineSummary {
    // An uninstalled (or still-installing) adapter is unusable regardless of
    // Node health: install is mandatory, and `missing` is what disables the
    // engine in the picker until the Get flow in Settings finishes.
    let install_state = if adapter_needs_install {
        EngineInstallState::Missing
    } else {
        match runtime.as_ref().map(|runtime| &runtime.availability) {
            // Still probing. `Unknown` keeps the engine selectable: a prompt
            // waits for the same probe in `require_runtime`, and treating it
            // as unusable would move a cold launch off the user's engine.
            None => EngineInstallState::Unknown,
            Some(RuntimeAvailability::Ready) => EngineInstallState::Ready,
            Some(RuntimeAvailability::Missing { .. }) => EngineInstallState::Missing,
            Some(RuntimeAvailability::Unsupported { .. }) => EngineInstallState::Unsupported,
            // A runtime that answered with garbage cannot start an adapter, so
            // it is unusable in exactly the way `Missing` is. Reporting
            // `Unknown` here let the engine look selectable and fail only once
            // a prompt ran; the `runtime` field below still carries which
            // component misbehaved.
            Some(RuntimeAvailability::Invalid { .. }) => EngineInstallState::Missing,
        }
    };
    let auth_state = match adapter.auth_state {
        HarnessAuthState::Ready => EngineAuthState::Ready,
        // ACP cannot tell "never signed in" from "signed in, then the session
        // lapsed" — both arrive as not-authenticated. The adapter having worked
        // earlier in this generation is the only evidence that separates them,
        // and it is the difference between "Sign in" and "Sign in again".
        HarnessAuthState::Missing if adapter.credentials_seen => EngineAuthState::Expired,
        HarnessAuthState::Missing => EngineAuthState::Missing,
        HarnessAuthState::Unknown | HarnessAuthState::Authenticating => EngineAuthState::Unknown,
    };
    let display_name = engine_display_name(&definition);
    // The live adapter's advertised models (refreshed on every session/new)
    // win over the possibly stale probe cache.
    let (models, default_model_id) = if adapter.models.is_empty() {
        (models, default_model_id)
    } else {
        (adapter.models.clone(), adapter.default_model_id.clone())
    };
    // Same precedence for the mode/thought-level selects.
    let selects = adapter
        .session_selects
        .clone()
        .or(cached_selects)
        .unwrap_or_default();
    // Per model, this generation's observations are newer than the cache's.
    let mut model_selects = cached_model_selects;
    model_selects.extend(adapter.model_selects.clone());
    // A running adapter has the methods first-hand; the cache is what makes an
    // offer possible before anything has started. An adapter that is up and
    // advertised none has genuinely none, so it wins even when empty.
    let auth_methods = if adapter.lifecycle == AdapterLifecycleState::Ready {
        adapter
            .auth_methods
            .iter()
            .map(engine_auth_method)
            .collect()
    } else {
        cached_auth_methods
    };
    EngineSummary {
        engine_id: EngineID(definition.agent_id.0.clone()),
        display_name,
        brand: definition.presentation.brand.map(str::to_owned),
        auth_state,
        install_state,
        capabilities: {
            let mut capabilities = vec![
                EngineCapability::RepoRead,
                EngineCapability::FileEdit,
                EngineCapability::Shell,
                EngineCapability::Streaming,
                EngineCapability::Cancel,
            ];
            // Unlike its neighbours, this one is observed rather than assumed:
            // whether an agent accepts image blocks is something it tells us
            // during `initialize`. Before any adapter has started — which is
            // the whole of a fresh launch, and exactly when someone pastes a
            // screenshot into an empty composer — the live status says
            // `false` for want of an answer, so fall back to what the model
            // catalog probe last observed.
            if adapter.capabilities.prompt_image || cached_prompt_image {
                capabilities.push(EngineCapability::ImageInput);
            }
            // ACP does not say when a mode change takes effect. claude-agent-acp
            // forwards it to the running query; codex-acp reads its mode only
            // when a turn starts. Unknown agents get the next-turn wording.
            if definition.harness_id() == crate::agent::CLAUDE_AGENT_ID {
                capabilities.push(EngineCapability::LiveModeChange);
            }
            if definition.permission_bypass_mode().is_none() {
                capabilities.push(EngineCapability::AlwaysAllowToolActions);
            }
            capabilities
        },
        models: models
            .into_iter()
            .map(|model| {
                let observed = model_selects.remove(&model.model_id);
                EngineModelOption {
                    model_id: model.model_id,
                    display_name: model.display_name,
                    default_thought_level_id: observed
                        .as_ref()
                        .and_then(|selects| selects.default_thought_level_id.clone()),
                    thought_levels: observed.map(|selects| {
                        selects
                            .thought_levels
                            .into_iter()
                            .map(engine_select_option)
                            .collect()
                    }),
                }
            })
            .collect(),
        default_model_id,
        modes: selects
            .modes
            .into_iter()
            .map(engine_select_option)
            .collect(),
        default_mode_id: selects.default_mode_id,
        thought_levels: selects
            .thought_levels
            .into_iter()
            .map(engine_select_option)
            .collect(),
        default_thought_level_id: selects.default_thought_level_id,
        steering_capability: map_steering_capability(adapter.capabilities.steering),
        auth_methods,
        runtime: Some(super::agents::map_runtime_status(runtime)),
    }
}

fn engine_select_option(option: crate::agent::AgentSelectOption) -> EngineSelectOption {
    EngineSelectOption {
        id: option.id,
        display_name: option.display_name,
    }
}

pub(super) fn engine_auth_method(method: &HarnessAuthMethod) -> AgentAuthMethod {
    AgentAuthMethod {
        method_id: method.method_id.clone(),
        name: method.name.clone(),
        description: method.description.clone(),
        kind: match method.kind {
            HarnessAuthMethodKind::Agent => AgentAuthMethodKind::Agent,
            HarnessAuthMethodKind::Terminal => AgentAuthMethodKind::Terminal,
        },
    }
}

use crate::agent::{
    AdapterLifecycleState, AdapterStatus, AgentDefinition, AgentModelOption, AgentModelSelects,
    AgentRuntimeStatus, AgentSessionSelects, HarnessAuthMethod, HarnessAuthMethodKind,
    HarnessAuthState, RuntimeAvailability,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> AgentDefinition {
        AgentDefinition::for_test("codex")
    }

    fn summary_for(adapter: AdapterStatus, cached: Vec<AgentAuthMethod>) -> EngineSummary {
        harness_engine(
            definition(),
            Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
            adapter,
            Vec::new(),
            None,
            None,
            BTreeMap::new(),
            false,
            cached,
            false,
        )
    }

    /// Mandatory install: an uninstalled adapter's engine is unusable even on
    /// a perfectly healthy Node.
    #[test]
    fn an_uninstalled_adapter_reports_the_engine_missing() {
        let summary = harness_engine(
            definition(),
            Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
            AdapterStatus::stopped(),
            Vec::new(),
            None,
            None,
            BTreeMap::new(),
            false,
            Vec::new(),
            true,
        );
        assert_eq!(summary.install_state, EngineInstallState::Missing);
    }

    /// No verdict is not a problem: the row stays selectable and says it is
    /// still checking, unless install already rules the engine out.
    #[test]
    fn a_running_probe_reports_checking_without_disabling_the_engine() {
        let summary_with = |adapter_needs_install| {
            harness_engine(
                definition(),
                None,
                AdapterStatus::stopped(),
                Vec::new(),
                None,
                None,
                BTreeMap::new(),
                false,
                Vec::new(),
                adapter_needs_install,
            )
        };
        let checking = summary_with(false);
        assert_eq!(checking.install_state, EngineInstallState::Unknown);
        assert_eq!(
            checking.runtime.map(|runtime| runtime.availability),
            Some(ottam_protocol::AgentRuntimeAvailability::Checking)
        );
        assert_eq!(
            summary_with(true).install_state,
            EngineInstallState::Missing
        );
    }

    /// A profile is named after its product once: an existing setup's label
    /// already carries the product name, a managed profile's does not.
    #[test]
    fn an_instance_engine_names_its_product_once() {
        let named = |label: &str| {
            let mut definition = definition();
            definition.instance = Some(crate::agent::AgentInstance {
                id: crate::agent::AgentId::new("codex.profile"),
                harness_id: crate::agent::HarnessId("codex".into()),
                label: label.into(),
                home: crate::agent::HomeLayout::Managed,
                claimed: true,
            });
            harness_engine(
                definition,
                Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
                AdapterStatus::stopped(),
                Vec::new(),
                None,
                None,
                BTreeMap::new(),
                false,
                Vec::new(),
                false,
            )
            .display_name
        };
        assert_eq!(named("Test Agent — Your setup"), "Test Agent — Your setup");
        assert_eq!(named("test agent"), "Test Agent");
        assert_eq!(named("Work"), "Test Agent — Work");
        assert_eq!(named("Test Agents"), "Test Agent — Test Agents");
    }

    /// Each model carries its own levels; this generation's observations
    /// override the catalog's, and unobserved models report none.
    #[test]
    fn per_model_selects_prefer_live_observations_over_the_cache() {
        let levels = |id: &str| AgentModelSelects {
            thought_levels: vec![crate::agent::AgentSelectOption {
                id: id.into(),
                display_name: id.into(),
            }],
            default_thought_level_id: Some(id.into()),
        };
        let model = |id: &str| AgentModelOption {
            model_id: id.into(),
            display_name: id.into(),
        };
        let mut adapter = AdapterStatus::stopped();
        adapter.model_selects.insert("glm".into(), levels("max"));
        let summary = harness_engine(
            definition(),
            Some(AgentRuntimeStatus::ready("v22.0.0", "10.0.0")),
            adapter,
            vec![model("glm"), model("kimi"), model("new")],
            None,
            None,
            BTreeMap::from([
                ("glm".to_string(), levels("high")),
                ("kimi".to_string(), levels("low")),
            ]),
            false,
            Vec::new(),
            false,
        );
        let defaults: Vec<_> = summary
            .models
            .iter()
            .map(|model| model.default_thought_level_id.as_deref())
            .collect();
        assert_eq!(defaults, [Some("max"), Some("low"), None]);
        assert_eq!(
            summary.models[0].thought_levels.as_ref().map(Vec::len),
            Some(1)
        );
        assert_eq!(summary.models[2].thought_levels, None);
    }

    fn auth_state_for(adapter: AdapterStatus) -> EngineAuthState {
        summary_for(adapter, Vec::new()).auth_state
    }

    fn cached_method(id: &str) -> AgentAuthMethod {
        AgentAuthMethod {
            method_id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind: AgentAuthMethodKind::Agent,
        }
    }

    /// Cold, the cache is the only thing that can offer a sign-in — the whole
    /// reason it holds sign-ins at all.
    #[test]
    fn a_stopped_engine_reports_the_cached_sign_ins() {
        let summary = summary_for(AdapterStatus::stopped(), vec![cached_method("chat-gpt")]);
        assert_eq!(summary.auth_methods, vec![cached_method("chat-gpt")]);
    }

    /// And once it is up it speaks for itself, including when what it says is
    /// "none" — a stale cached offer would open a browser for nothing.
    #[test]
    fn a_running_engine_overrides_the_cache_even_with_nothing_to_offer() {
        let mut adapter = AdapterStatus::stopped();
        adapter.lifecycle = AdapterLifecycleState::Ready;
        let summary = summary_for(adapter, vec![cached_method("chat-gpt")]);
        assert!(summary.auth_methods.is_empty());
    }

    #[test]
    fn signed_out_without_prior_credentials_reads_as_missing() {
        let mut adapter = AdapterStatus::stopped();
        adapter.auth_state = HarnessAuthState::Missing;
        assert_eq!(auth_state_for(adapter), EngineAuthState::Missing);
    }

    /// The only path to `Expired`: this generation authenticated at some point,
    /// so being signed out now is a lapse rather than an absence.
    #[test]
    fn signed_out_after_working_credentials_reads_as_expired() {
        let mut adapter = AdapterStatus::stopped();
        adapter.auth_state = HarnessAuthState::Missing;
        adapter.credentials_seen = true;
        assert_eq!(auth_state_for(adapter), EngineAuthState::Expired);
    }

    /// The latch only ever qualifies `Missing` — a working adapter is `Ready`
    /// regardless, and an indeterminate one stays `Unknown`.
    #[test]
    fn the_latch_does_not_disturb_the_other_auth_states() {
        for (harness, expected) in [
            (HarnessAuthState::Ready, EngineAuthState::Ready),
            (HarnessAuthState::Unknown, EngineAuthState::Unknown),
            (HarnessAuthState::Authenticating, EngineAuthState::Unknown),
        ] {
            let mut adapter = AdapterStatus::stopped();
            adapter.auth_state = harness;
            adapter.credentials_seen = true;
            assert_eq!(auth_state_for(adapter), expected);
        }
    }
}
