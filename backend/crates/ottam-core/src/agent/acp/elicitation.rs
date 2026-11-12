//! `elicitation/create` forms as questions, and answers back as form content.
//!
//! Core normalizes every adapter's schema here so the app never reads
//! adapter keys. A select and its free-text companion are one question. The
//! companion is the property marked with AIR's `customAnswer` (Ottam
//! advertises AIR, so Claude marks it `{questionId}` and Codex marks it
//! `true` beside `_meta.codex.questionId`), else the `<key>_custom` property
//! OpenCode and Claude name, or, in oh-my-pi's forms only, the `<key>__other`
//! property it names. Codex's `_meta.codex.isSecret` is a secure text
//! question. Anything else is one question per property, and a
//! property no question kind fits refuses the whole form.

use ottam_protocol::{
    AgentQuestion, AgentQuestionAnswer, AgentQuestionKind, AgentQuestionOption,
};
use serde_json::{json, Map, Value};

const CUSTOM_SUFFIX: &str = "_custom";
/// oh-my-pi's companion name, paired only for oh-my-pi's own forms: another
/// agent's form may hold a real field that happens to end this way.
const OMP_OTHER_SUFFIX: &str = "__other";

/// A form core can show.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Form {
    pub(crate) session_id: String,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) title: String,
    pub(crate) questions: Vec<AgentQuestion>,
    fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    key: String,
    shape: Shape,
    other: Option<Other>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Single,
    Multi,
    Text,
    Number { integer: bool },
    Boolean,
}

/// Where a select's free-text answer goes.
#[derive(Debug, Clone, PartialEq)]
struct Other {
    key: String,
    /// Codex reads free text typed into the select itself as its custom
    /// answer, and its select is required; its companion is a note on a
    /// pick. So text with no pick goes in the select.
    into_select_without_pick: bool,
}

/// Why a request isn't a form core can show. The label is the diagnostic's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Refusal {
    /// URL mode, or a mode the schema doesn't know.
    NotForm,
    /// Scoped to a request rather than a session: nothing to route it by.
    NoSession,
    /// A property no question kind fits.
    Unrepresentable,
}

impl Refusal {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::NotForm => "elicitation:mode",
            Self::NoSession => "elicitation:scope",
            Self::Unrepresentable => "elicitation:field",
        }
    }
}

#[cfg(test)]
pub(super) fn parse(params: &Value) -> Result<Form, Refusal> {
    parse_form(params, false)
}

/// `omp_other`: pair `<key>__other` companions, for oh-my-pi's forms.
pub(crate) fn parse_form(params: &Value, omp_other: bool) -> Result<Form, Refusal> {
    if params.get("mode").and_then(Value::as_str) != Some("form") {
        return Err(Refusal::NotForm);
    }
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(Refusal::NoSession)?
        .to_owned();
    let tool_call_id = params
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let title = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let schema = params
        .get("requestedSchema")
        .ok_or(Refusal::Unrepresentable)?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(Refusal::Unrepresentable)?;
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();

    // Companions first, so their selects know they take free text.
    let mut companions: Vec<(String, Other)> = Vec::new();
    for (key, property) in properties {
        if let Some((select, into_select_without_pick)) =
            companion_of(key, property, properties, omp_other)
        {
            companions.push((
                select,
                Other {
                    key: key.clone(),
                    into_select_without_pick,
                },
            ));
        }
    }
    let is_companion = |key: &str| companions.iter().any(|(_, other)| other.key == key);
    let question_count = properties.keys().filter(|key| !is_companion(key)).count();

    let mut questions = Vec::new();
    let mut fields = Vec::new();
    for (key, property) in properties {
        if is_companion(key) {
            continue;
        }
        let other = companions
            .iter()
            .find(|(select, _)| select == key)
            .map(|(_, other)| other.clone());
        let (shape, kind) = question_kind(property, other.is_some())?;
        let (header, prompt) = wording(key, property, &title, question_count == 1);
        questions.push(AgentQuestion {
            id: format!("q{}", questions.len()),
            header,
            prompt,
            required: required.contains(&key.as_str()),
            kind,
        });
        fields.push(Field {
            key: key.clone(),
            shape,
            other,
        });
    }
    if questions.is_empty() {
        return Err(Refusal::Unrepresentable);
    }
    Ok(Form {
        session_id,
        tool_call_id,
        title,
        questions,
        fields,
    })
}

