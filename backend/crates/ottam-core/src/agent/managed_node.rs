//! The Node runtime Ottam installs and owns.
//!
//! Every ACP adapter is a Node program, so a Mac with no Node — or with one
//! too old for an adapter's `engines.node` — cannot run an agent at all. Rather
//! than sending the user to nodejs.org and hoping, Ottam downloads a pinned
//! Node into `~/.ottam/runtime/node/<version>/` and resolves to it when the
//! system has nothing better.
//!
//! The pin lives in the app binary rather than a remote registry, so updating
//! it is an app update. Zed took the other route and shipped a version its
//! registry had pinned before it existed on npm, which broke agents silently
//! (zed#53309); a constant cannot get ahead of the release it ships in.

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::net::{DownloadError, DownloadProgress, DownloadRequest, HttpClient};

/// The vendored line is the newest *active LTS*, not the newest release.
///
/// This is a preference, not a rejection threshold — unlike the per-engine
/// floors in `registry.rs`, which track what each adapter actually demands.
/// Conflating them would only reject working machines: a floor of 26 would
/// refuse a perfectly good Node 25. Revisit when 26 goes LTS; it is a
/// one-constant change plus the two digests below.
const NODE_VERSION: &str = "v24.22.1";
const DARWIN_ARM64_SHA256: &str =
    "89c682593d76cd44233fa53b1563264e7de9cbb189cad169135c5b4fadfbc0a7";
const DARWIN_X64_SHA256: &str = "d132557e7ee2b85af08b0c8c34338e65c6430f2238d01f727728ef42dc83b035";

const DEFAULT_DIST_BASE: &str = "https://nodejs.org/dist";

/// Unpacking ~50 MB is seconds of work; this only has to stop a wedged tar.
const EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// bsdtar, shipped with macOS and gzip-capable. Not `/usr/bin/git`'s situation:
/// tar needs no Xcode Command Line Tools install.
const TAR: &str = "/usr/bin/tar";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallProgress {
    Download(DownloadProgress),
    Extracting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallError {
    UnsupportedArchitecture(String),
    Download(DownloadError),
    Extract(String),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedArchitecture(arch) => {
                write!(formatter, "no Node build for this architecture ({arch})")
            }
            Self::Download(error) => write!(formatter, "{error}"),
            Self::Extract(detail) => write!(formatter, "could not unpack Node: {detail}"),
            Self::Io(detail) => write!(formatter, "file error: {detail}"),
            Self::Cancelled => write!(formatter, "cancelled"),
        }
    }
}

impl std::error::Error for InstallError {}

/// Lets an integration test serve a tarball from a local socket instead of
/// reaching nodejs.org.
#[cfg(test_seams)]
fn test_dist_override() -> (Option<String>, Option<String>) {
    let configured_base = std::env::var("OTTAM_NODE_DIST_BASE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let dist_sha256 = configured_base.as_ref().and_then(|_| {
        std::env::var("OTTAM_NODE_DIST_SHA256")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    });
    (configured_base, dist_sha256)
}

/// Ottam's own Node install, rooted at `~/.ottam/runtime/node`.
#[derive(Debug, Clone)]
pub(crate) struct ManagedNode {
    root: PathBuf,
    dist_base: String,
    /// Replaces the pinned digest, and only ever set alongside an overridden
    /// `dist_base` — a test serving its own tarball has to be able to state
    /// what that tarball hashes to. Requiring both means a stray environment
    /// variable cannot weaken verification of the real download.
    dist_sha256: Option<String>,
}

impl ManagedNode {
    /// None when the user has opted out, or when there is no usable app home —
    /// both mean "behave exactly as Ottam did before this existed".
    pub fn from_environment() -> Option<Self> {
        if std::env::var("OTTAM_MANAGED_NODE").as_deref() == Ok("off") {
            return None;
        }
        let root = crate::storage::ottam_home().ok()?.join("runtime/node");
        Some(Self::at(root))
    }

    pub fn at(root: PathBuf) -> Self {
        #[cfg(test_seams)]
        let (configured_base, dist_sha256) = test_dist_override();
        // Together the two would install any Node from any URL, so a release
        // build always downloads the pinned build from nodejs.org.
        #[cfg(not(test_seams))]
        let (configured_base, dist_sha256) = (None, None);
        Self {
            root,
            dist_base: normalize_dist_base(configured_base),
            dist_sha256,
        }
    }

