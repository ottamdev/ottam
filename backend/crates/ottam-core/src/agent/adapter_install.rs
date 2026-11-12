//! Installed adapter packages, and the npm and binary steps that install them.
//!
//! Adapters used to launch as `npx -y <pkg>@<pin>` — a registry hit on every
//! cold start and nothing usable offline. This module owns the installed
//! alternative: each agent's pinned package lands in
//! `~/.ottam/agents/<id>/adapter/<version>/` with a recorded manifest, and
//! the launch spec points at the installed entrypoint.
//!
//! Two layers, deliberately separate. [`ManagedAdapterInstalls`] is the
//! source-agnostic part — directories, manifests, atomic publish — and is the
//! seam a future vendor-binary source plugs into. [`npm_install_into`] is the
//! npm-specific part: it fetches through npm because only npm reproduces the
//! dependency tree (codex-acp's native binary arrives via a transitive
//! platform-specific optional dependency), and npm already verifies each
//! tarball against the registry's integrity data, which the manifest records.
//! [`binary_install_into`] is the vendor-binary source: one download per
//! architecture, checked against a pinned sha256 before anything unpacks.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::types::{AdapterPackage, AdapterSource, BinaryAdapter, BinaryArchive};
use crate::net::{DownloadError, DownloadRequest, HttpClient};

/// npm resolves and downloads a full dependency tree; give it real time, but
/// not forever — a wedged registry connection must not pin the install UI.
const NPM_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Unpacking one member of a vendor tarball is local work; this only guards
/// against a wedged `tar`.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(120);

/// bsdtar, as for the managed Node runtime.
const TAR: &str = "/usr/bin/tar";

/// What an install wrote, recorded beside it. `resolved`/`integrity` are npm's
/// own answers from the lockfile, or a binary's URL and pinned digest — the
/// auditable "what exactly landed here".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdapterManifest {
    pub name: String,
    pub version: String,
    pub resolved: Option<String>,
    pub integrity: Option<String>,
    /// The script `node` runs, or the executable itself for a binary,
    /// relative to the version directory.
    pub entrypoint: PathBuf,
    pub node_version: Option<String>,
}

impl AdapterManifest {
    /// Whether this install is the same *kind* as the package's source — npm
    /// tree or vendor binary — regardless of version. An older version can
    /// only stand in for a pin it could actually be launched like.
    fn matches_source_kind(&self, source: &AdapterSource) -> bool {
        match source {
            AdapterSource::Npm => self.entrypoint.starts_with("node_modules"),
            AdapterSource::Binary(binary) => {
                self.entrypoint == Path::new("bin").join(&binary.command)
            }
        }
    }

    /// Whether this install came from the package's current source: npm for
    /// an npm package, or this host's pinned binary, with the same URL,
    /// digest, and command. A changed source at the same version reinstalls.
    fn matches_source(&self, source: &AdapterSource) -> bool {
        match source {
            AdapterSource::Npm => self.entrypoint.starts_with("node_modules"),
            AdapterSource::Binary(binary) => binary.for_host().is_some_and(|artifact| {
                self.resolved.as_deref() == Some(artifact.url.as_str())
                    && self.integrity == Some(format!("sha256:{}", artifact.sha256))
                    && self.entrypoint == Path::new("bin").join(&binary.command)
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdapterInstallError {
    /// npm itself failed — nonzero exit, timeout, or it could not be spawned.
    Npm(String),
    /// A binary download failed or did not match its pinned digest.
    Download(String),
    /// npm succeeded but what it wrote cannot be described: no package dir,
    /// no usable `bin`, unreadable lockfile.
    Manifest(String),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for AdapterInstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Npm(detail) => write!(formatter, "npm install failed: {detail}"),
            Self::Download(detail) => write!(formatter, "download failed: {detail}"),
            Self::Manifest(detail) => {
                write!(formatter, "the installed package is unusable: {detail}")
            }
            Self::Io(detail) => write!(formatter, "file error: {detail}"),
            Self::Cancelled => write!(formatter, "cancelled"),
        }
    }
}

impl std::error::Error for AdapterInstallError {}

/// The installed-adapter store, rooted at `~/.ottam/agents`.
///
/// Layout per agent: `<root>/<id>/adapter/<version>/` beside the isolated
/// `<root>/<id>/home` the home policy owns. Keyed by agent id rather than
/// package name so a future source change (vendor binaries) moves nothing.
#[derive(Debug, Clone)]
pub(crate) struct ManagedAdapterInstalls {
    root: PathBuf,
}

/// One installed, launchable version of an agent's adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledAdapter {
    pub version: String,
    pub directory: PathBuf,
    pub entrypoint: PathBuf,
}

pub(crate) struct PublishedAdapter {
    destination: PathBuf,
    backup: Option<PathBuf>,
    /// The commit marker for a publish with no backup (a pin bump or first
    /// install); `None` when `backup` already marks it.
    pending: Option<PathBuf>,
}

impl ManagedAdapterInstalls {
    /// None when there is no usable app home, or when a test build is told
    /// `OTTAM_ADAPTER_INSTALLS=off` — both mean "behave exactly as Ottam did
    /// before installed adapters existed": npx-on-demand, no install
    /// requirement. The integration-test harness sets the opt-out globally so
    /// existing suites stay hermetic; only the install tests turn it back on.
    /// Release builds have no opt-out, since it re-enables unverified npx.
    pub fn from_environment() -> Option<Self> {
        #[cfg(test_seams)]
        if std::env::var("OTTAM_ADAPTER_INSTALLS").as_deref() == Ok("off") {
            return None;
        }
        let root = crate::storage::ottam_home().ok()?.join("agents");
        Some(Self::at(root))
    }

    pub fn at(root: PathBuf) -> Self {
        let installs = Self { root };
        installs.recover_interrupted_publishes();
        installs
    }

    fn adapter_root(&self, agent_id: &str) -> PathBuf {
        self.root.join(agent_id).join("adapter")
    }

    fn version_dir(&self, agent_id: &str, version: &str) -> PathBuf {
        self.adapter_root(agent_id).join(version)
    }

    fn rollback_dir(&self, agent_id: &str, version: &str) -> PathBuf {
        self.adapter_root(agent_id)
            .join(format!(".rollback-{version}"))
    }

    fn pending_marker(&self, agent_id: &str, version: &str) -> PathBuf {
        self.adapter_root(agent_id)
            .join(format!(".pending-{version}"))
    }

    /// A pending marker that survived process exit means the version beside
    /// it never verified: remove both.
    fn recover_pending(&self, agent_id: &str, version: &str) -> Result<(), AdapterInstallError> {
        remove_tree_if_exists(&self.version_dir(agent_id, version))?;
        match std::fs::remove_file(self.pending_marker(agent_id, version)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AdapterInstallError::Io(error.to_string())),
        }
    }

    fn recover_rollback(&self, agent_id: &str, version: &str) -> Result<(), AdapterInstallError> {
        let rollback = self.rollback_dir(agent_id, version);
        if !rollback.exists() {
            return Ok(());
        }
        let destination = self.version_dir(agent_id, version);
        remove_tree_if_exists(&destination)?;
        std::fs::rename(rollback, destination)
            .map_err(|error| AdapterInstallError::Io(error.to_string()))
    }

    /// A rollback directory or pending marker is the commit marker: if it
    /// survived process exit, the candidate was never accepted. Restore the
    /// verified tree (or remove the unverified version) before any
    /// installed-state query can observe the candidate. Discard directories
    /// have crossed the commit point and are only cleanup debris.
    fn recover_interrupted_publishes(&self) {
        let Ok(agents) = std::fs::read_dir(&self.root) else {
            return;
        };
        for agent in agents.flatten() {
            let agent_id = agent.file_name().to_string_lossy().into_owned();
            let adapter = agent.path().join("adapter");
            let Ok(entries) = std::fs::read_dir(adapter) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let (version, recovered) = if let Some(version) = name.strip_prefix(".rollback-") {
                    (version, self.recover_rollback(&agent_id, version))
                } else if let Some(version) = name.strip_prefix(".pending-") {
                    (version, self.recover_pending(&agent_id, version))
                } else {
                    if name.starts_with(".discard-") {
                        let _ = remove_tree_if_exists(&entry.path());
                    }
                    continue;
                };
                if let Err(error) = recovered {
                    tracing::warn!(
                        target: "ottam_core::agent",
                        agent_id,
                        version,
                        error = %error,
                        "failed to recover interrupted adapter publish"
                    );
                }
            }
        }
    }

