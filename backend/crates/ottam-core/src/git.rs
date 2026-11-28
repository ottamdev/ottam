use crate::core_error::CoreError;
use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
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

/// The commit a branch or ref names, or `None` when no such ref exists.
pub fn resolve_commit(repo_path: &str, reference: &str) -> Result<Option<String>, CoreError> {
    if reference.is_empty()
        || reference.starts_with('-')
        || reference.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(CoreError::new(
            "worktreeInvalidBaseRef",
            "The selected base branch is not a valid git reference.",
        )
        .with_path(repo_path)
        .recoverable(true));
    }
    let spec = format!("{reference}^{{commit}}");
    let result = run_git_with_allowed_exit_codes(
        repo_path,
        &["rev-parse", "--verify", "--quiet", &spec],
        &[0, 1],
    )?;
    let commit = result.stdout.trim();
    Ok((result.status_code == Some(0) && !commit.is_empty()).then(|| commit.to_string()))
}

/// Branches on the primary checkout that a new worktree can cut from, plus
/// HEAD and dirty-tree context for the composer picker.
pub fn worktree_base_branches(
    repo_path: &str,
) -> Result<ottam_protocol::WorktreeBaseBranchesResult, CoreError> {
    use ottam_protocol::{WorktreeBaseBranch, WorktreeBaseBranchKind, WorktreeBaseBranchesResult};

    let status = run_git(
        repo_path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    let primary_has_uncommitted_changes = !status.is_empty();

    let Some(head) = head_commit(repo_path)? else {
        return Ok(WorktreeBaseBranchesResult {
            current_branch: None,
            detached_head: None,
            default_branch_ref: None,
            primary_has_uncommitted_changes,
            branches: Vec::new(),
        });
    };

    let symbolic = run_git_with_allowed_exit_codes(
        repo_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        &[0, 1],
    )?;
    let (current_branch, detached_head) = if symbolic.status_code == Some(0) {
        let name = symbolic.stdout.trim();
        if name.is_empty() {
            (None, Some(head.chars().take(7).collect()))
        } else {
            (Some(name.to_string()), None)
        }
    } else {
        (None, Some(head.chars().take(7).collect()))
    };

    let refs_output = run_git(
        repo_path,
        [
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(symref)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;

    let mut local = Vec::new();
    let mut remote = Vec::new();
    let mut origin_head_target = None;

    for line in refs_output.lines() {
        if line.is_empty() {
            continue;
        }
        let (ref_name, symref) = line.split_once('\t').unwrap_or((line, ""));
        if !symref.is_empty() {
            if ref_name == "refs/remotes/origin/HEAD" {
                origin_head_target = Some(symref.to_string());
            }
            continue;
        }
        if let Some(name) = ref_name.strip_prefix("refs/heads/") {
            local.push(WorktreeBaseBranch {
                name: name.to_string(),
                ref_name: ref_name.to_string(),
                kind: WorktreeBaseBranchKind::Local,
            });
        } else if let Some(name) = ref_name.strip_prefix("refs/remotes/") {
            remote.push(WorktreeBaseBranch {
                name: name.to_string(),
                ref_name: ref_name.to_string(),
                kind: WorktreeBaseBranchKind::Remote,
            });
        }
    }

    let default_branch_ref = detect_default_branch_ref(&local, &remote, origin_head_target.as_deref());
    let current_ref = current_branch.as_deref().map(|name| format!("refs/heads/{name}"));

    let mut ordered = Vec::with_capacity(local.len() + remote.len());
    if let Some(ref current_ref) = current_ref {
        if let Some(pos) = local.iter().position(|branch| &branch.ref_name == current_ref) {
            ordered.push(local.remove(pos));
        }
    }
    if let Some(ref default_ref) = default_branch_ref {
        if let Some(pos) = local.iter().position(|branch| &branch.ref_name == default_ref) {
            ordered.push(local.remove(pos));
        }
    }
    let (managed, regular): (Vec<_>, Vec<_>) =
        local.into_iter().partition(|branch| branch.name.starts_with("ottam/"));
    ordered.extend(regular);
    ordered.extend(managed);

    if let Some(ref default_ref) = default_branch_ref {
        if let Some(pos) = remote.iter().position(|branch| &branch.ref_name == default_ref) {
            ordered.push(remote.remove(pos));
        }
    }
    ordered.extend(remote);

    Ok(WorktreeBaseBranchesResult {
        current_branch,
        detached_head,
        default_branch_ref,
        primary_has_uncommitted_changes,
        branches: ordered,
    })
}

fn detect_default_branch_ref(
    local: &[ottam_protocol::WorktreeBaseBranch],
    remote: &[ottam_protocol::WorktreeBaseBranch],
    origin_head_target: Option<&str>,
) -> Option<String> {
    if let Some(target) = origin_head_target {
        if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
            let local_ref = format!("refs/heads/{name}");
            if local.iter().any(|branch| branch.ref_name == local_ref) {
                return Some(local_ref);
            }
        }
        if remote.iter().any(|branch| branch.ref_name == target) {
            return Some(target.to_string());
        }
    }
    for candidate in ["refs/heads/main", "refs/heads/master"] {
        if local.iter().any(|branch| branch.ref_name == candidate) {
            return Some(candidate.to_string());
        }
    }
    for candidate in ["refs/remotes/origin/main", "refs/remotes/origin/master"] {
        if remote.iter().any(|branch| branch.ref_name == candidate) {
            return Some(candidate.to_string());
        }
    }
    None
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
pub fn worktree_add(primary_path: &str, worktree_path: &Path, branch: &str) -> Result<(), CoreError> {
    let worktree_path = worktree_path.to_string_lossy();
    run_git_with_timeout(
        primary_path,
        &["worktree", "add", &worktree_path, branch],
        &[0],
        WORKTREE_TIMEOUT,
    )
    .map(|_| ())
}

/// Create `branch` at `commit` only if no branch has the name: one ref
/// transaction, so it claims the name against any other writer. False when
/// the name was taken.
pub fn create_branch(primary_path: &str, branch: &str, commit: &str) -> Result<bool, CoreError> {
    let output = run_git_with_timeout(
        primary_path,
        &["branch", "--no-track", "--", branch, commit],
        &[0, 128],
        WORKTREE_TIMEOUT,
    )?;
    if output.status_code == Some(0) {
        return Ok(true);
    }
    if branch_commit(primary_path, branch)?.is_some() {
        return Ok(false);
    }
    Err(CoreError::new(
        "worktreeCreateFailed",
        format!("git could not create the branch {branch}"),
    )
    .recoverable(true))
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

/// What becomes of a worktree's branch when the worktree goes. Status and
/// removal both decide by this, so the confirmation names the branch's fate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchFate {
    Missing,
    /// Still at the commit it started from: removal deletes it.
    Unchanged,
    /// Moved off that commit: removal keeps it.
    Moved { tip: String },
}

pub fn branch_fate(repo_path: &str, branch: &str, base_commit: &str) -> Result<BranchFate, CoreError> {
    Ok(match branch_commit(repo_path, branch)? {
        None => BranchFate::Missing,
        Some(tip) if tip == base_commit => BranchFate::Unchanged,
        Some(tip) => BranchFate::Moved { tip },
    })
}

/// What a worktree holds that its removal would discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeChanges {
    /// Changed, staged and untracked paths.
    pub uncommitted: u32,
    /// Commits on the worktree's branch since `base_commit`.
    pub branch_commits: u32,
    /// Whether removal keeps the branch ([`BranchFate::Moved`]).
    pub branch_kept: bool,
    /// Commits at the worktree's `HEAD` that no branch, tag or remote holds,
    /// as after the agent detached `HEAD`: removal keeps them under
    /// [`recovery_ref`].
    pub unbranched_commits: u32,
}

pub fn worktree_changes(
    worktree_path: &str,
    branch: &str,
    base_commit: &str,
) -> Result<WorktreeChanges, CoreError> {
    let status = run_git(
        worktree_path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let fate = branch_fate(worktree_path, branch, base_commit)?;
    let branch_commits = match &fate {
        BranchFate::Moved { tip } => commit_count(worktree_path, &[&format!("{base_commit}..{tip}")])?,
        BranchFate::Missing | BranchFate::Unchanged => 0,
    };
    let unbranched_commits = commit_count(
        worktree_path,
        &["HEAD", "--not", "--branches", "--tags", "--remotes"],
    )?;
    Ok(WorktreeChanges {
        uncommitted: porcelain_entry_count(&status),
        branch_commits,
        branch_kept: matches!(fate, BranchFate::Moved { .. }),
        unbranched_commits,
    })
}

/// Where removal keeps a worktree's unbranched commits: `ottam/<name>` keeps
/// them under `refs/ottam/removed/<name>`.
pub fn recovery_ref(branch: &str) -> String {
    format!("refs/ottam/removed/{}", branch.strip_prefix("ottam/").unwrap_or(branch))
}

/// A worktree's `HEAD.lock`, git's own lock on its HEAD: while held, no
/// commit or checkout in that worktree moves HEAD. Dropping it releases the
/// lock; after `git worktree remove` it is already gone with the admin dir.
pub struct HeadLock(PathBuf);

impl Drop for HeadLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        held_head_locks().remove(&self.0);
    }
}

/// The `HEAD.lock`s this process holds. A startup retry can overlap a
/// request's removal of the same worktree; the second waits its turn rather
/// than sharing a lock the first would drop under it.
fn held_head_locks() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    static HELD: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a removal writes into `HEAD.lock`, with its process id, so a later
/// launch can tell a crashed removal's lock from a running git command's or
/// a live removal's.
const HEAD_LOCK_MARKER: &str = "ottam: removing this worktree, pid ";

fn head_lock_marker(pid: u32) -> String {
    format!("{HEAD_LOCK_MARKER}{pid}\n")
}

/// A marked lock is free to take over only when its owner is gone: this
/// process holds no lock there (checked by the caller), and the pid it names
/// is this process or one no longer running.
fn is_abandoned_head_lock(contents: &str) -> bool {
    let Some(pid) = contents
        .strip_prefix(HEAD_LOCK_MARKER)
        .and_then(|rest| rest.trim_end().parse::<u32>().ok())
    else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks whether the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    !alive
}

fn lock_head(worktree_path: &str) -> Result<HeadLock, CoreError> {
    let admin = run_git_with_allowed_exit_codes(worktree_path, &["rev-parse", "--absolute-git-dir"], &[0])?;
    let path = Path::new(admin.stdout.trim()).join("HEAD.lock");
    let busy = || CoreError::new("gitBusy", "a git command is running in the worktree").recoverable(true);
    let mut held = held_head_locks();
    if held.contains(&path) {
        return Err(busy());
    }
    let marker = head_lock_marker(std::process::id());
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(marker.as_bytes()));
    match created {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let abandoned = std::fs::read_to_string(&path).is_ok_and(|contents| is_abandoned_head_lock(&contents));
            if !abandoned {
                return Err(busy());
            }
            std::fs::write(&path, &marker)
                .map_err(|error| CoreError::new("gitLockFailed", error.to_string()).recoverable(true))?;
        }
        Err(error) => return Err(CoreError::new("gitLockFailed", error.to_string()).recoverable(true)),
    }
    held.insert(path.clone());
    Ok(HeadLock(path))
}

