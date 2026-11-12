//! Claude Code's OAuth refresh lock, as the CLI bundled at the adapter pin
//! holds it. With `<home>` = `CLAUDE_CONFIG_DIR`, a refresh takes two
//! proper-lockfile directories and then records its owner:
//!
//! - `<home>/.oauth_refresh.lock`, the current lock;
//! - `<realpath(home)>.lock`, the legacy lock older CLIs take, held alongside;
//! - `<home>/.oauth_refresh.lock.owner`, JSON naming the holder's pid, start
//!   time, host, and both locks' birth times.
//!
//! A lock may be removed only when Core can account for its owner: the owner
//! record proves the holder is gone from this host, or Core can account for
//! every process that has used its isolated home. Age is not ownership.
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const CLAUDE_REFRESH_LOCK: &str = ".oauth_refresh.lock";
const OWNER_RECORD: &str = ".oauth_refresh.lock.owner";
/// The CLI reads at most this much of the record.
const OWNER_RECORD_LIMIT: u64 = 4096;

/// How long a managed home's own teardown waits for a refresh before it stops
/// the adapter anyway. Shared homes have no such bound.
pub(crate) const MANAGED_REFRESH_WAIT: Duration = Duration::from_secs(30);

/// Whether a refresh may be in progress. `path` is a home's current lock; the
/// home's legacy lock counts too. A lock whose recorded owner is proven gone
/// is not a refresh.
pub(crate) fn is_refreshing(path: Option<&Path>) -> bool {
    path.is_some_and(|path| {
        let locks = Locks::of(path);
        locks.held() && abandoned(&locks, Host::current().as_ref()).is_none()
    })
}

/// No timer is allowed to destroy a transport while its shared home is refreshing.
pub(crate) async fn wait_for_release(path: Option<&Path>) {
    while is_refreshing(path) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// [`wait_for_release`] for a home Core owns, which stops waiting after
/// `bound`. False when the lock was still held.
pub(crate) async fn wait_for_release_within(path: &Path, bound: Duration) -> bool {
    tokio::time::timeout(bound, wait_for_release(Some(path)))
        .await
        .is_ok()
}

/// Removes the home's locks and owner record when the record proves its
/// holder is gone. True when a lock was removed.
pub(crate) fn clear_if_owner_gone(home: &Path) -> bool {
    clear_abandoned(&Locks::of(&home.join(CLAUDE_REFRESH_LOCK)), Host::current().as_ref())
}

struct Locks {
    current: PathBuf,
    /// None when the home has no real path, so nothing can be locked there.
    legacy: Option<PathBuf>,
    owner: PathBuf,
}

impl Locks {
    fn of(current: &Path) -> Self {
        let home = current.parent().unwrap_or(Path::new("/"));
        // `<home>.lock` names a sibling: append, since `with_extension` would
        // replace a dotted directory name's suffix.
        let legacy = std::fs::canonicalize(home).ok().and_then(|real| {
            let mut name = real.file_name()?.to_os_string();
            name.push(".lock");
            Some(real.with_file_name(name))
        });
        Self {
            current: current.to_path_buf(),
            legacy,
            owner: home.join(OWNER_RECORD),
        }
    }

    fn held(&self) -> bool {
        let exists = |path: &Path| path.try_exists().unwrap_or(true);
        exists(&self.current) || self.legacy.as_deref().is_some_and(exists)
    }
}

/// The CLI's record schema; unknown fields are ignored, as its parser does.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerRecord {
    pid: i64,
    proc_start: Option<String>,
    pid_domain: String,
    pid_space: String,
    lock_birthtime_ms: f64,
    legacy_lock_birthtime_ms: Option<f64>,
}

fn read_owner(path: &Path) -> Option<OwnerRecord> {
    // Never follow a symlink, or read more than the CLI would write.
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > OWNER_RECORD_LIMIT {
        return None;
    }
    let record: OwnerRecord = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let positive = |time: f64| time.is_finite() && time > 0.0;
    (record.proc_start.as_ref().is_none_or(|start| start.len() <= 64)
        && record.pid_domain.len() <= 512
        && record.pid_space.len() <= 512
        && positive(record.lock_birthtime_ms)
        && record.legacy_lock_birthtime_ms.is_none_or(positive))
    .then_some(record)
}

