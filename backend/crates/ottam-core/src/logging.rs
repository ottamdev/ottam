use std::env;
use std::fs::{self, File, OpenOptions};
use std::panic::{Location, PanicHookInfo};
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const DEFAULT_FILTER: &str = "ottam_core=info,warn";

pub struct LoggingGuard {
    _file_guard: WorkerGuard,
    _stderr_guard: Option<WorkerGuard>,
    pub log_file: PathBuf,
}

pub fn init_from_environment() -> Result<LoggingGuard, String> {
    let log_file = resolve_log_file()?;
    if let Some(parent) = log_file.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create log directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let log_file_handle = create_private_log_file(&log_file)?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(log_file_handle);
    let file_layer = fmt::layer()
        .compact()
        .with_ansi(false)
        .with_target(true)
        .with_writer(file_writer);

    let (filter, invalid_filter) = env_filter_from_environment();

    if mirror_to_stderr() {
        let (stderr_writer, stderr_guard) = tracing_appender::non_blocking(std::io::stderr());
        let stderr_layer = fmt::layer()
            .compact()
            .with_ansi(false)
            .with_target(true)
            .with_writer(stderr_writer);
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(stderr_layer)
            .try_init()
            .map_err(|error| format!("failed to initialize tracing subscriber: {error}"))?;
        emit_startup_events(&log_file, invalid_filter);
        return Ok(LoggingGuard {
            _file_guard: file_guard,
            _stderr_guard: Some(stderr_guard),
            log_file,
        });
    }

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .try_init()
        .map_err(|error| format!("failed to initialize tracing subscriber: {error}"))?;
    emit_startup_events(&log_file, invalid_filter);
    Ok(LoggingGuard {
        _file_guard: file_guard,
        _stderr_guard: None,
        log_file,
    })
}

/// Logs every panic through `tracing` (thread and location), then runs the
/// previous hook so the default stderr report, which the app captures to
/// `core-*.stderr.log`, still appears. The panic message stays in that report
/// only: it can carry formatted values, and payloads stay out of the trace log.
/// Install after logging is initialized.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let report = PanicReport::new(std::thread::current().name(), info.location());
        tracing::error!(
            thread = %report.thread,
            location = %report.location,
            "ottam-core panicked; message in core stderr log"
        );
        previous(info);
    }));
}

#[derive(Debug, PartialEq, Eq)]
struct PanicReport {
    thread: String,
    location: String,
}

impl PanicReport {
    fn new(thread: Option<&str>, location: Option<&Location<'_>>) -> Self {
        Self {
            thread: thread.unwrap_or("<unnamed>").to_string(),
            location: location.map_or_else(|| "<unknown>".to_string(), Location::to_string),
        }
    }
}

fn emit_startup_events(log_file: &Path, invalid_filter: Option<InvalidFilter>) {
    tracing::info!(
        log_file = %log_file.display(),
        pid = std::process::id(),
        "ottam-core logging initialized"
    );
    if let Some(invalid) = invalid_filter {
        tracing::warn!(
            source = invalid.source,
            value = invalid.value,
            error = invalid.error,
            fallback = DEFAULT_FILTER,
            "invalid log filter; using quiet default"
        );
    }
}

fn env_filter_from_environment() -> (EnvFilter, Option<InvalidFilter>) {
    for source in ["OTTAM_LOG", "RUST_LOG"] {
        let Ok(value) = env::var(source) else {
            continue;
        };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        match EnvFilter::try_new(trimmed) {
            Ok(filter) => return (filter, None),
            Err(error) => {
                let fallback = EnvFilter::try_new(DEFAULT_FILTER)
                    .expect("default ottam-core log filter is valid");
                return (
                    fallback,
                    Some(InvalidFilter {
                        source,
                        value: trimmed.to_string(),
                        error: error.to_string(),
                    }),
                );
            }
        }
    }

    (
        EnvFilter::try_new(DEFAULT_FILTER).expect("default ottam-core log filter is valid"),
        None,
    )
}

fn resolve_log_file() -> Result<PathBuf, String> {
    if let Some(configured) = non_empty_env("OTTAM_CORE_LOG_FILE") {
        return Ok(PathBuf::from(configured));
    }

    Ok(resolve_log_dir()?.join(format!("core-{}.log", std::process::id())))
}

fn resolve_log_dir() -> Result<PathBuf, String> {
    if let Some(configured) = non_empty_env("OTTAM_LOG_DIR") {
        return Ok(PathBuf::from(configured));
    }
    Ok(ottam_home()?.join("logs"))
}

fn ottam_home() -> Result<PathBuf, String> {
    if let Some(configured) = non_empty_env("OTTAM_HOME") {
        return Ok(PathBuf::from(configured));
    }
    let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".ottam"))
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn mirror_to_stderr() -> bool {
    matches!(
        env::var("OTTAM_LOG_TO_STDERR").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

#[cfg(unix)]
fn create_private_log_file(path: &Path) -> Result<File, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("failed to open backend log {}: {error}", path.display()))?;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to secure backend log {}: {error}", path.display()))?;

    Ok(file)
}

#[cfg(not(unix))]
fn create_private_log_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open backend log {}: {error}", path.display()))
}

struct InvalidFilter {
    source: &'static str,
    value: String,
    error: String,
}

#[cfg(test)]
mod tests {
    use super::{create_private_log_file, PanicReport};
    use std::panic::Location;

    #[test]
    fn panic_report_formats_thread_and_location() {
        let location = Location::caller();
        let report = PanicReport::new(Some("main"), Some(location));
        assert_eq!(
            report,
            PanicReport {
                thread: "main".to_string(),
                location: format!("{}:{}:{}", file!(), location.line(), location.column()),
            }
        );

        let unnamed = PanicReport::new(None, None);
        assert_eq!(unnamed.thread, "<unnamed>");
        assert_eq!(unnamed.location, "<unknown>");
    }

    #[cfg(unix)]
    #[test]
    fn create_private_log_file_tightens_existing_permissions() {
        use std::fs;
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::TempDir::new().expect("creates temp dir");
        let log_path = temp_dir.path().join("core-existing.log");
        fs::write(&log_path, "existing\n").expect("writes existing log");
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o644))
            .expect("loosens test permissions");

        let mut log_file = create_private_log_file(&log_path).expect("opens private log");
        writeln!(log_file, "next").expect("appends to log");

        let mode = fs::metadata(&log_path)
            .expect("reads log metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
