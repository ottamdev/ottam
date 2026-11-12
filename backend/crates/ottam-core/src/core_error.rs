use ottam_protocol::ProjectID;
use serde_json::{json, Value};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError {
    pub kind: &'static str,
    pub message: String,
    pub path: Option<String>,
    pub project_id: Option<String>,
    pub recoverable: bool,
}

impl CoreError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            path: None,
            project_id: None,
            recoverable: false,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_project_id(mut self, project_id: &ProjectID) -> Self {
        self.project_id = Some(project_id.0.clone());
        self
    }

    pub fn recoverable(mut self, recoverable: bool) -> Self {
        self.recoverable = recoverable;
        self
    }

    pub fn data(&self) -> Value {
        let mut value = json!({
            "kind": self.kind,
            "recoverable": self.recoverable,
        });
        if let Some(object) = value.as_object_mut() {
            if let Some(path) = &self.path {
                object.insert("path".to_string(), Value::String(path.clone()));
            }
            if let Some(project_id) = &self.project_id {
                object.insert("projectID".to_string(), Value::String(project_id.clone()));
            }
        }
        value
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}

impl From<crate::storage::StorageError> for CoreError {
    fn from(error: crate::storage::StorageError) -> Self {
        Self::new(error.kind, error.message).recoverable(error.recoverable)
    }
}
