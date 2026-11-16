//! Locating the Node toolchain the ACP adapters run on.
//!
//! The app is launched by Launch Services, so it inherits launchd's PATH —
//! `/usr/bin:/bin:/usr/sbin:/sbin`. Every mainstream way of installing Node on
//! macOS (Homebrew, nvm, fnm, Volta) puts it somewhere else, so a bare
//! `Command::new("node")` fails on machines that do have Node installed, and
//! reports the same "node is required" as a machine that genuinely lacks it.
//!
//! Resolution therefore walks its own chain and hands absolute paths to both
//! the version probe and the adapter spawn. The same login-shell run also
//! captures the user's PATH, the base for adapters and the Node tooling core
//! runs for them (see `docs/architecture/backend/acp.md`, Runtime Resolution).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

/// How long a failed resolution is reused before searching again.
///
/// A negative answer has to expire: the user installing Node is exactly the
/// event we want to notice, and the "Check again" affordance in the engine
/// picker is only honest if a retry re-runs the search. It cannot be zero
/// either — the login-shell probe costs up to `LOGIN_SHELL_PROBE_TIMEOUT`, and
/// `engine.list` polls.
const UNRESOLVED_CACHE_TTL: Duration = Duration::from_secs(30);

/// A login shell sources the user's full rc chain (nvm, fnm, Volta shims are
/// all installed that way), which is slower than the 2s the version probes get.
const LOGIN_SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

const DEFAULT_SHELL: &str = "/bin/zsh";

/// Where Node lands when it is not on the launchd PATH, in preference order.
/// Homebrew first: it is both the most common and the least surprising.
const WELL_KNOWN_NODE_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Version-manager roots, relative to `$HOME`. Each holds one directory per
/// installed Node version; the highest version wins.
const VERSIONED_NODE_ROOTS: &[(&str, &str)] = &[
    (".nvm/versions/node", "bin"),
    (
        "Library/Application Support/fnm/node-versions",
        "installation/bin",
    ),
];