/// The select `key` answers for, and whether its text belongs in the
/// select when nothing is picked.
fn companion_of(
    key: &str,
    property: &Value,
    properties: &Map<String, Value>,
    omp_other: bool,
) -> Option<(String, bool)> {
    if property.get("type").and_then(Value::as_str) != Some("string") {
        return None;
    }
    let meta = property.get("_meta");
    let marker = meta
        .and_then(|meta| meta.pointer("/jetbrains/air/customAnswer"))
        .or_else(|| meta.and_then(|meta| meta.get("_askUserQuestionCustomAnswer")));
    let target = match marker {
        Some(Value::Object(marker)) => marker
            .get("questionId")
            .and_then(Value::as_str)
            .map(|select| (select.to_owned(), false)),
        Some(Value::Bool(true)) => meta
            .and_then(|meta| meta.pointer("/codex/questionId"))
            .and_then(Value::as_str)
            .map(|select| (select.to_owned(), true)),
        _ => None,
    }
    .or_else(|| {
        omp_other
            .then(|| key.strip_suffix(OMP_OTHER_SUFFIX))
            .flatten()
            .or_else(|| key.strip_suffix(CUSTOM_SUFFIX))
            .map(|select| (select.to_owned(), false))
    })?;
    properties
        .get(&target.0)
        .filter(|select| {
            select_options(select)
                .or_else(|| select.get("items").and_then(select_options))
                .is_some()
        })
        .map(|_| target)
}

fn question_kind(
    property: &Value,
    allows_other: bool,
) -> Result<(Shape, AgentQuestionKind), Refusal> {
    let kind = property.get("type").and_then(Value::as_str);
    match kind {
        Some("string") => {
            if let Some(options) = select_options(property) {
                return Ok((
                    Shape::Single,
                    AgentQuestionKind::SingleSelect {
                        options: options?,
                        allows_other,
                        default: property
                            .get("default")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                ));
            }
            let secure = property
                .pointer("/_meta/codex/isSecret")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok((
                Shape::Text,
                AgentQuestionKind::Text {
                    secure,
                    // A secret is never prefilled.
                    default: property
                        .get("default")
                        .and_then(Value::as_str)
                        .filter(|_| !secure)
                        .map(str::to_owned),
                },
            ))
        }
        Some("array") => {
            let options = property
                .get("items")
                .and_then(select_options)
                .ok_or(Refusal::Unrepresentable)??;
            Ok((
                Shape::Multi,
                AgentQuestionKind::MultiSelect {
                    options,
                    allows_other,
                    min_selected: count(property.get("minItems")),
                    max_selected: count(property.get("maxItems")),
                    default: property
                        .get("default")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                },
            ))
        }
        Some(number @ ("number" | "integer")) => Ok((
            Shape::Number {
                integer: number == "integer",
            },
            AgentQuestionKind::Number {
                integer: number == "integer",
                minimum: property.get("minimum").and_then(Value::as_f64),
                maximum: property.get("maximum").and_then(Value::as_f64),
                default: property.get("default").and_then(Value::as_f64),
            },
        )),
        Some("boolean") => Ok((
            Shape::Boolean,
            AgentQuestionKind::Boolean {
                default: property.get("default").and_then(Value::as_bool),
            },
        )),
        _ => Err(Refusal::Unrepresentable),
    }
}

/// `None` when the property is not a select; `Some(Err)` when it is one
/// whose options aren't strings.
fn select_options(property: &Value) -> Option<Result<Vec<AgentQuestionOption>, Refusal>> {
    if let Some(entries) = ["oneOf", "anyOf"]
        .iter()
        .find_map(|key| property.get(*key).and_then(Value::as_array))
    {
        return Some(
            entries
                .iter()
                .map(|entry| {
                    let value = entry
                        .get("const")
                        .and_then(Value::as_str)
                        .ok_or(Refusal::Unrepresentable)?;
                    Ok(AgentQuestionOption {
                        value: value.to_owned(),
                        label: entry
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or(value)
                            .to_owned(),
                        description: entry
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    })
                })
                .collect(),
        );
    }
    let values = property.get("enum").and_then(Value::as_array)?;
    let names = property.get("enumNames").and_then(Value::as_array);
    Some(
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.as_str().ok_or(Refusal::Unrepresentable)?;
                Ok(AgentQuestionOption {
                    value: value.to_owned(),
                    label: names
                        .and_then(|names| names.get(index))
                        .and_then(Value::as_str)
                        .unwrap_or(value)
                        .to_owned(),
                    description: None,
                })
            })
            .collect(),
    )
}

