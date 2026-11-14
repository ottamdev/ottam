use super::super::types::{
    AgentModelOption, AgentModelSelects, AgentSelectOption, AgentSessionSelects, HarnessError,
    HarnessErrorKind, HarnessSessionId, ModelSwitch, SessionConfigRequest,
};
use agent_client_protocol::schema::v1::{
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SetSessionConfigOptionRequest,
};
use std::future::Future;
use std::pin::Pin;

pub(super) trait SessionConfigSetter {
    fn set_session_config(
        &self,
        request: SetSessionConfigOptionRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
                + Send
                + '_,
        >,
    >;
}

/// One selector advertised by an agent's `session/new` or `session/load` response.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ConfigSelect {
    config_id: SessionConfigId,
    pub(super) current_value: String,
    pub(super) options: Vec<AgentSelectOption>,
}

/// The selectors Ottam consumes from session config options: the
/// model, the permission/collaboration mode, and the thought (effort) level.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SessionSelects {
    pub(super) model: Option<ConfigSelect>,
    pub(super) mode: Option<ConfigSelect>,
    thought_level: Option<ConfigSelect>,
}

impl SessionSelects {
    pub(super) fn thought_level(&self) -> Option<&ConfigSelect> {
        self.thought_level.as_ref()
    }

    /// The cacheable half: what the agent advertised beyond models, with an
    /// absent select reading as an empty list (a real "none" answer).
    pub(super) fn advertised(&self) -> AgentSessionSelects {
        AgentSessionSelects {
            modes: self
                .mode
                .as_ref()
                .map(|select| select.options.clone())
                .unwrap_or_default(),
            default_mode_id: self
                .mode
                .as_ref()
                .map(|select| select.current_value.clone()),
            thought_levels: self
                .thought_level
                .as_ref()
                .map(|select| select.options.clone())
                .unwrap_or_default(),
            default_thought_level_id: self
                .thought_level
                .as_ref()
                .map(|select| select.current_value.clone()),
        }
    }

    /// The current model's thought-level select, keyed by that model. `None`
    /// without a model select: there is no model to attribute the level to.
    /// A missing thought select is a real answer (this model has no effort).
    pub(super) fn model_selects(&self) -> Option<(String, AgentModelSelects)> {
        let model = self.model.as_ref()?;
        let advertised = self.advertised();
        let selects = AgentModelSelects {
            thought_levels: advertised.thought_levels,
            default_thought_level_id: advertised.default_thought_level_id,
        };
        Some((model.current_value.clone(), selects))
    }
}

/// What applying a `session/new` request left behind.
#[derive(Debug)]
pub(super) struct AppliedSessionConfig {
    /// The session's selects after any model switch: the switch's full
    /// answer replaces `session/new`'s, since agents rebuild their options
    /// for the new model.
    pub(super) selects: SessionSelects,
    /// Whether `selects` came from a model switch. Its thought level is then
    /// the switched model's own, read before any explicit level is applied,
    /// and is worth recording even when a later request is refused.
    pub(super) switched: bool,
    /// The session's model and thought level on the same terms as `mode`;
    /// meaningful only when `mode` is `Ok`.
    pub(super) model: Option<String>,
    pub(super) thought_level: Option<String>,
    /// The session's mode: the requested one when the agent took it, else the
    /// one it started on. An error refuses the session.
    pub(super) mode: Result<Option<String>, HarnessError>,
}

/// What a live model switch left behind, including when it failed partway.
#[derive(Debug)]
pub(super) struct AppliedLiveModel {
    /// The session's selects afterwards: a full switch answer replaces them.
    pub(super) selects: SessionSelects,
    /// Whether `selects` came from a model switch, as in [`AppliedSessionConfig`].
    pub(super) switched: bool,
    /// The session's model and thought level afterwards: what the agent
    /// confirmed, or `None` once a request it may have taken was refused.
    pub(super) model: Option<String>,
    pub(super) thought_level: Option<String>,
    /// An error fails the run. The fields above still record what the agent
    /// took before it, so a later run doesn't trust a stale model.
    pub(super) outcome: Result<(), HarnessError>,
}

/// How [`apply_config_select`] left one selector.
enum Applied {
    /// Nothing requested, nothing to set, or the request failed.
    Untouched,
    /// The agent was already on the requested value.
    AlreadyCurrent,
    /// Set; the agent's config options after the change.
    Set(Vec<SessionConfigOption>),
}

pub(super) fn model_option(option: &AgentSelectOption) -> AgentModelOption {
    AgentModelOption {
        model_id: option.id.clone(),
        display_name: option.display_name.clone(),
    }
}

/// Find the select for one category among the config options advertised by
/// session creation or restoration. Prefers the matching `category`, falling
/// back to an option whose id is exactly `fallback_id`. Grouped values are
/// flattened.
fn extract_select(
    options: &[SessionConfigOption],
    category: SessionConfigOptionCategory,
    fallback_id: &str,
) -> Option<ConfigSelect> {
    let option = options
        .iter()
        .find(|option| option.category.as_ref() == Some(&category))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.id.0.as_ref() == fallback_id)
        })?;
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let values = match &select.options {
        SessionConfigSelectOptions::Ungrouped(values) => values
            .iter()
            .map(|value| AgentSelectOption {
                id: value.value.0.to_string(),
                display_name: value.name.clone(),
            })
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| &group.options)
            .map(|value| AgentSelectOption {
                id: value.value.0.to_string(),
                display_name: value.name.clone(),
            })
            .collect(),
        _ => Vec::new(),
    };
    Some(ConfigSelect {
        config_id: option.id.clone(),
        current_value: select.current_value.0.to_string(),
        options: values,
    })
}

pub(super) fn extract_session_selects(
    config_options: Option<&[SessionConfigOption]>,
) -> SessionSelects {
    let Some(options) = config_options else {
        return SessionSelects::default();
    };
    SessionSelects {
        model: extract_select(options, SessionConfigOptionCategory::Model, "model"),
        mode: extract_select(options, SessionConfigOptionCategory::Mode, "mode"),
        thought_level: extract_select(
            options,
            SessionConfigOptionCategory::ThoughtLevel,
            "thought_level",
        ),
    }
}