    /// The manifest for one installed version, if it parses.
    pub fn read_manifest(&self, agent_id: &str, version: &str) -> Option<AdapterManifest> {
        let path = self.version_dir(agent_id, version).join("manifest.json");
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The absolute entrypoint for the pinned package, if this exact pin is
    /// installed and intact.
    ///
    /// The versioned directory is published by a rename as the last step of an
    /// install, so its presence already means "complete"; the manifest match
    /// is what rejects a directory left by an older app's different pin.
    pub fn installed_entrypoint(
        &self,
        agent_id: &str,
        package: &AdapterPackage,
    ) -> Option<PathBuf> {
        let manifest = self.read_manifest(agent_id, &package.version)?;
        if manifest.name != package.name
            || manifest.version != package.version
            || !manifest.matches_source(&package.source)
        {
            return None;
        }
        let entrypoint = self
            .version_dir(agent_id, &package.version)
            .join(&manifest.entrypoint);
        entrypoint.is_file().then_some(entrypoint)
    }

    /// Whether some *other* version of the same package is on disk — the
    /// standing consent that lets a pin bump install without asking again.
    /// Deliberately looser than [`Self::superseded_install`]: an older install
    /// too broken to launch still says the user chose this agent, and its
    /// repair matters more, not less.
    pub fn installed_other_version(&self, agent_id: &str, package: &AdapterPackage) -> bool {
        let Ok(entries) = std::fs::read_dir(self.adapter_root(agent_id)) else {
            return false;
        };
        entries.flatten().any(|entry| {
            let version = entry.file_name();
            let version = version.to_string_lossy();
            version != package.version
                && self
                    .read_manifest(agent_id, &version)
                    .is_some_and(|manifest| manifest.name == package.name)
        })
    }

    /// The newest intact install older than the pin — what launches while an
    /// app update's new pin installs. A newer one (left by a rolled-back app)
    /// never stands in: it may speak an ACP contract this core predates.
    pub fn superseded_install(
        &self,
        agent_id: &str,
        package: &AdapterPackage,
    ) -> Option<InstalledAdapter> {
        let entries = std::fs::read_dir(self.adapter_root(agent_id)).ok()?;
        let mut candidates: Vec<InstalledAdapter> = entries
            .flatten()
            .filter_map(|entry| {
                let version = entry.file_name().to_string_lossy().into_owned();
                if version.starts_with('.')
                    || version_order_key(&version) >= version_order_key(&package.version)
                {
                    return None;
                }
                let manifest = self.read_manifest(agent_id, &version)?;
                if manifest.name != package.name
                    || manifest.version != version
                    || !manifest.matches_source_kind(&package.source)
                {
                    return None;
                }
                let entrypoint = entry.path().join(&manifest.entrypoint);
                entrypoint.is_file().then_some(InstalledAdapter {
                    version,
                    directory: entry.path(),
                    entrypoint,
                })
            })
            .collect();
        candidates.sort_by(|left, right| {
            version_order_key(&right.version).cmp(&version_order_key(&left.version))
        });
        candidates.into_iter().next()
    }

    /// What launches for this package now: the exact pin when it is installed,
    /// otherwise the newest older version standing in for it.
    pub fn launchable(&self, agent_id: &str, package: &AdapterPackage) -> Option<InstalledAdapter> {
        if let Some(entrypoint) = self.installed_entrypoint(agent_id, package) {
            return Some(InstalledAdapter {
                version: package.version.clone(),
                directory: self.version_dir(agent_id, &package.version),
                entrypoint,
            });
        }
        self.superseded_install(agent_id, package)
    }

    pub fn staging_dir(&self, agent_id: &str) -> PathBuf {
        self.adapter_root(agent_id)
            .join(format!(".staging-{}", std::process::id()))
    }

    /// A staging directory from a killed install would otherwise accumulate.
    pub fn remove_stale_staging(&self, agent_id: &str) {
        let Ok(entries) = std::fs::read_dir(self.adapter_root(agent_id)) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".staging-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Publish atomically. Until this rename lands there is no versioned
    /// directory, so a crash anywhere earlier leaves nothing to mistake for a
    /// finished install. Until [`Self::commit_publish`], a `.rollback-` backup
    /// (same-pin reinstall) or a `.pending-` marker (new version) marks the
    /// candidate unverified, and startup recovery removes it.
    ///
    /// Deliberately does *not* prune the version it supersedes — see
    /// [`Self::prune_superseded`], which the caller runs once the new version
    /// has verified.
    pub fn publish(
        &self,
        staging: &Path,
        agent_id: &str,
        version: &str,
    ) -> Result<PublishedAdapter, AdapterInstallError> {
        let destination = self.version_dir(agent_id, version);
        let rollback = self.rollback_dir(agent_id, version);

        self.recover_rollback(agent_id, version)?;
        let (backup, pending) = if destination.exists() {
            std::fs::rename(&destination, &rollback)
                .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
            (Some(rollback), None)
        } else {
            // Marker before the rename: a crash between the two leaves only
            // the marker, which recovery deletes.
            let marker = self.pending_marker(agent_id, version);
            if let Err(error) = std::fs::write(&marker, b"") {
                let _ = std::fs::remove_dir_all(staging);
                return Err(AdapterInstallError::Io(error.to_string()));
            }
            (None, Some(marker))
        };
        if let Err(error) = std::fs::rename(staging, &destination) {
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, &destination);
            }
            if let Some(pending) = &pending {
                let _ = std::fs::remove_file(pending);
            }
            let _ = std::fs::remove_dir_all(staging);
            return Err(AdapterInstallError::Io(error.to_string()));
        }
        Ok(PublishedAdapter {
            destination,
            backup,
            pending,
        })
    }