/// The identity the CLI records for its process namespace.
struct Host {
    domain: String,
    space: String,
}

impl Host {
    #[cfg(not(test))]
    fn current() -> Option<Self> {
        Self::this_mac()
    }

    /// Fixed, so tests on any platform can write a record that matches.
    #[cfg(test)]
    fn current() -> Option<Self> {
        Some(Self {
            domain: "darwin".into(),
            space: "ottam-test.local".into(),
        })
    }

    /// The CLI's `pidDomain` and `pidSpace` on macOS. Elsewhere its domain
    /// describes a namespace Core does not reproduce, so no record matches.
    #[cfg_attr(test, allow(dead_code))]
    fn this_mac() -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let mut buffer = [0u8; 256];
        if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
            return None;
        }
        let length = buffer.iter().position(|byte| *byte == 0)?;
        Some(Self {
            domain: "darwin".into(),
            space: String::from_utf8(buffer[..length].to_vec()).ok()?,
        })
    }
}

enum LockState {
    Missing,
    /// Birth time in milliseconds since the epoch.
    Born(f64),
    /// A symlink, file, or unreadable entry: nothing Core can account for.
    Unknown,
}

fn lock_state(path: &Path) -> LockState {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LockState::Missing,
        Ok(metadata) if metadata.is_dir() => metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(LockState::Unknown, |since| {
                LockState::Born(since.as_secs_f64() * 1000.0)
            }),
        _ => LockState::Unknown,
    }
}

/// Whether a lock is the one the record describes. Missing counts: the
/// holder may have released it, or Core removed it.
fn described(path: Option<&Path>, recorded: Option<f64>) -> bool {
    // Within a millisecond: the CLI's runtime and Rust round the same
    // timestamp differently.
    match path.map_or(LockState::Missing, lock_state) {
        LockState::Missing => true,
        LockState::Born(born) => recorded.is_some_and(|recorded| (born - recorded).abs() < 1.0),
        LockState::Unknown => false,
    }
}

/// The record, when every lock present is the one it describes and its holder
/// is proven gone. Mirrors the CLI's own takeover check.
fn abandoned(locks: &Locks, host: Option<&Host>) -> Option<OwnerRecord> {
    let host = host?;
    let record = read_owner(&locks.owner)?;
    (record.pid_domain == host.domain
        && record.pid_space == host.space
        && described(Some(&locks.current), Some(record.lock_birthtime_ms))
        && described(locks.legacy.as_deref(), record.legacy_lock_birthtime_ms)
        && locks.held()
        && holder_gone(record.pid, record.proc_start.as_deref()))
    .then_some(record)
}

fn clear_abandoned(locks: &Locks, host: Option<&Host>) -> bool {
    let Some(record) = abandoned(locks, host) else {
        return false;
    };
    // The CLI's order: legacy, then current, each re-checked just before its
    // removal. Never recursive.
    let remove = |path: &Path, recorded: Option<f64>| match lock_state(path) {
        LockState::Missing => Some(false),
        LockState::Born(_) if described(Some(path), recorded) => {
            std::fs::remove_dir(path).ok().map(|()| true)
        }
        _ => None,
    };
    let mut removed = false;
    for (path, recorded) in [
        (locks.legacy.as_deref(), record.legacy_lock_birthtime_ms),
        (Some(locks.current.as_path()), Some(record.lock_birthtime_ms)),
    ] {
        let Some(path) = path else { continue };
        match remove(path, recorded) {
            Some(done) => removed |= done,
            // Changed since the check, or not removable: leave the rest.
            None => return removed,
        }
    }
    if read_owner(&locks.owner).is_some_and(|now| {
        now.pid == record.pid && now.lock_birthtime_ms == record.lock_birthtime_ms
    }) {
        let _ = std::fs::remove_file(&locks.owner);
    }
    if removed {
        tracing::info!(target: "ottam_core::agent",
            "removed an OAuth refresh lock whose owner has exited");
    }
    removed
}

/// The CLI's test: a valid pid that no longer exists, is a zombie, or has a
/// different start time than recorded (the pid was reused). Anything Core
/// cannot establish leaves the holder alive.
fn holder_gone(pid: i64, recorded_start: Option<&str>) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } != 0 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    let Some((state, start)) = process_identity(pid) else {
        return false;
    };
    state.starts_with('Z') || recorded_start.is_some_and(|recorded| normalized(recorded) != start)
}