/// Lock the worktree's HEAD, then point [`recovery_ref`] at it when commits
/// there are held by no branch, tag or remote. Hold the lock until the
/// folder is gone, so no commit lands after the read, and removing the
/// folder loses none, whatever was committed after the confirmation.
/// Returns the lock and the ref written.
pub fn keep_unbranched_head(
    worktree_path: &str,
    branch: &str,
) -> Result<(HeadLock, Option<String>), CoreError> {
    let lock = lock_head(worktree_path)?;
    let Some(head) = head_commit(worktree_path)? else {
        return Ok((lock, None));
    };
    if commit_count(worktree_path, &[&head, "--not", "--branches", "--tags", "--remotes"])? == 0 {
        return Ok((lock, None));
    }
    let reference = recovery_ref(branch);
    run_git_with_allowed_exit_codes(worktree_path, &["update-ref", &reference, &head], &[0])?;
    Ok((lock, Some(reference)))
}

fn commit_count(repo_path: &str, revisions: &[&str]) -> Result<u32, CoreError> {
    let mut args = vec!["rev-list", "--count"];
    args.extend_from_slice(revisions);
    Ok(run_git_with_allowed_exit_codes(repo_path, &args, &[0])?
        .stdout
        .trim()
        .parse::<u32>()
        .unwrap_or(0))
}

