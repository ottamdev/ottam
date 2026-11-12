//! Bounded diagnostics for agent traffic core cannot use. They hold labels
//! and payload shapes only, never provider values.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex as StdMutex};

const MAX_KEYS: usize = 64;
const MAX_LABEL_CHARS: usize = 64;
const MAX_SHAPE_DEPTH: usize = 3;
const MAX_SHAPE_CHARS: usize = 256;
const MAX_DROPPED_PATHS: usize = 16;

static UNHANDLED: LazyLock<BoundedCounts> = LazyLock::new(Default::default);

/// Occurrence counts per `(agent, kind)`. Distinct keys are capped so a
/// misbehaving stream cannot grow memory; later keys collapse into
/// `("overflow", "other")`.
#[derive(Default)]
pub(crate) struct BoundedCounts {
    counts: StdMutex<HashMap<(String, String), u64>>,
}

impl BoundedCounts {
    /// Counts one occurrence and returns the key it landed under with its new
    /// count. Callers log only when [`is_log_point`] holds for that count.
    pub(crate) fn record(&self, agent: &str, kind: &str) -> ((String, String), u64) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (label(agent), label(kind));
        let key = if counts.len() < MAX_KEYS || counts.contains_key(&key) {
            key
        } else {
            ("overflow".into(), "other".into())
        };
        let count = counts.entry(key.clone()).or_default();
        *count = count.saturating_add(1);
        (key, *count)
    }

    #[cfg(test)]
    pub(crate) fn count(&self, agent: &str, kind: &str) -> u64 {
        self.counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(label(agent), label(kind)))
            .copied()
            .unwrap_or_default()
    }
}

/// Log only at powers of two, so a high-volume stream cannot grow logs
/// linearly with its updates.
pub(crate) fn is_log_point(count: u64) -> bool {
    count.is_power_of_two()
}

/// Counts agent traffic core received but could not use, under a label such
/// as `update:<tag>` or `request:<method>`, and warns at each log point. Every
/// case shares one message, so `Unhandled agent traffic` finds them all in
/// the backend log. `shape` runs only when the warn does, and must name keys
/// and types, never values: see [`shape`] and [`dropped_paths`].
pub(crate) fn record_unhandled(agent: &str, label: &str, shape: impl FnOnce() -> String) {
    let ((agent, label), occurrences) = UNHANDLED.record(agent, label);
    if is_log_point(occurrences) {
        let shape = shape();
        tracing::warn!(agent = %agent, label = %label, occurrences, shape = %shape,
            "Unhandled agent traffic");
    }
}

#[cfg(test)]
pub(crate) fn unhandled_count(agent: &str, label: &str) -> u64 {
    UNHANDLED.count(agent, label)
}

/// A value's keys and JSON types, never its values: `{a:str,b:[{c:num}]}`.
/// Objects nested deeper than [`MAX_SHAPE_DEPTH`] print as `{…}`, an array
/// shows only its first item, and the result is capped at [`MAX_SHAPE_CHARS`].
pub(crate) fn shape(value: &Value) -> String {
    let mut shape = String::new();
    write_shape(value, MAX_SHAPE_DEPTH, &mut shape);
    capped(shape)
}

fn write_shape(value: &Value, depth: usize, out: &mut String) {
    if out.len() > MAX_SHAPE_CHARS {
        return;
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(_) => out.push_str("bool"),
        Value::Number(_) => out.push_str("num"),
        Value::String(_) => out.push_str("str"),
        Value::Array(items) => {
            out.push('[');
            if let Some(first) = items.first() {
                write_shape(first, depth, out);
            }
            out.push(']');
        }
        Value::Object(_) if depth == 0 => out.push_str("{…}"),
        Value::Object(fields) => {
            out.push('{');
            for (index, (key, field)) in fields.iter().enumerate() {
                // Stop walking a wide object once the shape is full, so the
                // work stays bounded however many keys the frame has.
                if out.len() > MAX_SHAPE_CHARS {
                    out.push('…');
                    break;
                }
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&label(key));
                out.push(':');
                write_shape(field, depth - 1, out);
            }
            out.push('}');
        }
    }
}

/// Paths that `raw` carries and `typed` lacks, where `typed` is the same value
/// after a round-trip through its typed model: the fields that parse dropped.
/// Null and empty values are skipped, since serializers omit those, as is any
/// `(path, value)` that `omitted_default` says the model leaves out as its
/// default. Array items are compared pairwise and named `field[]`; an array the
/// model shortened, by skipping items it couldn't parse, is named whole.
pub(crate) fn dropped_paths(
    raw: &Value,
    typed: &Value,
    omitted_default: &dyn Fn(&str, &Value) -> bool,
) -> Vec<String> {
    let mut paths = Vec::new();
    collect_dropped(raw, typed, omitted_default, "", &mut paths);
    paths
}