    /// Verification succeeded: the replacement is now authoritative.
    pub fn commit_publish(&self, published: PublishedAdapter) -> Result<(), AdapterInstallError> {
        if let Some(backup) = published.backup {
            // Rename is the atomic commit point. Recovery only recognizes the
            // rollback name, so a crash while deleting the discard keeps the
            // verified candidate and merely leaves cleanup debris.
            let discard = backup.with_file_name(
                backup
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(".rollback-adapter")
                    .replacen(".rollback-", ".discard-", 1),
            );
            remove_tree_if_exists(&discard)?;
            std::fs::rename(&backup, &discard)
                .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
            let _ = remove_tree_if_exists(&discard);
        }
        if let Some(pending) = published.pending {
            // Removing the marker is the commit point for a new version.
            std::fs::remove_file(&pending)
                .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
        }
        Ok(())
    }

    /// Verification failed or was cancelled: remove the candidate and put the
    /// previously verified same-pin tree back exactly where launch expects it.
    pub fn rollback_publish(&self, published: PublishedAdapter) -> Result<(), AdapterInstallError> {
        remove_tree_if_exists(&published.destination)?;
        if let Some(backup) = published.backup {
            std::fs::rename(&backup, &published.destination)
                .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
        }
        if let Some(pending) = published.pending {
            let _ = std::fs::remove_file(pending);
        }
        Ok(())
    }

    /// Remove every installed version of one agent's adapter — the user's
    /// *Remove*. Leaves the agent's `home` sibling alone; that is a separate
    /// decision the caller makes.
    pub fn remove_all(&self, agent_id: &str) -> Result<(), AdapterInstallError> {
        match std::fs::remove_dir_all(self.adapter_root(agent_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AdapterInstallError::Io(error.to_string())),
        }
    }

