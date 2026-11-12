//! Walks the schema bundle into a small IR for rendering.
//!
//! Every bundle entry is a standalone schemars document with entry-local
//! `definitions`; the same definition name appears in many entries. Definitions
//! are collected in first-occurrence order (deterministic because the bundle is
//! built with `preserve_order`), and a repeated name must be structurally equal
//! to its first occurrence — two Rust types sharing a name with different shapes
//! would otherwise merge silently.

use super::overrides;
use serde_json::Value;

pub struct SwiftModel {
    pub string_enums: Vec<StringEnum>,
    pub structs: Vec<StructDef>,
    pub tagged_enums: Vec<TaggedEnum>,
}

/// Internally-tagged Rust enum: `oneOf` object branches sharing one
/// single-value string-enum property (the tag).
pub struct TaggedEnum {
    pub name: String,
    /// Wire name of the tag property (`kind`, `outcome`, ...).
    pub tag: String,
    pub variants: Vec<Variant>,
}

pub struct Variant {
    /// Tag value on the wire; also the Swift case name.
    pub tag_value: String,
    pub fields: Vec<Field>,
}

pub struct StringEnum {
    pub name: String,
    pub cases: Vec<EnumCase>,
}

pub struct EnumCase {
    pub name: String,
    /// Rust variant doc comment, carried through schemars `description`.
    pub doc: Option<String>,
}

pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
}

pub struct Field {
    /// JSON property name on the wire.
    pub wire_name: String,
    /// Swift property name (differs only via PROPERTY_RENAMES).
    pub name: String,
    pub ty: SwiftType,
    /// Swift expression used when a schema-defaulted key is absent.
    pub decode_default: Option<String>,
    /// Rust field doc comment, carried through schemars `description`.
    pub doc: Option<String>,
}

#[derive(Clone, PartialEq)]
pub enum SwiftType {
    Named(String),
    Array(Box<SwiftType>),
    Optional(Box<SwiftType>),
}

impl SwiftType {
    fn named(name: &str) -> Self {
        SwiftType::Named(name.to_string())
    }

    pub fn render(&self) -> String {
        match self {
            SwiftType::Named(name) => name.clone(),
            SwiftType::Array(inner) => format!("[{}]", inner.render()),
            SwiftType::Optional(inner) => format!("{}?", inner.render()),
        }
    }

    pub fn is_optional(&self) -> bool {
        matches!(self, SwiftType::Optional(_))
    }
}

pub fn build(bundle: &Value) -> SwiftModel {
    let definitions = collect_definitions(bundle);
    let mut string_enums = Vec::new();
    let mut structs = Vec::new();
    let mut tagged_enums = Vec::new();
    for (name, schema) in &definitions {
        if overrides::is_skipped(name) {
            continue;
        }
        if let Some(cases) = string_enum_cases(schema) {
            string_enums.push(StringEnum {
                name: name.clone(),
                cases,
            });
        } else if let Some(tagged) = tagged_enum(name, schema) {
            tagged_enums.push(tagged);
        } else if schema.get("type").and_then(Value::as_str) == Some("object")
            && schema.get("oneOf").is_none()
        {
            structs.push(parse_struct(name, schema));
        } else {
            panic!(
                "definition `{name}` has a shape the Swift emitter does not handle; \
                 add handling or list it in SKIP_TYPES/DEFERRED_TYPES"
            );
        }
    }
    SwiftModel {
        string_enums,
        structs,
        tagged_enums,
    }
}