fn collect_dropped(
    raw: &Value,
    typed: &Value,
    omitted_default: &dyn Fn(&str, &Value) -> bool,
    path: &str,
    paths: &mut Vec<String>,
) {
    match (raw, typed) {
        (Value::Object(raw), Value::Object(typed)) => {
            for (key, field) in raw {
                if is_empty(field) {
                    continue;
                }
                let child = if path.is_empty() {
                    label(key)
                } else {
                    format!("{path}.{}", label(key))
                };
                match typed.get(key) {
                    Some(typed_field) => {
                        collect_dropped(field, typed_field, omitted_default, &child, paths);
                    }
                    None if omitted_default(&child, field) => {}
                    None => push_dropped(child, paths),
                }
            }
        }
        // Skipped items shift the rest, so pairs only line up at equal length.
        (Value::Array(raw), Value::Array(typed)) if raw.len() != typed.len() => {
            push_dropped(format!("{path}[]"), paths);
        }
        (Value::Array(raw), Value::Array(typed)) => {
            let child = format!("{path}[]");
            for (field, typed_field) in raw.iter().zip(typed) {
                collect_dropped(field, typed_field, omitted_default, &child, paths);
            }
        }
        _ => {}
    }
}

fn push_dropped(path: String, paths: &mut Vec<String>) {
    if paths.len() < MAX_DROPPED_PATHS && !paths.contains(&path) {
        paths.push(path);
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

fn capped(text: String) -> String {
    if text.chars().count() <= MAX_SHAPE_CHARS {
        return text;
    }
    let mut capped = text.chars().take(MAX_SHAPE_CHARS).collect::<String>();
    capped.push('…');
    capped
}

fn label(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_collapse_into_overflow_after_the_key_cap() {
        let counts = BoundedCounts::default();
        for index in 0..MAX_KEYS {
            counts.record("agent", &format!("kind-{index}"));
        }
        let (key, count) = counts.record("agent", "one-too-many");
        assert_eq!(key, ("overflow".to_string(), "other".to_string()));
        assert_eq!(count, 1);
        let (key, count) = counts.record("agent", "kind-0");
        assert_eq!(key, ("agent".to_string(), "kind-0".to_string()));
        assert_eq!(count, 2);
    }

    #[test]
    fn labels_drop_control_characters_and_are_capped() {
        let counts = BoundedCounts::default();
        let ((agent, kind), _) = counts.record("a\ngent", &"k".repeat(200));
        assert_eq!(agent, "agent");
        assert_eq!(kind.chars().count(), MAX_LABEL_CHARS);
    }

    #[test]
    fn logs_only_at_powers_of_two() {
        let logged = (1..=20)
            .filter(|count| is_log_point(*count))
            .collect::<Vec<_>>();
        assert_eq!(logged, vec![1, 2, 4, 8, 16]);
    }

    #[test]
    fn unhandled_shape_names_keys_and_types_never_values() {
        let value = json!({
            "sessionUpdate": "brand_new_kind",
            "secret": "PRIVATE-TEXT",
            "count": 3,
            "items": [{"text": "PRIVATE-TEXT", "done": false}],
            "nested": {"one": {"two": {"three": {"four": "PRIVATE-TEXT"}}}},
            "gone": null
        });
        let shape = shape(&value);
        assert_eq!(
            shape,
            "{sessionUpdate:str,secret:str,count:num,items:[{text:str,done:bool}],\
             nested:{one:{two:{…}}},gone:null}"
        );
        assert!(!shape.contains("PRIVATE-TEXT"));
    }

    #[test]
    fn unhandled_shape_is_capped() {
        let wide = (0..200)
            .map(|index| (format!("field{index}"), json!("PRIVATE-TEXT")))
            .collect::<serde_json::Map<_, _>>();
        let wide = Value::Object(wide);
        let shape = shape(&wide);
        assert_eq!(shape.chars().count(), MAX_SHAPE_CHARS + 1);
        assert!(shape.ends_with('…'));
        // The walk itself stops at the cap, not just the result.
        let mut walked = String::new();
        write_shape(&wide, MAX_SHAPE_DEPTH, &mut walked);
        assert!(
            walked.len() < 2 * MAX_SHAPE_CHARS,
            "walked {}",
            walked.len()
        );
    }

    #[test]
    fn unhandled_dropped_paths_name_fields_the_typed_model_lost() {
        let raw = json!({
            "kept": "x",
            "added": "PRIVATE-TEXT",
            "omitted": null,
            "empty": [],
            "content": {"text": "x", "newField": 1, "status": "pending"},
            "items": [{"id": 1, "extra": true}, {"id": 2, "extra": false}],
            "skipped": [{"type": "text"}, {"type": "hologram"}],
            "status": "pending"
        });
        let typed = json!({
            "kept": "x",
            "content": {"text": "x"},
            "items": [{"id": 1}, {"id": 2}],
            "skipped": [{"type": "text"}]
        });
        let pending_is_default = |path: &str, value: &Value| path == "status" && value == "pending";
        assert_eq!(
            dropped_paths(&raw, &typed, &pending_is_default),
            // Only the default at its own path is excused.
            [
                "added",
                "content.newField",
                "content.status",
                "items[].extra",
                "skipped[]"
            ]
        );
        assert!(dropped_paths(&typed, &typed, &|_, _| false).is_empty());
    }

    #[test]
    fn unhandled_traffic_is_counted_per_agent_and_label() {
        let mut shaped = 0;
        for _ in 0..3 {
            record_unhandled("diagnostics-test", "update:brand_new_kind", || {
                shaped += 1;
                String::new()
            });
        }
        assert_eq!(
            unhandled_count("diagnostics-test", "update:brand_new_kind"),
            3
        );
        // The shape is built only at log points: counts 1 and 2.
        assert_eq!(shaped, 2);
    }
}
