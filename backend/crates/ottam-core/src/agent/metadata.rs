//! Explicit mappings for Codex ACP 2.1.1 and Claude ACP 0.85.1, both of
//! which treat Ottam as an AIR client (`_meta.jetbrains.air`).
use ottam_protocol::{
    ConversationSessionFailure, ConversationSessionLimit, ConversationSessionSignal,
    TranscriptAdapterMetadata,
};
use serde_json::Value;

fn text(value: Option<&Value>, limit: usize) -> Option<String> {
    let value = value?.as_str()?;
    if value.len() > limit {
        return None;
    }
    Some(value.chars().filter(|c| !c.is_control()).collect())
}

pub(crate) fn has_unknown_extensions(value: &Value) -> bool {
    let Some(meta) = value.get("_meta").and_then(Value::as_object) else {
        return false;
    };
    meta.iter().any(|(namespace, fields)| {
        let known: &[&str] = match namespace.as_str() {
            "codex" => &["threadStatus", "error"],
            "claudeCode" => &["parentToolUseId", "toolName"],
            "jetbrains" => {
                return fields.pointer("/air/version").and_then(Value::as_u64) != Some(1)
                    || fields
                        .pointer("/air")
                        .and_then(Value::as_object)
                        .is_some_and(|air| {
                            air.keys()
                                .any(|key| !matches!(key.as_str(), "version" | "sessionFailure"))
                        })
            }
            // `session_signal` reads the first three and plan usage reads
            // `unifiedWindows` (`agent/plan_usage.rs`); the overage fields
            // are unused.
            "_claude/rateLimit" => &[
                "status",
                "rateLimitType",
                "resetsAt",
                "overageStatus",
                "overageResetsAt",
                "overageDisabledReason",
                "isUsingOverage",
                "unifiedWindows",
            ],
            "_claude/origin" => &["kind"],
            // A model id string on each `usage_update`; unused until per-model
            // usage lands.
            "_claude/model" => &[],
            "terminal_info" => &["terminal_id", "cwd"],
            "terminal_output" | "terminal_output_delta" => &["terminal_id", "data"],
            "terminal_exit" => &["terminal_id", "exit_code", "signal"],
            // Core moves a tagged child's frames to its own session
            // (`agent/acp/child_sessions.rs`).
            "opencode/child-session" => &["id", "parentID", "depth", "title"],
            _ => return true,
        };
        fields
            .as_object()
            .is_some_and(|fields| fields.keys().any(|key| !known.contains(&key.as_str())))
    })
}

/// Result origins claude-agent-acp lists in `AUTONOMOUS_RESULT_ORIGINS`
/// (unchanged from 0.85.1 to 0.95.0): cycles the agent ran on its own, never a
/// user's turn.
const AUTONOMOUS_ORIGINS: &[&str] = &[
    "task-notification",
    "peer",
    "coordinator",
    "observer",
    "observer-activity",
];