/// Entries in `status --porcelain=v1 -z` output. A rename or copy carries its
/// source path as a second field, which is not a second change; either
/// column can hold it (an unstaged rename after `git add -N` is ` R`).
fn porcelain_entry_count(status: &str) -> u32 {
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    let mut count = 0;
    while let Some(entry) = fields.next() {
        count += 1;
        let columns = entry.as_bytes().get(..2).unwrap_or_default();
        if columns.iter().any(|column| matches!(column, b'R' | b'C')) {
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
    match branch_fate(repo_path, branch, base_commit)? {
        BranchFate::Missing => Ok(false),
        BranchFate::Moved { .. } => Ok(true),
        BranchFate::Unchanged => {
            // `update-ref` deletes a checked-out branch, which `branch -D`
            // refuses; a checkout that reused the name keeps it.
            if branch_checked_out(repo_path, branch)? {
                return Ok(true);
            }
            // Deletes only while the ref still holds `base_commit`, so a
            // commit that lands after `branch_fate` read it keeps the branch.
            let reference = format!("refs/heads/{branch}");
            let deleted = run_git_with_allowed_exit_codes(
                repo_path,
                &["update-ref", "-d", &reference, base_commit],
                &[0, 1, 128],
            )?;
            if deleted.status_code == Some(0) {
                Ok(false)
            } else {
                Ok(branch_commit(repo_path, branch)?.is_some())
            }
        }
    }
}

/// Whether any worktree of the repository has `branch` checked out.
fn branch_checked_out(repo_path: &str, branch: &str) -> Result<bool, CoreError> {
    let worktrees = run_git(repo_path, ["worktree", "list", "--porcelain"])?;
    let checked_out = format!("branch refs/heads/{branch}");
    Ok(worktrees.lines().any(|line| line == checked_out))
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

#[cfg(test)]
mod tests {
    use super::{
        branch_commit, create_branch, delete_branch_if_unchanged, keep_unbranched_head,
        head_lock_marker, porcelain_entry_count, run_git_with_allowed_exit_codes,
    };
    use std::path::Path;

    #[test]
    fn worktree_changes_count_a_rename_once() {
        assert_eq!(porcelain_entry_count(""), 0);
        assert_eq!(
            porcelain_entry_count(" M src/a.rs\0?? notes.txt\0R  new.rs\0old.rs\0"),
            3
        );
        // Unstaged, after `git add -N new.rs`.
        assert_eq!(porcelain_entry_count(" R new.rs\0old.rs\0 M src/a.rs\0"), 2);
        assert_eq!(porcelain_entry_count("RM new.rs\0old.rs\0"), 1);
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let repo = repo.to_string_lossy();
        run_git_with_allowed_exit_codes(&repo, args, &[0])
            .unwrap_or_else(|error| panic!("git {args:?} failed: {}", error.message))
            .stdout
            .trim()
            .to_string()
    }

    fn repo_with_commit() -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        git(repo, &["config", "user.email", "ottam@example.invalid"]);
        git(repo, &["config", "user.name", "Ottam"]);
        git(repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let base = git(repo, &["rev-parse", "HEAD"]);
        (temp, base)
    }

    #[test]
    fn a_branch_is_claimed_once_and_another_writers_branch_is_left_alone() {
        let (temp, base) = repo_with_commit();
        let repo = temp.path();
        let path = repo.to_string_lossy().into_owned();

        assert_eq!(create_branch(&path, "ottam/claimed", &base).ok(), Some(true));
        // Someone else's branch at the same name and base stays as it was.
        git(repo, &["commit", "--quiet", "--allow-empty", "-m", "theirs"]);
        let theirs = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["branch", "ottam/theirs", &theirs]);
        assert_eq!(create_branch(&path, "ottam/theirs", &base).ok(), Some(false));
        assert_eq!(branch_commit(&path, "ottam/theirs").ok(), Some(Some(theirs)));
        assert!(create_branch(&path, "not a valid ref", &base).is_err());
    }

    #[test]
    fn a_detached_commit_is_kept_under_a_recovery_ref() {
        let (temp, base) = repo_with_commit();
        let repo = temp.path();
        let path = repo.to_string_lossy().into_owned();

        let commit = |message: &str| {
            run_git_with_allowed_exit_codes(&path, &["commit", "--quiet", "--allow-empty", "-m", message], &[0])
        };

        // Every commit on a branch: nothing to keep.
        let (lock, kept) = keep_unbranched_head(&path, "ottam/quiet-fox-1a2b").expect("keep");
        assert_eq!(kept, None);
        drop(lock);

        git(repo, &["checkout", "--quiet", "--detach", &base]);
        git(repo, &["commit", "--quiet", "--allow-empty", "-m", "detached"]);
        let detached = git(repo, &["rev-parse", "HEAD"]);
        let (lock, kept) = keep_unbranched_head(&path, "ottam/quiet-fox-1a2b").expect("keep");
        assert_eq!(kept.as_deref(), Some("refs/ottam/removed/quiet-fox-1a2b"));
        assert_eq!(git(repo, &["rev-parse", "refs/ottam/removed/quiet-fox-1a2b"]), detached);

        // A straggler committing between the read and the removal fails, so
        // the ref still holds everything HEAD has.
        assert!(commit("straggler").is_err());
        assert_eq!(git(repo, &["rev-parse", "HEAD"]), detached);
        drop(lock);
        assert!(commit("after").is_ok());

        let head_lock = repo.join(".git").join("HEAD.lock");
        let busy = || keep_unbranched_head(&path, "ottam/quiet-fox-1a2b").err().map(|error| error.kind);

        // An overlapping removal in this process waits; its failure leaves
        // the first removal's lock in place.
        let (first, _) = keep_unbranched_head(&path, "ottam/quiet-fox-1a2b").expect("first");
        assert_eq!(busy(), Some("gitBusy"));
        assert!(head_lock.exists());
        assert!(commit("overlap").is_err());
        drop(first);
        assert!(!head_lock.exists());

        // A running git command's lock is not taken, nor a live removal's
        // in another process; a crashed removal's is.
        std::fs::write(&head_lock, "0123abcd\n").unwrap();
        assert_eq!(busy(), Some("gitBusy"));
        std::fs::write(&head_lock, head_lock_marker(1)).unwrap();
        assert_eq!(busy(), Some("gitBusy"));
        let mut exited = std::process::Command::new("true").spawn().expect("spawn");
        let dead = exited.id();
        exited.wait().expect("wait");
        std::fs::write(&head_lock, head_lock_marker(dead)).unwrap();
        let (lock, _) = keep_unbranched_head(&path, "ottam/quiet-fox-1a2b").expect("take over");
        assert_eq!(std::fs::read_to_string(&head_lock).unwrap(), head_lock_marker(std::process::id()));
        drop(lock);
        assert!(!head_lock.exists());
    }

    #[test]
    fn an_unchanged_branch_is_deleted_and_a_moved_or_checked_out_one_kept() {
        let (temp, base) = repo_with_commit();
        let repo = temp.path();
        let path = repo.to_string_lossy().into_owned();

        git(repo, &["branch", "ottam/unchanged", &base]);
        assert_eq!(delete_branch_if_unchanged(&path, "ottam/unchanged", &base).ok(), Some(false));
        assert_eq!(branch_commit(&path, "ottam/unchanged").ok(), Some(None));

        // A commit after the worktree was cut keeps the branch.
        git(repo, &["commit", "--quiet", "--allow-empty", "-m", "later"]);
        let later = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["branch", "ottam/moved", &later]);
        assert_eq!(delete_branch_if_unchanged(&path, "ottam/moved", &base).ok(), Some(true));

        // A checkout that reused the name keeps it, even at the base commit.
        git(repo, &["branch", "ottam/reused", &base]);
        let reuse = temp.path().join("reuse");
        git(repo, &["worktree", "add", "--quiet", &reuse.to_string_lossy(), "ottam/reused"]);
        assert_eq!(delete_branch_if_unchanged(&path, "ottam/reused", &base).ok(), Some(true));
        assert!(branch_commit(&path, "ottam/reused").ok().flatten().is_some());

        assert_eq!(delete_branch_if_unchanged(&path, "ottam/missing", &base).ok(), Some(false));
    }
}