/// Parses an internally-tagged enum: `oneOf` where every branch is an object
/// carrying the same single-value string-enum tag property. A `#[serde(flatten)]`
/// wrapper (AgentRequestPermissionResult) adds an outer `"type": "object"`
/// alongside the `oneOf`; that quirk is normalized here, asserting the outer
/// object stays empty so a schemars output change cannot slip through silently.
fn tagged_enum(name: &str, schema: &Value) -> Option<TaggedEnum> {
    let branches = schema.get("oneOf").and_then(Value::as_array)?;
    if !branches
        .iter()
        .all(|branch| branch.get("type").and_then(Value::as_str) == Some("object"))
    {
        return None;
    }
    assert!(
        schema.get("properties").is_none() && schema.get("required").is_none(),
        "tagged enum `{name}` has outer properties/required beyond the flatten quirk; \
         schemars output changed shape"
    );

    let swift_name = overrides::swift_type_name(name).to_string();
    let mut tag: Option<String> = None;
    let mut variants = Vec::new();
    for branch in branches {
        let properties = branch
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("tagged enum `{name}` branch without properties"));
        let required: Vec<&str> = branch
            .get("required")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let (tag_key, tag_value) = properties
            .iter()
            .find_map(|(key, value)| {
                let cases = plain_string_enum_cases(value)?;
                match cases.as_slice() {
                    [only] => Some((key.clone(), only.clone())),
                    _ => None,
                }
            })
            .unwrap_or_else(|| panic!("tagged enum `{name}` branch without a tag property"));
        match &tag {
            Some(existing) => assert_eq!(
                existing, &tag_key,
                "tagged enum `{name}` mixes tag keys across branches"
            ),
            None => tag = Some(tag_key.clone()),
        }
        let fields = properties
            .iter()
            .filter(|(key, _)| **key != tag_key)
            .map(|(field_name, field_schema)| {
                let ty = field_type(
                    &swift_name,
                    field_name,
                    field_schema,
                    required.contains(&field_name.as_str()),
                );
                Field {
                    wire_name: field_name.clone(),
                    name: field_name.clone(),
                    decode_default: schema_decode_default(
                        &swift_name,
                        field_name,
                        field_schema,
                        &ty,
                    ),
                    ty,
                    doc: None,
                }
            })
            .collect();
        variants.push(Variant { tag_value, fields });
    }
    Some(TaggedEnum {
        name: swift_name,
        tag: tag.expect("tagged enum has at least one branch"),
        variants,
    })
}

fn parse_struct(name: &str, schema: &Value) -> StructDef {
    let swift_name = overrides::swift_type_name(name).to_string();
    let empty = serde_json::Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields = properties
        .iter()
        .map(|(field_name, field_schema)| {
            let ty = field_type(
                &swift_name,
                field_name,
                field_schema,
                required.contains(&field_name.as_str()),
            );
            Field {
                wire_name: field_name.clone(),
                name: overrides::swift_property_name(&swift_name, field_name)
                    .unwrap_or(field_name)
                    .to_string(),
                decode_default: schema_decode_default(&swift_name, field_name, field_schema, &ty),
                ty,
                doc: field_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }
        })
        .collect();
    StructDef {
        name: swift_name,
        fields,
    }
}

fn field_type(type_name: &str, field: &str, schema: &Value, required: bool) -> SwiftType {
    // `true` schema: any JSON; optionality follows the required list.
    if schema.as_bool() == Some(true) {
        let json = SwiftType::named("JSONValue");
        return if required {
            json
        } else {
            SwiftType::Optional(Box::new(json))
        };
    }
    let object = schema
        .as_object()
        .unwrap_or_else(|| panic!("unsupported schema for `{type_name}.{field}`"));

    // `allOf: [{"$ref": ...}]` — schemars' wrapper when a $ref field also
    // carries a doc comment or a serde default.
    if let Some(all_of) = object.get("allOf").and_then(Value::as_array) {
        assert!(
            all_of.len() == 1,
            "unsupported allOf shape for `{type_name}.{field}`"
        );
        return field_type(type_name, field, &all_of[0], required);
    }

    // A schema carrying only `default` (no type constraint) is a
    // `#[serde(default)] serde_json::Value` field: any JSON, always present on
    // encode, so non-optional on the Swift side.
    if !object.contains_key("type")
        && !object.contains_key("$ref")
        && !object.contains_key("anyOf")
        && object.contains_key("default")
    {
        return SwiftType::named("JSONValue");
    }

    // `anyOf: [X, {"type": "null"}]` — schemars' Option<$ref-type>.
    if let Some(any_of) = object.get("anyOf").and_then(Value::as_array) {
        assert!(
            any_of.len() == 2 && any_of[1] == serde_json::json!({"type": "null"}),
            "unsupported anyOf shape for `{type_name}.{field}`"
        );
        return SwiftType::Optional(Box::new(field_type(type_name, field, &any_of[0], true)));
    }

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let definition = reference
            .strip_prefix("#/definitions/")
            .unwrap_or_else(|| panic!("non-local $ref for `{type_name}.{field}`: {reference}"));
        // References to skipped forward-compat enums decay to plain String —
        // the Swift side tolerates unknown states by not typing them.
        if definition == "TerminalOutputDeliveryState" {
            return SwiftType::named("String");
        }
        return SwiftType::named(overrides::swift_type_name(definition));
    }

    // `type` is either "X" or ["X", "null"].
    let (base, nullable) = match object.get("type") {
        Some(Value::String(ty)) => (ty.as_str(), false),
        Some(Value::Array(types)) => {
            let non_null: Vec<&str> = types
                .iter()
                .filter_map(Value::as_str)
                .filter(|ty| *ty != "null")
                .collect();
            assert!(
                non_null.len() == 1 && types.len() == 2,
                "unsupported type union for `{type_name}.{field}`"
            );
            (non_null[0], true)
        }
        _ => panic!("unsupported schema for `{type_name}.{field}`: {object:?}"),
    };

    let resolved = match base {
        "string" => SwiftType::named(overrides::string_field_type(type_name, field)),
        "boolean" => SwiftType::named("Bool"),
        "number" => SwiftType::named("Double"),
        "integer" => {
            let format = object.get("format").and_then(Value::as_str);
            SwiftType::named(match format {
                Some("uint16") => "UInt16",
                Some("uint64") => "UInt64",
                // Rust u32/i32/i64 all surface as Int in the app's Swift.
                Some("uint32") | Some("int32") | Some("int64") | None => "Int",
                Some(other) => panic!("unknown integer format `{other}` for `{type_name}.{field}`"),
            })
        }
        "array" => {
            let items = object
                .get("items")
                .unwrap_or_else(|| panic!("array without items for `{type_name}.{field}`"));
            SwiftType::Array(Box::new(field_type(type_name, field, items, true)))
        }
        other => panic!("unsupported schema type `{other}` for `{type_name}.{field}`"),
    };
    if nullable {
        SwiftType::Optional(Box::new(resolved))
    } else {
        resolved
    }
}