/// Application of the requested selections to a freshly created session via
/// `session/set_config_option`.
///
/// Mostly best-effort: a missing selector or a failed request logs and moves
/// on, leaving that selector on the agent's default. The one hard failure is a
/// value the agent advertises the selector for but no longer offers — a model
/// id that rotted in the catalog cache, say. Running on the default there
/// would record one model on the run while another answered, so the session
/// is refused with [`HarnessErrorKind::SelectionUnavailable`] and the caller
/// can pick again from the list this handshake just refreshed.
///
/// A model switch can change the thought-level select (its levels and its
/// current value), so the level requested after it is checked against the
/// switch's answer rather than `session/new`'s.
pub(super) async fn apply_session_config(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    selects: &SessionSelects,
    config: &SessionConfigRequest,
) -> AppliedSessionConfig {
    let mut applied = AppliedSessionConfig {
        selects: selects.clone(),
        switched: false,
        model: None,
        thought_level: None,
        mode: Ok(None),
    };
    applied.mode = apply_requests(connection, session_id, &mut applied, config).await;
    applied
}

async fn apply_requests(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    applied: &mut AppliedSessionConfig,
    config: &SessionConfigRequest,
) -> Result<Option<String>, HarnessError> {
    let model = apply_config_select(
        connection,
        session_id,
        "model",
        applied.selects.model.as_ref(),
        config.model_id.as_deref(),
    )
    .await?;
    let model_taken = !matches!(model, Applied::Untouched);
    if let Applied::Set(options) = model {
        // ACP answers with the full option list. One without the model select
        // is partial and says nothing about the new model's selects.
        let switched = extract_session_selects(Some(&options));
        if switched.model.is_some() {
            applied.selects = switched;
            applied.switched = true;
        }
    }
    applied.model = confirmed(model_taken, config.model_id.as_deref(), applied.selects.model.as_ref());
    let selects = &applied.selects;
    let mut mode = selects
        .mode
        .as_ref()
        .map(|select| select.current_value.clone());
    let mode_request = config.mode_id.as_deref();
    match apply_config_select(
        connection,
        session_id,
        "mode",
        selects.mode.as_ref(),
        mode_request,
    )
    .await?
    {
        Applied::Untouched => {}
        Applied::AlreadyCurrent | Applied::Set(_) => mode = mode_request.map(str::to_owned),
    }
    let thought_level = apply_config_select(
        connection,
        session_id,
        "thought level",
        selects.thought_level.as_ref(),
        config.thought_level_id.as_deref(),
    )
    .await?;
    applied.thought_level = confirmed(
        !matches!(thought_level, Applied::Untouched),
        config.thought_level_id.as_deref(),
        applied.selects.thought_level.as_ref(),
    );
    Ok(mode)
}

/// The value a select is on after a request: the requested one when the
/// agent took it, else the one it advertised.
fn confirmed(taken: bool, requested: Option<&str>, select: Option<&ConfigSelect>) -> Option<String> {
    requested
        .filter(|_| taken)
        .map(str::to_owned)
        .or_else(|| select.map(|select| select.current_value.clone()))
}

/// Switch a held session's model, then its thought level, before its next
/// prompt. Unlike `session/new`'s best-effort application, a live switch the
/// agent refuses fails with [`HarnessErrorKind::SelectionUnavailable`]: the
/// session is already on some other model, and the run must not claim a model
/// that never answered. An agent advertising no such select keeps today's
/// behavior: the request is ignored. So, for a just-restored session, is a
/// value the load does not list (see [`ModelSwitch::restored`]).
///
/// The caller asks only for values that differ from what the agent last
/// confirmed (`confirmed_model`, `confirmed_level`), so each one requested is
/// sent; the advertised `current_value` may predate a change the agent
/// announced, or be the concrete id behind a restored alias. The one exception
/// is the level after a full switch answer: that answer is current, and the
/// agent may already have moved to the requested level on its own.
pub(super) async fn apply_live_model(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    selects: &SessionSelects,
    confirmed_model: Option<&str>,
    confirmed_level: Option<&str>,
    switch: &ModelSwitch,
) -> AppliedLiveModel {
    let mut applied = AppliedLiveModel {
        selects: selects.clone(),
        switched: false,
        model: confirmed_model.map(str::to_owned),
        thought_level: confirmed_level.map(str::to_owned),
        outcome: Ok(()),
    };
    applied.outcome = switch_model_then_level(connection, session_id, switch, &mut applied).await;
    applied
}

async fn switch_model_then_level(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    switch: &ModelSwitch,
    applied: &mut AppliedLiveModel,
) -> Result<(), HarnessError> {
    let unlisted_on_restore = |select: &ConfigSelect, kind: &str, requested: &str| {
        let unlisted = switch.restored && !advertises(select, requested);
        if unlisted {
            tracing::info!(target: "ottam_core::agent", session_id = %session_id.0, select = kind,
                requested, restored = %select.current_value,
                "restored session does not list the requested value; keeping the restored one");
        }
        unlisted
    };
    let model = switch.model_id.as_deref();
    let thought_level = switch.thought_level_id.as_deref();
    // A kept restore records the pick as current: the restored value stands
    // for it, and later runs on this session must not try to switch to it.
    match (model, applied.selects.model.clone().as_ref()) {
        (None, _) => {}
        (Some(requested), None) => {
            tracing::debug!(target: "ottam_core::agent", session_id = %session_id.0, requested,
                "agent advertised no model select; keeping the session's model");
        }
        (Some(requested), Some(select)) if unlisted_on_restore(select, "model", requested) => {
            applied.model = Some(requested.to_owned());
        }
        (Some(requested), Some(select)) => {
            let answer = set_select_verified(connection, session_id, "model", select, requested, |selects| {
                selects.model.as_ref()
            })
            .await
            .inspect_err(|_| {
                // A switch that went out may have moved the level with it.
                if advertises(select, requested) {
                    applied.model = None;
                    applied.thought_level = None;
                }
            })?;
            let switched = extract_session_selects(Some(&answer));
            if switched.model.is_some() {
                applied.selects = switched;
                applied.switched = true;
                applied.thought_level = applied
                    .selects
                    .thought_level
                    .as_ref()
                    .map(|select| select.current_value.clone());
            }
            applied.model = Some(requested.to_owned());
        }
    }
    let Some(requested) = thought_level else {
        return Ok(());
    };
    let Some(select) = applied.selects.thought_level.clone() else {
        // The app sends a level only to models that offer it; a switch to a
        // model without levels leaves nothing to set.
        tracing::debug!(target: "ottam_core::agent", session_id = %session_id.0, requested,
            "agent advertised no thought level select; keeping the session's level");
        return Ok(());
    };
    if applied.switched && select.current_value == requested {
        return Ok(());
    }
    if unlisted_on_restore(&select, "thought level", requested) {
        applied.thought_level = Some(requested.to_owned());
        return Ok(());
    }
    set_select_verified(connection, session_id, "thought level", &select, requested, |selects| {
        selects.thought_level.as_ref()
    })
    .await
    .inspect_err(|_| {
        if advertises(&select, requested) {
            applied.thought_level = None;
        }
    })?;
    applied.thought_level = Some(requested.to_owned());
    Ok(())
}