/// Whether a `usage_update` ends a cycle the agent started on its own. Claude
/// stamps each result's usage with `_meta["_claude/origin"]`; user turns carry
/// other kinds (`human`, `channel`).
pub(crate) fn autonomous_origin(value: &Value) -> bool {
    value
        .pointer("/_meta/_claude~1origin/kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| AUTONOMOUS_ORIGINS.contains(&kind))
}

pub(crate) fn transcript_metadata(value: &Value) -> TranscriptAdapterMetadata {
    let air = value
        .pointer("/_meta/jetbrains/air")
        .filter(|air| air.get("version").and_then(Value::as_u64) == Some(1));
    TranscriptAdapterMetadata {
        // Codex's message phase; AIR moved it out of `_meta.codex`.
        phase: text(air.and_then(|air| air.get("phase")), 64)
            .filter(|phase| matches!(phase.as_str(), "commentary" | "final_answer")),
        parent_tool_call_id: text(value.pointer("/_meta/claudeCode/parentToolUseId"), 4096),
        // Neither pinned adapter currently exposes a parent message reference.
        parent_message_id: None,
        session_id: None,
        // Core's routing sets it, never the adapter.
        subagent_thread_id: None,
    }
}

pub(crate) fn session_signal(value: &Value) -> Option<ConversationSessionSignal> {
    let mut signal = ConversationSessionSignal {
        activity: text(value.pointer("/_meta/codex/threadStatus/type"), 64).filter(|kind| {
            matches!(
                kind.as_str(),
                "active" | "idle" | "notLoaded" | "systemError"
            )
        }),
        ..Default::default()
    };
    if value
        .pointer("/_meta/jetbrains/air/version")
        .and_then(Value::as_u64)
        == Some(1)
    {
        if let Some(failure) = value.pointer("/_meta/jetbrains/air/sessionFailure") {
            if let (Some(id), Some(revision), Some(category), Some(severity), Some(message)) = (
                text(failure.get("id"), 4096),
                failure.get("revision").and_then(Value::as_u64),
                text(failure.get("category"), 64),
                text(failure.get("severity"), 16),
                text(failure.get("title"), 4096),
            ) {
                if matches!(severity.as_str(), "warning" | "error") {
                    signal.failure = Some(ConversationSessionFailure {
                        id,
                        revision: Some(revision),
                        category,
                        severity,
                        message,
                        retryable: failure.get("actions").and_then(Value::as_array).map(
                            |actions| {
                                actions
                                    .iter()
                                    .any(|action| action.as_str() == Some("retry"))
                            },
                        ),
                        retry_at: None,
                    });
                }
            }
        }
    }
    // Legacy Codex status errors are still accepted for peers that did not
    // negotiate AIR. They are observations, never an independent Run terminal.
    if let Some(error) = value.pointer("/_meta/codex/error") {
        if let Some(message) = text(error.get("message"), 4096) {
            let retry = error.get("willRetry").and_then(Value::as_bool);
            signal.failure = Some(ConversationSessionFailure {
                id: text(error.get("turnId"), 4096).unwrap_or_else(|| "session-error".into()),
                revision: None,
                category: "service".into(),
                severity: if retry == Some(true) {
                    "warning"
                } else {
                    "error"
                }
                .into(),
                message,
                retryable: retry,
                retry_at: None,
            });
        }
    }
    if let Some(limit) = value.pointer("/_meta/_claude~1rateLimit") {
        if let Some(status) = text(limit.get("status"), 64) {
            signal.limit = Some(ConversationSessionLimit {
                status,
                kind: text(limit.get("rateLimitType"), 64),
                resets_at: limit.get("resetsAt").and_then(Value::as_i64),
            });
        }
    }
    if signal.failure.as_ref().is_some_and(|failure| {
        failure.severity == "warning"
            && matches!(
                failure.category.as_str(),
                "connection" | "service" | "limit"
            )
    }) {
        signal.activity = Some("retrying".into());
    }
    (signal.activity.is_some() || signal.failure.is_some() || signal.limit.is_some())
        .then_some(signal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pinned_adapter_metadata_fixtures_are_owned_and_bounded() {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/session-signals.json")).unwrap();
        assert_eq!(
            transcript_metadata(&fixtures[0]).phase.as_deref(),
            Some("commentary")
        );
        assert_eq!(
            transcript_metadata(&fixtures[1])
                .parent_tool_call_id
                .as_deref(),
            Some("tool-1")
        );
        assert_eq!(
            session_signal(&fixtures[2]).unwrap().activity.as_deref(),
            Some("active")
        );
        let retry = session_signal(&fixtures[3]).unwrap();
        assert_eq!(retry.activity.as_deref(), Some("retrying"));
        assert_eq!(retry.failure.unwrap().revision, Some(1));
        assert_eq!(
            session_signal(&fixtures[4])
                .unwrap()
                .limit
                .unwrap()
                .resets_at,
            Some(2000000000)
        );
        assert_eq!(
            session_signal(&fixtures[5])
                .unwrap()
                .failure
                .unwrap()
                .retryable,
            Some(true)
        );
        assert!(session_signal(&json!({"_meta":{"unknown":{"body":"never retained"}}})).is_none());
        let mut future = fixtures[3].clone();
        future["_meta"]["jetbrains"]["air"]["version"] = json!(2);
        assert!(session_signal(&future).is_none());
        let mut oversized = fixtures[5].clone();
        oversized["_meta"]["jetbrains"]["air"]["sessionFailure"]["title"] = json!("x".repeat(5000));
        assert!(session_signal(&oversized).is_none());
        assert_eq!(
            transcript_metadata(
                &json!({"_meta":{"jetbrains":{"air":{"version":1,"phase":"future"}}}})
            )
            .phase,
            None
        );
        assert_eq!(
            transcript_metadata(
                &json!({"_meta":{"jetbrains":{"air":{"version":2,"phase":"commentary"}}}})
            )
            .phase,
            None
        );
    }

    #[test]
    fn claude_usage_model_is_a_known_extension() {
        let usage = json!({
            "sessionUpdate": "usage_update",
            "used": 1,
            "size": 2,
            "_meta": {"_claude/model": "claude-sonnet-5-5", "_claude/origin": {"kind": "user"}}
        });
        assert!(!has_unknown_extensions(&usage));
        assert!(has_unknown_extensions(
            &json!({"_meta": {"_claude/future": "value"}})
        ));
    }

    /// The pinned fixtures' `usage_update`s and `session_info_update`s reach
    /// the runner as `AgentUpdate::Other`, which logs unknown `_meta` keys.
    #[test]
    fn pinned_fixture_updates_have_no_unknown_extensions() {
        let fixtures = [
            include_str!("fixtures/claude-agent-acp-0.100.0.json"),
            include_str!("fixtures/codex-acp-2.5.0.json"),
            include_str!("fixtures/omp-18.16.0.json"),
            include_str!("fixtures/opencode-2.2.5.json"),
        ];
        let mut checked = 0;
        for fixture in fixtures {
            let frames: Vec<Value> = serde_json::from_str(fixture).unwrap();
            for update in frames
                .iter()
                .filter_map(|frame| frame.pointer("/params/update"))
            {
                let kind = update.get("sessionUpdate").and_then(Value::as_str);
                if matches!(kind, Some("usage_update" | "session_info_update")) {
                    assert!(!has_unknown_extensions(update), "{update}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }
}