    /// Keep only `keep`, dropping the versions it replaces. npm trees run to
    /// ~100 MB for engines that carry a native binary; an upgrade must not
    /// stack them.
    ///
    /// Runs after verification, never as part of publishing: an older version
    /// on disk is the standing consent that lets a pin bump reinstall without
    /// asking. Pruning at publish time and then rolling back a failed handshake
    /// would leave no version at all, silently demoting the user to a manual
    /// *Get* for a failure that should simply be retried.
    ///
    /// `in_use` guards versions a live adapter was launched from: the adapter
    /// loads modules and spawns its CLI out of that tree for as long as it
    /// runs. Those wait for a later pass — at the latest the next launch.
    pub fn prune_superseded(&self, agent_id: &str, keep: &str, in_use: impl Fn(&Path) -> bool) {
        let Ok(entries) = std::fs::read_dir(self.adapter_root(agent_id)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != keep && !name.starts_with('.') && !in_use(&entry.path()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Orders version strings as semver does, which is all a "newest older
/// install" choice needs: the release numerically (`2.10.0` after `2.9.1`,
/// `v24` after `v9`), then a pre-release before its release (`1.0.0-rc.1`
/// before `1.0.0`), pre-release fields compared numerically where both are
/// numbers. A leading `v` and build metadata (`+…`) are ignored.
pub(crate) fn version_order_key(version: &str) -> VersionOrderKey {
    let version = version.trim_start_matches('v');
    let version = version
        .split_once('+')
        .map_or(version, |(version, _)| version);
    let (release, pre_release) = version
        .split_once('-')
        .map_or((version, None), |(release, pre)| (release, Some(pre)));
    VersionOrderKey {
        release: release
            .split('.')
            .map_while(|part| part.parse::<u64>().ok())
            .collect(),
        is_release: pre_release.is_none(),
        pre_release: pre_release
            .map(|pre| {
                pre.split('.')
                    .map(|part| match part.parse::<u64>() {
                        Ok(number) => PreReleaseField::Number(number),
                        Err(_) => PreReleaseField::Text(part.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// See [`version_order_key`]. Field order is comparison order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VersionOrderKey {
    release: Vec<u64>,
    is_release: bool,
    pre_release: Vec<PreReleaseField>,
}

/// Numbers sort before text, as in semver.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PreReleaseField {
    Number(u64),
    Text(String),
}

fn remove_tree_if_exists(path: &Path) -> Result<(), AdapterInstallError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AdapterInstallError::Io(error.to_string())),
    }
}

/// Run `npm install` into a staging directory and describe what it wrote.
///
/// The manifest is written into staging *last*, after the package has been
/// inspected, so a staging directory without one is always safe to delete.
/// Scripts are allowed deliberately — parity with the `npx -y` trust level
/// this replaces, and some transitive dependencies need their install hooks.
pub(crate) async fn npm_install_into(
    staging: &Path,
    package: &AdapterPackage,
    npm: &Path,
    bin_dir: Option<&Path>,
    node_version: Option<String>,
    cancel: &CancellationToken,
) -> Result<AdapterManifest, AdapterInstallError> {
    std::fs::create_dir_all(staging).map_err(|error| AdapterInstallError::Io(error.to_string()))?;

    let output = run_npm_install(staging, package, npm, bin_dir, cancel).await?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AdapterInstallError::Npm(if detail.is_empty() {
            format!("npm exited with {}", output.status)
        } else {
            detail
        }));
    }

    let entrypoint = package_entrypoint(staging, &package.name)?;
    let (resolved, integrity) = lockfile_provenance(staging, &package.name);

    let manifest = AdapterManifest {
        name: package.name.clone(),
        version: package.version.clone(),
        resolved,
        integrity,
        entrypoint,
        node_version,
    };
    let serialized = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| AdapterInstallError::Manifest(error.to_string()))?;
    std::fs::write(staging.join("manifest.json"), serialized)
        .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    Ok(manifest)
}

/// Download a binary adapter into a staging directory and describe it.
///
/// The download is checked against the pinned digest before it is renamed
/// into place, so nothing unverified is ever unpacked. As with npm, the
/// manifest is written last.
pub(crate) async fn binary_install_into(
    staging: &Path,
    package: &AdapterPackage,
    binary: &BinaryAdapter,
    http: &HttpClient,
    cancel: &CancellationToken,
) -> Result<AdapterManifest, AdapterInstallError> {
    let artifact = binary.for_host().ok_or_else(|| {
        AdapterInstallError::Download(format!(
            "{} has no build for {}",
            package.name,
            std::env::consts::ARCH
        ))
    })?;
    std::fs::create_dir_all(staging).map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    let download = staging.join(".download");
    http.download(
        DownloadRequest::new(&artifact.url, &download).expecting_sha256(&artifact.sha256),
        cancel,
        |_| {},
    )
    .await
    .map_err(|error| match error {
        DownloadError::Cancelled => AdapterInstallError::Cancelled,
        other => AdapterInstallError::Download(other.to_string()),
    })?;
    if cancel.is_cancelled() {
        return Err(AdapterInstallError::Cancelled);
    }

    let entrypoint = PathBuf::from("bin").join(&binary.command);
    let executable = staging.join(&entrypoint);
    std::fs::create_dir_all(staging.join("bin"))
        .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    match &artifact.archive {
        BinaryArchive::Raw => std::fs::rename(&download, &executable)
            .map_err(|error| AdapterInstallError::Io(error.to_string()))?,
        BinaryArchive::Tgz { member } => {
            let unpacked = staging.join(".unpacked");
            extract_member(&download, member, &unpacked).await?;
            std::fs::rename(unpacked.join(member), &executable).map_err(|error| {
                AdapterInstallError::Manifest(format!("{member} is not in the archive: {error}"))
            })?;
            remove_tree_if_exists(&unpacked)?;
            let _ = std::fs::remove_file(&download);
        }
    }
    set_executable(&executable)?;

    let manifest = AdapterManifest {
        name: package.name.clone(),
        version: package.version.clone(),
        resolved: Some(artifact.url.clone()),
        integrity: Some(format!("sha256:{}", artifact.sha256)),
        entrypoint,
        node_version: None,
    };
    let serialized = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| AdapterInstallError::Manifest(error.to_string()))?;
    std::fs::write(staging.join("manifest.json"), serialized)
        .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    Ok(manifest)
}

/// Unpack one member of a gzipped tarball into `destination`.
async fn extract_member(
    tarball: &Path,
    member: &str,
    destination: &Path,
) -> Result<(), AdapterInstallError> {
    std::fs::create_dir_all(destination)
        .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    let tarball = tarball.to_string_lossy().into_owned();
    let destination = destination.to_string_lossy().into_owned();
    let member = member.to_string();
    let output = tokio::task::spawn_blocking(move || {
        super::registry::command_output_args_with_timeout(
            TAR,
            &["-xzf", &tarball, "-C", &destination, &member],
            EXTRACT_TIMEOUT,
        )
    })
    .await
    .map_err(|error| AdapterInstallError::Io(error.to_string()))?
    .map_err(|error| AdapterInstallError::Io(error.to_string()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AdapterInstallError::Manifest(if detail.is_empty() {
            format!("tar exited with {}", output.status)
        } else {
            detail
        }));
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), AdapterInstallError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| AdapterInstallError::Io(error.to_string()))
}

/// The npm invocation, cancellable and bounded.
///
/// Same shape as the registry's probe runner: spawn in its own process group,
/// poll, and on timeout or cancel kill the whole group — npm spawns script
/// children that would otherwise outlive it holding the pipes.
async fn run_npm_install(
    staging: &Path,
    package: &AdapterPackage,
    npm: &Path,
    bin_dir: Option<&Path>,
    cancel: &CancellationToken,
) -> Result<std::process::Output, AdapterInstallError> {
    let npm = npm.to_path_buf();
    let staging = staging.to_path_buf();
    let spec = package.spec();
    let bin_dir = bin_dir.map(Path::to_path_buf);
    let cancel = cancel.clone();

    tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(&npm);
        command.args([
            "install",
            "--prefix",
            &staging.to_string_lossy(),
            "--save-exact",
            "--no-audit",
            "--no-fund",
            "--no-progress",
            "--loglevel=error",
            &spec,
        ]);
        // npm is itself a `#!/usr/bin/env node` script, and package install
        // hooks spawn `node` — both re-resolve through PATH.
        if let Some(dir) = &bin_dir {
            let inherited = std::env::var("PATH").unwrap_or_default();
            let path = if inherited.is_empty() {
                dir.to_string_lossy().into_owned()
            } else {
                format!("{}:{inherited}", dir.to_string_lossy())
            };
            command.env("PATH", path);
        }
        super::registry::command_output_until(command, NPM_INSTALL_TIMEOUT, || {
            cancel.is_cancelled()
        })
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut => {
                AdapterInstallError::Npm("npm install timed out".to_string())
            }
            std::io::ErrorKind::Interrupted => AdapterInstallError::Cancelled,
            _ => AdapterInstallError::Npm(error.to_string()),
        })
    })
    .await
    .map_err(|error| AdapterInstallError::Npm(error.to_string()))?
}