fn count(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
}

/// The question's (header, prompt). Codex puts the question in `title` and
/// its header in `description`; everyone else the other way round. Claude
/// leaves a lone question's text to the form's message.
fn wording(key: &str, property: &Value, message: &str, alone: bool) -> (Option<String>, String) {
    let title = property
        .get("title")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    let description = property
        .get("description")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    if property.pointer("/_meta/codex").is_some() {
        return (
            description.map(str::to_owned),
            title.unwrap_or(key).to_owned(),
        );
    }
    match (title, description) {
        (title, Some(description)) => (title.map(str::to_owned), description.to_owned()),
        (title, None) if alone && !message.is_empty() => {
            (title.map(str::to_owned), message.to_owned())
        }
        (Some(title), None) => (None, title.to_owned()),
        (None, None) => (None, key.to_owned()),
    }
}

impl Form {
    /// The `accept` content for `answers`, keyed back to the adapter's
    /// properties. Values the form didn't offer are dropped; an unanswered
    /// question is omitted.
    pub(crate) fn content(&self, answers: &[AgentQuestionAnswer]) -> Map<String, Value> {
        let mut content = Map::new();
        for (question, field) in self.questions.iter().zip(&self.fields) {
            let Some(answer) = answers
                .iter()
                .find(|answer| answer.question_id == question.id)
            else {
                continue;
            };
            let other = answer
                .other
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty() && field.other.is_some());
            match field.shape {
                Shape::Single => {
                    let pick = answer
                        .values
                        .iter()
                        .find(|value| offers(&question.kind, value));
                    match (pick, other, field.other.as_ref()) {
                        (Some(pick), _, _) => {
                            content.insert(field.key.clone(), json!(pick));
                            if let (Some(text), Some(companion)) = (other, field.other.as_ref()) {
                                content.insert(companion.key.clone(), json!(text));
                            }
                        }
                        (None, Some(text), Some(companion))
                            if companion.into_select_without_pick =>
                        {
                            content.insert(field.key.clone(), json!(text));
                        }
                        (None, Some(text), Some(companion)) => {
                            content.insert(companion.key.clone(), json!(text));
                        }
                        _ => {}
                    }
                }
                Shape::Multi => {
                    let picks = answer
                        .values
                        .iter()
                        .filter(|value| offers(&question.kind, value))
                        .collect::<Vec<_>>();
                    if !picks.is_empty() {
                        content.insert(field.key.clone(), json!(picks));
                    }
                    if let (Some(text), Some(companion)) = (other, field.other.as_ref()) {
                        content.insert(companion.key.clone(), json!(text));
                    }
                }
                Shape::Text => {
                    if let Some(text) = answer.values.first() {
                        content.insert(field.key.clone(), json!(text));
                    }
                }
                Shape::Number { integer } => {
                    let number = answer
                        .values
                        .first()
                        .and_then(|value| value.trim().parse::<f64>().ok())
                        .filter(|number| number.is_finite());
                    match number {
                        Some(number) if integer && number.fract() == 0.0 => {
                            content.insert(field.key.clone(), json!(number as i64));
                        }
                        Some(number) if !integer => {
                            content.insert(field.key.clone(), json!(number));
                        }
                        _ => {}
                    }
                }
                Shape::Boolean => match answer.values.first().map(String::as_str) {
                    Some("true") => {
                        content.insert(field.key.clone(), json!(true));
                    }
                    Some("false") => {
                        content.insert(field.key.clone(), json!(false));
                    }
                    _ => {}
                },
            }
        }
        content
    }
}

fn options(kind: &AgentQuestionKind) -> &[AgentQuestionOption] {
    match kind {
        AgentQuestionKind::SingleSelect { options, .. }
        | AgentQuestionKind::MultiSelect { options, .. } => options,
        _ => &[],
    }
}

fn offers(kind: &AgentQuestionKind, value: &str) -> bool {
    options(kind).iter().any(|option| option.value == value)
}

pub(crate) fn accept(content: Map<String, Value>) -> Value {
    json!({"action": "accept", "content": content})
}