/// `ps` state and start time, formatted as the CLI records `procStart`.
fn process_identity(pid: i32) -> Option<(String, String)> {
    // The CLI pins locale and zone; local time would make every live holder
    // look like a reused pid.
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut fields = text.split_whitespace();
    let state = fields.next()?.to_string();
    let start = fields.collect::<Vec<_>>().join(" ");
    (!start.is_empty()).then_some((state, start))
}

fn normalized(start: &str) -> String {
    start.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Access is serialized by the registry. Managed launches permanently share
/// the home: their sessions and sign-in terminals can outlive registry status
/// changes. Only a fresh Core, or probes with confirmed shutdown and no managed
/// launch, may clean up a lock with no readable owner record. A cancelled
/// probe leaves its count outstanding, which conservatively disables that
/// cleanup until Core restarts. A lock whose owner record proves its holder
/// gone is removed whatever the home's use; one whose record names a live or
/// unverifiable holder never is.
#[derive(Default)]
pub(crate) struct HomeUse {
    shared: bool,
    probes: usize,
}

impl HomeUse {
    pub(crate) fn managed_start(&mut self, home: &Path) {
        self.clean(home);
        self.shared = true;
    }

    pub(crate) fn probe_start(&mut self, home: &Path) {
        self.clean(home);
        self.probes += 1;
    }

    pub(crate) fn probe_finished(&mut self, home: &Path, stopped: bool) {
        self.shared |= !stopped;
        self.probes -= 1;
        self.clean(home);
    }

    fn clean(&self, home: &Path) {
        if clear_if_owner_gone(home) || self.shared || self.probes != 0 {
            return;
        }
        let locks = Locks::of(&home.join(CLAUDE_REFRESH_LOCK));
        // A readable record names an owner Core can check, and one that
        // survived it (a CLI an earlier Core left running) may still be
        // refreshing. Only an ownerless lock falls back to home use.
        if read_owner(&locks.owner).is_some() {
            return;
        }
        // Never recursively remove an unexpected directory or follow a symlink.
        let mut removed = false;
        for path in [Some(&locks.current), locks.legacy.as_ref()].into_iter().flatten() {
            removed |= std::fs::remove_dir(path).is_ok();
        }
        if removed {
            let _ = std::fs::remove_file(&locks.owner);
            tracing::info!(target: "ottam_core::agent",
                "removed an OAuth refresh lock from an exclusively owned agent home");
        }
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    pub(crate) fn born(path: &Path) -> f64 {
        match lock_state(path) {
            LockState::Born(born) => born,
            _ => panic!("no birth time for {path:?}"),
        }
    }

    /// What the CLI writes once it holds both locks, on the test host.
    pub(crate) fn write_owner(home: &Path, pid: u32, proc_start: Option<&str>) {
        let current = home.join(CLAUDE_REFRESH_LOCK);
        let legacy = Locks::of(&current).legacy.unwrap();
        let host = Host::current().unwrap();
        let mut record = serde_json::json!({
            "pid": pid,
            "pidDomain": host.domain,
            "pidSpace": host.space,
            "lockBirthtimeMs": born(&current),
        });
        if let Some(start) = proc_start {
            record["procStart"] = start.into();
        }
        if legacy.exists() {
            record["legacyLockBirthtimeMs"] = born(&legacy).into();
        }
        std::fs::write(home.join(OWNER_RECORD), record.to_string()).unwrap();
    }

    pub(crate) fn exited_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();
        child.id()
    }

    pub(crate) fn legacy_lock(home: &Path) -> PathBuf {
        Locks::of(&home.join(CLAUDE_REFRESH_LOCK)).legacy.unwrap()
    }

    /// The home as a refresh whose holder exited leaves it: both locks and
    /// a record naming a reaped pid.
    pub(crate) fn abandoned_refresh(home: &Path) {
        std::fs::create_dir(home.join(CLAUDE_REFRESH_LOCK)).unwrap();
        std::fs::create_dir(legacy_lock(home)).unwrap();
        write_owner(home, exited_pid(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{born, exited_pid, write_owner};
    use super::*;

    /// A home inside a scratch directory, so its legacy sibling is too.
    fn home() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        (root, home)
    }

    fn lock(home: &Path) -> PathBuf {
        let path = home.join(CLAUDE_REFRESH_LOCK);
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn legacy_lock(home: &Path) -> PathBuf {
        let path = super::fixture::legacy_lock(home);
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn test_host() -> Host {
        Host::current().unwrap()
    }

    fn clear(home: &Path, host: &Host) -> bool {
        clear_abandoned(&Locks::of(&home.join(CLAUDE_REFRESH_LOCK)), Some(host))
    }

    #[test]
    fn refresh_lock_held_by_an_exited_owner_is_cleared_with_its_record() {
        let (_root, home) = home();
        let current = lock(&home);
        let legacy = legacy_lock(&home);
        write_owner(&home, exited_pid(), Some("Sat Oct 31 00:37:03 2026"));
        assert!(clear(&home, &test_host()));
        assert!(!current.exists());
        assert!(!legacy.exists());
        assert!(!home.join(OWNER_RECORD).exists());
    }

    #[test]
    fn refresh_lock_legacy_half_left_by_an_exited_owner_is_cleared() {
        // The incident: Core removed the current lock and left the legacy one.
        let (_root, home) = home();
        let current = lock(&home);
        let legacy = legacy_lock(&home);
        write_owner(&home, exited_pid(), None);
        std::fs::remove_dir(&current).unwrap();
        assert!(clear(&home, &test_host()));
        assert!(!legacy.exists());
    }

    #[test]
    fn refresh_lock_held_by_a_live_owner_is_kept() {
        let (_root, home) = home();
        let current = lock(&home);
        let me = std::process::id();
        let (_, start) = process_identity(me as i32).unwrap();
        for recorded in [Some(start.as_str()), None] {
            write_owner(&home, me, recorded);
            assert!(!clear(&home, &test_host()));
            assert!(current.exists());
            assert!(home.join(OWNER_RECORD).exists());
        }
    }

    /// A CLI an earlier Core left running may still be refreshing when a
    /// fresh Core first uses the home, which looks exclusive to it.
    #[test]
    fn refresh_lock_held_by_a_live_owner_survives_exclusive_home_cleanup() {
        let (_root, home) = home();
        let current = lock(&home);
        let legacy = legacy_lock(&home);
        write_owner(&home, std::process::id(), None);
        HomeUse::default().probe_start(&home);
        HomeUse::default().managed_start(&home);
        assert!(current.exists() && legacy.exists());
        assert!(home.join(OWNER_RECORD).exists());
    }

    #[test]
    fn refresh_lock_whose_owner_pid_was_reused_is_cleared() {
        let (_root, home) = home();
        let current = lock(&home);
        write_owner(&home, std::process::id(), Some("Mon Jan  1 00:00:00 2001"));
        assert!(clear(&home, &test_host()));
        assert!(!current.exists());
    }

    #[test]
    fn refresh_lock_recorded_on_another_host_is_kept() {
        let (_root, home) = home();
        let current = lock(&home);
        write_owner(&home, exited_pid(), None);
        let other = Host {
            space: "another-mac.local".into(),
            ..test_host()
        };
        assert!(!clear(&home, &other));
        assert!(current.exists());
    }

    #[test]
    fn refresh_lock_not_described_by_the_record_is_kept() {
        let (_root, home) = home();
        let current = lock(&home);
        write_owner(&home, exited_pid(), None);
        // A legacy lock the record doesn't name belongs to someone else.
        let legacy = legacy_lock(&home);
        assert!(!clear(&home, &test_host()));
        assert!(current.exists() && legacy.exists());
        // A newer current lock whose holder hasn't written its record yet.
        std::fs::remove_dir(&legacy).unwrap();
        let record = std::fs::read(home.join(OWNER_RECORD)).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(&record).unwrap();
        record["lockBirthtimeMs"] = (born(&current) - 5_000.0).into();
        std::fs::write(home.join(OWNER_RECORD), record.to_string()).unwrap();
        assert!(!clear(&home, &test_host()));
        assert!(current.exists());
    }

    #[test]
    fn refresh_lock_without_a_readable_record_falls_back_to_home_use() {
        for record in [None, Some("{not json"), Some(r#"{"pid":"13210"}"#)] {
            let (_root, home) = home();
            let mut shared = HomeUse::default();
            shared.managed_start(&home);
            let current = lock(&home);
            if let Some(record) = record {
                std::fs::write(home.join(OWNER_RECORD), record).unwrap();
            }
            assert!(!clear(&home, &test_host()));
            assert!(is_refreshing(Some(&current)));
            shared.probe_start(&home);
            shared.probe_finished(&home, true);
            assert!(current.exists(), "a shared home keeps an ownerless lock");
            HomeUse::default().probe_start(&home);
            assert!(!current.exists(), "an exclusive home clears it");
        }
    }

    #[test]
    fn refresh_lock_owner_record_symlink_is_not_followed() {
        let (root, home) = home();
        lock(&home);
        let target = root.path().join("elsewhere");
        std::fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, home.join(OWNER_RECORD)).unwrap();
        assert!(read_owner(&home.join(OWNER_RECORD)).is_none());
    }

    #[test]
    fn refresh_lock_legacy_name_is_appended_to_the_real_home() {
        let (root, _) = home();
        let dotted = root.path().join("claude.home");
        std::fs::create_dir(&dotted).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dotted, &link).unwrap();
        let legacy = Locks::of(&link.join(CLAUDE_REFRESH_LOCK)).legacy.unwrap();
        assert_eq!(
            legacy,
            std::fs::canonicalize(root.path()).unwrap().join("claude.home.lock")
        );
    }

    /// So no wait outlasts a holder that has already exited.
    #[test]
    fn refresh_lock_with_an_exited_owner_is_not_a_refresh() {
        let (_root, home) = home();
        super::fixture::abandoned_refresh(&home);
        assert!(!is_refreshing(Some(&home.join(CLAUDE_REFRESH_LOCK))));
        write_owner(&home, std::process::id(), None);
        assert!(is_refreshing(Some(&home.join(CLAUDE_REFRESH_LOCK))));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn refresh_lock_host_is_named_as_the_cli_names_it() {
        let host = Host::this_mac().unwrap();
        let output = std::process::Command::new("/bin/hostname").output().unwrap();
        assert_eq!(host.domain, "darwin");
        assert_eq!(host.space, String::from_utf8(output.stdout).unwrap().trim());
    }

    #[test]
    fn refresh_lock_legacy_half_alone_still_reads_as_refreshing() {
        let (_root, home) = home();
        let legacy = legacy_lock(&home);
        assert!(is_refreshing(Some(&home.join(CLAUDE_REFRESH_LOCK))));
        std::fs::remove_dir(legacy).unwrap();
        assert!(!is_refreshing(Some(&home.join(CLAUDE_REFRESH_LOCK))));
    }

    #[test]
    fn refresh_lock_is_cleaned_before_first_launch_and_after_exclusive_probe() {
        let (_root, home) = home();
        let mut state = HomeUse::default();
        let old = lock(&home);
        let old_legacy = legacy_lock(&home);
        state.probe_start(&home);
        assert!(!old.exists() && !old_legacy.exists());
        let orphan = lock(&home);
        state.probe_finished(&home, true);
        assert!(!orphan.exists());
    }

    #[test]
    fn refresh_lock_survives_managed_launch_during_probe_and_later_restarts() {
        let (_root, home) = home();
        let mut state = HomeUse::default();
        state.probe_start(&home);
        let active = lock(&home);
        state.managed_start(&home);
        state.probe_finished(&home, true);
        state.managed_start(&home);
        state.probe_start(&home);
        state.probe_finished(&home, true);
        assert!(
            active.exists(),
            "a sign-in terminal may still own this lock"
        );
    }

    #[test]
    fn refresh_lock_waits_for_every_probe_and_preserves_unconfirmed_shutdown() {
        for stopped in [true, false] {
            let (_root, home) = home();
            let mut state = HomeUse::default();
            state.probe_start(&home);
            state.probe_start(&home);
            let active = lock(&home);
            state.probe_finished(&home, true);
            assert!(active.exists());
            state.probe_finished(&home, stopped);
            assert_eq!(active.exists(), !stopped);
            if !stopped {
                state.managed_start(&home);
                assert!(active.exists());
            }
        }
    }

    #[test]
    fn refresh_lock_cleanup_preserves_nonempty_directories() {
        let (_root, home) = home();
        let path = lock(&home);
        std::fs::write(path.join("owner"), "unexpected").unwrap();
        HomeUse::default().managed_start(&home);
        assert!(path.exists());
    }
}
