//! Consume content at the adapter boundary. Only owned fields and file bytes
//! survive; transport envelopes never enter the transcript checkpoint writer.
use super::{Part, Update, LIMIT};
use base64::Engine;
use serde_json::Value;

pub(super) fn extract(payload: &mut Value) -> Vec<Update> {
    let complete = matches!(
        payload.get("status").and_then(Value::as_str),
        Some("completed" | "failed")
    );
    let mut updates = Vec::new();
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_owned);
    let (name, kind, title) = (text("name"), text("kind"), text("title"));
    let label = format!("{} {}", kind.as_deref().unwrap_or_default(), title.as_deref().unwrap_or_default())
        .to_lowercase();
    let device = super::is_device_call(payload);
    if let Some(input) = payload.get_mut("rawInput") {
        let file_change =
            super::is_file_change(name.as_deref(), kind.as_deref(), title.as_deref(), device, input);
        if file_change {
            let mut parts = Vec::new();
            if input.is_string() {
                if let Value::String(text) = input.take() {
                    parts.push(Part::text(
                        if label.contains("patch") {
                            "Patch"
                        } else {
                            "Contents"
                        },
                        text,
                        None,
                    ));
                }
            } else {
                extract_inputs(input, None, &mut parts);
                if label.contains("patch") {
                    for key in ["input", "arguments"] {
                        if let Some(value) = input.get_mut(key).filter(|v| v.is_string()) {
                            if let Value::String(text) = value.take() {
                                parts.push(Part::text("Patch", text, None));
                            }
                        }
                    }
                }
            }
            updates.push(Update {
                role: "fileChangeInput".into(),
                parts: Some(parts),
                append: false,
                publish: true,
                upstream: None,
            });
        } else if input.is_null() || input.as_object().is_some_and(serde_json::Map::is_empty) {
            updates.push(Update {
                role: "fileChangeInput".into(),
                parts: Some(Vec::new()),
                append: false,
                publish: true,
                upstream: None,
            });
        }
    }
    let mut output = Update {
        role: "output".into(),
        parts: None,
        append: false,
        publish: complete,
        upstream: payload.get("truncated").and_then(Value::as_bool),
    };
    let raw = payload.as_object_mut().and_then(|p| p.remove("rawOutput"));
    let mut content = payload.as_object_mut().and_then(|p| p.remove("content"));
    // ACP file changes may supply diffs instead of raw inputs. Consume them
    // once as requested changes; a duplicate raw-input representation wins.
    if let Some(values) = content.as_mut().and_then(Value::as_array_mut) {
        let mut diffs = Vec::new();
        values.retain(|block| {
            if block.get("type").and_then(Value::as_str) == Some("diff") {
                content_part(block, &mut diffs);
                false
            } else {
                true
            }
        });
        if !diffs.is_empty() {
            if !updates.iter().any(|u| {
                u.role == "fileChangeInput" && u.parts.as_ref().is_some_and(|p| !p.is_empty())
            }) {
                updates.retain(|u| u.role != "fileChangeInput");
                updates.push(Update {
                    role: "fileChangeInput".into(),
                    parts: Some(diffs),
                    append: false,
                    publish: true,
                    upstream: None,
                });
            }
            if values.is_empty() {
                content = None;
            }
        }
    }
    if let Some(raw) = &raw {
        output.upstream = output
            .upstream
            .or_else(|| raw.get("truncated").and_then(Value::as_bool));
        if let Some(code) = raw.get("exit_code").or_else(|| raw.get("exitCode")) {
            payload["exitCode"] = code.clone();
        }
    }
    if let Some(code) = payload.pointer("/_meta/terminal_exit/exit_code").cloned() {
        payload["exitCode"] = code;
    }
    // Both pinned adapters define these metadata values as delivered bytes:
    // command output arrives once, in these chunks, never also as a snapshot.
    let terminal = payload
        .pointer_mut("/_meta/terminal_output/data")
        .map(Value::take);
    let terminal = terminal.or_else(|| {
        payload
            .pointer_mut("/_meta/terminal_output_delta/data")
            .map(Value::take)
    });
    if let Some(Value::String(text)) = terminal {
        output.parts = Some(vec![Part::text("Output", text, None)]);
        output.append = true;
    } else if let Some(content) = content {
        let mut parts = Vec::new();
        if let Some(values) = content.as_array() {
            for block in values.iter().take(128) {
                content_part(block, &mut parts);
            }
            if values.len() > 128 {
                parts.push(Part::unavailable("Additional content", "captureLimit"));
            }
        }
        // Terminal references and empty content do not erase independently
        // delivered terminal bytes. Explicit empty ordinary content does.
        if !parts.is_empty() || content.as_array().is_some_and(Vec::is_empty) {
            output.parts = Some(parts);
        }
    }
    if output.parts.is_none() {
        if let Some(raw) = raw {
            output.parts = Some(vec![match raw {
                Value::String(text) => Part::text("Output", text, None),
                value => Part::json("Result", &value),
            }]);
        }
    }
    updates.push(output);
    updates
}

