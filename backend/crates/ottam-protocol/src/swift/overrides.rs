//! Presentation configuration the schema bundle cannot express. These tables are
//! config about the Swift rendering, not a second source of truth for the wire
//! format — the shapes themselves always come from the schema bundle.

/// Raw-string wire ID newtypes. `#[serde(transparent)]` erases these from the
/// schema (they inline as bare `{"type": "string"}`), so the Swift structs are
/// emitted from this list.
pub const ID_TYPES: &[&str] = &[
    "ProjectID",
    "ProjectSourceID",
    "WorkspaceID",
    "CoreInstanceID",
    "McpServerID",
    "NamedAgentID",
    "EngineID",
    "AgentID",
    "AgentSessionID",
    "AgentAuthSessionID",
    "ConversationID",
    "ConversationRunID",
    "ConversationEventID",
    "ConversationMessageID",
    "ConversationRunInputID",
    "ConversationAttachmentID",
    "TerminalSessionID",
    "TerminalID",
    "RuntimeInstallID",
    "AgentInstallID",
];

/// Schema definitions that must NOT be emitted, with the reason each stays out.
pub const SKIP_TYPES: &[&str] = &[
    // Hand-written untagged envelope ID (string-or-number-or-null) in
    // ProtocolTypes.swift; envelope infrastructure, not a domain type.
    "RequestID",
    // The Swift side deliberately keeps `TerminalOutputDeltaParams.deliveryState`
    // as `String?` so an unknown state degrades gracefully instead of failing
    // the decode.
    "TerminalOutputDeliveryState",
    // Diff models live in the OttamCore module on the Swift side, not in
    // OttamBackendClient; out of generation scope.
    "DiffSnapshot",
    "DiffFile",
    "DiffHunk",
    "DiffLine",
    "OmittedFile",
];

/// Not yet migrated — emitted in a later slice, still hand-written today.
pub const DEFERRED_TYPES: &[&str] = &[];

/// Complex schema defaults that have no general Swift literal. The schema still
/// decides whether the field defaults; this table only supplies the expression.
pub const COMPLEX_SCHEMA_DEFAULTS: &[(&str, &str, &str)] =
    &[("ConversationTranscriptItem", "adapterMetadata", ".init()")];

/// Compatibility defaults that schemars cannot see. Combining
/// `serde(default)` with `skip_serializing_if` omits the default annotation from
/// this field's schema even though Rust accepts a missing key.
pub const COMPAT_DECODE_DEFAULTS: &[(&str, &str, &str)] = &[
    ("TerminalOutputDeltaParams", "replaySuggested", "false"),
    ("ConversationMessage", "attachments", "[]"),
    ("ConversationRunSummary", "usageUnavailable", "false"),
    ("McpImportCandidate", "disabledInSource", "false"),
];

/// Comments retained for selected generated default decodes.
pub const DECODE_DEFAULT_COMMENTS: &[(&str, &str, &str)] = &[(
    "EngineSummary",
    "models",
    "Tolerate a bundled core binary that predates model catalogs.",
)];

/// Wire property name → Swift property name, where spelling differs. Forces an
/// explicit CodingKeys on the owning type.
pub const PROPERTY_RENAMES: &[(&str, &str, &str)] = &[];

pub fn complex_schema_default(type_name: &str, field: &str) -> Option<&'static str> {
    COMPLEX_SCHEMA_DEFAULTS
        .iter()
        .find(|(ty, fld, _)| *ty == type_name && *fld == field)
        .map(|(_, _, expression)| *expression)
}

pub fn compat_decode_default(type_name: &str, field: &str) -> Option<&'static str> {
    COMPAT_DECODE_DEFAULTS
        .iter()
        .find(|(ty, fld, _)| *ty == type_name && *fld == field)
        .map(|(_, _, expression)| *expression)
}

pub fn decode_default_comment(type_name: &str, field: &str) -> Option<&'static str> {
    DECODE_DEFAULT_COMMENTS
        .iter()
        .find(|(ty, fld, _)| *ty == type_name && *fld == field)
        .map(|(_, _, comment)| *comment)
}