/// The installed package's `bin` target, relative to the staging root.
///
/// Run through `node` directly rather than the `.bin/` shim so nothing
/// re-resolves through `env` at launch.
fn package_entrypoint(staging: &Path, name: &str) -> Result<PathBuf, AdapterInstallError> {
    let package_dir = Path::new("node_modules").join(name);
    let manifest_path = staging.join(&package_dir).join("package.json");
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        AdapterInstallError::Manifest(format!("no package.json for {name}: {error}"))
    })?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        AdapterInstallError::Manifest(format!("unreadable package.json: {error}"))
    })?;

    let target = match parsed.get("bin") {
        Some(serde_json::Value::String(path)) => path.clone(),
        Some(serde_json::Value::Object(entries)) => {
            // The conventional key is the package's own binary name; accept a
            // single entry of any name, refuse to guess among several.
            let binary = name.rsplit('/').next().unwrap_or(name);
            match entries.get(binary).or_else(|| {
                (entries.len() == 1).then(|| entries.values().next().expect("one entry"))
            }) {
                Some(serde_json::Value::String(path)) => path.clone(),
                _ => {
                    return Err(AdapterInstallError::Manifest(format!(
                        "{name} declares {} bin entries and none is named {binary}",
                        entries.len()
                    )))
                }
            }
        }
        _ => {
            return Err(AdapterInstallError::Manifest(format!(
                "{name} declares no bin entry"
            )))
        }
    };

    let entrypoint = package_dir.join(target);
    if !staging.join(&entrypoint).is_file() {
        return Err(AdapterInstallError::Manifest(format!(
            "{name}'s bin target {} does not exist",
            entrypoint.display()
        )));
    }
    Ok(entrypoint)
}