fn extract_inputs(value: &mut Value, path: Option<String>, parts: &mut Vec<Part>) {
    match value {
        Value::Object(fields) => {
            let path = fields
                .get("file_path")
                .or_else(|| fields.get("path"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(path);
            for (key, value) in fields.iter_mut() {
                if [
                    "content",
                    "contents",
                    "text",
                    "oldText",
                    "newText",
                    "old_string",
                    "new_string",
                    "patch",
                ]
                .contains(&key.as_str())
                    && value.is_string()
                {
                    if let Value::String(text) = value.take() {
                        let name = match key.as_str() {
                            "oldText" | "old_string" => "Before",
                            "newText" | "new_string" => "After",
                            "patch" => "Patch",
                            _ => "Contents",
                        };
                        parts.push(Part::text(name, text, path.clone()));
                    }
                } else {
                    extract_inputs(value, path.clone(), parts);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                extract_inputs(value, path.clone(), parts);
            }
        }
        _ => {}
    }
}

fn content_part(block: &Value, parts: &mut Vec<Part>) {
    if block.get("type").and_then(Value::as_str) == Some("terminal") {
        return;
    }
    if block.get("type").and_then(Value::as_str) == Some("diff") {
        let path = block.get("path").and_then(Value::as_str).map(str::to_owned);
        for (key, name) in [("oldText", "Before"), ("newText", "After")] {
            if let Some(text) = block.get(key).and_then(Value::as_str) {
                parts.push(Part::text(name, text.into(), path.clone()));
            }
        }
        return;
    }
    let block = block
        .get("content")
        .filter(|v| v.is_object())
        .unwrap_or(block);
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(Part::text("Output", text.into(), None));
            }
        }
        Some("image") => {
            let mime = block.get("mimeType").and_then(Value::as_str).unwrap_or("");
            let data = block.get("data").and_then(Value::as_str).unwrap_or("");
            if data.is_empty() {
                let mut reference = Part::unavailable("Image reference", "bytesUnavailable");
                reference.path = block.get("uri").and_then(Value::as_str).map(str::to_owned);
                parts.push(reference);
            } else if data.len() > LIMIT.div_ceil(3) * 4 {
                parts.push(Part::unavailable("Image", "captureLimit"));
            } else if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                let valid = match mime {
                    "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                    "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
                    "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                    "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
                    _ => false,
                };
                if valid && bytes.len() <= LIMIT {
                    parts.push(Part {
                        name: "Image".into(),
                        format: mime.into(),
                        bytes,
                        path: None,
                        reason: None,
                    });
                } else {
                    parts.push(Part::unavailable("Image", "invalidImage"));
                }
            } else {
                parts.push(Part::unavailable("Image", "invalidImage"));
            }
        }
        _ => parts.push(Part::unavailable("Content reference", "unsupportedContent")),
    }
}
