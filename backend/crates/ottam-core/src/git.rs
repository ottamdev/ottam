use crate::core_error::CoreError;
use std::env;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_GIT_PATH: &str = "/usr/bin/git";
const DEFAULT_GIT_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_GIT_OUTPUT_MAX_BYTES: usize = 16 * 1024 * 1024;
const GIT_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct GitCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub status_code: Option<i32>,
}

struct CapturedGitStream {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

struct GitStreamReader {
    receiver: Receiver<Result<CapturedGitStream, String>>,
    exceeded_limit: Arc<AtomicBool>,
}

impl GitStreamReader {
    fn exceeded_limit(&self) -> bool {
        self.exceeded_limit.load(Ordering::SeqCst)
    }
}

/// Run blocking git work on the blocking pool.
///
/// Every helper here spawns a subprocess and waits on it with a sleeping poll
/// loop. The server dispatches requests from one serial task, so calling any of
/// them directly from a handler stalls *every* other in-flight request — a
/// snapshot of a repository with many untracked files spawns one subprocess per
/// file and holds the dispatcher for seconds.
pub async fn run_blocking<T, F>(operation: &'static str, action: F) -> Result<T, CoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CoreError> + Send + 'static,
{
    match tokio::task::spawn_blocking(action).await {
        Ok(result) => result,
        Err(error) => Err(CoreError::new(
            "gitTaskFailed",
            format!("git {operation} task failed: {error}"),
        )),
    }
}

pub fn run_git<const N: usize>(repo_path: &str, args: [&str; N]) -> Result<String, CoreError> {
    run_git_with_allowed_exit_codes(repo_path, &args, &[0]).map(|result| result.stdout)
}

pub fn run_git_with_allowed_exit_codes(
    repo_path: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
) -> Result<GitCommandResult, CoreError> {
    run_git_with_timeout(repo_path, args, allowed_exit_codes, git_timeout())
}

/// [`run_git_with_allowed_exit_codes`] with its own deadline, for commands
/// whose cost grows with the checkout (a worktree's checkout or removal) and
/// so cannot share the snapshot timeout.
pub fn run_git_with_timeout(
    repo_path: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
    timeout: Duration,
) -> Result<GitCommandResult, CoreError> {
    let started = Instant::now();
    let git_path = git_executable();
    let mut command = Command::new(&git_path);
    command
        .arg("-C")
        .arg(repo_path)
        .args(args)
        // Core's stdin is the JSONL request stream; git must never read it.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        CoreError::new("gitCommandFailed", format!("failed to run git: {error}"))
            .with_path(repo_path)
            .recoverable(true)
    })?;
    let max_bytes = git_output_max_bytes();
    let stdout = child.stdout.take().ok_or_else(|| {
        CoreError::new(
            "gitCommandFailed",
            format!("failed to capture git {:?} stdout", args),
        )
        .with_path(repo_path)
        .recoverable(true)
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CoreError::new(
            "gitCommandFailed",
            format!("failed to capture git {:?} stderr", args),
        )
        .with_path(repo_path)
        .recoverable(true)
    })?;
    let stdout_reader = read_git_stream(stdout, max_bytes);
    let stderr_reader = read_git_stream(stderr, max_bytes);

    let status = loop {
        if stdout_reader.exceeded_limit() || stderr_reader.exceeded_limit() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(git_output_limit_error(repo_path, args, max_bytes));
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    if stdout_reader.exceeded_limit() || stderr_reader.exceeded_limit() {
                        return Err(git_output_limit_error(repo_path, args, max_bytes));
                    }
                    return Err(CoreError::new(
                        "gitTimeout",
                        format!("git {:?} timed out after {} ms", args, timeout.as_millis()),
                    )
                    .with_path(repo_path)
                    .recoverable(true));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CoreError::new(
                    "gitCommandFailed",
                    format!("failed to wait for git {:?}: {error}", args),
                )
                .with_path(repo_path)
                .recoverable(true));
            }
        }
    };

    if stdout_reader.exceeded_limit() || stderr_reader.exceeded_limit() {
        return Err(git_output_limit_error(repo_path, args, max_bytes));
    }

    let stdout = collect_git_stream(stdout_reader, repo_path, args, "stdout")?;
    let stderr = collect_git_stream(stderr_reader, repo_path, args, "stderr")?;
    if stdout.exceeded_limit || stderr.exceeded_limit {
        return Err(git_output_limit_error(repo_path, args, max_bytes));
    }

    let status_code = status.code();
    if !status_code
        .map(|code| allowed_exit_codes.contains(&code))
        .unwrap_or(false)
    {
        tracing::warn!(
            args = ?args,
            exit_status = ?status_code,
            stderr_bytes = stderr.bytes.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "git command failed"
        );
        return Err(CoreError::new(
            "gitCommandExitedNonZero",
            format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&stderr.bytes).trim()
            ),
        )
        .with_path(repo_path)
        .recoverable(true));
    }

    tracing::debug!(
        args = ?args,
        stdout_bytes = stdout.bytes.len(),
        stderr_bytes = stderr.bytes.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "git command completed"
    );
    Ok(GitCommandResult {
        stdout: String::from_utf8_lossy(&stdout.bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr.bytes).to_string(),
        status_code,
    })
}