/// Whether a select lists a value: a refused request for one was sent, and the
/// agent may have taken it anyway.
fn advertises(select: &ConfigSelect, requested: &str) -> bool {
    select.options.iter().any(|option| option.id == requested)
}

/// Set one select on a held session and require the agent to take it: the
/// value must be advertised, the request must succeed, and an answer that
/// reports this select must report the requested value. An answer without it
/// is partial and accepted, as `session/new` reads one.
async fn set_select_verified(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    kind: &str,
    select: &ConfigSelect,
    requested: &str,
    reported: impl Fn(&SessionSelects) -> Option<&ConfigSelect>,
) -> Result<Vec<SessionConfigOption>, HarnessError> {
    let refused = |reason: &str| {
        tracing::warn!(target: "ottam_core::agent", session_id = %session_id.0, select = kind,
            requested, reason, "agent did not take a live session config change; refusing to prompt");
        HarnessError::new(
            HarnessErrorKind::SelectionUnavailable,
            format!("Could not switch this conversation to the {kind} \"{requested}\". Choose an available {kind}, then send again."),
        )
    };
    if !advertises(select, requested) {
        return Err(refused("not advertised"));
    }
    let answer = connection
        .set_session_config(SetSessionConfigOptionRequest::new(
            session_id.0.clone(),
            select.config_id.clone(),
            requested,
        ))
        .await
        .map_err(|error| refused(&error.message))?;
    let answered = extract_session_selects(Some(&answer));
    if reported(&answered).is_some_and(|select| select.current_value != requested) {
        return Err(refused("acknowledged a different value"));
    }
    tracing::info!(
        target: "ottam_core::agent",
        session_id = %session_id.0,
        select = kind,
        requested,
        "applied session config via session/set_config_option"
    );
    Ok(answer)
}

/// A restored session must not silently run with a different permission policy.
/// Model and effort are not reapplied here: the caller compares the run's pick
/// with what the load reported and switches through [`apply_live_model`].
///
/// Answers the session's mode afterwards.
pub(super) async fn apply_restored_mode(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    selects: &SessionSelects,
    requested: Option<&str>,
) -> Result<Option<String>, HarnessError> {
    let Some(requested) = requested else {
        return Ok(selects
            .mode
            .as_ref()
            .map(|select| select.current_value.clone()));
    };
    let unavailable = || {
        HarnessError::new(
        HarnessErrorKind::SelectionUnavailable,
        format!("Could not restore permission mode \"{requested}\". Choose an available mode and resend."),
    )
    };
    let select = selects.mode.as_ref().ok_or_else(|| HarnessError::new(
        HarnessErrorKind::SelectionUnavailable,
        format!("This agent did not report permission settings when restoring the conversation, so mode \"{requested}\" could not be verified. Start a new conversation to apply this mode."),
    ))?;
    if select.current_value == requested {
        return Ok(Some(requested.to_owned()));
    }
    set_mode_verified(connection, session_id, select, requested, unavailable).await?;
    tracing::info!(session_id = %session_id.0, requested, "restored session permission mode");
    Ok(Some(requested.to_owned()))
}

/// Switch a live session's permission mode before its next prompt. The caller
/// asks only when the requested mode differs from the one the agent last
/// confirmed, so this always sends: the advertised `current_value` may predate
/// a change the agent made itself, such as leaving plan mode.
pub(super) async fn apply_live_mode(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    select: Option<&ConfigSelect>,
    requested: &str,
) -> Result<(), HarnessError> {
    let unavailable = || {
        HarnessError::new(
            HarnessErrorKind::SelectionUnavailable,
            format!("Could not switch to permission mode \"{requested}\". Choose an available mode and resend."),
        )
    };
    let select = select.ok_or_else(unavailable)?;
    set_mode_verified(connection, session_id, select, requested, unavailable).await?;
    tracing::info!(session_id = %session_id.0, requested, "switched live session permission mode");
    Ok(())
}

/// Set the mode and require the agent's acknowledgment to report it.
async fn set_mode_verified(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    select: &ConfigSelect,
    requested: &str,
    unavailable: impl Fn() -> HarnessError,
) -> Result<(), HarnessError> {
    if !select.options.iter().any(|option| option.id == requested) {
        return Err(unavailable());
    }
    let acknowledged = connection
        .set_session_config(SetSessionConfigOptionRequest::new(
            session_id.0.clone(),
            select.config_id.clone(),
            requested,
        ))
        .await
        .map_err(|error| {
            tracing::warn!(session_id = %session_id.0, requested, error = %error.message,
            "failed to set permission mode; refusing to prompt");
            unavailable()
        })?;
    if extract_session_selects(Some(&acknowledged))
        .mode
        .as_ref()
        .is_none_or(|mode| mode.current_value != requested)
    {
        return Err(unavailable());
    }
    Ok(())
}

