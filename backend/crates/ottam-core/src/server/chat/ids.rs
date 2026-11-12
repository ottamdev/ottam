use crate::storage::error::StorageError;
use crate::storage::models::{
    ConversationAttachmentId, ConversationId, ConversationRunId, ConversationRunInputId, ProjectId,
    ProjectSourceId, WorkspaceId,
};
use ottam_protocol::{
    ConversationAttachmentID, ConversationID, ConversationRunID, ConversationRunInputID, ProjectID,
    ProjectSourceID, WorkspaceID,
};

pub(super) fn parse_workspace_id(value: &WorkspaceID) -> Result<WorkspaceId, StorageError> {
    WorkspaceId::try_from(value.0.as_str()).map_err(|_| unknown_workspace())
}

pub(super) fn parse_attachment_id(
    value: &ConversationAttachmentID,
) -> Result<ConversationAttachmentId, StorageError> {
    ConversationAttachmentId::try_from(value.0.as_str()).map_err(|_| {
        StorageError::new("unknownAttachmentID", "the attachment is unavailable").recoverable(true)
    })
}

pub(super) fn parse_conversation_id(
    value: &ConversationID,
) -> Result<ConversationId, StorageError> {
    ConversationId::try_from(value.0.as_str()).map_err(|_| {
        StorageError::new("unknownConversationID", "Conversation is unavailable").recoverable(true)
    })
}

pub(super) fn parse_run_id(value: &ConversationRunID) -> Result<ConversationRunId, StorageError> {
    ConversationRunId::try_from(value.0.as_str()).map_err(|_| unknown_run())
}

pub(super) fn parse_input_id(
    value: &ConversationRunInputID,
) -> Result<ConversationRunInputId, StorageError> {
    ConversationRunInputId::try_from(value.0.as_str()).map_err(|_| {
        StorageError::new("unknownRunInputID", "the ConversationRunInput is unknown")
            .recoverable(true)
    })
}

pub(super) fn parse_project_id(value: &ProjectID) -> Result<ProjectId, StorageError> {
    ProjectId::try_from(value.0.as_str()).map_err(|_| {
        StorageError::new("unknownProjectID", "the Project is unknown").recoverable(true)
    })
}

pub(super) fn parse_source_id(value: &ProjectSourceID) -> Result<ProjectSourceId, StorageError> {
    ProjectSourceId::try_from(value.0.as_str()).map_err(|_| {
        StorageError::new("unknownProjectSourceID", "the ProjectSource is unknown")
            .recoverable(true)
    })
}

pub(super) fn unknown_workspace() -> StorageError {
    StorageError::new("unknownWorkspaceID", "Workspace is unavailable").recoverable(true)
}

pub(super) fn unknown_run() -> StorageError {
    StorageError::new(
        "unknownConversationRunID",
        "the run does not belong to this Conversation",
    )
    .recoverable(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_wire_ids_keep_their_typed_error_contract() {
        let malformed = "not-a-uuid".to_string();
        let cases = [
            (
                "workspace",
                parse_workspace_id(&WorkspaceID(malformed.clone())).expect_err("workspace id"),
                "unknownWorkspaceID",
                "Workspace is unavailable",
            ),
            (
                "attachment",
                parse_attachment_id(&ConversationAttachmentID(malformed.clone()))
                    .expect_err("attachment id"),
                "unknownAttachmentID",
                "the attachment is unavailable",
            ),
            (
                "conversation",
                parse_conversation_id(&ConversationID(malformed.clone()))
                    .expect_err("conversation id"),
                "unknownConversationID",
                "Conversation is unavailable",
            ),
            (
                "run",
                parse_run_id(&ConversationRunID(malformed.clone())).expect_err("run id"),
                "unknownConversationRunID",
                "the run does not belong to this Conversation",
            ),
            (
                "input",
                parse_input_id(&ConversationRunInputID(malformed.clone())).expect_err("input id"),
                "unknownRunInputID",
                "the ConversationRunInput is unknown",
            ),
            (
                "project",
                parse_project_id(&ProjectID(malformed.clone())).expect_err("project id"),
                "unknownProjectID",
                "the Project is unknown",
            ),
            (
                "source",
                parse_source_id(&ProjectSourceID(malformed)).expect_err("source id"),
                "unknownProjectSourceID",
                "the ProjectSource is unknown",
            ),
        ];

        for (name, error, kind, message) in cases {
            assert_eq!(error.kind, kind, "{name} kind");
            assert_eq!(error.message, message, "{name} message");
            assert!(error.recoverable, "{name} must remain recoverable");
        }
    }
}