pub fn canonical_git_root(path: &str) -> Result<String, CoreError> {
    if path.trim().is_empty() {
        return Err(CoreError::new("pathRequired", "path is required").recoverable(true));
    }

    let output = run_git(path, ["rev-parse", "--show-toplevel"]).map_err(|error| {
        if error.kind == "gitCommandExitedNonZero" {
            CoreError::new(
                "notGitRepository",
                format!("path is not a git repository: {}", error.message),
            )
            .with_path(path)
            .recoverable(true)
        } else {
            error
        }
    })?;
    let root = output.trim().to_string();
    if root.is_empty() {
        return Err(
            CoreError::new("emptyGitRoot", "git returned an empty project root")
                .with_path(path)
                .recoverable(true),
        );
    }
    std::fs::canonicalize(Path::new(&root))
        .map_err(|error| {
            CoreError::new(
                "canonicalizeFailed",
                format!("failed to canonicalize project root {root}: {error}"),
            )
            .with_path(root.clone())
            .recoverable(true)
        })
        .map(|path| path.to_string_lossy().to_string())
}

/// Checking out or deleting a whole tree takes longer than a status read.
const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);

/// The commit `HEAD` names, or `None` in a repository with no commits yet.
pub fn head_commit(repo_path: &str) -> Result<Option<String>, CoreError> {
    let result = run_git_with_allowed_exit_codes(
        repo_path,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        &[0, 1],
    )?;
    let commit = result.stdout.trim();
    Ok((result.status_code == Some(0) && !commit.is_empty()).then(|| commit.to_string()))
}

/// The commit a local branch points at, or `None` when there is no such branch.
pub fn branch_commit(repo_path: &str, branch: &str) -> Result<Option<String>, CoreError> {
    let reference = format!("refs/heads/{branch}");
    let result = run_git_with_allowed_exit_codes(
        repo_path,
        &["rev-parse", "--verify", "--quiet", &reference],
        &[0, 1],
    )?;
    let commit = result.stdout.trim();
    Ok((result.status_code == Some(0) && !commit.is_empty()).then(|| commit.to_string()))
}

/// `git worktree add -b <branch> <path> <commit>` from the primary checkout.
pub fn worktree_add(
    primary_path: &str,
    worktree_path: &Path,
    branch: &str,
    commit: &str,
) -> Result<(), CoreError> {
    let worktree_path = worktree_path.to_string_lossy();
    run_git_with_timeout(
        primary_path,
        &["worktree", "add", "-b", branch, &worktree_path, commit],
        &[0],
        WORKTREE_TIMEOUT,
    )
    .map(|_| ())
}

/// `git worktree remove` from the primary checkout. Forced twice, so neither
/// uncommitted changes nor a lock keep the directory: the user confirmed the
/// removal.
pub fn worktree_remove(primary_path: &str, worktree_path: &Path) -> Result<(), CoreError> {
    let worktree_path = worktree_path.to_string_lossy();
    run_git_with_timeout(
        primary_path,
        &["worktree", "remove", "--force", "--force", &worktree_path],
        &[0],
        WORKTREE_TIMEOUT,
    )
    .map(|_| ())
}

/// Forget worktrees whose directories are gone.
pub fn worktree_prune(primary_path: &str) -> Result<(), CoreError> {
    run_git(primary_path, ["worktree", "prune"]).map(|_| ())
}