pub fn swift_property_name(type_name: &str, wire_name: &str) -> Option<&'static str> {
    PROPERTY_RENAMES
        .iter()
        .find(|(ty, wire, _)| *ty == type_name && *wire == wire_name)
        .map(|(_, _, swift)| *swift)
}

pub fn has_property_rename(type_name: &str) -> bool {
    PROPERTY_RENAMES.iter().any(|(ty, ..)| *ty == type_name)
}

/// Rust definition name → Swift type name, where they differ.
pub const TYPE_RENAMES: &[(&str, &str)] = &[
    ("ProtocolError", "AgentProtocolError"),
    // The permission response result is a flattened tagged outcome; the app
    // names the enum for what it is.
    ("AgentRequestPermissionResult", "AgentPermissionOutcome"),
    ("AgentAskQuestionResult", "AgentQuestionOutcome"),
];

/// Unambiguous wire-field-name → ID type. Applies to any `{"type": "string"}`
/// property with this exact name, in every type.
pub const ID_FIELDS: &[(&str, &str)] = &[
    ("projectID", "ProjectID"),
    ("projectSourceID", "ProjectSourceID"),
    ("sourceID", "ProjectSourceID"),
    ("workspaceID", "WorkspaceID"),
    ("serverID", "McpServerID"),
    ("coreInstanceID", "CoreInstanceID"),
    ("engineID", "EngineID"),
    ("startedByEngineID", "EngineID"),
    ("agentID", "AgentID"),
    ("agentSessionID", "AgentSessionID"),
    ("authSessionID", "AgentAuthSessionID"),
    ("conversationID", "ConversationID"),
    ("childConversationID", "ConversationID"),
    ("parentConversationID", "ConversationID"),
    ("namedAgentID", "NamedAgentID"),
    ("runID", "ConversationRunID"),
    ("eventID", "ConversationEventID"),
    ("messageID", "ConversationMessageID"),
    ("runInputID", "ConversationRunInputID"),
    ("requestedAgainstRunID", "ConversationRunID"),
    ("targetRunID", "ConversationRunID"),
    ("deferredReportRunID", "ConversationRunID"),
    ("delegatedFromRunID", "ConversationRunID"),
    ("retryOfInputID", "ConversationRunInputID"),
    ("attachmentID", "ConversationAttachmentID"),
    ("attachmentIDs", "ConversationAttachmentID"),
    ("contextProjectIDs", "ProjectID"),
    ("disabledWorkspaceIDs", "WorkspaceID"),
    ("terminalID", "TerminalID"),
    ("runtimeSessionID", "TerminalSessionID"),
    ("installID", "RuntimeInstallID"),
];

/// `sessionID` is ambiguous (agent vs terminal); resolved per owning type.
pub const TYPE_FIELD_ID_TYPES: &[(&str, &str, &str)] = &[
    (
        "AgentRequestPermissionParams",
        "sessionID",
        "AgentSessionID",
    ),
    ("AgentAskQuestionParams", "sessionID", "AgentSessionID"),
    ("McpImportCandidate", "existingServerID", "McpServerID"),
    ("TerminalSessionSummary", "terminalID", "TerminalID"),
    ("TerminalSessionSummary", "sessionID", "TerminalSessionID"),
    ("TerminalWriteParams", "sessionID", "TerminalSessionID"),
    ("TerminalResizeParams", "sessionID", "TerminalSessionID"),
    ("TerminalCloseParams", "sessionID", "TerminalSessionID"),
    (
        "TerminalOutputDeltaParams",
        "sessionID",
        "TerminalSessionID",
    ),
    ("TerminalExitedParams", "sessionID", "TerminalSessionID"),
    ("AgentAuthTerminal", "sessionID", "TerminalSessionID"),
    // `installID` defaults to `RuntimeInstallID` via ID_FIELDS; the agent
    // trio's is per-agent, not app-scoped.
    ("AgentInstallResult", "installID", "AgentInstallID"),
    ("AgentInstallCancelParams", "installID", "AgentInstallID"),
    ("AgentInstallEventParams", "installID", "AgentInstallID"),
];