/// npm's own record of what it fetched, from the v7+ lockfile. Absent entries
/// degrade to `None` — provenance is worth recording, not worth failing over.
fn lockfile_provenance(staging: &Path, name: &str) -> (Option<String>, Option<String>) {
    let Ok(bytes) = std::fs::read(staging.join("package-lock.json")) else {
        return (None, None);
    };
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return (None, None);
    };
    let entry = parsed
        .get("packages")
        .and_then(|packages| packages.get(format!("node_modules/{name}")));
    let field = |key: &str| {
        entry
            .and_then(|entry| entry.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    (field("resolved"), field("integrity"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn package() -> AdapterPackage {
        AdapterPackage {
            name: "@scope/test-acp".to_string(),
            version: "1.2.3".to_string(),
            source: crate::agent::AdapterSource::Npm,
        }
    }

    fn write_manifest(installs: &ManagedAdapterInstalls, agent: &str, manifest: &AdapterManifest) {
        let dir = installs.version_dir(agent, &manifest.version);
        std::fs::create_dir_all(&dir).expect("dirs");
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(manifest).expect("serialize"),
        )
        .expect("write");
    }

    fn manifest(entrypoint: &str) -> AdapterManifest {
        AdapterManifest {
            name: "@scope/test-acp".to_string(),
            version: "1.2.3".to_string(),
            resolved: None,
            integrity: None,
            entrypoint: PathBuf::from(entrypoint),
            node_version: None,
        }
    }

    #[test]
    fn an_intact_install_resolves_to_its_entrypoint() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        write_manifest(
            &installs,
            "claude",
            &manifest("node_modules/@scope/test-acp/acp.js"),
        );
        let entrypoint = installs
            .version_dir("claude", "1.2.3")
            .join("node_modules/@scope/test-acp/acp.js");
        std::fs::create_dir_all(entrypoint.parent().expect("parent")).expect("dirs");
        std::fs::write(&entrypoint, "// adapter").expect("write");

        assert_eq!(
            installs.installed_entrypoint("claude", &package()),
            Some(entrypoint)
        );
    }

    #[test]
    fn a_missing_entrypoint_or_mismatched_pin_is_not_installed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());

        // Manifest without its entrypoint file: not installed.
        write_manifest(
            &installs,
            "claude",
            &manifest("node_modules/@scope/test-acp/acp.js"),
        );
        assert_eq!(installs.installed_entrypoint("claude", &package()), None);

        // A different package under the same version directory: not this pin.
        let mut foreign = manifest("node_modules/@scope/test-acp/acp.js");
        foreign.name = "@scope/other".to_string();
        write_manifest(&installs, "codex", &foreign);
        assert_eq!(installs.installed_entrypoint("codex", &package()), None);
    }

    #[test]
    fn a_changed_source_at_the_same_version_is_not_installed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let binary = |url: &str, sha256: &str| AdapterPackage {
            source: AdapterSource::Binary(BinaryAdapter {
                command: "test-acp".to_string(),
                artifacts: vec![crate::agent::BinaryArtifact {
                    arch: std::env::consts::ARCH,
                    url: url.to_string(),
                    sha256: sha256.to_string(),
                    archive: BinaryArchive::Raw,
                }],
            }),
            ..package()
        };
        let pinned = binary("https://example.test/a", "aa");

        // An npm install of the same name and version is not the binary.
        write_manifest(
            &installs,
            "agent",
            &manifest("node_modules/@scope/test-acp/acp.js"),
        );
        let script = installs
            .version_dir("agent", "1.2.3")
            .join("node_modules/@scope/test-acp/acp.js");
        std::fs::create_dir_all(script.parent().expect("parent")).expect("dirs");
        std::fs::write(&script, "// adapter").expect("write");
        assert_eq!(installs.installed_entrypoint("agent", &pinned), None);
        assert_eq!(
            installs.installed_entrypoint("agent", &package()),
            Some(script)
        );

        // The pinned binary is installed; a new URL or digest is not.
        let mut installed = manifest("bin/test-acp");
        installed.resolved = Some("https://example.test/a".to_string());
        installed.integrity = Some("sha256:aa".to_string());
        write_manifest(&installs, "agent", &installed);
        let executable = installs.version_dir("agent", "1.2.3").join("bin/test-acp");
        std::fs::create_dir_all(executable.parent().expect("parent")).expect("dirs");
        std::fs::write(&executable, "binary").expect("write");
        assert_eq!(
            installs.installed_entrypoint("agent", &pinned),
            Some(executable)
        );
        assert_eq!(installs.installed_entrypoint("agent", &package()), None);
        for changed in [
            binary("https://example.test/b", "aa"),
            binary("https://example.test/a", "bb"),
        ] {
            assert_eq!(installs.installed_entrypoint("agent", &changed), None);
        }
    }

    fn write_entrypoint(
        installs: &ManagedAdapterInstalls,
        agent: &str,
        manifest: &AdapterManifest,
    ) {
        write_manifest(installs, agent, manifest);
        let script = installs
            .version_dir(agent, &manifest.version)
            .join(&manifest.entrypoint);
        std::fs::create_dir_all(script.parent().expect("parent")).expect("dirs");
        std::fs::write(&script, "// adapter").expect("write");
    }

    #[test]
    fn an_older_install_stands_in_for_the_pin_until_it_lands() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        assert_eq!(installs.superseded_install("claude", &package()), None);
        assert_eq!(installs.launchable("claude", &package()), None);

        let entry = "node_modules/@scope/test-acp/acp.js";
        for version in ["1.0.9", "1.0.10"] {
            let mut old = manifest(entry);
            old.version = version.to_string();
            write_entrypoint(&installs, "claude", &old);
        }
        // A newer-looking directory whose entrypoint is gone cannot launch.
        let mut broken = manifest(entry);
        broken.version = "1.1.0".to_string();
        write_manifest(&installs, "claude", &broken);
        // A different package in the store is never a stand-in.
        let mut foreign = manifest(entry);
        foreign.name = "@scope/other".to_string();
        foreign.version = "1.2.0".to_string();
        write_entrypoint(&installs, "claude", &foreign);

        let standing = installs
            .launchable("claude", &package())
            .expect("an older install launches");
        assert_eq!(standing.version, "1.0.10", "newest by version, not by name");
        assert_eq!(
            standing.entrypoint,
            installs.version_dir("claude", "1.0.10").join(entry)
        );

        // A manifest alone is consent, even without a launchable entrypoint.
        assert!(installs.installed_other_version("claude", &package()));

        // Once the pin is installed it wins, and is not its own stand-in.
        write_entrypoint(&installs, "claude", &manifest(entry));
        assert_eq!(
            installs
                .launchable("claude", &package())
                .map(|install| install.version),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            installs
                .superseded_install("claude", &package())
                .map(|install| install.version),
            Some("1.0.10".to_string())
        );
    }

    /// After an app rollback the store may hold only a version newer than the
    /// pin. It may need a newer ACP contract, so nothing launches until the
    /// pin itself is restored — but it still consents to that reinstall.
    #[test]
    fn a_newer_install_never_stands_in_for_the_pin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let entry = "node_modules/@scope/test-acp/acp.js";
        for version in ["1.2.10", "2.0.0"] {
            let mut newer = manifest(entry);
            newer.version = version.to_string();
            write_entrypoint(&installs, "claude", &newer);
        }
        assert_eq!(installs.superseded_install("claude", &package()), None);
        assert_eq!(installs.launchable("claude", &package()), None);
        assert!(installs.installed_other_version("claude", &package()));

        // A pre-release of the pin's own version is older than it.
        let mut candidate = manifest(entry);
        candidate.version = "1.2.3-rc.1".to_string();
        write_entrypoint(&installs, "claude", &candidate);
        assert_eq!(
            installs
                .launchable("claude", &package())
                .map(|install| install.version),
            Some("1.2.3-rc.1".to_string())
        );
    }

    #[test]
    fn an_npm_install_cannot_stand_in_for_a_binary_pin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let mut old = manifest("node_modules/@scope/test-acp/acp.js");
        old.version = "1.0.0".to_string();
        write_entrypoint(&installs, "agent", &old);
        let binary = AdapterPackage {
            source: AdapterSource::Binary(BinaryAdapter {
                command: "test-acp".to_string(),
                artifacts: Vec::new(),
            }),
            ..package()
        };
        assert_eq!(installs.launchable("agent", &binary), None);

        let mut old_binary = manifest("bin/test-acp");
        old_binary.version = "1.1.0".to_string();
        write_entrypoint(&installs, "agent", &old_binary);
        assert_eq!(
            installs
                .launchable("agent", &binary)
                .map(|install| install.version),
            Some("1.1.0".to_string()),
            "an older binary stands in whatever its URL and digest were"
        );
    }

    #[test]
    fn version_order_is_numeric() {
        assert!(version_order_key("2.10.0") > version_order_key("2.9.1"));
        assert!(version_order_key("v24.22.1") > version_order_key("v9.0.0"));
        assert!(version_order_key("0.95.0-beta.1") < version_order_key("0.95.0"));
        assert!(version_order_key("0.95.0-beta.10") > version_order_key("0.95.0-beta.9"));
        assert!(version_order_key("0.95.0-beta.1") > version_order_key("0.94.9"));
        assert_eq!(
            version_order_key("1.2.3+build.5"),
            version_order_key("v1.2.3")
        );
    }

    /// Publishing moves staging into place and leaves the version it
    /// supersedes alone — that one is the standing consent a failed handshake
    /// still needs. Pruning is a separate, post-verification step.
    #[test]
    fn publish_replaces_staging_and_keeps_the_old_version() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let mut old = manifest("acp.js");
        old.version = "1.0.0".to_string();
        write_manifest(&installs, "claude", &old);

        let staging = installs.staging_dir("claude");
        std::fs::create_dir_all(&staging).expect("dirs");
        std::fs::write(staging.join("marker"), "new").expect("write");

        let published = installs
            .publish(&staging, "claude", "1.2.3")
            .expect("publish");
        assert!(published.destination.join("marker").is_file());
        assert!(!staging.exists(), "staging must move, not copy");
        assert!(
            installs.version_dir("claude", "1.0.0").exists(),
            "publishing must not prune: a failed handshake still needs the old \
             version as standing consent"
        );

        installs.commit_publish(published).expect("commit publish");
        let busy = installs.version_dir("claude", "1.0.0");
        installs.prune_superseded("claude", "1.2.3", |dir| dir == busy);
        assert!(
            installs.version_dir("claude", "1.0.0").exists(),
            "a version a live adapter runs from was pruned"
        );
        installs.prune_superseded("claude", "1.2.3", |_| false);
        assert!(
            !installs.version_dir("claude", "1.0.0").exists(),
            "the superseded version survived an explicit prune"
        );
        assert!(
            installs.version_dir("claude", "1.2.3").exists(),
            "pruning removed the version it was told to keep"
        );
    }

    #[test]
    fn failed_same_pin_publish_restores_the_verified_tree() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        write_manifest(&installs, "claude", &manifest("old.js"));
        let destination = installs.version_dir("claude", "1.2.3");
        std::fs::write(destination.join("old.js"), "verified").expect("old entrypoint");

        let staging = installs.staging_dir("claude");
        std::fs::create_dir_all(&staging).expect("staging");
        std::fs::write(staging.join("candidate.js"), "unverified").expect("candidate");
        let published = installs
            .publish(&staging, "claude", "1.2.3")
            .expect("publish candidate");
        assert!(destination.join("candidate.js").is_file());

        installs
            .rollback_publish(published)
            .expect("restore previous version");
        assert!(destination.join("old.js").is_file());
        assert!(!destination.join("candidate.js").exists());
    }

    #[test]
    fn restart_recovers_an_uncommitted_same_pin_publish() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        write_manifest(&installs, "claude", &manifest("old.js"));
        let destination = installs.version_dir("claude", "1.2.3");
        std::fs::write(destination.join("old.js"), "verified").expect("old entrypoint");
        let staging = installs.staging_dir("claude");
        std::fs::create_dir_all(&staging).expect("staging");
        std::fs::write(staging.join("candidate.js"), "unverified").expect("candidate");
        let _uncommitted = installs
            .publish(&staging, "claude", "1.2.3")
            .expect("publish candidate");

        let recovered = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let destination = recovered.version_dir("claude", "1.2.3");
        assert!(destination.join("old.js").is_file());
        assert!(!destination.join("candidate.js").exists());
        assert!(!recovered.rollback_dir("claude", "1.2.3").exists());
    }

    /// A pin bump publishes into a fresh directory, so there is no rollback
    /// backup to mark it uncommitted. A crash during verification must still
    /// leave the older verified version as what launches after restart.
    #[test]
    fn interrupted_pin_bump_is_not_installed_after_reopen() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let entry = "node_modules/@scope/test-acp/acp.js";
        let mut old = manifest(entry);
        old.version = "1.0.0".to_string();
        write_entrypoint(&installs, "claude", &old);

        let staging = installs.staging_dir("claude");
        let script = staging.join(entry);
        std::fs::create_dir_all(script.parent().expect("parent")).expect("staging");
        std::fs::write(&script, "// unverified").expect("candidate");
        std::fs::write(
            staging.join("manifest.json"),
            serde_json::to_vec(&manifest(entry)).expect("serialize"),
        )
        .expect("candidate manifest");
        let _uncommitted = installs
            .publish(&staging, "claude", "1.2.3")
            .expect("publish candidate");

        let recovered = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        assert_eq!(recovered.installed_entrypoint("claude", &package()), None);
        assert!(!recovered.version_dir("claude", "1.2.3").exists());
        assert_eq!(
            recovered
                .launchable("claude", &package())
                .map(|install| install.version),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn a_committed_pin_bump_survives_reopen() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let entry = "node_modules/@scope/test-acp/acp.js";
        let staging = installs.staging_dir("claude");
        let script = staging.join(entry);
        std::fs::create_dir_all(script.parent().expect("parent")).expect("staging");
        std::fs::write(&script, "// verified").expect("candidate");
        std::fs::write(
            staging.join("manifest.json"),
            serde_json::to_vec(&manifest(entry)).expect("serialize"),
        )
        .expect("candidate manifest");
        let published = installs
            .publish(&staging, "claude", "1.2.3")
            .expect("publish candidate");
        installs.commit_publish(published).expect("commit");

        let recovered = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        assert!(recovered.installed_entrypoint("claude", &package()).is_some());
        let leftovers: Vec<_> = std::fs::read_dir(recovered.adapter_root("claude"))
            .expect("adapter root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "commit left markers: {leftovers:?}");
    }

    #[test]
    fn stale_staging_directories_are_cleared() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        let stale = installs.adapter_root("claude").join(".staging-999");
        std::fs::create_dir_all(&stale).expect("dirs");
        write_manifest(&installs, "claude", &manifest("acp.js"));

        installs.remove_stale_staging("claude");

        assert!(!stale.exists());
        assert!(installs.version_dir("claude", "1.2.3").exists());
    }

    #[test]
    fn removing_all_reports_real_io_failures_but_accepts_an_absent_tree() {
        let directory = tempfile::tempdir().expect("tempdir");
        let installs = ManagedAdapterInstalls::at(directory.path().to_path_buf());
        installs.remove_all("missing").expect("absence is removed");

        let adapter = installs.adapter_root("claude");
        std::fs::create_dir_all(adapter.parent().expect("agent dir")).expect("agent dir");
        std::fs::write(&adapter, "not a directory").expect("blocking file");
        let error = installs
            .remove_all("claude")
            .expect_err("file is not a tree");
        assert!(matches!(error, AdapterInstallError::Io(_)));
    }

    fn stage_package(staging: &Path, name: &str, bin_json: &str) {
        let dir = staging.join("node_modules").join(name);
        std::fs::create_dir_all(&dir).expect("dirs");
        std::fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"{name}","version":"1.2.3","bin":{bin_json}}}"#),
        )
        .expect("write");
    }

    #[test]
    fn bin_entries_resolve_by_string_own_name_or_single_entry() {
        let directory = tempfile::tempdir().expect("tempdir");
        let staging = directory.path();

        stage_package(staging, "string-bin", r#""cli.js""#);
        std::fs::write(staging.join("node_modules/string-bin/cli.js"), "").expect("write");
        assert_eq!(
            package_entrypoint(staging, "string-bin").expect("string bin"),
            PathBuf::from("node_modules/string-bin/cli.js")
        );

        stage_package(
            staging,
            "@scope/named",
            r#"{"named":"dist/main.js","other":"x.js"}"#,
        );
        std::fs::create_dir_all(staging.join("node_modules/@scope/named/dist")).expect("dirs");
        std::fs::write(staging.join("node_modules/@scope/named/dist/main.js"), "").expect("write");
        assert_eq!(
            package_entrypoint(staging, "@scope/named").expect("own-name bin"),
            PathBuf::from("node_modules/@scope/named/dist/main.js")
        );

        stage_package(staging, "single", r#"{"anything":"run.js"}"#);
        std::fs::write(staging.join("node_modules/single/run.js"), "").expect("write");
        assert_eq!(
            package_entrypoint(staging, "single").expect("single-entry bin"),
            PathBuf::from("node_modules/single/run.js")
        );

        stage_package(staging, "ambiguous", r#"{"a":"a.js","b":"b.js"}"#);
        assert!(matches!(
            package_entrypoint(staging, "ambiguous"),
            Err(AdapterInstallError::Manifest(_))
        ));
    }

    #[test]
    fn lockfile_provenance_reads_npm_v7_entries() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("package-lock.json"),
            r#"{"packages":{"node_modules/@scope/test-acp":{"resolved":"https://registry.npmjs.org/x.tgz","integrity":"sha512-abc"}}}"#,
        )
        .expect("write");
        assert_eq!(
            lockfile_provenance(directory.path(), "@scope/test-acp"),
            (
                Some("https://registry.npmjs.org/x.tgz".to_string()),
                Some("sha512-abc".to_string())
            )
        );
        assert_eq!(
            lockfile_provenance(directory.path(), "absent"),
            (None, None)
        );
    }

    /// End to end against a fake npm that scaffolds what real npm writes.
    #[tokio::test]
    async fn npm_install_into_records_a_manifest() {
        let directory = tempfile::tempdir().expect("tempdir");
        let staging = directory.path().join("staging");
        let npm = directory.path().join("npm");
        std::fs::write(
            &npm,
            concat!(
                "#!/bin/sh\n",
                "prefix=$3\n",
                "dir=\"$prefix/node_modules/@scope/test-acp\"\n",
                "mkdir -p \"$dir\"\n",
                "printf '%s' '{\"name\":\"@scope/test-acp\",\"version\":\"1.2.3\",\"bin\":\"acp.js\"}' > \"$dir/package.json\"\n",
                "printf '%s' '// adapter' > \"$dir/acp.js\"\n",
                "printf '%s' '{\"packages\":{\"node_modules/@scope/test-acp\":{\"resolved\":\"https://registry.npmjs.org/t.tgz\",\"integrity\":\"sha512-xyz\"}}}' > \"$prefix/package-lock.json\"\n",
            ),
        )
        .expect("write npm");
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let manifest = npm_install_into(
            &staging,
            &package(),
            &npm,
            None,
            Some("v24.22.1".to_string()),
            &CancellationToken::new(),
        )
        .await
        .expect("install");

        assert_eq!(
            manifest.entrypoint,
            PathBuf::from("node_modules/@scope/test-acp/acp.js")
        );
        assert_eq!(
            manifest.resolved.as_deref(),
            Some("https://registry.npmjs.org/t.tgz")
        );
        assert_eq!(manifest.integrity.as_deref(), Some("sha512-xyz"));
        assert!(staging.join("manifest.json").is_file());
    }

    #[tokio::test]
    async fn a_failing_npm_reports_its_stderr() {
        let directory = tempfile::tempdir().expect("tempdir");
        let npm = directory.path().join("npm");
        std::fs::write(&npm, "#!/bin/sh\necho 'E404 not found' >&2\nexit 1\n").expect("write");
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let error = npm_install_into(
            &directory.path().join("staging"),
            &package(),
            &npm,
            None,
            None,
            &CancellationToken::new(),
        )
        .await
        .expect_err("must fail");
        assert_eq!(
            error,
            AdapterInstallError::Npm("E404 not found".to_string())
        );
    }

    #[tokio::test]
    async fn cancelling_kills_npm_and_reports_cancelled() {
        let directory = tempfile::tempdir().expect("tempdir");
        let npm = directory.path().join("npm");
        std::fs::write(&npm, "#!/bin/sh\nsleep 60\n").expect("write");
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let cancel = CancellationToken::new();
        let staging = directory.path().join("staging");
        let package = package();
        let install = npm_install_into(&staging, &package, &npm, None, None, &cancel);
        tokio::pin!(install);

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(150)) => cancel.cancel(),
            _ = &mut install => panic!("npm finished before the cancel"),
        }
        assert_eq!(install.await, Err(AdapterInstallError::Cancelled));
    }
}