/// Translate a schemars `default` into the Swift expression used by a tolerant
/// decoder. Whether a field defaults comes only from the schema; the override
/// table is limited to complex values that have no general Swift literal.
fn schema_decode_default(
    type_name: &str,
    field: &str,
    schema: &Value,
    ty: &SwiftType,
) -> Option<String> {
    if let Some(expression) = overrides::compat_decode_default(type_name, field) {
        return Some(expression.to_string());
    }
    let default = schema.as_object()?.get("default")?;
    if default.is_null() && ty.is_optional() {
        // Synthesized Optional decoding already maps both missing and null to nil.
        return None;
    }
    if let Some(expression) = overrides::complex_schema_default(type_name, field) {
        return Some(expression.to_string());
    }
    let expression = match default {
        Value::Null if *ty == SwiftType::named("JSONValue") => ".null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) if *ty == SwiftType::named("String") => {
            serde_json::to_string(value).expect("JSON string serializes")
        }
        Value::String(value) => format!(".{value}"),
        Value::Array(values) if values.is_empty() => "[]".to_string(),
        _ => panic!(
            "schema default for `{type_name}.{field}` cannot be rendered as Swift; \
             add it to COMPLEX_SCHEMA_DEFAULTS"
        ),
    };
    Some(expression)
}

/// Definition-name → schema, first occurrence wins, later occurrences asserted
/// structurally identical.
fn collect_definitions(bundle: &Value) -> Vec<(String, Value)> {
    let schemas = bundle
        .get("schemas")
        .and_then(Value::as_object)
        .expect("schema bundle has a schemas map");
    let mut ordered: Vec<(String, Value)> = Vec::new();
    for (entry_name, entry) in schemas {
        let Some(definitions) = entry.get("definitions").and_then(Value::as_object) else {
            continue;
        };
        for (name, schema) in definitions {
            match ordered.iter().find(|(existing, _)| existing == name) {
                Some((_, existing_schema)) => assert_eq!(
                    existing_schema, schema,
                    "definition `{name}` in bundle entry `{entry_name}` differs from an \
                     earlier occurrence; two Rust types with the same name would merge"
                ),
                None => ordered.push((name.clone(), schema.clone())),
            }
        }
    }
    ordered
}

/// `{"type": "string", "enum": [...]}` → the case list. When variants carry doc
/// comments, schemars splits the enum into `oneOf` branches of string enums
/// (each with a `description`); those fold back into one flat case list, with
/// the branch description attached to its first case.
fn string_enum_cases(schema: &Value) -> Option<Vec<EnumCase>> {
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        let mut cases = Vec::new();
        for branch in branches {
            let branch_cases = plain_string_enum_cases(branch)?;
            let doc = branch
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
            for (index, name) in branch_cases.into_iter().enumerate() {
                cases.push(EnumCase {
                    name,
                    doc: if index == 0 { doc.clone() } else { None },
                });
            }
        }
        return Some(cases);
    }
    Some(
        plain_string_enum_cases(schema)?
            .into_iter()
            .map(|name| EnumCase { name, doc: None })
            .collect(),
    )
}

fn plain_string_enum_cases(schema: &Value) -> Option<Vec<String>> {
    if schema.get("type").and_then(Value::as_str) != Some("string") {
        return None;
    }
    let cases = schema.get("enum")?.as_array()?;
    Some(
        cases
            .iter()
            .map(|case| {
                case.as_str()
                    .expect("string enum cases are strings")
                    .to_string()
            })
            .collect(),
    )
}
