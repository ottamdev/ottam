//! Oh-My-Pi mounts rarely used tools, MCP tools among them, as `xd://<tool>`
//! devices (its `tools.xdev` setting, on by default). The model runs one with
//! `write xd://<tool>`, the JSON arguments as the content, and reads its docs
//! with `read xd://<tool>[/<topic>]`. Both reach Ottam as an ordinary
//! `write` or `read` with a `path`, which would record a tool call as a file
//! change. This rewrites them as what they are; the caption omp writes for
//! the call stays its title. Only Oh-My-Pi's updates and permission requests
//! pass through it.
use super::super::types::HarnessUpdateKind;
use serde_json::Value;

const SCHEME: &str = "xd://";

/// Marks a call this rewrites as a device run: its `rawInput` is the device's
/// arguments, never a file change, whatever omp captions it or the device is
/// called (not every device is an `mcp__` tool).
pub(crate) const DEVICE_CALL_STAMP: &str = "ottamDeviceCall";

/// Rewrites the tool call a session update carries; see [`rewrite`].
pub(super) fn rewrite_update(update: HarnessUpdateKind) -> HarnessUpdateKind {
    match update {
        HarnessUpdateKind::ToolCall(payload) => HarnessUpdateKind::ToolCall(rewrite(payload)),
        HarnessUpdateKind::ToolCallUpdate(payload) => {
            HarnessUpdateKind::ToolCallUpdate(rewrite(payload))
        }
        other => other,
    }
}

/// Rewrites a `tool_call` or `tool_call_update` payload that drives an
/// `xd://` device; leaves every other payload alone. Any verb but `read`
/// runs the device, named by its whole path (`<tool>[/<topic>]`).
pub(super) fn rewrite(mut payload: Value) -> Value {
    let Some(device) = payload
        .pointer("/rawInput/path")
        .and_then(Value::as_str)
        .and_then(|path| path.strip_prefix(SCHEME))
        .map(|device| device.trim_end_matches('/'))
        .filter(|device| !device.is_empty())
        .map(str::to_owned)
    else {
        return payload;
    };
    let runs_tool = match payload.get("name").and_then(Value::as_str) {
        Some(name) => name != "read",
        None => payload.pointer("/rawInput/content").is_some(),
    };
    // Neither is a file: no file path to open, no change to show.
    payload["kind"] = "other".into();
    if !runs_tool {
        return payload;
    }
    let mut input = payload["rawInput"].take();
    let arguments = match input.get_mut("content").map(Value::take) {
        Some(Value::String(text)) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Some(value) => value,
        // Another verb's input, minus the device it named.
        None => {
            if let Some(fields) = input.as_object_mut() {
                fields.remove("path");
            }
            input
        }
    };
    payload["name"] = device.into();
    payload["rawInput"] = arguments;
    payload[DEVICE_CALL_STAMP] = true.into();
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Oh-My-Pi 18.16.0, recorded 2026-11-03 with an MCP server named
    /// `ottam` (`omp-18.16.0-xd-devices.json`).
    #[test]
    fn a_device_write_is_a_call_of_that_tool() {
        let call = rewrite(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "write_1_abfbb2166",
            "name": "write",
            "title": "Message parent done",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"path": "xd://mcp__ottam_message_parent", "content": "{\"message\":\"done\"}"}
        }));
        assert_eq!(call["name"], "mcp__ottam_message_parent");
        assert_eq!(call["kind"], "other");
        assert_eq!(call["title"], "Message parent done");
        assert_eq!(call["rawInput"], json!({"message": "done"}));
        assert_eq!(call[DEVICE_CALL_STAMP], true);
    }

    #[test]
    fn unparsable_arguments_stay_text_and_missing_ones_are_empty() {
        let text = rewrite(json!({
            "name": "write",
            "rawInput": {"path": "xd://report_issue", "content": "not json"}
        }));
        assert_eq!(text["name"], "report_issue");
        assert_eq!(text["rawInput"], "not json");
        let bare = rewrite(json!({"name": "write", "rawInput": {"path": "xd://report_issue"}}));
        assert_eq!(bare["rawInput"], json!({}));
    }

    #[test]
    fn other_verbs_run_the_device_and_topics_stay_in_its_name() {
        let edit = rewrite(json!({
            "name": "edit",
            "kind": "edit",
            "rawInput": {"path": "xd://mcp__ottam_message_parent", "oldText": "a", "newText": "b"}
        }));
        assert_eq!(edit["name"], "mcp__ottam_message_parent");
        assert_eq!(edit["kind"], "other");
        assert_eq!(edit["rawInput"], json!({"oldText": "a", "newText": "b"}));
        let topic = rewrite(json!({
            "name": "write",
            "rawInput": {"path": "xd://mcp__ottam_get_session_messages/notes/", "content": "{}"}
        }));
        assert_eq!(topic["name"], "mcp__ottam_get_session_messages/notes");
        assert_eq!(topic["rawInput"], json!({}));
    }

    /// Oh-My-Pi 18.12.6, the pin before 18.16.0, may send no `name` (not
    /// recorded); content marks a run.
    #[test]
    fn without_a_name_content_decides() {
        let run = rewrite(json!({
            "rawInput": {"path": "xd://mcp__ottam_get_conversation_info", "content": "{}"}
        }));
        assert_eq!(run["name"], "mcp__ottam_get_conversation_info");
        assert_eq!(run["rawInput"], json!({}));
        let docs =
            rewrite(json!({"rawInput": {"path": "xd://mcp__ottam_get_conversation_info"}}));
        assert_eq!(docs.get("name"), None);
        assert_eq!(docs["kind"], "other");
    }

    #[test]
    fn a_device_read_is_not_a_file_read() {
        let docs = rewrite(json!({
            "name": "read",
            "title": "Reading tool docs",
            "kind": "read",
            "rawInput": {"path": "xd://mcp__ottam_get_conversation_info"}
        }));
        assert_eq!(docs["name"], "read");
        assert_eq!(docs["kind"], "other");
        assert_eq!(docs.get(DEVICE_CALL_STAMP), None, "a docs read runs nothing");
        assert_eq!(
            docs["rawInput"],
            json!({"path": "xd://mcp__ottam_get_conversation_info"})
        );
    }

    #[test]
    fn other_paths_and_updates_are_untouched() {
        for payload in [
            json!({"name": "write", "kind": "edit", "rawInput": {"path": "notes.txt", "content": "x"}}),
            json!({"name": "read", "kind": "read", "rawInput": {"path": "local://scratch.md"}}),
            json!({"name": "write", "kind": "edit", "rawInput": {"path": "xd://", "content": "{}"}}),
            json!({"toolCallId": "write_0", "status": "completed"}),
        ] {
            assert_eq!(rewrite(payload.clone()), payload);
        }
    }
}
