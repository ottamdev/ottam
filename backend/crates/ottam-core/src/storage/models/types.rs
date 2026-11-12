use crate::storage::error::StorageError;
use chrono::{DateTime, Utc};
use std::fmt;
use uuid::Uuid;

macro_rules! domain_id {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StorageError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Uuid::parse_str(value).map(Self).map_err(|_| {
                    StorageError::new(
                        "storageCorrupt",
                        concat!("persisted ", $label, " is not a valid UUID"),
                    )
                })
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_from(value.as_str())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

domain_id!(WorkspaceId, "WorkspaceID");
domain_id!(ProjectId, "ProjectID");
domain_id!(ProjectSourceId, "ProjectSourceID");
domain_id!(ConversationId, "ConversationID");
domain_id!(ConversationMessageId, "ConversationMessageID");
domain_id!(AgentSessionId, "AgentSessionID");
domain_id!(ConversationRunId, "ConversationRunID");
domain_id!(ConversationRunInputId, "ConversationRunInputID");
domain_id!(RunEventId, "RunEventID");
domain_id!(ConversationAttachmentId, "ConversationAttachmentID");

macro_rules! storage_enum {
    ($name:ident, $label:literal, { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_storage_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StorageError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(StorageError::new(
                        "storageCorrupt",
                        concat!("persisted ", $label, " is invalid"),
                    )),
                }
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_from(value.as_str())
            }
        }
    };
}

storage_enum!(WorkingRootKind, "working-root kind", {
    WorkspaceFiles => "workspaceFiles",
    Project => "project",
});

storage_enum!(ConversationProjectRole, "ConversationProject role", {
    Primary => "primary",
    Context => "context",
});

storage_enum!(MessageRole, "ConversationMessage role", {
    User => "user",
    System => "system",
});

storage_enum!(MessageSource, "ConversationMessage source", {
    User => "user",
    Delegation => "delegation",
    NoteDelivery => "noteDelivery",
});

storage_enum!(AgentSessionStatus, "AgentSession status", {
    Active => "active",
    Closed => "closed",
    Lost => "lost",
});

storage_enum!(SessionOrigin, "ConversationRun session origin", {
    New => "new",
    Continued => "continued",
    Restored => "restored",
});

storage_enum!(RunStatus, "ConversationRun status", {
    Queued => "queued",
    Running => "running",
    Completed => "completed",
    Failed => "failed",
    Canceled => "canceled",
    Interrupted => "interrupted",
});

storage_enum!(RunOrigin, "ConversationRun origin", {
    User => "user",
    Autonomous => "autonomous",
});

storage_enum!(RunInputKind, "ConversationRunInput kind", {
    Initial => "initial",
    Steer => "steer",
    Retry => "retry",
});

storage_enum!(RunInputStatus, "ConversationRunInput status", {
    Pending => "pending",
    Sending => "sending",
    Delivered => "delivered",
    DeliveryUnknown => "deliveryUnknown",
    Failed => "failed",
    Superseded => "superseded",
});

storage_enum!(DeliveryMode, "ConversationRunInput delivery mode", {
    Prompt => "prompt",
    NativeSteer => "nativeSteer",
    InterruptThenPrompt => "interruptThenPrompt",
});

storage_enum!(DeliveryEvidence, "ConversationRunInput delivery evidence", {
    AdapterConfirmed => "adapterConfirmed",
    UserMarkedDelivered => "userMarkedDelivered",
});

storage_enum!(InputResolution, "ConversationRunInput resolution", {
    MarkedDelivered => "markedDelivered",
    RetryCreated => "retryCreated",
});

storage_enum!(AttachmentKind, "ConversationAttachment kind", {
    Image => "image",
});

storage_enum!(AttachmentMimeType, "ConversationAttachment MIME type", {
    Png => "image/png",
    Jpeg => "image/jpeg",
    Gif => "image/gif",
    Webp => "image/webp",
});

impl AttachmentMimeType {
    /// The extension Core gives the stored file. Chosen from the sniffed type,
    /// never from a caller-supplied file name.
    pub fn file_extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }
}

storage_enum!(AttachmentSourceKind, "ConversationAttachment source kind", {
    Pasted => "pasted",
    Dropped => "dropped",
    Picked => "picked",
});

storage_enum!(RunSourceRole, "ConversationRunSource role", {
    Primary => "primary",
    Context => "context",
});

storage_enum!(RunEventKind, "RunEvent kind", {
    RunStatus => "runStatus",
    PermissionRequested => "permissionRequested",
    PermissionResolved => "permissionResolved",
    SteerRequested => "steerRequested",
    SteerDelivered => "steerDelivered",
    RunInterrupted => "runInterrupted",
    Usage => "usage",
    SessionMetadata => "sessionMetadata",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSourceKind {
    LocalGit,
}

impl ProjectSourceKind {
    pub fn as_storage_str(self) -> &'static str {
        match self {
            Self::LocalGit => "localGit",
        }
    }
}

impl TryFrom<String> for ProjectSourceKind {
    type Error = StorageError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "localGit" => Ok(Self::LocalGit),
            _ => Err(StorageError::new(
                "storageCorrupt",
                "persisted ProjectSource kind is invalid",
            )),
        }
    }
}

pub(super) fn timestamp(value: i64, field: &'static str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::from_timestamp_millis(value).ok_or_else(|| {
        StorageError::new(
            "storageCorrupt",
            format!("persisted {field} timestamp is invalid"),
        )
    })
}

pub(super) fn optional_timestamp(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    value.map(|value| timestamp(value, field)).transpose()
}