/// Answers whether the agent took a value Ottam sent, and with what.
async fn apply_config_select(
    connection: &impl SessionConfigSetter,
    session_id: &HarnessSessionId,
    kind: &str,
    select: Option<&ConfigSelect>,
    requested: Option<&str>,
) -> Result<Applied, HarnessError> {
    let advertised = select.map(|select| {
        select
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>()
    });
    tracing::debug!(
        target: "ottam_core::agent",
        session_id = %session_id.0,
        select = kind,
        advertised = ?advertised,
        "session/new advertised config options"
    );
    let Some(requested) = requested else {
        return Ok(Applied::Untouched);
    };
    let Some(select) = select else {
        tracing::warn!(
            target: "ottam_core::agent",
            session_id = %session_id.0,
            select = kind,
            requested,
            "agent advertised no such config option; running on its default"
        );
        return Ok(Applied::Untouched);
    };
    if select.current_value == requested {
        return Ok(Applied::AlreadyCurrent);
    }
    if !select.options.iter().any(|option| option.id == requested) {
        tracing::warn!(
            target: "ottam_core::agent",
            session_id = %session_id.0,
            select = kind,
            requested,
            advertised = ?advertised,
            "requested value is not advertised by the agent; refusing the session"
        );
        return Err(HarnessError::new(
            HarnessErrorKind::SelectionUnavailable,
            format!("This agent no longer offers the {kind} \"{requested}\". Choose an available {kind}, then send again."),
        ));
    }
    let request = SetSessionConfigOptionRequest::new(
        session_id.0.clone(),
        select.config_id.clone(),
        requested,
    );
    match connection.set_session_config(request).await {
        Ok(options) => {
            tracing::info!(
                target: "ottam_core::agent",
                session_id = %session_id.0,
                select = kind,
                requested,
                "applied session config via session/set_config_option"
            );
            Ok(Applied::Set(options))
        }
        Err(error) => {
            tracing::warn!(
                target: "ottam_core::agent",
                session_id = %session_id.0,
                select = kind,
                requested,
                error = %error.message,
                "failed to set session config; running on the agent's default"
            );
            Ok(Applied::Untouched)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::ErrorCode;
    use serde_json::Value;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSetter {
        requests: Mutex<Vec<Value>>,
        failing_config_id: Option<String>,
        acknowledged_mode: Option<String>,
        /// The full option list to answer a model switch with.
        model_switch_answer: Option<Vec<SessionConfigOption>>,
    }

    impl RecordingSetter {
        fn failing(config_id: &str) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                failing_config_id: Some(config_id.to_string()),
                acknowledged_mode: None,
                model_switch_answer: None,
            }
        }

        fn requests(&self) -> Vec<Value> {
            self.requests.lock().expect("recording setter lock").clone()
        }
    }

    impl SessionConfigSetter for RecordingSetter {
        fn set_session_config(
            &self,
            request: SetSessionConfigOptionRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<SessionConfigOption>, agent_client_protocol::Error>>
                    + Send
                    + '_,
            >,
        > {
            let should_fail = self
                .failing_config_id
                .as_deref()
                .is_some_and(|config_id| request.config_id.0.as_ref() == config_id);
            self.requests
                .lock()
                .expect("recording setter lock")
                .push(serde_json::to_value(&request).expect("serializes config request"));
            let switch_answer = self
                .model_switch_answer
                .clone()
                .filter(|_| request.config_id.0.as_ref() == "model-select");
            Box::pin(async move {
                if let Some(answer) = switch_answer {
                    Ok(answer)
                } else if should_fail {
                    Err(agent_client_protocol::Error::from(ErrorCode::InternalError))
                } else {
                    let agent_client_protocol::schema::v1::SessionConfigOptionValue::ValueId {
                        value,
                    } = request.value
                    else {
                        panic!("expected select");
                    };
                    let value = self
                        .acknowledged_mode
                        .clone()
                        .map(Into::into)
                        .unwrap_or(value);
                    Ok(vec![SessionConfigOption::select(
                        request.config_id,
                        "Mode",
                        value,
                        Vec::<agent_client_protocol::schema::v1::SessionConfigSelectOption>::new(),
                    )
                    .category(SessionConfigOptionCategory::Mode)])
                }
            })
        }
    }

    fn select(config_id: &str, current_value: &str, options: &[&str]) -> ConfigSelect {
        ConfigSelect {
            config_id: SessionConfigId::new(config_id),
            current_value: current_value.to_string(),
            options: options
                .iter()
                .map(|value| AgentSelectOption {
                    id: (*value).to_string(),
                    display_name: (*value).to_string(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn restored_mode_reapplies_only_permissions_and_requires_acknowledgment() {
        let setter = RecordingSetter::default();
        let session_id = HarnessSessionId::new("restored");
        let selects = SessionSelects {
            mode: Some(select("permission-mode", "default", &["default", "auto"])),
            model: Some(select("model", "concrete-model", &["concrete-model"])),
            thought_level: None,
        };
        apply_restored_mode(&setter, &session_id, &selects, Some("auto"))
            .await
            .unwrap();
        assert_eq!(
            setter.requests(),
            vec![serde_json::json!({
                "sessionId": "restored", "configId": "permission-mode", "value": "auto"
            })]
        );
        let failing = RecordingSetter::failing("permission-mode");
        let substituted = RecordingSetter {
            acknowledged_mode: Some("acceptEdits".into()),
            ..RecordingSetter::default()
        };
        assert_eq!(
            apply_restored_mode(&substituted, &session_id, &selects, Some("auto"))
                .await
                .unwrap_err()
                .kind,
            HarnessErrorKind::SelectionUnavailable
        );
        assert_eq!(
            apply_restored_mode(&failing, &session_id, &selects, Some("auto"))
                .await
                .unwrap_err()
                .kind,
            HarnessErrorKind::SelectionUnavailable
        );
        assert_eq!(
            apply_restored_mode(&setter, &session_id, &selects, Some("missing"))
                .await
                .unwrap_err()
                .kind,
            HarnessErrorKind::SelectionUnavailable
        );
        assert_eq!(
            apply_restored_mode(
                &setter,
                &session_id,
                &SessionSelects::default(),
                Some("auto")
            )
            .await
            .unwrap_err()
            .kind,
            HarnessErrorKind::SelectionUnavailable
        );
    }

    #[tokio::test]
    async fn restored_mode_leaves_unselected_and_already_matching_sessions_untouched() {
        let setter = RecordingSetter::default();
        let session_id = HarnessSessionId::new("restored");
        apply_restored_mode(&setter, &session_id, &SessionSelects::default(), None)
            .await
            .unwrap();
        let selects = SessionSelects {
            mode: Some(select("mode", "auto", &["auto"])),
            ..SessionSelects::default()
        };
        assert_eq!(
            apply_restored_mode(&setter, &session_id, &selects, Some("auto"))
                .await
                .unwrap()
                .as_deref(),
            Some("auto")
        );
        assert_eq!(
            apply_restored_mode(&setter, &session_id, &selects, None)
                .await
                .unwrap()
                .as_deref(),
            Some("auto"),
            "an unselected restore reports the mode the agent restored"
        );
        assert!(setter.requests().is_empty());
    }

    /// A live session's advertised `current_value` can predate the agent's own
    /// change (leaving plan mode), so a re-pick of that stale value still sends.
    #[tokio::test]
    async fn live_mode_always_sends_and_requires_acknowledgment() {
        let setter = RecordingSetter::default();
        let session_id = HarnessSessionId::new("live");
        let stale = select("mode", "plan", &["default", "plan"]);
        apply_live_mode(&setter, &session_id, Some(&stale), "plan")
            .await
            .unwrap();
        assert_eq!(
            setter.requests(),
            vec![serde_json::json!({
                "sessionId": "live", "configId": "mode", "value": "plan"
            })]
        );
        for error in [
            apply_live_mode(&setter, &session_id, None, "plan").await,
            apply_live_mode(&setter, &session_id, Some(&stale), "missing").await,
            apply_live_mode(
                &RecordingSetter::failing("mode"),
                &session_id,
                Some(&stale),
                "plan",
            )
            .await,
        ] {
            assert_eq!(
                error.unwrap_err().kind,
                HarnessErrorKind::SelectionUnavailable
            );
        }
    }

    #[tokio::test]
    async fn new_session_config_reports_the_resulting_mode() {
        let session_id = HarnessSessionId::new("new");
        let selects = SessionSelects {
            mode: Some(select("mode", "default", &["default", "plan"])),
            ..SessionSelects::default()
        };
        let request = |mode: Option<&str>| SessionConfigRequest {
            model_id: None,
            mode_id: mode.map(str::to_owned),
            thought_level_id: None,
            mcp_servers: Vec::new(),
            meta: None,
        };
        let applied = apply_session_config(
            &RecordingSetter::default(),
            &session_id,
            &selects,
            &request(Some("plan")),
        )
        .await;
        assert_eq!(applied.mode.unwrap().as_deref(), Some("plan"));
        let failed = apply_session_config(
            &RecordingSetter::failing("mode"),
            &session_id,
            &selects,
            &request(Some("plan")),
        )
        .await;
        assert_eq!(
            failed.mode.unwrap().as_deref(),
            Some("default"),
            "a refused set keeps the agent default"
        );
        let unselected = apply_session_config(
            &RecordingSetter::default(),
            &session_id,
            &selects,
            &request(None),
        )
        .await;
        assert_eq!(unselected.mode.unwrap().as_deref(), Some("default"));
    }

    #[test]
    fn model_select_extraction_prefers_category_and_flattens_groups() {
        use agent_client_protocol::schema::v1::{
            SessionConfigOption, SessionConfigSelectGroup, SessionConfigSelectOption,
        };

        // Category wins even when another option is literally named "model".
        let by_category = vec![
            SessionConfigOption::select(
                "model",
                "Decoy",
                "slow",
                vec![SessionConfigSelectOption::new("slow", "Slow")],
            ),
            SessionConfigOption::select(
                "llm",
                "Model",
                "sonnet",
                vec![
                    SessionConfigSelectOption::new("sonnet", "Sonnet"),
                    SessionConfigSelectOption::new("haiku", "Haiku"),
                ],
            )
            .category(SessionConfigOptionCategory::Model),
        ];
        let select = extract_select(&by_category, SessionConfigOptionCategory::Model, "model")
            .expect("model select");
        assert_eq!(select.config_id.0.as_ref(), "llm");
        assert_eq!(select.current_value, "sonnet");
        assert_eq!(
            select
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["sonnet", "haiku"]
        );

        // Falls back to the option whose id is "model" when no category is set.
        let by_id = vec![SessionConfigOption::select(
            "model",
            "Model",
            "gpt",
            vec![SessionConfigSelectOption::new("gpt", "GPT")],
        )];
        let select = extract_select(&by_id, SessionConfigOptionCategory::Model, "model")
            .expect("model select by id");
        assert_eq!(select.config_id.0.as_ref(), "model");

        // Grouped values are flattened.
        let grouped = vec![SessionConfigOption::select(
            "model",
            "Model",
            "a",
            vec![
                SessionConfigSelectGroup::new(
                    "fast",
                    "Fast",
                    vec![SessionConfigSelectOption::new("a", "A")],
                ),
                SessionConfigSelectGroup::new(
                    "smart",
                    "Smart",
                    vec![SessionConfigSelectOption::new("b", "B")],
                ),
            ],
        )];
        let select = extract_select(&grouped, SessionConfigOptionCategory::Model, "model")
            .expect("grouped model select");
        assert_eq!(
            select
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        // Boolean options and absent config options yield nothing.
        let boolean = vec![SessionConfigOption::boolean("model", "Model", true)];
        assert_eq!(
            extract_select(&boolean, SessionConfigOptionCategory::Model, "model"),
            None
        );
        assert_eq!(
            extract_select(&[], SessionConfigOptionCategory::Model, "model"),
            None
        );
    }

    /// Mode and thought-level selects ride the same extraction; the advertised
    /// snapshot records "none" as empty lists so it is cacheable either way.
    #[test]
    fn session_selects_capture_mode_and_thought_level() {
        use agent_client_protocol::schema::v1::{SessionConfigOption, SessionConfigSelectOption};

        let options = vec![
            SessionConfigOption::select(
                "permission-mode",
                "Mode",
                "auto",
                vec![
                    SessionConfigSelectOption::new("auto", "Auto"),
                    SessionConfigSelectOption::new("plan", "Plan"),
                ],
            )
            .category(SessionConfigOptionCategory::Mode),
            SessionConfigOption::select(
                "effort",
                "Effort",
                "medium",
                vec![
                    SessionConfigSelectOption::new("low", "Low"),
                    SessionConfigSelectOption::new("medium", "Medium"),
                    SessionConfigSelectOption::new("high", "High"),
                ],
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
        ];
        let selects = extract_session_selects(Some(&options));
        assert_eq!(selects.model, None);
        let advertised = selects.advertised();
        assert_eq!(
            advertised
                .modes
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["auto", "plan"]
        );
        assert_eq!(advertised.default_mode_id.as_deref(), Some("auto"));
        assert_eq!(
            advertised
                .thought_levels
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high"]
        );
        assert_eq!(
            advertised.default_thought_level_id.as_deref(),
            Some("medium")
        );

        // An agent with no selects still yields a real (empty) answer.
        let none = extract_session_selects(Some(&[])).advertised();
        assert!(none.modes.is_empty());
        assert_eq!(none.default_mode_id, None);
        assert!(none.thought_levels.is_empty());
        assert_eq!(none.default_thought_level_id, None);
    }

    #[tokio::test]
    async fn applying_session_config_orders_requests_and_skips_inapplicable_values() {
        let session_id = HarnessSessionId::new("session-config-test");
        let selects = SessionSelects {
            model: Some(select("model-select", "sonnet", &["sonnet", "opus"])),
            mode: Some(select("mode-select", "auto", &["auto", "plan"])),
            thought_level: Some(select(
                "thought-select",
                "medium",
                &["low", "medium", "high"],
            )),
        };
        let setter = RecordingSetter::default();
        apply_session_config(
            &setter,
            &session_id,
            &selects,
            &SessionConfigRequest {
                model_id: Some("opus".to_string()),
                mode_id: Some("auto".to_string()),
                thought_level_id: Some("high".to_string()),
                mcp_servers: Vec::new(),
                meta: None,
            },
        )
        .await
        .mode
        .expect("advertised selections apply");
        assert_eq!(
            setter.requests(),
            vec![
                serde_json::json!({
                    "sessionId": "session-config-test",
                    "configId": "model-select",
                    "value": "opus"
                }),
                serde_json::json!({
                    "sessionId": "session-config-test",
                    "configId": "thought-select",
                    "value": "high"
                }),
            ],
            "model, mode, and thought level are considered in that order; the current mode is skipped"
        );

        let skipped = RecordingSetter::default();
        apply_session_config(
            &skipped,
            &session_id,
            &SessionSelects {
                model: Some(select("model-select", "sonnet", &["sonnet", "opus"])),
                mode: None,
                thought_level: None,
            },
            &SessionConfigRequest {
                model_id: Some("sonnet".to_string()),
                mode_id: Some("plan".to_string()),
                thought_level_id: None,
                mcp_servers: Vec::new(),
                meta: None,
            },
        )
        .await
        .mode
        .expect("a missing selector is not a refusal");
        assert!(
            skipped.requests().is_empty(),
            "current values and missing selectors stay on the agent default"
        );
    }

    fn select_option(
        config_id: &str,
        category: SessionConfigOptionCategory,
        current_value: &str,
        values: &[&str],
    ) -> SessionConfigOption {
        SessionConfigOption::select(
            config_id.to_string(),
            config_id.to_string(),
            current_value.to_string(),
            values
                .iter()
                .map(|value| {
                    agent_client_protocol::schema::v1::SessionConfigSelectOption::new(
                        (*value).to_string(),
                        (*value).to_string(),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .category(category)
    }

    /// omp applies each model's own default level on a switch and Claude
    /// re-lists its levels per model: the switch's answer is what the run
    /// gets without an explicit level, and what a requested level must match.
    #[tokio::test]
    async fn a_model_switch_reports_and_validates_against_the_new_models_levels() {
        let session_id = HarnessSessionId::new("switch");
        let selects = SessionSelects {
            model: Some(select("model-select", "kimi", &["kimi", "glm"])),
            mode: None,
            thought_level: Some(select("thought-select", "high", &["low", "high"])),
        };
        let switching = || RecordingSetter {
            model_switch_answer: Some(vec![
                select_option(
                    "model-select",
                    SessionConfigOptionCategory::Model,
                    "glm",
                    &["kimi", "glm"],
                ),
                select_option(
                    "thought-select",
                    SessionConfigOptionCategory::ThoughtLevel,
                    "max",
                    &["low", "max"],
                ),
            ]),
            ..RecordingSetter::default()
        };
        let request = |thought_level_id: Option<&str>| SessionConfigRequest {
            model_id: Some("glm".to_string()),
            mode_id: None,
            thought_level_id: thought_level_id.map(str::to_owned),
            mcp_servers: Vec::new(),
            meta: None,
        };

        let setter = switching();
        let applied = apply_session_config(&setter, &session_id, &selects, &request(None)).await;
        assert!(applied.mode.is_ok(), "the switch applies");
        assert!(applied.switched);
        assert_eq!(
            applied.selects.model_selects(),
            Some((
                "glm".to_string(),
                AgentModelSelects {
                    thought_levels: select("t", "max", &["low", "max"]).options,
                    default_thought_level_id: Some("max".to_string()),
                }
            ))
        );
        assert_eq!(setter.requests().len(), 1, "no level was requested");

        let setter = switching();
        apply_session_config(&setter, &session_id, &selects, &request(Some("max")))
            .await
            .mode
            .expect("a level only the new model offers is accepted");
        assert_eq!(
            setter.requests().len(),
            1,
            "the switch already put the session on max"
        );

        let setter = switching();
        let refused =
            apply_session_config(&setter, &session_id, &selects, &request(Some("high"))).await;
        assert!(
            refused.switched,
            "a refusal still reports the switched model, so its levels are learned"
        );
        assert_eq!(
            refused.selects.model_selects(),
            applied.selects.model_selects()
        );
        assert_eq!(
            refused
                .mode
                .expect_err("a level the new model dropped is refused")
                .kind,
            HarnessErrorKind::SelectionUnavailable
        );

        assert_eq!(
            selects.model_selects(),
            Some((
                "kimi".to_string(),
                AgentModelSelects {
                    thought_levels: select("t", "high", &["low", "high"]).options,
                    default_thought_level_id: Some("high".to_string()),
                }
            )),
            "session/new reports the starting model's own select"
        );
    }

    fn live_selects() -> SessionSelects {
        SessionSelects {
            model: Some(select("model-select", "kimi", &["kimi", "glm"])),
            mode: Some(select("mode-select", "default", &["default", "plan"])),
            thought_level: Some(select("thought-select", "high", &["low", "high"])),
        }
    }

    /// The switched model answers with its own levels and default (omp).
    fn switching_to_glm() -> RecordingSetter {
        RecordingSetter {
            model_switch_answer: Some(vec![
                select_option(
                    "model-select",
                    SessionConfigOptionCategory::Model,
                    "glm",
                    &["kimi", "glm"],
                ),
                select_option(
                    "thought-select",
                    SessionConfigOptionCategory::ThoughtLevel,
                    "max",
                    &["low", "max"],
                ),
            ]),
            ..RecordingSetter::default()
        }
    }

    fn model_switch(model: Option<&str>, level: Option<&str>, restored: bool) -> ModelSwitch {
        ModelSwitch {
            model_id: model.map(str::to_owned),
            thought_level_id: level.map(str::to_owned),
            restored,
        }
    }

    /// Switch the live session, which the agent confirmed on kimi at high.
    async fn switch_live(setter: &RecordingSetter, switch: ModelSwitch) -> AppliedLiveModel {
        apply_live_model(
            setter,
            &HarnessSessionId::new("live"),
            &live_selects(),
            Some("kimi"),
            Some("high"),
            &switch,
        )
        .await
    }

    /// The live session (omp, mid-conversation) takes the model, then the
    /// level against the switched model's own select.
    #[tokio::test]
    async fn live_model_switch_sets_the_model_then_the_level() {
        let setter = switching_to_glm();
        let applied = switch_live(&setter, model_switch(Some("glm"), Some("low"), false)).await;
        applied.outcome.as_ref().unwrap();
        assert_eq!(
            setter.requests(),
            vec![
                serde_json::json!({"sessionId": "live", "configId": "model-select", "value": "glm"}),
                serde_json::json!({"sessionId": "live", "configId": "thought-select", "value": "low"}),
            ]
        );
        assert!(applied.switched);
        assert_eq!(applied.model.as_deref(), Some("glm"));
        assert_eq!(applied.thought_level.as_deref(), Some("low"));
        assert_eq!(
            applied.selects.thought_level().unwrap().options,
            select("t", "max", &["low", "max"]).options,
            "the session keeps the switched model's levels"
        );

        let setter = switching_to_glm();
        let applied = switch_live(&setter, model_switch(Some("glm"), Some("max"), false)).await;
        applied.outcome.as_ref().unwrap();
        assert_eq!(setter.requests().len(), 1, "the switch already put it on max");
        assert_eq!(applied.thought_level.as_deref(), Some("max"));

        let setter = RecordingSetter::default();
        let applied = switch_live(&setter, model_switch(None, Some("low"), false)).await;
        applied.outcome.as_ref().unwrap();
        assert_eq!(
            setter.requests(),
            vec![serde_json::json!({"sessionId": "live", "configId": "thought-select", "value": "low"})],
            "a level change alone leaves the model alone"
        );
        assert_eq!(applied.model.as_deref(), Some("kimi"));
    }

    /// A live switch is strict where `session/new` is best-effort: the run
    /// must not prompt the old model while recording the new one.
    #[tokio::test]
    async fn live_model_switch_refusals_fail_the_run() {
        let substituted = RecordingSetter {
            model_switch_answer: Some(vec![select_option(
                "model-select",
                SessionConfigOptionCategory::Model,
                "kimi",
                &["kimi", "glm"],
            )]),
            ..RecordingSetter::default()
        };
        for (setter, switch) in [
            (RecordingSetter::failing("model-select"), model_switch(Some("glm"), None, false)),
            (substituted, model_switch(Some("glm"), None, false)),
            (RecordingSetter::default(), model_switch(Some("gone"), None, false)),
            (switching_to_glm(), model_switch(Some("glm"), Some("high"), false)),
            (RecordingSetter::failing("thought-select"), model_switch(None, Some("low"), false)),
        ] {
            let applied = switch_live(&setter, switch).await;
            assert_eq!(
                applied.outcome.expect_err("refused").kind,
                HarnessErrorKind::SelectionUnavailable
            );
        }

        let setter = RecordingSetter::default();
        let ignored = apply_live_model(
            &setter,
            &HarnessSessionId::new("live"),
            &SessionSelects::default(),
            None,
            None,
            &model_switch(Some("glm"), Some("low"), false),
        )
        .await;
        ignored
            .outcome
            .expect("an agent with no model select keeps today's behavior");
        assert_eq!(ignored.model, None);
        assert!(setter.requests().is_empty());
    }

    /// A refused switch still reports what the agent took before it, and
    /// forgets what it may have taken unconfirmed, so the next run compares
    /// against the agent's real model instead of skipping a needed switch.
    #[tokio::test]
    async fn a_refused_live_switch_keeps_what_the_agent_confirmed() {
        let level_refused = RecordingSetter {
            failing_config_id: Some("thought-select".to_string()),
            ..switching_to_glm()
        };
        let applied = switch_live(&level_refused, model_switch(Some("glm"), Some("low"), false)).await;
        assert!(applied.outcome.is_err());
        assert!(applied.switched, "the model switch went through");
        assert_eq!(applied.model.as_deref(), Some("glm"));
        assert_eq!(applied.thought_level, None, "the refused level may have been taken");
        assert_eq!(applied.selects.model.unwrap().current_value, "glm");

        let model_refused =
            switch_live(&RecordingSetter::failing("model-select"), model_switch(Some("glm"), Some("low"), false)).await;
        assert!(model_refused.outcome.is_err());
        assert_eq!((model_refused.model, model_refused.thought_level), (None, None));

        let unlisted = switch_live(&RecordingSetter::default(), model_switch(Some("gone"), None, false)).await;
        assert!(unlisted.outcome.is_err());
        assert_eq!(
            (unlisted.model.as_deref(), unlisted.thought_level.as_deref()),
            (Some("kimi"), Some("high")),
            "nothing went out for a value the agent doesn't list"
        );
    }

    /// A load can list concrete ids rather than the alias a run saved: the
    /// restored session keeps its model, and the pick stands for it so later
    /// runs don't try to switch to it again.
    #[tokio::test]
    async fn restored_session_keeps_a_model_the_load_does_not_list() {
        let setter = RecordingSetter::default();
        let selects = SessionSelects {
            model: Some(select("model-select", "claude-sonnet-4-5-20250929", &["claude-sonnet-4-5-20250929"])),
            mode: None,
            thought_level: Some(select("thought-select", "high", &["low", "high"])),
        };
        let concrete = Some("claude-sonnet-4-5-20250929");
        let applied = apply_live_model(
            &setter,
            &HarnessSessionId::new("restored"),
            &selects,
            concrete,
            Some("high"),
            &model_switch(Some("sonnet"), Some("medium"), true),
        )
        .await;
        applied.outcome.as_ref().unwrap();
        assert!(setter.requests().is_empty());
        assert_eq!(applied.model.as_deref(), Some("sonnet"));
        assert_eq!(applied.thought_level.as_deref(), Some("medium"));

        // A later level change keeps the alias the session confirmed rather
        // than the concrete id the load listed.
        let level_only = apply_live_model(
            &setter,
            &HarnessSessionId::new("restored"),
            &selects,
            Some("sonnet"),
            Some("medium"),
            &model_switch(None, Some("low"), false),
        )
        .await;
        level_only.outcome.as_ref().unwrap();
        assert_eq!(level_only.model.as_deref(), Some("sonnet"));
        assert_eq!(level_only.thought_level.as_deref(), Some("low"));

        let error = apply_live_model(
            &setter,
            &HarnessSessionId::new("live"),
            &selects,
            concrete,
            Some("high"),
            &model_switch(Some("sonnet"), None, false),
        )
        .await
        .outcome
        .unwrap_err();
        assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable, "a live session is strict");
    }

    #[tokio::test]
    async fn new_session_config_reports_the_confirmed_model_and_level() {
        let session_id = HarnessSessionId::new("new");
        let request = |model: Option<&str>, level: Option<&str>| SessionConfigRequest {
            model_id: model.map(str::to_owned),
            mode_id: None,
            thought_level_id: level.map(str::to_owned),
            mcp_servers: Vec::new(),
            meta: None,
        };
        let applied = apply_session_config(
            &switching_to_glm(),
            &session_id,
            &live_selects(),
            &request(Some("glm"), Some("low")),
        )
        .await;
        assert_eq!(applied.model.as_deref(), Some("glm"));
        assert_eq!(applied.thought_level.as_deref(), Some("low"));
        let defaults =
            apply_session_config(&RecordingSetter::default(), &session_id, &live_selects(), &request(None, None))
                .await;
        assert_eq!(defaults.model.as_deref(), Some("kimi"));
        assert_eq!(defaults.thought_level.as_deref(), Some("high"));
        let failed = apply_session_config(
            &RecordingSetter::failing("model-select"),
            &session_id,
            &live_selects(),
            &request(Some("glm"), None),
        )
        .await;
        assert_eq!(
            failed.model.as_deref(),
            Some("kimi"),
            "a set the agent refused is not recorded as taken"
        );
    }

    /// The catalog-rot case: the agent still has a model selector, but the id
    /// the user picked (from a cached list) is no longer on it. Running on the
    /// default would record one model on the run while another answered, so
    /// the session is refused before anything later is applied.
    #[tokio::test]
    async fn an_unadvertised_selection_refuses_the_session() {
        let setter = RecordingSetter::default();
        let error = apply_session_config(
            &setter,
            &HarnessSessionId::new("stale-model-session"),
            &SessionSelects {
                model: Some(select(
                    "model-select",
                    "opus[1m]",
                    &["opus[1m]", "claude-fable-5-1[1m]"],
                )),
                mode: None,
                thought_level: Some(select("thought-select", "medium", &["low", "high"])),
            },
            &SessionConfigRequest {
                model_id: Some("claude-fable-5[1m]".to_string()),
                mode_id: None,
                thought_level_id: Some("high".to_string()),
                mcp_servers: Vec::new(),
                meta: None,
            },
        )
        .await
        .mode
        .expect_err("a stale model id refuses the session");
        assert_eq!(error.kind, HarnessErrorKind::SelectionUnavailable);
        assert!(
            error.message.contains("claude-fable-5[1m]"),
            "the message names the stale id: {}",
            error.message
        );
        assert!(
            setter.requests().is_empty(),
            "nothing is applied to a session that is being refused"
        );
    }

    #[tokio::test]
    async fn a_failed_session_config_request_does_not_block_later_selections() {
        let setter = RecordingSetter::failing("model-select");
        apply_session_config(
            &setter,
            &HarnessSessionId::new("best-effort-session"),
            &SessionSelects {
                model: Some(select("model-select", "sonnet", &["sonnet", "opus"])),
                mode: Some(select("mode-select", "auto", &["auto", "plan"])),
                thought_level: None,
            },
            &SessionConfigRequest {
                model_id: Some("opus".to_string()),
                mode_id: Some("plan".to_string()),
                thought_level_id: None,
                mcp_servers: Vec::new(),
                meta: None,
            },
        )
        .await
        .mode
        .expect("a rejected request is best-effort, not a refusal");
        assert_eq!(
            setter
                .requests()
                .iter()
                .map(|request| request["configId"].as_str().expect("configId"))
                .collect::<Vec<_>>(),
            ["model-select", "mode-select"],
            "a failed setter call is best-effort and does not stop later selections"
        );
    }
}