/// `*ID`-suffixed fields that really are plain strings on the Swift side
/// (adapter-defined or cross-object references without a newtype).
pub const PLAIN_STRING_ID_FIELDS: &[&str] = &[
    "harnessID",
    "itemID",
    "payloadID",
    "providerMessageID",
    "providerToolCallID",
    "optionID",
    "methodID",
    "modelID",
    "defaultModelID",
    "modeID",
    "defaultModeID",
    "currentModeID",
    "thoughtLevelID",
    "defaultThoughtLevelID",
];

/// Same ruling, scoped to one owning type — for field names too generic to
/// allow globally without gutting the fail-closed check.
pub const TYPE_PLAIN_STRING_ID_FIELDS: &[(&str, &str)] = &[
    ("ConversationSessionSignal", "sessionId"),
    ("ConversationSessionFailure", "id"),
    ("TranscriptAdapterMetadata", "sessionId"),
    ("TranscriptAdapterMetadata", "parentMessageId"),
    ("TranscriptAdapterMetadata", "parentToolCallId"),
    // An OAuth client ID an authorization server issued.
    ("McpSignInParams", "clientID"),
    ("TranscriptBackgroundTask", "id"),
    // Adapter-scoped AIR task ids, opaque to Ottam.
    ("ConversationStopBackgroundTaskParams", "asyncTaskID"),
    ("ConversationSessionConfiguration", "id"),
    ("ConversationSessionMetadata", "currentModeId"),
    ("TranscriptPermission", "requestID"),
    // Core-assigned subagent thread ids: opaque strings clients group by.
    ("ConversationTranscriptItem", "subagentThreadID"),
    ("TranscriptSubagentThread", "id"),
    ("TranscriptSubagentThread", "parentThreadID"),
    ("TranscriptSubagentFollowUp", "threadID"),
    ("TranscriptAdapterMetadata", "subagentThreadID"),
    ("ConversationListTranscriptParams", "subagentThreadID"),
    ("AgentRequestPermissionParams", "subagentThreadID"),
    ("AgentAskQuestionParams", "subagentThreadID"),
    ("TranscriptQuestion", "requestID"),
    // Core-assigned within one question request.
    ("AgentQuestionAnswer", "questionID"),
    ("AgentQuestion", "id"),
    // An adapter-defined select value (mode, thought level), not a wire ID.
    ("EngineSelectOption", "id"),
];

/// Default values for memberwise-init parameters (beyond the automatic `= nil`
/// on Optionals). Swift synthesizes an internal memberwise init for any struct
/// without one, and an extension init with the same labels collides with it —
/// so defaults must live in the generated init itself.
pub const INIT_PARAM_DEFAULTS: &[(&str, &str, &str)] = &[
    ("McpServerSummary", "signedIn", "false"),
    ("McpImportCandidate", "disabledInSource", "false"),
    ("ConversationRunSummary", "usageUnavailable", "false"),
    ("ConversationListTranscriptResult", "sessionStates", "[]"),
    ("ConversationTranscriptDeltaParams", "sessionStates", "[]"),
    ("ConversationTranscriptItem", "adapterMetadata", ".init()"),
    ("ConversationTranscriptItem", "payloads", "[]"),
    ("ConversationTranscriptItem", "previewOmitted", "false"),
    ("TranscriptPayload", "parts", "[]"),
    ("TranscriptPayload", "revision", "0"),
    ("TranscriptPayloadPart", "chunkDigests", "[]"),
    ("ConversationListTranscriptResult", "capture", "[]"),
    ("ConversationTranscriptDeltaParams", "capture", "[]"),
    (
        "ConversationTranscriptDeltaParams",
        "recoveryRequired",
        "false",
    ),
    ("ConversationListTranscriptResult", "backgroundTasks", "[]"),
    ("ConversationTranscriptDeltaParams", "backgroundTasks", "[]"),
    ("ConversationListTranscriptResult", "subagentThreads", "[]"),
    ("ConversationTranscriptDeltaParams", "subagentThreads", "[]"),
    ("TranscriptCaptureStatus", "gaps", "[]"),
    ("ConversationRunSummary", "origin", ".user"),
    // Attachments are the exception, not the rule, on a message submission —
    // every existing call site sends none.
    ("ConversationMessage", "attachments", "[]"),
    // Only delegation sets another source.
    ("ConversationMessage", "source", ".user"),
    ("ConversationInitialMessage", "attachmentIDs", "[]"),
    ("ConversationPostMessageParams", "attachmentIDs", "[]"),
    ("ConversationSteerRunParams", "attachmentIDs", "[]"),
];

