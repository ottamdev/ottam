use std::fmt;

#[derive(Debug)]
pub struct StorageError {
    pub kind: &'static str,
    pub message: String,
    pub recoverable: bool,
    source_detail: Option<String>,
    /// The source is Diesel's own message about a value it could not read or
    /// write, which may quote that value, rather than SQLite's.
    source_quotes_values: bool,
}

impl StorageError {
    pub(crate) fn into_core_exit_error(self) -> ottam_protocol::CoreExitError {
        ottam_protocol::CoreExitError {
            kind: self.kind.into(),
            message: self.message,
            details: self.source_detail,
        }
    }

    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            recoverable: false,
            source_detail: None,
            source_quotes_values: false,
        }
    }

    pub fn recoverable(mut self, recoverable: bool) -> Self {
        self.recoverable = recoverable;
        self
    }

    pub fn not_found(entity: &'static str) -> Self {
        Self::new("storageNotFound", format!("{entity} was not found")).recoverable(true)
    }

    pub(crate) fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_detail = Some(source.into());
        self
    }

    /// The failure core does not expect in normal operation, whose cause the
    /// client's generic message hides: a schema that drifted from its
    /// migrations, a corrupt file. Conflicts, constraints, busy and missing
    /// records are expected outcomes their callers already report.
    pub(crate) fn is_unexpected(&self) -> bool {
        matches!(self.kind, "storageUnavailable" | "storageCorrupt")
    }

    /// The underlying SQLite or connection message, which names tables,
    /// columns, constraints and files, never row values, so it is safe to
    /// log. Diesel's messages about a value it could not read or write may
    /// quote it; for those this is only their kind.
    pub(crate) fn source_detail(&self) -> Option<&str> {
        if self.source_quotes_values {
            return Some("a value Diesel could not read or write (message withheld)");
        }
        self.source_detail.as_deref()
    }

    pub(crate) fn classify(
        detail: impl Into<String>,
        fallback_kind: &'static str,
        fallback_message: &'static str,
    ) -> Self {
        let detail = detail.into();
        let normalized = detail.to_ascii_lowercase();
        if normalized.contains("database is locked")
            || normalized.contains("database is busy")
            || normalized.contains("database table is locked")
        {
            return Self::new("storageBusy", "the database is busy")
                .recoverable(true)
                .with_source(detail);
        }
        if normalized.contains("database disk image is malformed")
            || normalized.contains("database corruption")
            || normalized.contains("file is not a database")
            || normalized.contains("not a database")
        {
            return Self::new("storageCorrupt", "the database is corrupt").with_source(detail);
        }
        Self::new(fallback_kind, fallback_message)
            .recoverable(true)
            .with_source(detail)
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

impl From<diesel::result::Error> for StorageError {
    fn from(error: diesel::result::Error) -> Self {
        use diesel::result::{DatabaseErrorKind, Error};
        match error {
            Error::NotFound => Self::not_found("record"),
            Error::DatabaseError(kind, information) => {
                let detail = information.message().to_string();
                match kind {
                    DatabaseErrorKind::UniqueViolation => Self::new(
                        "storageConflict",
                        "a unique storage constraint was violated",
                    )
                    .recoverable(true)
                    .with_source(detail),
                    DatabaseErrorKind::ForeignKeyViolation
                    | DatabaseErrorKind::CheckViolation
                    | DatabaseErrorKind::NotNullViolation => {
                        Self::new("storageConstraint", "a storage constraint was violated")
                            .recoverable(true)
                            .with_source(detail)
                    }
                    // SQLite enforces ON DELETE RESTRICT with an internal
                    // RAISE(ABORT) trigger, so it reports
                    // SQLITE_CONSTRAINT_TRIGGER, which Diesel leaves Unknown.
                    _ if detail == "FOREIGN KEY constraint failed" => {
                        Self::new("storageConstraint", "a storage constraint was violated")
                            .recoverable(true)
                            .with_source(detail)
                    }
                    _ => Self::classify(
                        detail,
                        "storageUnavailable",
                        "the database operation failed",
                    ),
                }
            }
            other @ (Error::DeserializationError(_) | Error::SerializationError(_)) => {
                let mut error = Self::classify(
                    other.to_string(),
                    "storageUnavailable",
                    "the database operation failed",
                );
                error.source_quotes_values = true;
                error
            }
            other => Self::classify(
                other.to_string(),
                "storageUnavailable",
                "the database operation failed",
            ),
        }
    }
}