    pub fn version(&self) -> &'static str {
        NODE_VERSION
    }

    /// The installed `node`, if this version is on disk and runnable.
    ///
    /// The versioned directory is published by a rename as the last step of an
    /// install, so its presence already means "complete" — no sentinel file.
    pub fn installed_node(&self) -> Option<PathBuf> {
        let node = self.version_dir().join("bin/node");
        is_executable_file(&node).then_some(node)
    }

    /// The `node` to run now: the pinned one, or — while an app update's new
    /// pin has not landed yet — the newest older managed runtime still on
    /// disk. A pin bump must not turn a working machine into "Node not found";
    /// the older runtime keeps the agents running while the new one downloads.
    /// A newer runtime, left by a rolled-back app, never runs: this build's
    /// adapters were verified against the pin, not against it.
    pub fn usable_node(&self) -> Option<PathBuf> {
        let pin = super::adapter_install::version_order_key(NODE_VERSION);
        self.installed_node().or_else(|| {
            self.other_installs()
                .into_iter()
                .filter(|(version, _)| super::adapter_install::version_order_key(version) < pin)
                .map(|(_, dir)| dir.join("bin/node"))
                .find(|node| is_executable_file(node))
        })
    }

    /// Another runtime on disk while the pin is not installed yet — the
    /// standing consent for installing the pin without asking, as another
    /// adapter version is for its pin. Usually an older one standing in for
    /// an app update's new pin; a newer one only consents to restoring the
    /// pin after a rollback. None once the pin is on disk, or on a machine
    /// that never installed a managed runtime (a first install waits for the
    /// user).
    pub fn superseded_version(&self) -> Option<String> {
        if self.installed_node().is_some() {
            return None;
        }
        self.other_installs()
            .into_iter()
            .find(|(_, dir)| is_executable_file(&dir.join("bin/node")))
            .map(|(version, _)| version)
    }

    /// Every versioned directory other than the pin, newest first.
    fn other_installs(&self) -> Vec<(String, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut installs: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name.starts_with('v') && name != NODE_VERSION).then(|| (name, entry.path()))
            })
            .collect();
        installs.sort_by(|left, right| {
            super::adapter_install::version_order_key(&right.0)
                .cmp(&super::adapter_install::version_order_key(&left.0))
        });
        installs
    }

    fn version_dir(&self) -> PathBuf {
        self.root.join(NODE_VERSION)
    }

    /// Download, verify, unpack, and publish the pinned Node.
    ///
    /// Idempotent: an install that is already on disk returns immediately, so
    /// a stray second request costs nothing.
    pub async fn install(
        &self,
        http: &HttpClient,
        cancel: &CancellationToken,
        on_progress: impl Fn(InstallProgress),
    ) -> Result<PathBuf, InstallError> {
        if let Some(node) = self.installed_node() {
            return Ok(node);
        }
        let release = Release::for_host()?;

        // A staging directory from a killed install would otherwise accumulate.
        self.remove_stale_staging();

        let tarball = self.root.join("downloads").join(release.file_name());
        let expected_sha256 = self
            .dist_sha256
            .clone()
            .unwrap_or_else(|| release.sha256.to_string());
        http.download(
            DownloadRequest::new(release.url(&self.dist_base), &tarball)
                .expecting_sha256(expected_sha256),
            cancel,
            |progress| on_progress(InstallProgress::Download(progress)),
        )
        .await
        .map_err(|error| match error {
            DownloadError::Cancelled => InstallError::Cancelled,
            other => InstallError::Download(other),
        })?;

        if cancel.is_cancelled() {
            let _ = std::fs::remove_file(&tarball);
            return Err(InstallError::Cancelled);
        }

        on_progress(InstallProgress::Extracting);
        let staging = self.staging_dir();
        let extracted = self.extract(&tarball, &staging).await;
        let _ = std::fs::remove_file(&tarball);
        if let Err(error) = extracted {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }

        // Extraction is not itself interruptible, so re-check before the
        // rename: without this, Cancel pressed once the UI says "Extracting"
        // still ends in `ready`.
        if cancel.is_cancelled() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(InstallError::Cancelled);
        }

        // Publish atomically. Until this rename lands there is no versioned
        // directory, so a crash anywhere above leaves nothing to mistake for a
        // finished install.
        let destination = self.version_dir();
        let _ = std::fs::remove_dir_all(&destination);
        std::fs::rename(&staging, &destination).map_err(|error| {
            let _ = std::fs::remove_dir_all(&staging);
            InstallError::Io(error.to_string())
        })?;

        // Older runtimes are deliberately left in place: adapters launched
        // before this install still run on them. `prune_superseded` removes
        // them once nothing does.

        self.installed_node().ok_or_else(|| {
            InstallError::Extract("the unpacked runtime has no bin/node".to_string())
        })
    }

    async fn extract(&self, tarball: &Path, staging: &Path) -> Result<(), InstallError> {
        let _ = std::fs::remove_dir_all(staging);
        std::fs::create_dir_all(staging).map_err(|error| InstallError::Io(error.to_string()))?;

        let tarball = tarball.to_path_buf();
        let staging = staging.to_path_buf();
        // The tarball's single root is `node-<version>-<arch>/`, which we strip
        // so the version directory holds `bin/`, `lib/`, `include/` directly.
        let output = tokio::task::spawn_blocking(move || {
            super::registry::command_output_args_with_timeout(
                TAR,
                &[
                    "-xzf",
                    &tarball.to_string_lossy(),
                    "--strip-components=1",
                    "-C",
                    &staging.to_string_lossy(),
                ],
                EXTRACT_TIMEOUT,
            )
        })
        .await
        .map_err(|error| InstallError::Extract(error.to_string()))?
        .map_err(|error| InstallError::Extract(error.to_string()))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(InstallError::Extract(if detail.is_empty() {
                format!("tar exited with {}", output.status)
            } else {
                detail
            }));
        }
        Ok(())
    }

    fn staging_dir(&self) -> PathBuf {
        self.root.join(format!(".staging-{}", std::process::id()))
    }

    fn remove_stale_staging(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".staging-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Keep only the version this build pins, once it is installed. An
    /// upgrade otherwise leaves every past runtime on disk at ~100 MB each.
    ///
    /// `in_use` guards runtimes a live adapter was launched from: deleting
    /// one would break every `node` that adapter spawns through its PATH.
    /// Those are left for a later pass — at the latest the next launch, when
    /// nothing runs yet.
    pub fn prune_superseded(&self, in_use: impl Fn(&Path) -> bool) {
        if self.installed_node().is_none() {
            return;
        }
        for (_, dir) in self.other_installs() {
            if !in_use(&dir) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        let _ = std::fs::remove_dir_all(self.root.join("downloads"));
    }
}

struct Release {
    arch: &'static str,
    sha256: &'static str,
}

impl Release {
    fn for_host() -> Result<Self, InstallError> {
        match std::env::consts::ARCH {
            "aarch64" => Ok(Self {
                arch: "darwin-arm64",
                sha256: DARWIN_ARM64_SHA256,
            }),
            "x86_64" => Ok(Self {
                arch: "darwin-x64",
                sha256: DARWIN_X64_SHA256,
            }),
            other => Err(InstallError::UnsupportedArchitecture(other.to_string())),
        }
    }

    fn file_name(&self) -> String {
        format!("node-{NODE_VERSION}-{}.tar.gz", self.arch)
    }

    fn url(&self, dist_base: &str) -> String {
        format!("{dist_base}/{NODE_VERSION}/{}", self.file_name())
    }
}

/// A trailing slash on the override is the obvious way to write it and must not
/// produce a doubled separator in the URL.
fn normalize_dist_base(configured: Option<String>) -> String {
    configured
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DIST_BASE.to_string())
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, "#!/bin/sh\necho v24.22.1\n").expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn an_installed_runtime_is_found_by_its_versioned_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let managed = ManagedNode::at(directory.path().to_path_buf());
        assert_eq!(managed.installed_node(), None);

        let node = directory.path().join(NODE_VERSION).join("bin/node");
        write_executable(&node);

        assert_eq!(managed.installed_node(), Some(node));
    }

    #[test]
    fn a_non_executable_node_does_not_count_as_installed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let managed = ManagedNode::at(directory.path().to_path_buf());
        let node = directory.path().join(NODE_VERSION).join("bin/node");
        std::fs::create_dir_all(node.parent().expect("parent")).expect("dirs");
        std::fs::write(&node, "not runnable").expect("write");

        assert_eq!(managed.installed_node(), None);
    }

    #[test]
    fn pruning_keeps_the_pinned_version_and_clears_downloads() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let managed = ManagedNode::at(root.to_path_buf());
        write_executable(&root.join(NODE_VERSION).join("bin/node"));
        write_executable(&root.join("v22.9.0").join("bin/node"));
        std::fs::create_dir_all(root.join("downloads")).expect("dirs");

        managed.prune_superseded(|_| false);

        assert!(root.join(NODE_VERSION).exists(), "the pin was removed");
        assert!(!root.join("v22.9.0").exists(), "an old version survived");
        assert!(!root.join("downloads").exists(), "the tarball survived");
    }

    #[test]
    fn pruning_spares_runtimes_in_use_and_waits_for_the_pin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let managed = ManagedNode::at(root.to_path_buf());
        write_executable(&root.join("v22.9.0").join("bin/node"));
        write_executable(&root.join("v24.1.0").join("bin/node"));

        // Without the pin, the older runtimes are all there is to run on.
        managed.prune_superseded(|_| false);
        assert!(root.join("v22.9.0").exists() && root.join("v24.1.0").exists());

        write_executable(&root.join(NODE_VERSION).join("bin/node"));
        let busy = root.join("v24.1.0");
        managed.prune_superseded(|dir| dir == busy);
        assert!(
            !root.join("v22.9.0").exists(),
            "an idle old runtime survived"
        );
        assert!(
            root.join("v24.1.0").exists(),
            "a runtime in use was removed"
        );
    }

    /// After an app rollback only a newer runtime may be on disk. It never
    /// runs, but it still consents to restoring the pin in the background.
    #[test]
    fn a_newer_runtime_never_stands_in_for_the_pin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let managed = ManagedNode::at(root.to_path_buf());
        write_executable(&root.join("v99.0.0").join("bin/node"));
        assert_eq!(managed.usable_node(), None);
        assert_eq!(managed.superseded_version().as_deref(), Some("v99.0.0"));
    }

    #[test]
    fn an_older_runtime_stands_in_until_the_pin_lands() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let managed = ManagedNode::at(root.to_path_buf());
        assert_eq!(managed.usable_node(), None);
        assert_eq!(managed.superseded_version(), None);

        write_executable(&root.join("v9.0.0").join("bin/node"));
        write_executable(&root.join("v24.1.0").join("bin/node"));
        // A half-removed directory without a runnable node is skipped.
        std::fs::create_dir_all(root.join("v24.9.0")).expect("dirs");

        assert_eq!(
            managed.usable_node(),
            Some(root.join("v24.1.0").join("bin/node")),
            "the newest runnable older runtime stands in, by version not by name"
        );
        assert_eq!(managed.superseded_version().as_deref(), Some("v24.1.0"));
        assert_eq!(
            managed.installed_node(),
            None,
            "the pin itself is not installed"
        );

        let pinned = root.join(NODE_VERSION).join("bin/node");
        write_executable(&pinned);
        assert_eq!(managed.usable_node(), Some(pinned));
        assert_eq!(managed.superseded_version(), None);
    }

    #[test]
    fn stale_staging_directories_are_cleared() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let managed = ManagedNode::at(root.to_path_buf());
        std::fs::create_dir_all(root.join(".staging-999")).expect("dirs");
        write_executable(&root.join(NODE_VERSION).join("bin/node"));

        managed.remove_stale_staging();

        assert!(!root.join(".staging-999").exists());
        assert!(root.join(NODE_VERSION).exists(), "a real install was hit");
    }

    #[test]
    fn the_release_url_follows_the_nodejs_dist_layout() {
        let release = Release::for_host().expect("a supported architecture");
        assert_eq!(
            release.url("https://nodejs.org/dist"),
            format!(
                "https://nodejs.org/dist/{NODE_VERSION}/node-{NODE_VERSION}-{}.tar.gz",
                release.arch
            )
        );
        assert!(
            ["darwin-arm64", "darwin-x64"].contains(&release.arch),
            "unexpected arch {}",
            release.arch
        );
    }

    #[test]
    fn a_trailing_slash_on_the_dist_override_does_not_double_up() {
        assert_eq!(
            normalize_dist_base(Some("http://127.0.0.1:8080/dist/".to_string())),
            "http://127.0.0.1:8080/dist"
        );
        assert_eq!(
            normalize_dist_base(Some("  ".to_string())),
            DEFAULT_DIST_BASE
        );
        assert_eq!(normalize_dist_base(None), DEFAULT_DIST_BASE);
    }
}