pub fn init_param_default(type_name: &str, field: &str) -> Option<&'static str> {
    INIT_PARAM_DEFAULTS
        .iter()
        .find(|(ty, fld, _)| *ty == type_name && *fld == field)
        .map(|(_, _, default)| *default)
}

/// Extra conformances beyond `Codable, Equatable, Sendable`. (`Identifiable`
/// lives in the extensions file — its `id` is computed — but `Hashable` must be
/// declared on the type itself for synthesis to work across files.)
pub const EXTRA_CONFORMANCES: &[(&str, &[&str])] = &[
    ("EngineModelOption", &["Hashable"]),
    ("EngineSelectOption", &["Hashable"]),
];

/// Doc comments preserved from the hand-written file. `field` is empty for a
/// type-level comment. Each string is one `///` line.
pub const DOC_COMMENTS: &[(&str, &str, &[&str])] = &[
    (
        "ConversationRunSummary",
        "",
        &[
            "One run of a Conversation: the engine and settings it ran with, its status,",
            "the harness session it ran on, and whether the user or the agent started it.",
        ],
    ),
    (
        "ConversationListEventsResult",
        "",
        &[
            "Persisted run events in stream order; entries reuse the live delta shape so",
            "historical timelines rebuild through the same code path as live updates.",
        ],
    ),
    (
        "ConversationStartRunParams",
        "modelID",
        &[
            "Adapter-defined model id; nil runs the engine's default model and",
            "omits the key on the wire (older core binaries reject unknown fields).",
        ],
    ),
];

pub fn is_skipped(name: &str) -> bool {
    SKIP_TYPES.contains(&name) || DEFERRED_TYPES.contains(&name)
}

pub fn swift_type_name(definition_name: &str) -> &str {
    TYPE_RENAMES
        .iter()
        .find(|(rust, _)| *rust == definition_name)
        .map(|(_, swift)| *swift)
        .unwrap_or(definition_name)
}

/// Resolve a `{"type": "string"}` property to an ID newtype, plain `String`, or
/// fail closed: an `ID`-suffixed field we have no ruling on is a generation
/// error, so a new wire ID forces a conscious table entry.
pub fn string_field_type(type_name: &str, field: &str) -> &'static str {
    if let Some((_, _, id_type)) = TYPE_FIELD_ID_TYPES
        .iter()
        .find(|(t, f, _)| *t == type_name && *f == field)
    {
        return id_type;
    }
    if let Some((_, id_type)) = ID_FIELDS.iter().find(|(f, _)| *f == field) {
        return id_type;
    }
    if field.ends_with("ID") || field.ends_with("Id") || field == "id" {
        assert!(
            PLAIN_STRING_ID_FIELDS.contains(&field)
                || TYPE_PLAIN_STRING_ID_FIELDS.contains(&(type_name, field)),
            "field `{type_name}.{field}` looks like a wire ID but has no entry in \
             ID_FIELDS, TYPE_FIELD_ID_TYPES, PLAIN_STRING_ID_FIELDS, or \
             TYPE_PLAIN_STRING_ID_FIELDS"
        );
    }
    "String"
}