pub(crate) fn decline() -> Value {
    json!({"action": "decline"})
}

pub(crate) fn cancel() -> Value {
    json!({"action": "cancel"})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(id: &str, values: &[&str], other: Option<&str>) -> AgentQuestionAnswer {
        AgentQuestionAnswer {
            question_id: id.into(),
            values: values.iter().map(|value| value.to_string()).collect(),
            other: other.map(str::to_owned),
        }
    }

    /// claude-agent-acp 0.85.1 `askUserQuestionsToCreateRequest` for an AIR
    /// client, three questions, the second multi-select.
    fn claude_form() -> Value {
        let custom = |index: usize| {
            let marker = json!({"questionId": format!("question_{index}"), "isCustomAnswer": true});
            json!({
                "type": "string",
                "title": "Other",
                "description": "Type your own answer, or add a note to the option you chose above (optional).",
                "_meta": {
                    "_askUserQuestionCustomAnswer": marker,
                    "jetbrains": {"air": {"version": 1, "customAnswer": marker}}
                }
            })
        };
        json!({
            "mode": "form",
            "sessionId": "session",
            "toolCallId": "toolu_1",
            "message": "Please answer the following questions.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "question_0": {"type": "string", "title": "Approach", "description": "Which approach?",
                        "oneOf": [
                            {"const": "Refactor", "title": "Refactor", "description": "Smaller diff."},
                            {"const": "Rewrite", "title": "Rewrite"}
                        ]},
                    "question_0_custom": custom(0),
                    "question_1": {"type": "array", "title": "Targets", "description": "Which targets?",
                        "items": {"anyOf": [
                            {"const": "macOS", "title": "macOS"},
                            {"const": "Linux", "title": "Linux"},
                            {"const": "Windows", "title": "Windows"}
                        ]}},
                    "question_1_custom": custom(1),
                    "question_2": {"type": "string", "title": "Tests", "description": "Run tests?",
                        "oneOf": [{"const": "Yes", "title": "Yes"}, {"const": "No", "title": "No"}]},
                    "question_2_custom": custom(2)
                }
            }
        })
    }

    #[test]
    fn claude_questions_pair_each_select_with_its_custom_answer() {
        let form = parse(&claude_form()).unwrap();
        assert_eq!(form.session_id, "session");
        assert_eq!(form.tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(form.questions.len(), 3);
        let first = &form.questions[0];
        assert_eq!(
            (
                first.id.as_str(),
                first.header.as_deref(),
                first.prompt.as_str()
            ),
            ("q0", Some("Approach"), "Which approach?")
        );
        assert!(!first.required);
        let AgentQuestionKind::SingleSelect {
            options,
            allows_other,
            ..
        } = &first.kind
        else {
            panic!("single select");
        };
        assert!(allows_other);
        assert_eq!(options[0].description.as_deref(), Some("Smaller diff."));
        assert!(matches!(
            form.questions[1].kind,
            AgentQuestionKind::MultiSelect {
                allows_other: true,
                ..
            }
        ));

        let content = form.content(&[
            answer("q0", &[], Some("Neither")),
            answer("q1", &["macOS", "Linux", "BeOS"], Some("FreeBSD")),
            answer("q2", &["Yes"], Some("but quickly")),
        ]);
        assert_eq!(
            Value::Object(content),
            json!({
                "question_0_custom": "Neither",
                "question_1": ["macOS", "Linux"],
                "question_1_custom": "FreeBSD",
                "question_2": "Yes",
                "question_2_custom": "but quickly"
            })
        );
    }

    #[test]
    fn a_lone_claude_question_takes_its_prompt_from_the_message() {
        let form = parse(&json!({
            "mode": "form",
            "sessionId": "session",
            "message": "Which approach should I take?",
            "requestedSchema": {"type": "object", "properties": {
                "question_0": {"type": "string", "title": "Approach",
                    "oneOf": [{"const": "A", "title": "A"}, {"const": "B", "title": "B"}]},
                "question_0_custom": {"type": "string", "title": "Other"}
            }}
        }))
        .unwrap();
        assert_eq!(form.questions.len(), 1);
        assert_eq!(form.questions[0].prompt, "Which approach should I take?");
        assert_eq!(form.questions[0].header.as_deref(), Some("Approach"));
        assert_eq!(form.tool_call_id, None);
    }

    /// codex-acp 2.1.1 `handleUserInput` for an AIR client: the note field
    /// is marked `customAnswer: true` beside `codex.questionId`.
    fn codex_form() -> Value {
        json!({
            "sessionId": "thread",
            "toolCallId": "item_1",
            "mode": "form",
            "message": "Codex needs your input to continue.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "approach": {"type": "string", "title": "Which approach should I take?",
                        "description": "Approach",
                        "_meta": {"codex": {"isOther": true, "isSecret": false}},
                        "oneOf": [{"const": "Refactor", "title": "Refactor"}, {"const": "Rewrite", "title": "Rewrite"}]},
                    "approach_note": {"type": "string", "title": "Additional answer or note",
                        "_meta": {"codex": {"questionId": "approach", "role": "user_note", "isSecret": false},
                            "_askUserQuestionCustomAnswer": true,
                            "jetbrains": {"air": {"version": 1, "customAnswer": true}}}},
                    "token": {"type": "string", "title": "API token", "description": "Token",
                        "_meta": {"codex": {"isOther": false, "isSecret": true}}}
                },
                "required": ["approach", "token"]
            },
            "_meta": {"codex": {"autoResolutionMs": null}}
        })
    }

    #[test]
    fn codex_free_text_without_a_pick_goes_in_the_select() {
        let form = parse(&codex_form()).unwrap();
        assert_eq!(form.questions.len(), 2);
        assert_eq!(form.questions[0].prompt, "Which approach should I take?");
        assert_eq!(form.questions[0].header.as_deref(), Some("Approach"));
        assert!(form.questions[0].required);
        assert!(matches!(
            form.questions[1].kind,
            AgentQuestionKind::Text {
                secure: true,
                default: None
            }
        ));

        let typed = form.content(&[
            answer("q0", &[], Some("Both")),
            answer("q1", &["s3cret"], None),
        ]);
        assert_eq!(
            Value::Object(typed),
            json!({"approach": "Both", "token": "s3cret"})
        );
        let noted = form.content(&[answer("q0", &["Refactor"], Some("carefully"))]);
        assert_eq!(
            Value::Object(noted),
            json!({"approach": "Refactor", "approach_note": "carefully"})
        );
    }

    /// OpenCode 2.0.27 `elicitation.ts`: `q<n>` fields with a `<key>_custom`
    /// companion and no `_meta`.
    #[test]
    fn opencode_custom_fields_pair_by_name() {
        let form = parse(&json!({
            "mode": "form",
            "sessionId": "ses_1",
            "toolCallId": "call_1",
            "message": "Questions",
            "requestedSchema": {"type": "object", "properties": {
                "q0": {"type": "string", "title": "Approach", "description": "Which approach?",
                    "oneOf": [{"const": "A", "title": "A", "description": "First"}, {"const": "B", "title": "B"}],
                    "default": "A"},
                "q0_custom": {"type": "string", "title": "Approach (other)"},
                "q1": {"type": "array", "title": "Pick", "description": "Pick some",
                    "items": {"anyOf": [{"const": "x", "title": "X"}, {"const": "y", "title": "Y"}]},
                    "minItems": 1, "maxItems": 2, "default": ["x"]},
                "q1_custom": {"type": "string", "title": "Pick (other)"}
            }, "required": []}
        }))
        .unwrap();
        assert_eq!(form.questions.len(), 2);
        assert!(matches!(
            &form.questions[0].kind,
            AgentQuestionKind::SingleSelect { allows_other: true, default: Some(default), .. } if default == "A"
        ));
        assert!(matches!(
            &form.questions[1].kind,
            AgentQuestionKind::MultiSelect { min_selected: Some(1), max_selected: Some(2), default, .. } if default == &["x".to_string()]
        ));
        let content = form.content(&[answer("q0", &[], Some("C")), answer("q1", &["y"], None)]);
        assert_eq!(
            Value::Object(content),
            json!({"q0_custom": "C", "q1": ["y"]})
        );
        // Nothing answered: empty content, which OpenCode reads as a skip.
        assert!(form.content(&[]).is_empty());
    }

    /// oh-my-pi 18.11.1 `askDialog` (`modes/acp/acp-agent.ts`): `q<n>`
    /// selects, each with a `<key>__other` companion, and none required.
    #[test]
    fn omp_other_fields_pair_by_name() {
        let params = json!({
            "mode": "form",
            "sessionId": "s",
            "message": "Answer 2 questions",
            "requestedSchema": {"type": "object", "properties": {
                "q0": {"type": "string", "title": "Which approach?", "description": "Approach",
                    "oneOf": [{"const": "A", "title": "A", "description": "First"}, {"const": "B", "title": "B"}],
                    "default": "B"},
                "q0__other": {"type": "string", "title": "Other"},
                "q1": {"type": "array", "title": "Which parts?",
                    "items": {"anyOf": [{"const": "x", "title": "x"}, {"const": "y", "title": "y"}]}},
                "q1__other": {"type": "string", "title": "Other"}
            }}
        });
        // Another agent's form keeps a `__other` field as its own question.
        assert_eq!(parse(&params).unwrap().questions.len(), 4);
        let form = parse_form(&params, true).unwrap();
        assert_eq!(form.questions.len(), 2);
        assert!(matches!(
            &form.questions[0].kind,
            AgentQuestionKind::SingleSelect { allows_other: true, default: Some(default), .. } if default == "B"
        ));
        assert!(matches!(
            &form.questions[1].kind,
            AgentQuestionKind::MultiSelect {
                allows_other: true,
                ..
            }
        ));
        let content = form.content(&[
            answer("q0", &[], Some("C")),
            answer("q1", &["x", "y"], None),
        ]);
        assert_eq!(
            Value::Object(content),
            json!({"q0__other": "C", "q1": ["x", "y"]})
        );
    }

    #[test]
    fn plain_fields_become_native_questions() {
        let form = parse(&json!({
            "mode": "form",
            "sessionId": "s",
            "message": "Configure the server",
            "requestedSchema": {"type": "object", "properties": {
                "name": {"type": "string", "title": "Name"},
                "port": {"type": "integer", "title": "Port", "minimum": 1, "maximum": 65535},
                "ratio": {"type": "number", "title": "Ratio"},
                "verbose": {"type": "boolean", "title": "Verbose", "default": false},
                "level": {"type": "string", "title": "Level", "enum": ["lo", "hi"], "enumNames": ["Low", "High"]}
            }, "required": ["name"]}
        }))
        .unwrap();
        assert_eq!(form.questions.len(), 5);
        assert_eq!(form.questions[0].prompt, "Name");
        assert!(form.questions[0].required);
        let AgentQuestionKind::SingleSelect {
            options,
            allows_other,
            ..
        } = &form.questions[4].kind
        else {
            panic!("enum is a select");
        };
        assert!(!allows_other);
        assert_eq!(options[1].label, "High");
        let content = form.content(&[
            answer("q0", &["ottam"], None),
            answer("q1", &["8080"], None),
            answer("q2", &["0.5"], None),
            answer("q3", &["true"], None),
            answer("q4", &["hi"], None),
        ]);
        assert_eq!(
            Value::Object(content),
            json!({"name": "ottam", "port": 8080, "ratio": 0.5, "verbose": true, "level": "hi"})
        );
        let fractional = form.content(&[answer("q1", &["80.5"], None)]);
        assert!(fractional.is_empty());
    }

    #[test]
    fn forms_core_cannot_show_are_refused() {
        assert_eq!(
            parse(
                &json!({"mode": "url", "sessionId": "s", "url": "https://example.com", "elicitationId": "e", "message": "m"})
            ),
            Err(Refusal::NotForm)
        );
        assert_eq!(
            parse(
                &json!({"mode": "form", "requestId": 4, "message": "m", "requestedSchema": {"type": "object", "properties": {}}})
            ),
            Err(Refusal::NoSession)
        );
        for property in [
            json!({"type": "object", "properties": {}}),
            json!({"type": "array", "items": {"type": "string"}}),
            json!({"type": "string", "oneOf": [{"const": 1, "title": "one"}]}),
            json!({"title": "untyped"}),
        ] {
            assert_eq!(
                parse(&json!({"mode": "form", "sessionId": "s", "message": "m",
                    "requestedSchema": {"type": "object", "properties": {"field": property}}})),
                Err(Refusal::Unrepresentable)
            );
        }
        assert_eq!(
            parse(&json!({"mode": "form", "sessionId": "s", "message": "m",
                "requestedSchema": {"type": "object", "properties": {}}})),
            Err(Refusal::Unrepresentable)
        );
    }
}