/// What a worktree holds that its removal would discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeChanges {
    /// Changed, staged and untracked paths.
    pub uncommitted: u32,
    /// Commits on `HEAD` since `base_commit`.
    pub commits: u32,
}

pub fn worktree_changes(worktree_path: &str, base_commit: &str) -> Result<WorktreeChanges, CoreError> {
    let status = run_git(
        worktree_path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let range = format!("{base_commit}..HEAD");
    let commits = run_git_with_allowed_exit_codes(worktree_path, &["rev-list", "--count", &range], &[0])?
        .stdout
        .trim()
        .parse::<u32>()
        .unwrap_or(0);
    Ok(WorktreeChanges {
        uncommitted: porcelain_entry_count(&status),
        commits,
    })
}

/// Entries in `status --porcelain=v1 -z` output. A rename or copy carries its
/// source path as a second field, which is not a second change.
fn porcelain_entry_count(status: &str) -> u32 {
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    let mut count = 0;
    while let Some(entry) = fields.next() {
        count += 1;
        if entry.starts_with('R') || entry.starts_with('C') {
            fields.next();
        }
    }
    count
}

/// Delete `branch` when it still points at `base_commit`, so a worktree whose
/// agent committed nothing leaves no branch behind. Returns whether the branch
/// is still there afterwards.
pub fn delete_branch_if_unchanged(
    repo_path: &str,
    branch: &str,
    base_commit: &str,
) -> Result<bool, CoreError> {
    match branch_commit(repo_path, branch)? {
        None => Ok(false),
        Some(commit) if commit == base_commit => {
            run_git_with_allowed_exit_codes(repo_path, &["branch", "-D", branch], &[0])?;
            Ok(false)
        }
        Some(_) => Ok(true),
    }
}

fn read_git_stream<R>(mut reader: R, max_bytes: usize) -> GitStreamReader
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let exceeded_limit = Arc::new(AtomicBool::new(false));
    let reader_exceeded_limit = Arc::clone(&exceeded_limit);
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
        let mut exceeded_limit = false;
        let mut buffer = [0; 8 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(error) => {
                    let _ = sender.send(Err(format!("failed to read git output: {error}")));
                    return;
                }
            };
            if count == 0 {
                break;
            }
            if bytes.len() + count > max_bytes {
                exceeded_limit = true;
                reader_exceeded_limit.store(true, Ordering::SeqCst);
                let remaining = max_bytes.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..remaining]);
            } else if !exceeded_limit {
                bytes.extend_from_slice(&buffer[..count]);
            }
        }
        let _ = sender.send(Ok(CapturedGitStream {
            bytes,
            exceeded_limit,
        }));
    });
    GitStreamReader {
        receiver,
        exceeded_limit,
    }
}

fn collect_git_stream(
    reader: GitStreamReader,
    repo_path: &str,
    args: &[&str],
    stream_name: &str,
) -> Result<CapturedGitStream, CoreError> {
    match reader.receiver.recv_timeout(GIT_OUTPUT_DRAIN_TIMEOUT) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(CoreError::new(
            "gitCommandFailed",
            format!("failed to collect git {:?} {stream_name}: {error}", args),
        )
        .with_path(repo_path)
        .recoverable(true)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CoreError::new(
            "gitCommandFailed",
            format!(
                "timed out collecting git {:?} {stream_name} after {} ms",
                args,
                GIT_OUTPUT_DRAIN_TIMEOUT.as_millis()
            ),
        )
        .with_path(repo_path)
        .recoverable(true)),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CoreError::new(
            "gitCommandFailed",
            format!(
                "failed to collect git {:?} {stream_name}: reader disconnected",
                args
            ),
        )
        .with_path(repo_path)
        .recoverable(true)),
    }
}

fn git_output_limit_error(repo_path: &str, args: &[&str], max_bytes: usize) -> CoreError {
    CoreError::new(
        "gitOutputLimitExceeded",
        format!("git {:?} output exceeded {} bytes", args, max_bytes),
    )
    .with_path(repo_path)
    .recoverable(true)
}

fn git_executable() -> String {
    env::var("OTTAM_GIT")
        .ok()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_PATH.to_string())
}

fn git_timeout() -> Duration {
    env::var("OTTAM_GIT_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_GIT_TIMEOUT_MS))
}

fn git_output_max_bytes() -> usize {
    env::var("OTTAM_GIT_OUTPUT_MAX_BYTES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GIT_OUTPUT_MAX_BYTES)
}