/// How long `node --version` gets during resolution. Same budget the runtime
/// probe gives it — a local binary answers in milliseconds or is broken.
const NODE_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeSource {
    EnvOverride,
    LoginShell,
    WellKnown,
    /// The runtime Ottam downloaded and owns.
    Managed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedNodeRuntime {
    pub node: PathBuf,
    pub npx: PathBuf,
    /// The package manager beside `node`, for adapter installs. Derived the
    /// same way as `npx`; only the install path runs it.
    pub npm: PathBuf,
    pub source: NodeSource,
}

impl ResolvedNodeRuntime {
    /// The directory that goes on a child's PATH, first or last depending on
    /// the child ([`path_with_toolchain_first`], [`path_with_toolchain_last`]).
    pub fn bin_dir(&self) -> Option<&Path> {
        self.node.parent()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeResolution {
    Resolved(ResolvedNodeRuntime),
    /// Nothing found. Callers fall back to the bare `node`/`npx` names, which
    /// is exactly the pre-resolver behaviour — never worse than the status quo.
    Unresolved,
}

impl NodeResolution {
    fn resolved(&self) -> Option<&ResolvedNodeRuntime> {
        match self {
            Self::Resolved(runtime) => Some(runtime),
            Self::Unresolved => None,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct NodeResolver {
    cache: StdMutex<Option<(Instant, NodeResolution)>>,
    /// Held across a search: each harness probes on its own thread, and a cold
    /// cache would otherwise run one login shell apiece.
    search: StdMutex<()>,
    /// Bumped under `cache` by `invalidate`, so a search that started before an
    /// explicit refresh cannot answer it.
    invalidations: AtomicU64,
    /// Absent in test registries and when the user has opted out, which is
    /// exactly the pre-managed-runtime behaviour.
    managed: Option<super::managed_node::ManagedNode>,
    /// The login shell's PATH from the last search that got one. A later
    /// probe that fails keeps it: a timeout is no reason to drop a good PATH.
    login_path: StdMutex<Option<OsString>>,
}

impl NodeResolver {
    pub fn with_managed(managed: Option<super::managed_node::ManagedNode>) -> Self {
        Self {
            managed,
            ..Self::default()
        }
    }

    /// The managed runtime this resolver would consider, for the install path.
    pub fn managed(&self) -> Option<&super::managed_node::ManagedNode> {
        self.managed.as_ref()
    }

    /// Resolve, consulting the cache. May block for up to
    /// `LOGIN_SHELL_PROBE_TIMEOUT` on a cold miss, so callers must already be
    /// on a blocking-capable thread.
    ///
    /// The search also captures the login shell's PATH. Folding it in here
    /// rather than capturing it separately is what keeps a spawn from racing
    /// it: every adapter start resolves first, so by the time anything reads
    /// `child_path_base`, the shell has answered or given up.
    pub fn resolve(&self) -> NodeResolution {
        self.resolve_single_flight(|| {
            let env = |name: &str| std::env::var(name).ok();
            let shell = user_shell(&env);
            let probe = probe_login_shell(&shell);
            self.record_login_path(&shell, &probe);
            let resolution = resolve_with(
                env,
                |_| probe.answer.node,
                is_executable_file,
                || {
                    self.managed
                        .as_ref()
                        .and_then(|managed| managed.usable_node())
                },
                system_node_major,
            );
            // A miss re-searches every `UNRESOLVED_CACHE_TTL` while polled,
            // so only an answer is worth an info line.
            match &resolution {
                NodeResolution::Resolved(runtime) => tracing::info!(
                    source = ?runtime.source,
                    node = %runtime.node.display(),
                    "resolved Node runtime"
                ),
                NodeResolution::Unresolved => tracing::debug!("no Node runtime found"),
            }
            resolution
        })
    }

    /// Keep the probe's PATH, logging at info only when it changes: misses
    /// re-run the probe every `UNRESOLVED_CACHE_TTL`, and an unchanged PATH
    /// each time is noise. A failed probe keeps the previous PATH.
    fn record_login_path(&self, shell: &str, probe: &LoginShellProbe) {
        let mut stored = self
            .login_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let elapsed_ms = probe.elapsed.as_millis() as u64;
        match &probe.answer.path {
            Some(path) if stored.as_ref() != Some(path) => {
                tracing::info!(
                    shell,
                    mode = probe.mode,
                    elapsed_ms,
                    entries = std::env::split_paths(path).count(),
                    "captured login-shell PATH"
                );
                *stored = Some(path.clone());
            }
            Some(_) => tracing::debug!(shell, elapsed_ms, "login-shell PATH unchanged"),
            None if stored.is_none() => tracing::warn!(
                shell,
                mode = probe.mode,
                elapsed_ms,
                "login shell did not report a PATH; children keep core's own"
            ),
            None => tracing::debug!(shell, elapsed_ms, "login shell did not report a PATH"),
        }
    }

    /// The PATH children start from: the login shell's when a search has
    /// captured it, else core's own (launchd's, for a Finder launch).
    pub fn child_path_base(&self) -> OsString {
        path_base(self.login_path())
    }

    /// The login shell's PATH, once a search has captured it.
    pub fn login_path(&self) -> Option<OsString> {
        self.login_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Concurrent callers share one search; whoever waited reads its answer
    /// from the cache. An invalidation during the search leaves the cache
    /// empty, so the next waiter searches again.
    fn resolve_single_flight(&self, search: impl FnOnce() -> NodeResolution) -> NodeResolution {
        if let Some(resolution) = self.cached() {
            return resolution;
        }
        let _search = self
            .search
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(resolution) = self.cached() {
            return resolution;
        }
        let invalidations = self.invalidations.load(Ordering::Acquire);
        let resolution = search();
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.invalidations.load(Ordering::Acquire) == invalidations {
            *cache = Some((Instant::now(), resolution.clone()));
        }
        resolution
    }

    /// The last resolution, without ever searching. The spawn path uses this:
    /// every adapter start goes through the runtime probe first, so the cache
    /// is warm by then, and a cold read falling back to bare names is correct
    /// rather than worth blocking for.
    pub fn cached(&self) -> Option<NodeResolution> {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.as_ref().and_then(|(observed_at, resolution)| {
            // Ordinary polling treats a found toolchain as stable for the
            // process lifetime. The explicit refresh path invalidates it when
            // the user has installed or switched runtimes; only misses expire
            // automatically.
            match resolution {
                NodeResolution::Resolved(_) => Some(resolution.clone()),
                NodeResolution::Unresolved if observed_at.elapsed() < UNRESOLVED_CACHE_TTL => {
                    Some(resolution.clone())
                }
                NodeResolution::Unresolved => None,
            }
        })
    }

    /// The resolved runtime, if one has already been found.
    pub fn cached_runtime(&self) -> Option<ResolvedNodeRuntime> {
        self.cached()
            .as_ref()
            .and_then(NodeResolution::resolved)
            .cloned()
    }

    /// Forget both positive and negative answers so an explicit user refresh
    /// re-runs the full discovery chain. Positive answers normally live for the
    /// process lifetime, but version managers can switch the selected Node while
    /// Ottam is open.
    pub fn invalidate(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = None;
        self.invalidations.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(super) fn store_for_test(&self, resolution: NodeResolution) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = Some((Instant::now(), resolution));
    }

    #[cfg(test)]
    pub(super) fn store_login_path_for_test(&self, path: &str) {
        *self
            .login_path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path.into());
    }
}

/// `login_path` when captured, else core's own PATH.
pub(crate) fn path_base(login_path: Option<OsString>) -> OsString {
    login_path
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

/// `base` with `bin_dir` first, for processes that run Node tooling Ottam
/// chose: the version probes, `npm install`, and local MCP servers. npm and
/// npx are `#!/usr/bin/env node` scripts, and install hooks spawn `node`, so
/// all of them re-resolve `node` through PATH and must find the resolved one.
pub(crate) fn path_with_toolchain_first(base: &OsStr, bin_dir: Option<&Path>) -> OsString {
    let Some(bin_dir) = bin_dir else {
        return base.to_owned();
    };
    let mut path = bin_dir.as_os_str().to_owned();
    if !base.is_empty() {
        path.push(":");
        path.push(base);
    }
    path
}

/// `base` with `bin_dir` last, unless it is already on it, for adapters. An
/// adapter's environment reaches the agent's shell, where the user's own
/// `node` should win; the adapter itself runs by absolute path and needs no
/// PATH lookup.
pub(crate) fn path_with_toolchain_last(base: &OsStr, bin_dir: Option<&Path>) -> OsString {
    let Some(bin_dir) = bin_dir else {
        return base.to_owned();
    };
    if base.is_empty() {
        return bin_dir.as_os_str().to_owned();
    }
    if std::env::split_paths(base).any(|entry| entry == bin_dir) {
        return base.to_owned();
    }
    let mut path = base.to_owned();
    path.push(":");
    path.push(bin_dir);
    path
}

fn user_shell(env: &impl Fn(&str) -> Option<String>) -> String {
    env("SHELL")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_SHELL.to_string())
}

/// The resolution chain, with every effect injected so the ordering is testable
/// without touching the machine's real toolchain.
fn resolve_with(
    env: impl Fn(&str) -> Option<String>,
    login_shell_node: impl FnOnce(&str) -> Option<PathBuf>,
    is_executable_file: impl Fn(&Path) -> bool,
    managed_node: impl Fn() -> Option<PathBuf>,
    node_major: impl Fn(&Path) -> Option<u64>,
) -> NodeResolution {
    // 1. An explicit override is authoritative, including when it points at
    //    nothing: silently searching past a path the user set would turn a typo
    //    into a confusing "node is required" instead of an honest failure.
    if let Some(node) = env("OTTAM_NODE")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return resolution_from_node(
            PathBuf::from(node),
            NodeSource::EnvOverride,
            &is_executable_file,
        );
    }

    let system = system_node(&env, login_shell_node, &is_executable_file);

    // 2. The managed runtime is only a candidate once it is actually on disk.
    //    Checking that first is what keeps the version query below off the path
    //    of every machine that has never installed one.
    let Some(managed) = managed_node() else {
        return match system {
            Some((node, source)) => resolution_from_node(node, source, &is_executable_file),
            None => NodeResolution::Unresolved,
        };
    };

    // 3. Prefer the user's own Node when it can satisfy every engine. Below
    //    that line the managed runtime is strictly better: it clears every
    //    floor, where a system Node under `PREFERRED_NODE_MAJOR` leaves at
    //    least one engine reporting "Node too old". A system Node that will not
    //    report a version at all is broken, and loses for the same reason.
    //
    //    "Satisfies every engine" means the whole toolchain, not just node:
    //    npx runs the adapter and npm installs it, so a system Node missing
    //    either one cannot start an engine no matter how new it is. Preferring
    //    it anyway would make the managed runtime unable to repair the problem
    //    it was installed for — the user installs Node, and nothing changes.
    //    Checked first because it is a stat, where the version query below is a
    //    subprocess.
    let system_is_sufficient = system.as_ref().is_some_and(|(node, _)| {
        has_complete_toolchain(node, &is_executable_file)
            && node_major(node).is_some_and(|major| major >= super::registry::PREFERRED_NODE_MAJOR)
    });
    match system {
        Some((node, source)) if system_is_sufficient => {
            resolution_from_node(node, source, &is_executable_file)
        }
        _ => resolution_from_node(managed, NodeSource::Managed, &is_executable_file),
    }
}

/// Whether npx and npm sit beside this `node`, which is the layout every
/// source we search produces (see [`resolution_from_node`]). Absence means the
/// installation is incomplete rather than merely old.
///
/// Only consulted when a managed runtime exists to fall back to: with no
/// managed Node on disk, an incomplete system toolchain is still the only
/// answer there is, and reporting `node` honestly beats reporting nothing.
fn has_complete_toolchain(node: &Path, is_executable_file: &impl Fn(&Path) -> bool) -> bool {
    node.parent().is_some_and(|dir| {
        is_executable_file(&dir.join("npx")) && is_executable_file(&dir.join("npm"))
    })
}

/// The best Node the machine already has, ignoring anything Ottam installed.
fn system_node(
    env: &impl Fn(&str) -> Option<String>,
    login_shell_node: impl FnOnce(&str) -> Option<PathBuf>,
    is_executable_file: &impl Fn(&Path) -> bool,
) -> Option<(PathBuf, NodeSource)> {
    // The user's login shell knows about their version manager; we do not.
    let shell = user_shell(env);
    if let Some(node) = login_shell_node(&shell).filter(|path| is_executable_file(path)) {
        return Some((node, NodeSource::LoginShell));
    }

    // Fixed locations, for when the login shell is exotic or the probe timed
    // out. Version-manager roots are best-effort: the highest installed version
    // wins, which ignores any per-project `.nvmrc`.
    for dir in WELL_KNOWN_NODE_DIRS {
        let candidate = Path::new(dir).join("node");
        if is_executable_file(&candidate) {
            return Some((candidate, NodeSource::WellKnown));
        }
    }
    let home = env("HOME").filter(|value| !value.is_empty())?;
    for (root, bin_suffix) in VERSIONED_NODE_ROOTS {
        let candidate = highest_versioned_node(&Path::new(&home).join(root), bin_suffix)
            .filter(|path| is_executable_file(path));
        if let Some(node) = candidate {
            return Some((node, NodeSource::WellKnown));
        }
    }
    None
}

/// Ask a Node binary for its major version.
///
/// The runtime probe already does this per engine, but it runs *after*
/// resolution has picked a toolchain — too late to influence the choice. This
/// is only ever called when a managed runtime is installed and the answer could
/// change which one wins.
fn system_node_major(node: &Path) -> Option<u64> {
    let output = super::registry::command_output_args_with_timeout(
        &node.to_string_lossy(),
        &["--version"],
        NODE_VERSION_PROBE_TIMEOUT,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    super::registry::parse_node_major(String::from_utf8_lossy(&output.stdout).trim())
}

fn resolution_from_node(
    node: PathBuf,
    source: NodeSource,
    is_executable_file: &impl Fn(&Path) -> bool,
) -> NodeResolution {
    // npx and npm ship beside node in every layout we search. If one somehow
    // does not, keep the resolved node and let that tool fall back to its bare
    // name rather than discarding a good answer.
    let sibling = |name: &str| {
        node.parent()
            .map(|dir| dir.join(name))
            .filter(|path| is_executable_file(path))
            .unwrap_or_else(|| PathBuf::from(name))
    };
    let npx = sibling("npx");
    let npm = sibling("npm");
    NodeResolution::Resolved(ResolvedNodeRuntime {
        node,
        npx,
        npm,
        source,
    })
}

/// The newest `<root>/<version>/<bin_suffix>/node`, comparing versions
/// numerically so `v20` beats `v9`.
fn highest_versioned_node(root: &Path, bin_suffix: &str) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for entry in std::fs::read_dir(root).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(version) = parse_version_components(&name.to_string_lossy()) else {
            continue;
        };
        let candidate = entry.path().join(bin_suffix).join("node");
        if best
            .as_ref()
            .is_none_or(|(best_version, _)| version > *best_version)
        {
            best = Some((version, candidate));
        }
    }
    best.map(|(_, path)| path)
}

fn parse_version_components(name: &str) -> Option<Vec<u64>> {
    let trimmed = name.strip_prefix('v').unwrap_or(name);
    let components: Option<Vec<u64>> = trimmed.split('.').map(|part| part.parse().ok()).collect();
    components.filter(|components| !components.is_empty())
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Marks our answers inside the shell's output so rc-file banners, direnv
/// notices, and version-manager chatter cannot be mistaken for a path.
const ANSWER_MARKER: &str = "__ottam_node__:";
const PATH_MARKER: &str = "__ottam_path__:";

/// Run by the shell's Node: the first `node` on PATH when it is that Node
/// itself (Homebrew's stable symlink, an nvm install), else its own real path.
/// It sits inside single quotes, so no `'`, `$`, or newline.
const NODE_PATH_SCRIPT: &str = r#"(() => { const fs = require("fs"); const own = process.execPath; for (const dir of (process.env.PATH || "").split(":")) { const entry = dir + "/node"; try { fs.accessSync(entry, fs.constants.X_OK); } catch { continue; } try { return fs.realpathSync(entry) === own ? entry : own; } catch { return own; } } return own; })()"#;

/// What one run of the user's shell reported.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LoginShellAnswer {
    /// PATH as the user's terminal has it. Only PATH: the rest of a shell's
    /// environment is session state (`PWD`, `SHLVL`, direnv) or settings
    /// that would silently change an agent, such as an exported API key
    /// switching it off subscription sign-in.
    path: Option<OsString>,
    node: Option<PathBuf>,
}

impl LoginShellAnswer {
    /// Either line counts: a shell that reports Node but no PATH still
    /// answered the part resolution needs, so the fallback mode is not tried.
    fn answered(&self) -> bool {
        self.path.is_some() || self.node.is_some()
    }
}

/// One probe: the answer, which mode gave it, and how long it took.
#[derive(Debug, Default)]
struct LoginShellProbe {
    answer: LoginShellAnswer,
    mode: Option<&'static str>,
    elapsed: Duration,
}

/// Ask the user's shell for its PATH and where Node lives.
///
/// The Node answer is the PATH entry unless that entry is a shim. asdf and mise put
/// a shim first on PATH that re-enters the version manager by bare name, and
/// that name is not on the launchd PATH the app spawns with, so a shim answers
/// with Node's own `process.execPath`: the real binary runs anywhere and has
/// npx and npm beside it. A plain entry stays as is, because Homebrew's
/// `/opt/homebrew/bin/node` survives `brew upgrade` and the versioned Cellar
/// path it points at does not. `command -v` remains the fallback so a Node that
/// will not start is reported invalid, not missing.
///
/// An interactive login shell (`-i -l`) goes first because it is what the
/// user's terminal runs: `.zprofile` for Homebrew, then `.zshrc`, where asdf,
/// nvm, fnm, and most PATH edits live. A plain login shell answering first
/// would hand back a PATH, and a Node, the terminal never sees. The plain
/// attempt remains the fallback for an interactive startup that hangs or
/// fails. Output is delimited rather than trusted, stdin is closed so nothing
/// can block on a read, and the timeout kills the whole process group.
fn probe_login_shell(shell: &str) -> LoginShellProbe {
    let script = format!(
        "printf '%s%s\\n' '{PATH_MARKER}' \"$PATH\"; printf '%s%s\\n' '{ANSWER_MARKER}' \"$(node -p '{NODE_PATH_SCRIPT}' 2>/dev/null || command -v node)\""
    );
    let started = Instant::now();
    for (mode, flags) in [
        ("interactive login", &["-i", "-l", "-c"][..]),
        ("login", &["-l", "-c"][..]),
    ] {
        let mut arguments = flags.to_vec();
        arguments.push(script.as_str());
        let Ok(output) = super::registry::command_output_args_with_timeout(
            shell,
            &arguments,
            LOGIN_SHELL_PROBE_TIMEOUT,
        ) else {
            continue;
        };
        let answer = parse_login_shell_output(&String::from_utf8_lossy(&output.stdout));
        if answer.answered() {
            return LoginShellProbe {
                answer,
                mode: Some(mode),
                elapsed: started.elapsed(),
            };
        }
    }
    LoginShellProbe {
        elapsed: started.elapsed(),
        ..LoginShellProbe::default()
    }
}

fn parse_login_shell_output(stdout: &str) -> LoginShellAnswer {
    let path = stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PATH_MARKER))
        .map(str::trim)
        .find(|path| path.split(':').any(|entry| entry.starts_with('/')))
        .map(OsString::from);
    LoginShellAnswer {
        path,
        node: parse_which_output(stdout),
    }
}

/// Pull the marked answer out of whatever else the shell printed. Only an
/// absolute path counts — a shell builtin or alias is not something we can
/// spawn.
fn parse_which_output(stdout: &str) -> Option<PathBuf> {
    let path = stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix(ANSWER_MARKER))
        .map(str::trim)
        .find(|answer| answer.starts_with('/'))?;
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::registry::PREFERRED_NODE_MAJOR;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{mpsc, Arc, Barrier};

    #[test]
    fn concurrent_cold_resolves_share_one_search() {
        let resolver = Arc::new(NodeResolver::default());
        let searches = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(8));
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let (resolver, searches, start) = (
                    Arc::clone(&resolver),
                    Arc::clone(&searches),
                    Arc::clone(&start),
                );
                std::thread::spawn(move || {
                    start.wait();
                    resolver.resolve_single_flight(|| {
                        searches.fetch_add(1, Ordering::SeqCst);
                        NodeResolution::Unresolved
                    })
                })
            })
            .collect();
        for caller in callers {
            assert_eq!(caller.join().unwrap(), NodeResolution::Unresolved);
        }
        assert_eq!(searches.load(Ordering::SeqCst), 1);
    }

    /// "Check Again" while an older search runs must not be answered by it.
    #[test]
    fn a_search_invalidated_midway_does_not_fill_the_cache() {
        let resolver = Arc::new(NodeResolver::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let searching = {
            let resolver = Arc::clone(&resolver);
            std::thread::spawn(move || {
                resolver.resolve_single_flight(|| {
                    started_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    NodeResolution::Unresolved
                })
            })
        };
        started_rx.recv().unwrap();
        resolver.invalidate();
        finish_tx.send(()).unwrap();
        searching.join().unwrap();
        assert_eq!(resolver.cached(), None);
    }

    fn never_shell(_: &str) -> Option<PathBuf> {
        None
    }

    /// Resolution on a machine that has never installed a managed runtime —
    /// the shape every one of these cases had before it existed. `node_major`
    /// panics on purpose: with nothing managed to compare against, asking a
    /// binary for its version is pure cost and must never happen.
    fn resolve_system_only(
        env: impl Fn(&str) -> Option<String>,
        login_shell_node: impl FnOnce(&str) -> Option<PathBuf>,
        is_executable_file: impl Fn(&Path) -> bool,
    ) -> NodeResolution {
        resolve_with(
            env,
            login_shell_node,
            is_executable_file,
            || None,
            |path| panic!("resolution asked {} for its version", path.display()),
        )
    }

    fn env_from(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs = pairs.to_vec();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn env_override_wins_over_every_other_source() {
        let resolution = resolve_system_only(
            env_from(&[("OTTAM_NODE", "/custom/bin/node")]),
            |_| Some(PathBuf::from("/opt/homebrew/bin/node")),
            |_| true,
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/custom/bin/node"));
        assert_eq!(runtime.npx, PathBuf::from("/custom/bin/npx"));
        assert_eq!(runtime.npm, PathBuf::from("/custom/bin/npm"));
        assert_eq!(runtime.source, NodeSource::EnvOverride);
    }

    #[test]
    fn env_override_is_reported_even_when_it_does_not_exist() {
        // The probe then fails honestly against the path the user chose, rather
        // than falling through and blaming a missing system Node.
        let resolution = resolve_system_only(
            env_from(&[("OTTAM_NODE", "/nonexistent/node")]),
            |_| Some(PathBuf::from("/opt/homebrew/bin/node")),
            |_| false,
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/nonexistent/node"));
        assert_eq!(runtime.npx, PathBuf::from("npx"));
    }

    #[test]
    fn blank_env_override_falls_through() {
        let resolution = resolve_system_only(
            env_from(&[("OTTAM_NODE", "   ")]),
            |_| Some(PathBuf::from("/shell/bin/node")),
            |_| true,
        );
        assert_eq!(
            resolution.resolved().expect("resolved").source,
            NodeSource::LoginShell
        );
    }

    #[test]
    fn login_shell_result_wins_over_well_known_dirs() {
        let resolution = resolve_system_only(
            env_from(&[("SHELL", "/bin/zsh")]),
            |shell| {
                assert_eq!(shell, "/bin/zsh");
                Some(PathBuf::from(
                    "/Users/test/.nvm/versions/node/v22.1.0/bin/node",
                ))
            },
            |_| true,
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(
            runtime.node,
            PathBuf::from("/Users/test/.nvm/versions/node/v22.1.0/bin/node")
        );
        assert_eq!(runtime.source, NodeSource::LoginShell);
    }

    #[test]
    fn login_shell_answer_is_ignored_when_not_executable() {
        let resolution = resolve_system_only(
            env_from(&[]),
            |_| Some(PathBuf::from("/stale/node")),
            |path| path.starts_with("/opt/homebrew"),
        );
        assert_eq!(
            resolution.resolved().expect("resolved").node,
            PathBuf::from("/opt/homebrew/bin/node")
        );
    }

    #[test]
    fn well_known_dirs_are_searched_in_order() {
        let resolution = resolve_system_only(env_from(&[]), never_shell, |path| {
            path.starts_with("/usr/local/bin")
        });
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/usr/local/bin/node"));
        assert_eq!(runtime.npx, PathBuf::from("/usr/local/bin/npx"));
        assert_eq!(runtime.source, NodeSource::WellKnown);
    }

    #[test]
    fn nothing_found_is_unresolved() {
        let resolution = resolve_system_only(env_from(&[]), never_shell, |_| false);
        assert_eq!(resolution, NodeResolution::Unresolved);
    }

    #[test]
    fn npx_falls_back_to_bare_name_when_missing_beside_node() {
        let resolution = resolve_system_only(env_from(&[]), never_shell, |path| {
            path == Path::new("/opt/homebrew/bin/node")
        });
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/opt/homebrew/bin/node"));
        assert_eq!(runtime.npx, PathBuf::from("npx"));
        assert_eq!(runtime.npm, PathBuf::from("npm"));
    }

    const MANAGED_NODE: &str = "/home/.ottam/runtime/node/v24.22.1/bin/node";

    /// A machine with a managed runtime on disk and a system Node reporting
    /// `system_major`.
    fn resolve_with_managed(
        system_node: Option<&str>,
        system_major: Option<u64>,
    ) -> NodeResolution {
        let system_node = system_node.map(PathBuf::from);
        resolve_with(
            env_from(&[]),
            {
                let system_node = system_node.clone();
                move |_| system_node
            },
            |_| true,
            || Some(PathBuf::from(MANAGED_NODE)),
            move |_| system_major,
        )
    }

    #[test]
    fn a_system_node_that_satisfies_every_engine_beats_the_managed_one() {
        // The managed runtime is a fallback, not a takeover: a user who already
        // has a good toolchain keeps using it, and the download stays unused.
        let resolution =
            resolve_with_managed(Some("/opt/homebrew/bin/node"), Some(PREFERRED_NODE_MAJOR));
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/opt/homebrew/bin/node"));
        assert_eq!(runtime.source, NodeSource::LoginShell);
    }

    #[test]
    fn a_system_node_below_the_preference_floor_loses_to_the_managed_one() {
        // Node 20 runs Codex but not Claude. The managed runtime clears both,
        // so it is strictly better than leaving one engine broken.
        let resolution = resolve_with_managed(
            Some("/opt/homebrew/bin/node"),
            Some(PREFERRED_NODE_MAJOR - 2),
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from(MANAGED_NODE));
        assert_eq!(runtime.source, NodeSource::Managed);
    }

    /// A system Node new enough to pass the floor but with no npx/npm beside
    /// it cannot run or install an adapter. If it kept winning, installing the
    /// managed runtime — the button offered to fix exactly this — would change
    /// nothing, and the engine would stay broken with the install reported
    /// complete.
    #[test]
    fn a_system_node_missing_npx_loses_to_the_managed_one() {
        let resolution = resolve_with(
            env_from(&[]),
            |_| Some(PathBuf::from("/opt/homebrew/bin/node")),
            // Everything in the managed toolchain exists; the system directory
            // holds only `node`.
            |path| path != Path::new("/opt/homebrew/bin/npx"),
            || Some(PathBuf::from(MANAGED_NODE)),
            |_| Some(PREFERRED_NODE_MAJOR),
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from(MANAGED_NODE));
        assert_eq!(runtime.source, NodeSource::Managed);
    }

    /// With nothing to fall back to, an incomplete system toolchain is still
    /// the honest answer — the resolver reports it rather than `Unresolved`.
    #[test]
    fn an_incomplete_system_node_still_answers_when_no_managed_runtime_exists() {
        let resolution = resolve_with(
            env_from(&[]),
            |_| Some(PathBuf::from("/opt/homebrew/bin/node")),
            |path| path != Path::new("/opt/homebrew/bin/npx"),
            || None,
            |_| Some(PREFERRED_NODE_MAJOR),
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/opt/homebrew/bin/node"));
        // The bare-name fallback stands in, and the runtime probe is what
        // reports npx missing.
        assert_eq!(runtime.npx, PathBuf::from("npx"));
    }

    #[test]
    fn a_system_node_that_will_not_report_a_version_loses_to_the_managed_one() {
        let resolution = resolve_with_managed(Some("/opt/homebrew/bin/node"), None);
        assert_eq!(
            resolution.resolved().expect("resolved").source,
            NodeSource::Managed
        );
    }

    #[test]
    fn the_managed_runtime_answers_when_the_machine_has_no_node_at_all() {
        let resolution = resolve_with_managed(None, None);
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from(MANAGED_NODE));
        assert_eq!(
            runtime.npx,
            PathBuf::from("/home/.ottam/runtime/node/v24.22.1/bin/npx"),
            "npx must come from the same toolchain as node"
        );
    }

    #[test]
    fn an_env_override_still_wins_over_an_installed_managed_runtime() {
        let resolution = resolve_with(
            env_from(&[("OTTAM_NODE", "/custom/bin/node")]),
            |_| None,
            |_| true,
            || Some(PathBuf::from(MANAGED_NODE)),
            |path| {
                panic!(
                    "the override should short-circuit, not probe {}",
                    path.display()
                )
            },
        );
        let runtime = resolution.resolved().expect("resolved");
        assert_eq!(runtime.node, PathBuf::from("/custom/bin/node"));
        assert_eq!(runtime.source, NodeSource::EnvOverride);
    }

    #[test]
    fn the_node_path_script_survives_single_quotes_in_any_shell() {
        for forbidden in ['\'', '$', '\n'] {
            assert!(!NODE_PATH_SCRIPT.contains(forbidden), "{forbidden:?}");
        }
    }

    #[test]
    fn one_shell_run_answers_both_path_and_node() {
        let output = format!(
            "Last login: Mon\ndirenv: loading\n{PATH_MARKER}/Users/test/.local/state/fnm_multishells/1_2/bin:/opt/homebrew/bin:/usr/bin\n{ANSWER_MARKER}/Users/test/.local/state/fnm_multishells/1_2/bin/node\n"
        );
        assert_eq!(
            parse_login_shell_output(&output),
            LoginShellAnswer {
                path: Some(
                    "/Users/test/.local/state/fnm_multishells/1_2/bin:/opt/homebrew/bin:/usr/bin"
                        .into()
                ),
                node: Some(PathBuf::from(
                    "/Users/test/.local/state/fnm_multishells/1_2/bin/node"
                )),
            }
        );
        // No Node is still a PATH worth having.
        assert_eq!(
            parse_login_shell_output(&format!("{PATH_MARKER}/usr/bin:/bin\n{ANSWER_MARKER}\n")),
            LoginShellAnswer {
                path: Some("/usr/bin:/bin".into()),
                node: None,
            }
        );
        // The script never ran, or printed nothing usable.
        assert_eq!(
            parse_login_shell_output("zsh: bad option\n"),
            LoginShellAnswer::default()
        );
        assert_eq!(
            parse_login_shell_output(&format!("{PATH_MARKER}\n")).path,
            None
        );
        // An unmarked PATH-looking line is somebody else's output.
        assert_eq!(
            parse_login_shell_output("/opt/homebrew/bin:/usr/bin\n").path,
            None
        );
    }

    /// A shell that prints only the Node line still answered: resolution must
    /// get that Node rather than fall through to the next mode or a
    /// well-known directory.
    #[test]
    fn a_node_only_answer_counts() {
        let answer = parse_login_shell_output(&format!("{ANSWER_MARKER}/fake/bin/node\n"));
        assert_eq!(answer.path, None);
        assert!(answer.answered());
        assert!(!parse_login_shell_output("zsh: bad option\n").answered());
    }

    #[test]
    fn a_failed_probe_keeps_the_previous_login_path() {
        let resolver = NodeResolver::default();
        let probe = |path: Option<&str>| LoginShellProbe {
            answer: LoginShellAnswer {
                path: path.map(OsString::from),
                node: None,
            },
            ..LoginShellProbe::default()
        };
        resolver.record_login_path("/bin/zsh", &probe(Some("/opt/homebrew/bin:/usr/bin")));
        resolver.record_login_path("/bin/zsh", &probe(None));
        assert_eq!(
            resolver.login_path(),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin"))
        );
        resolver.record_login_path("/bin/zsh", &probe(Some("/usr/bin")));
        assert_eq!(resolver.login_path(), Some(OsString::from("/usr/bin")));
    }

    #[test]
    fn tooling_puts_the_resolved_node_first() {
        let base = OsStr::new("/Users/test/.fnm/bin:/usr/bin");
        assert_eq!(
            path_with_toolchain_first(base, Some(Path::new("/managed/bin"))),
            OsString::from("/managed/bin:/Users/test/.fnm/bin:/usr/bin")
        );
        assert_eq!(path_with_toolchain_first(base, None), OsString::from(base));
        assert_eq!(
            path_with_toolchain_first(OsStr::new(""), Some(Path::new("/managed/bin"))),
            OsString::from("/managed/bin")
        );
    }

    /// Adapters: the user's own Node keeps winning in the agent's shell, and
    /// a Node already on the login PATH is not listed twice.
    #[test]
    fn adapters_put_the_resolved_node_last_once() {
        let base = OsStr::new("/Users/test/.fnm/bin:/opt/homebrew/bin:/usr/bin");
        assert_eq!(
            path_with_toolchain_last(base, Some(Path::new("/managed/bin"))),
            OsString::from("/Users/test/.fnm/bin:/opt/homebrew/bin:/usr/bin:/managed/bin")
        );
        assert_eq!(
            path_with_toolchain_last(base, Some(Path::new("/opt/homebrew/bin"))),
            OsString::from(base)
        );
        assert_eq!(path_with_toolchain_last(base, None), OsString::from(base));
        assert_eq!(
            path_with_toolchain_last(OsStr::new(""), Some(Path::new("/managed/bin"))),
            OsString::from("/managed/bin")
        );
    }

    #[test]
    fn child_path_base_prefers_the_captured_login_path() {
        let resolver = NodeResolver::default();
        assert_eq!(
            resolver.child_path_base(),
            std::env::var_os("PATH").unwrap_or_default()
        );
        resolver.store_login_path_for_test("/opt/homebrew/bin:/usr/bin");
        assert_eq!(
            resolver.child_path_base(),
            OsString::from("/opt/homebrew/bin:/usr/bin")
        );
    }

    #[test]
    fn which_output_is_read_from_the_marked_line_only() {
        let marked = |answer: &str| format!("{ANSWER_MARKER}{answer}\n");
        assert_eq!(
            parse_which_output(&marked("/opt/homebrew/bin/node")),
            Some(PathBuf::from("/opt/homebrew/bin/node"))
        );
        // The reason the marker exists: an interactive shell prints its own
        // startup chatter, and none of it is an answer.
        assert_eq!(
            parse_which_output(&format!(
                "Welcome to your shell!\ndirenv: loading\n{}",
                marked("/Users/test/.asdf/installs/nodejs/25.9.0/bin/node")
            )),
            Some(PathBuf::from(
                "/Users/test/.asdf/installs/nodejs/25.9.0/bin/node"
            ))
        );
        // An unmarked path is somebody else's output, not ours.
        assert_eq!(parse_which_output("/opt/homebrew/bin/node\n"), None);
        // Neither Node nor `command -v` answered, so the marker is there but empty.
        assert_eq!(parse_which_output(&marked("")), None);
        // A shell builtin or alias is not a path we can spawn.
        assert_eq!(parse_which_output(&marked("node: aliased to nodejs")), None);
        assert_eq!(parse_which_output("\n"), None);
    }

    #[test]
    fn versioned_roots_pick_the_highest_version_numerically() {
        let root =
            std::env::temp_dir().join(format!("ottam-node-resolver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for version in ["v9.11.2", "v20.3.1", "v18.0.0"] {
            std::fs::create_dir_all(root.join(version).join("bin")).expect("create version dir");
        }
        let highest = highest_versioned_node(&root, "bin").expect("highest");
        assert_eq!(highest, root.join("v20.3.1").join("bin").join("node"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolver_caches_a_resolved_runtime() {
        let resolver = NodeResolver::default();
        assert!(resolver.cached().is_none());
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/opt/homebrew/bin/node"),
            npx: PathBuf::from("/opt/homebrew/bin/npx"),
            npm: PathBuf::from("/opt/homebrew/bin/npm"),
            source: NodeSource::WellKnown,
        }));
        assert_eq!(
            resolver.cached_runtime().expect("cached").node,
            PathBuf::from("/opt/homebrew/bin/node")
        );
    }

    #[test]
    fn explicit_invalidation_discards_a_resolved_runtime() {
        let resolver = NodeResolver::default();
        resolver.store_for_test(NodeResolution::Resolved(ResolvedNodeRuntime {
            node: PathBuf::from("/old/bin/node"),
            npx: PathBuf::from("/old/bin/npx"),
            npm: PathBuf::from("/old/bin/npm"),
            source: NodeSource::LoginShell,
        }));

        resolver.invalidate();

        assert!(resolver.cached().is_none());
        assert!(resolver.cached_runtime().is_none());
    }

    #[test]
    fn unresolved_cache_expires_so_a_new_install_is_noticed() {
        let resolver = NodeResolver::default();
        {
            let mut cache = resolver.cache.lock().expect("lock");
            let stale = Instant::now() - (UNRESOLVED_CACHE_TTL + Duration::from_secs(1));
            *cache = Some((stale, NodeResolution::Unresolved));
        }
        assert!(resolver.cached().is_none());
    }
}
