use crate::core_error::CoreError;
use ottam_protocol::ProjectID;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn resolve_cwd(
    requested_cwd: Option<&str>,
    root: &Path,
    project_id: Option<&ProjectID>,
) -> Result<PathBuf, CoreError> {
    let cwd = match requested_cwd {
        Some(cwd) if !cwd.trim().is_empty() => PathBuf::from(cwd),
        _ => root.to_path_buf(),
    };
    let canonical = canonicalize_directory(&cwd).map_err(|message| {
        let error = CoreError::new("terminalCwdInvalid", message)
            .with_path(cwd.to_string_lossy())
            .recoverable(true);
        attach_project(error, project_id)
    })?;
    if !canonical.starts_with(root) {
        return Err(attach_project(
            CoreError::new(
                "terminalCwdOutsideWorkingRoot",
                "terminal cwd must be inside its working root",
            )
            .with_path(canonical.to_string_lossy())
            .recoverable(true),
            project_id,
        ));
    }
    Ok(canonical)
}

pub(super) fn canonicalize_directory(path: impl AsRef<Path>) -> Result<PathBuf, String> {
    let path = path.as_ref();
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("failed to resolve terminal cwd {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "terminal cwd is not a directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub(super) fn default_shell() -> String {
    let shell = std::env::var("SHELL").unwrap_or_default();
    if !shell.trim().is_empty()
        && fs::metadata(&shell)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
    {
        return shell;
    }
    "/bin/zsh".to_string()
}

pub(super) fn terminal_term() -> String {
    match std::env::var("TERM") {
        Ok(term) if !term.trim().is_empty() && term != "dumb" => term,
        _ => "xterm-ghostty".to_string(),
    }
}

pub(super) fn terminal_terminfo() -> Option<String> {
    if let Ok(terminfo) = std::env::var("TERMINFO") {
        let trimmed = terminfo.trim();
        if !trimmed.is_empty() && Path::new(trimmed).is_dir() {
            return Some(trimmed.to_string());
        }
    }

    let executable = std::env::current_exe().ok()?;
    let resources = executable.parent()?.parent()?.join("Resources");
    let terminfo = resources.join("terminfo");
    if terminfo.is_dir() {
        return Some(terminfo.to_string_lossy().into_owned());
    }

    let ghostty_terminfo = resources.join("ghostty").join("terminfo");
    if ghostty_terminfo.is_dir() {
        return Some(ghostty_terminfo.to_string_lossy().into_owned());
    }

    None
}

pub(super) fn attach_project(error: CoreError, project_id: Option<&ProjectID>) -> CoreError {
    match project_id {
        Some(project_id) => error.with_project_id(project_id),
        None => error,
    }
}
