//! Subscription usage windows: the five-hour and weekly limits the vendor
//! apps show (`docs/architecture/backend/acp.md` § Token And Plan Usage).
//!
//! Two sources feed one last-known snapshot per agent. Claude reports its
//! windows on `usage_update` during a turn (`_claude/rateLimit`). Between
//! turns a short-lived probe asks the CLI the adapter bundles: Claude Code's
//! `get_usage` control request, Codex's app-server `account/rateLimits/read`.
//! Each CLI reads its own login from the agent's home; core never handles a
//! credential.

use super::adapter_install::InstalledAdapter;
use super::{AccountKind, AgentId, AgentRegistry, HarnessAuthState};
use ottam_protocol::{
    AgentID, AgentPlanUsage, AgentPlanUsageChangedParams, AgentPlanUsageSource,
    AgentPlanUsageWindow, AgentPlanUsageWindowKind,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A snapshot younger than this answers a read without a probe.
const STALE_AFTER: Duration = Duration::from_secs(5 * 60);
const POLL_EVERY: Duration = Duration::from_secs(15 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a probe gets to exit after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);
const FIRST_RETRY: Duration = Duration::from_secs(60);
const MAX_RETRY: Duration = Duration::from_secs(60 * 60);

/// Each snapshot change, including a cleared one.
pub(crate) type PlanUsageSink = Arc<dyn Fn(AgentPlanUsageChangedParams) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Claude,
    Codex,
}

impl Provider {
    fn of(harness_id: &str) -> Option<Self> {
        match harness_id {
            super::CLAUDE_AGENT_ID => Some(Self::Claude),
            super::registry::CODEX_AGENT_ID => Some(Self::Codex),
            _ => None,
        }
    }
}

/// What one read produced. No windows means the account has no plan limits
/// (an API key, a third-party provider).
#[derive(Debug, Default, PartialEq)]
struct Reading {
    windows: Vec<AgentPlanUsageWindow>,
    limit_status: Option<String>,
}

#[derive(Default)]
struct Entry {
    usage: Option<AgentPlanUsage>,
    /// Bumped by every live update, so a probe that started before one
    /// cannot replace the newer reading when it lands.
    generation: u64,
    probing: bool,
    /// The last probe that answered; a failure leaves it so backoff alone
    /// decides the retry.
    probed_at: Option<Instant>,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Entry {
    /// Never two probes at once; otherwise a forced refresh always runs,
    /// and a routine one waits out freshness and failure backoff.
    fn admits_probe(&self, now: Instant, force: bool) -> bool {
        if self.probing {
            return false;
        }
        let fresh = self
            .probed_at
            .is_some_and(|at| now.duration_since(at) < STALE_AFTER);
        let waiting = self.retry_at.is_some_and(|at| now < at);
        force || !(fresh || waiting)
    }
}

pub(crate) struct PlanUsageService {
    registry: Arc<AgentRegistry>,
    sink: PlanUsageSink,
    entries: StdMutex<HashMap<String, Entry>>,
    polling: AtomicBool,
    this: Weak<Self>,
}

impl PlanUsageService {
    pub(crate) fn new(registry: Arc<AgentRegistry>, sink: PlanUsageSink) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            registry,
            sink,
            entries: StdMutex::new(HashMap::new()),
            polling: AtomicBool::new(false),
            this: this.clone(),
        })
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Plan usage applies to subscription accounts of agents with a source.
    fn provider(&self, agent: &AgentId) -> Option<Provider> {
        let definition = self.registry.definition(agent).ok()?;
        if !matches!(
            definition.presentation.account_kind,
            AccountKind::Subscription
        ) {
            return None;
        }
        Provider::of(definition.harness_id())
    }

    /// The last-known snapshot. A missing or stale one, or any when `force`
    /// asks, starts a probe whose result arrives through the sink; the first
    /// read also starts polling.
    pub(crate) fn snapshot(&self, agent: &AgentId, force: bool) -> Option<AgentPlanUsage> {
        self.provider(agent)?;
        self.start_polling();
        self.refresh(agent, force);
        self.entries()
            .get(&agent.0)
            .and_then(|entry| entry.usage.clone())
    }

    /// Fold a `usage_update`'s `_claude/rateLimit` into the snapshot. Windows
    /// the update omits keep their last value.
    pub(crate) fn record_session_update(&self, agent: &AgentId, payload: &Value) {
        let Some(reading) = claude_session_reading(payload) else {
            return;
        };
        if self.provider(agent) != Some(Provider::Claude) {
            return;
        }
        // Announced under the lock, so clients see changes in store order.
        let mut entries = self.entries();
        let entry = entries.entry(agent.0.clone()).or_default();
        let windows = merge_windows(
            entry
                .usage
                .take()
                .map(|usage| usage.windows)
                .unwrap_or_default(),
            reading.windows,
        );
        let usage = AgentPlanUsage {
            agent_id: AgentID(agent.0.clone()),
            windows,
            limit_status: reading.limit_status,
            source: AgentPlanUsageSource::Session,
            observed_at: now_millis(),
            error: None,
        };
        entry.usage = Some(usage.clone());
        entry.generation += 1;
        self.announce(agent, Some(usage));
    }

    /// Called with the store locked: the sink only queues the frame.
    fn announce(&self, agent: &AgentId, usage: Option<AgentPlanUsage>) {
        (self.sink)(AgentPlanUsageChangedParams {
            agent_id: AgentID(agent.0.clone()),
            usage,
        });
    }

    fn start_polling(&self) {
        if self.polling.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.this.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(POLL_EVERY);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(service) = this.upgrade() else {
                    return;
                };
                for definition in service.registry.definitions() {
                    service.refresh(&definition.agent_id, false);
                }
            }
        });
    }

    /// Probe unless one is running, or, without `force`, the snapshot is
    /// fresh or a failure's backoff has not elapsed.
    fn refresh(&self, agent: &AgentId, force: bool) {
        let Some(provider) = self.provider(agent) else {
            return;
        };
        let generation = {
            let mut entries = self.entries();
            let entry = entries.entry(agent.0.clone()).or_default();
            if !entry.admits_probe(Instant::now(), force) {
                return;
            }
            entry.probing = true;
            entry.generation
        };
        let this = self.this.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            let Some(service) = this.upgrade() else {
                return;
            };
            let outcome = service.probe(&agent, provider).await;
            service.settle_probe(&agent, generation, outcome);
        });
    }

    async fn probe(&self, agent: &AgentId, provider: Provider) -> Option<Result<Reading, String>> {
        let status = self.registry.cached_status(agent).await.ok()?;
        if status.auth_state == HarnessAuthState::Missing {
            return Some(Ok(Reading::default()));
        }
        let definition = self.registry.definition(agent).ok()?;
        let adapter = self.registry.launchable_adapter(&definition)?;
        let binary = probe_binary(provider, &adapter)?;
        let unset = super::launch_unset_variables(&definition);
        Some(run_probe(provider, &binary, &unset, &definition.launch.environment).await)
    }

    fn settle_probe(
        &self,
        agent: &AgentId,
        generation: u64,
        outcome: Option<Result<Reading, String>>,
    ) {
        // Announced under the lock, so clients see changes in store order.
        let mut entries = self.entries();
        let entry = entries.entry(agent.0.clone()).or_default();
        entry.probing = false;
        let usage = match outcome {
            // Nothing to run: an npx-only adapter, an unreadable status.
            None => {
                entry.probed_at = Some(Instant::now());
                return;
            }
            Some(Ok(reading)) => {
                entry.probed_at = Some(Instant::now());
                entry.failures = 0;
                entry.retry_at = None;
                if entry.generation != generation {
                    // A turn reported while the probe ran: its account
                    // windows are newer and stay. Only the probe reads
                    // per-model windows, so its set replaces them whole, and
                    // a model it no longer reports drops out. A probe with no
                    // plan limits at all is outvoted by the newer turn.
                    let Some(usage) = entry.usage.as_mut() else {
                        return;
                    };
                    if reading.windows.is_empty() {
                        return;
                    }
                    let scoped: Vec<_> = reading
                        .windows
                        .into_iter()
                        .filter(|window| window.model.is_some())
                        .collect();
                    let mut account = std::mem::take(&mut usage.windows);
                    account.retain(|window| window.model.is_none());
                    usage.windows = merge_windows(account, scoped);
                    // The probe settled this snapshot; clients waiting on
                    // a refresh read that from the source.
                    usage.source = AgentPlanUsageSource::Probe;
                    Some(usage.clone())
                } else if reading.windows.is_empty() {
                    if entry.usage.take().is_none() {
                        return;
                    }
                    None
                } else {
                    let usage = AgentPlanUsage {
                        agent_id: AgentID(agent.0.clone()),
                        windows: reading.windows,
                        limit_status: reading.limit_status,
                        source: AgentPlanUsageSource::Probe,
                        observed_at: now_millis(),
                        error: None,
                    };
                    entry.usage = Some(usage.clone());
                    Some(usage)
                }
            }
            Some(Err(error)) => {
                tracing::warn!(agent = %agent.0, %error, "Plan usage probe failed");
                entry.failures = entry.failures.saturating_add(1);
                // 1, 2, 4 … minutes, capped at an hour.
                let backoff = FIRST_RETRY
                    .saturating_mul(1 << entry.failures.min(7).saturating_sub(1))
                    .min(MAX_RETRY);
                entry.retry_at = Some(Instant::now() + backoff);
                let Some(usage) = entry.usage.as_mut() else {
                    return;
                };
                usage.error = Some(error);
                Some(usage.clone())
            }
        };
        self.announce(agent, usage);
    }
}

/// Windows are the same limit when kind and model match; a display label is
/// not an identity (a model could be named `wk`). Codex's unusual windows
/// share `Other`, so their length label tells them apart.
fn same_window(left: &AgentPlanUsageWindow, right: &AgentPlanUsageWindow) -> bool {
    left.kind == right.kind
        && left.model == right.model
        && (left.kind != AgentPlanUsageWindowKind::Other || left.label == right.label)
}

/// `current` with each of `updates` replacing its match, in display order.
fn merge_windows(
    mut current: Vec<AgentPlanUsageWindow>,
    updates: Vec<AgentPlanUsageWindow>,
) -> Vec<AgentPlanUsageWindow> {
    for window in updates {
        current.retain(|existing| !same_window(existing, &window));
        current.push(window);
    }
    // Stable: account-wide windows stay ahead of per-model ones.
    current.sort_by_key(|window| (window_order(window.kind), window.model.is_some()));
    current
}

fn window_order(kind: AgentPlanUsageWindowKind) -> u8 {
    match kind {
        AgentPlanUsageWindowKind::FiveHour => 0,
        AgentPlanUsageWindowKind::Weekly => 1,
        AgentPlanUsageWindowKind::Other => 2,
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

/// Locate the CLI inside the installed adapter's platform package:
/// `@anthropic-ai/claude-agent-sdk-<platform>/claude` and
/// `@openai/codex-<platform>/vendor/<triple>/bin/codex`.
fn probe_binary(provider: Provider, adapter: &InstalledAdapter) -> Option<PathBuf> {
    let modules = adapter.directory.join("node_modules");
    let (scope, prefix) = match provider {
        Provider::Claude => ("@anthropic-ai", "claude-agent-sdk-"),
        Provider::Codex => ("@openai", "codex-"),
    };
    let packages = std::fs::read_dir(modules.join(scope)).ok()?;
    for package in packages.flatten() {
        let name = package.file_name();
        if !name.to_string_lossy().starts_with(prefix) {
            continue;
        }
        let candidate = match provider {
            Provider::Claude => Some(package.path().join("claude")),
            Provider::Codex => std::fs::read_dir(package.path().join("vendor"))
                .ok()?
                .flatten()
                .map(|triple| triple.path().join("bin").join("codex"))
                .find(|path| path.is_file()),
        };
        if let Some(path) = candidate.filter(|path| path.is_file()) {
            return Some(path);
        }
    }
    None
}

async fn run_probe(
    provider: Provider,
    binary: &Path,
    unset: &[String],
    environment: &[(String, String)],
) -> Result<Reading, String> {
    let arguments: &[&str] = match provider {
        // No MCP servers and no transcript: the probe is never a session.
        Provider::Claude => &[
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--strict-mcp-config",
            "--no-session-persistence",
        ],
        Provider::Codex => &["-s", "read-only", "-a", "never", "app-server"],
    };
    let mut command = tokio::process::Command::new(binary);
    // The adapter launch's own removals: an instance's recorded-unset home
    // levers, and a launching Claude Code session's variables.
    for key in unset {
        command.env_remove(key);
    }
    let mut child = command
        .args(arguments)
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Couldn't start the usage probe: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("The usage probe has no stdin")?;
    let stdout = child.stdout.take().ok_or("The usage probe has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    let exchange = async {
        let send = |message: Value| format!("{message}\n");
        match provider {
            Provider::Claude => {
                let requests = send(
                    json!({"type": "control_request", "request_id": "init", "request": {"subtype": "initialize"}}),
                ) + &send(
                    json!({"type": "control_request", "request_id": "usage", "request": {"subtype": "get_usage", "skip_behaviors": true}}),
                );
                write(&mut stdin, &requests).await?;
            }
            Provider::Codex => {
                let request = send(
                    json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "ottam", "version": env!("CARGO_PKG_VERSION")}}}),
                );
                write(&mut stdin, &request).await?;
            }
        }
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| format!("Couldn't read the usage probe: {error}"))?
        {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match provider {
                Provider::Claude => {
                    if message["type"] != "control_response" {
                        continue;
                    }
                    let response = &message["response"];
                    if response["subtype"] == "error" {
                        return Err(response["error"]
                            .as_str()
                            .unwrap_or("Claude Code refused the usage request")
                            .to_string());
                    }
                    if response["request_id"] == "usage" {
                        return claude_probe_reading(&response["response"]);
                    }
                }
                Provider::Codex => {
                    if let Some(error) = message.get("error") {
                        return Err(error["message"]
                            .as_str()
                            .unwrap_or("Codex refused the usage request")
                            .to_string());
                    }
                    if message["id"] == 1 {
                        let requests = send(json!({"method": "initialized"}))
                            + &send(json!({"id": 2, "method": "account/rateLimits/read"}));
                        write(&mut stdin, &requests).await?;
                    } else if message["id"] == 2 {
                        return codex_probe_reading(&message["result"]);
                    }
                }
            }
        }
        Err("The usage probe exited without an answer".to_string())
    };
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, exchange)
        .await
        .unwrap_or_else(|_| Err("Usage refresh timed out".to_string()));
    // Closing stdin lets either CLI exit cleanly and remove its session
    // bookkeeping; one that lingers is killed.
    drop(stdin);
    if tokio::time::timeout(EXIT_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    outcome
}

async fn write(stdin: &mut tokio::process::ChildStdin, text: &str) -> Result<(), String> {
    stdin
        .write_all(text.as_bytes())
        .await
        .map_err(|error| format!("Couldn't write to the usage probe: {error}"))
}

/// `_meta["_claude/rateLimit"].unifiedWindows` on a `usage_update`:
/// utilization is a 0–1 fraction and `resetsAt` Unix seconds.
fn claude_session_reading(payload: &Value) -> Option<Reading> {
    let limit = payload.pointer("/_meta/_claude~1rateLimit")?;
    let windows: Vec<_> = [
        ("five_hour", AgentPlanUsageWindowKind::FiveHour, "5h"),
        ("seven_day", AgentPlanUsageWindowKind::Weekly, "wk"),
    ]
    .into_iter()
    .filter_map(|(key, kind, label)| {
        let window = limit.get("unifiedWindows")?.get(key)?;
        Some(AgentPlanUsageWindow {
            kind,
            label: label.to_string(),
            used_percent: percent(window.get("utilization")?.as_f64()? * 100.0),
            resets_at: window.get("resetsAt").and_then(Value::as_i64),
            model: None,
        })
    })
    .collect();
    if windows.is_empty() {
        return None;
    }
    Some(Reading {
        windows,
        limit_status: limit
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Claude Code's `get_usage` answer: utilization is 0–100 and `resets_at`
/// ISO 8601. `rate_limits_available` is false for API-key and third-party
/// accounts. A model's own weekly cap (Fable) is a `weekly_scoped` entry in
/// `limits`, which reports `percent` rather than `utilization`.
fn claude_probe_reading(response: &Value) -> Result<Reading, String> {
    if response
        .get("rate_limits_available")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Ok(Reading::default());
    }
    let limits = &response["rate_limits"];
    let mut windows: Vec<_> = [
        ("five_hour", AgentPlanUsageWindowKind::FiveHour, "5h"),
        ("seven_day", AgentPlanUsageWindowKind::Weekly, "wk"),
    ]
    .into_iter()
    .filter_map(|(key, kind, label)| {
        let window = limits.get(key)?;
        Some(AgentPlanUsageWindow {
            kind,
            label: label.to_string(),
            used_percent: percent(window.get("utilization")?.as_f64()?),
            resets_at: iso_seconds(window.get("resets_at")),
            model: None,
        })
    })
    .collect();
    let scoped = limits
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|limit| limit.get("kind").and_then(Value::as_str) == Some("weekly_scoped"))
        .filter_map(|limit| {
            let model = text(limit.pointer("/scope/model/display_name"))?;
            Some(AgentPlanUsageWindow {
                kind: AgentPlanUsageWindowKind::Weekly,
                label: model.clone(),
                used_percent: percent(limit.get("percent")?.as_f64()?),
                resets_at: iso_seconds(limit.get("resets_at")),
                model: Some(model),
            })
        });
    // Upstream names a scoped limit by model only; keep the first of any
    // two that share a name rather than show both under one label.
    for window in scoped {
        if !windows
            .iter()
            .any(|existing| same_window(existing, &window))
        {
            windows.push(window);
        }
    }
    Ok(Reading {
        windows,
        limit_status: None,
    })
}

fn iso_seconds(value: Option<&Value>) -> Option<i64> {
    let text = value?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp())
}

/// A short display name from the vendor, dropped when empty or oversized.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty() && text.len() <= 32).then(|| text.to_string())
}

/// Codex app-server `account/rateLimits/read`: `primary` / `secondary` with
/// `usedPercent` (0–100), `windowDurationMins`, and `resetsAt` (Unix seconds).
fn codex_probe_reading(result: &Value) -> Result<Reading, String> {
    let limits = &result["rateLimits"];
    let windows = ["primary", "secondary"]
        .into_iter()
        .filter_map(|key| {
            let window = limits.get(key)?;
            let minutes = window.get("windowDurationMins").and_then(Value::as_i64);
            let (kind, label) = codex_window_label(minutes);
            Some(AgentPlanUsageWindow {
                kind,
                label,
                used_percent: percent(window.get("usedPercent")?.as_f64()?),
                resets_at: window.get("resetsAt").and_then(Value::as_i64),
                model: None,
            })
        })
        .collect();
    let reached = limits
        .get("rateLimitReachedType")
        .is_some_and(|reached| !reached.is_null())
        || result.get("ordinaryUsageAllowed").and_then(Value::as_bool) == Some(false);
    Ok(Reading {
        windows,
        limit_status: reached.then(|| "rejected".to_string()),
    })
}

fn codex_window_label(minutes: Option<i64>) -> (AgentPlanUsageWindowKind, String) {
    match minutes {
        Some(300) => (AgentPlanUsageWindowKind::FiveHour, "5h".into()),
        Some(10080) => (AgentPlanUsageWindowKind::Weekly, "wk".into()),
        Some(minutes) if minutes >= 1440 && minutes % 1440 == 0 => (
            AgentPlanUsageWindowKind::Other,
            format!("{}d", minutes / 1440),
        ),
        Some(minutes) if minutes >= 60 => (
            AgentPlanUsageWindowKind::Other,
            format!("{}h", minutes / 60),
        ),
        Some(minutes) => (AgentPlanUsageWindowKind::Other, format!("{minutes}m")),
        None => (AgentPlanUsageWindowKind::Other, "limit".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        kind: AgentPlanUsageWindowKind,
        label: &str,
        used: f64,
        resets: i64,
    ) -> AgentPlanUsageWindow {
        AgentPlanUsageWindow {
            kind,
            label: label.to_string(),
            used_percent: used,
            resets_at: Some(resets),
            model: None,
        }
    }

    #[test]
    fn claude_session_update_reads_unified_windows_as_percent() {
        // Captured from claude-agent-acp 0.85.1
        // (fixtures/claude-native-subagents-0.85.1.json).
        let payload = json!({"sessionUpdate": "usage_update", "used": 1, "size": 2, "_meta": {
            "_claude/rateLimit": {"status": "allowed", "resetsAt": 1791690000,
                "rateLimitType": "five_hour", "overageStatus": "allowed",
                "isUsingOverage": false, "unifiedWindows": {
                    "five_hour": {"utilization": 0.21, "resetsAt": 1791690000},
                    "seven_day": {"utilization": 0.05, "resetsAt": 1792000000}}}}});
        let reading = claude_session_reading(&payload).unwrap();
        assert_eq!(reading.limit_status.as_deref(), Some("allowed"));
        assert_eq!(reading.windows.len(), 2);
        assert!((reading.windows[0].used_percent - 21.0).abs() < 1e-9);
        assert_eq!(reading.windows[0].kind, AgentPlanUsageWindowKind::FiveHour);
        assert_eq!(reading.windows[1].resets_at, Some(1792000000));
    }

    #[test]
    fn claude_session_update_without_windows_is_ignored() {
        let payload = json!({"_meta": {"_claude/rateLimit": {"status": "allowed_warning"}}});
        assert_eq!(claude_session_reading(&payload), None);
        assert_eq!(claude_session_reading(&json!({"used": 3})), None);
    }

    #[test]
    fn claude_probe_reads_windows_and_iso_resets() {
        // Shape from Claude Code 2.1.309's `get_usage`, trimmed.
        let response = json!({"subscription_type": "max", "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 12, "resets_at": "2026-11-06T06:29:59.904067+00:00"},
                "seven_day": {"utilization": 15, "resets_at": "2026-11-07T09:00:00.904090+00:00"},
                "seven_day_opus": null,
                "limits": [
                    {"kind": "session", "percent": 12, "resets_at": "2026-11-06T06:29:59+00:00", "scope": null},
                    {"kind": "weekly_scoped", "percent": 2, "resets_at": "2026-11-07T08:59:59.904231+00:00",
                     "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}},
                    {"kind": "weekly_scoped", "percent": 9, "scope": {"model": {"display_name": ""}}}]}});
        let reading = claude_probe_reading(&response).unwrap();
        assert_eq!(
            reading.windows,
            vec![
                window(
                    AgentPlanUsageWindowKind::FiveHour,
                    "5h",
                    12.0,
                    1_793_946_599
                ),
                window(AgentPlanUsageWindowKind::Weekly, "wk", 15.0, 1_794_042_000),
                AgentPlanUsageWindow {
                    model: Some("Fable".into()),
                    ..window(
                        AgentPlanUsageWindowKind::Weekly,
                        "Fable",
                        2.0,
                        1_794_041_999
                    )
                },
            ],
            "the unnamed scoped limit is dropped"
        );
    }

    #[test]
    fn a_live_update_keeps_the_probes_model_window() {
        let (service, announced) = service();
        let agent = AgentId::new(super::super::CLAUDE_AGENT_ID);
        seeded(&service, &agent, 0);
        service.settle_probe(
            &agent,
            0,
            Some(Ok(Reading {
                windows: vec![AgentPlanUsageWindow {
                    model: Some("Fable".into()),
                    ..window(AgentPlanUsageWindowKind::Weekly, "Fable", 2.0, 1)
                }],
                limit_status: None,
            })),
        );
        service.record_session_update(
            &agent,
            &json!({"_meta": {"_claude/rateLimit": {"status": "allowed",
                "unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": 1}}}}}),
        );
        let announced = announced.lock().unwrap();
        let labels: Vec<_> = announced
            .last()
            .and_then(|change| change.usage.as_ref())
            .map(|usage| usage.windows.iter().map(|w| w.label.clone()).collect())
            .unwrap_or_default();
        assert_eq!(labels, ["5h", "Fable"]);
    }

    #[test]
    fn claude_probe_without_plan_limits_reads_nothing() {
        let response =
            json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null});
        assert_eq!(claude_probe_reading(&response).unwrap(), Reading::default());
    }

    #[test]
    fn codex_probe_reads_primary_and_secondary() {
        // Shape from codex 0.160.1's app-server, trimmed.
        let result = json!({"ordinaryUsageAllowed": true, "rateLimits": {
            "limitId": "codex",
            "primary": {"usedPercent": 0, "windowDurationMins": 300, "resetsAt": 1793948990},
            "secondary": {"usedPercent": 2, "windowDurationMins": 10080, "resetsAt": 1794404662},
            "rateLimitReachedType": null}});
        let reading = codex_probe_reading(&result).unwrap();
        assert_eq!(
            reading.windows,
            vec![
                window(AgentPlanUsageWindowKind::FiveHour, "5h", 0.0, 1793948990),
                window(AgentPlanUsageWindowKind::Weekly, "wk", 2.0, 1794404662),
            ]
        );
        assert_eq!(reading.limit_status, None);
    }

    #[test]
    fn codex_probe_marks_a_reached_limit() {
        let result = json!({"ordinaryUsageAllowed": false, "rateLimits": {
            "primary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 1},
            "secondary": null, "rateLimitReachedType": "rateLimitReached"}});
        let reading = codex_probe_reading(&result).unwrap();
        assert_eq!(reading.limit_status.as_deref(), Some("rejected"));
        assert_eq!(reading.windows.len(), 1);
    }

    #[test]
    fn codex_labels_unusual_windows_by_length() {
        assert_eq!(codex_window_label(Some(15)).1, "15m");
        assert_eq!(codex_window_label(Some(120)).1, "2h");
        assert_eq!(codex_window_label(Some(43200)).1, "30d");
        assert_eq!(codex_window_label(None).1, "limit");
    }

    fn service() -> (
        Arc<PlanUsageService>,
        Arc<StdMutex<Vec<AgentPlanUsageChangedParams>>>,
    ) {
        let announced = Arc::new(StdMutex::new(Vec::new()));
        let sink_announced = Arc::clone(&announced);
        let sink: PlanUsageSink =
            Arc::new(move |change| sink_announced.lock().unwrap().push(change));
        (
            PlanUsageService::new(Arc::new(AgentRegistry::built_in()), sink),
            announced,
        )
    }

    fn seeded(service: &PlanUsageService, agent: &AgentId, generation: u64) -> AgentPlanUsage {
        let usage = AgentPlanUsage {
            agent_id: AgentID(agent.0.clone()),
            windows: vec![window(AgentPlanUsageWindowKind::FiveHour, "5h", 40.0, 1)],
            limit_status: Some("allowed_warning".into()),
            source: AgentPlanUsageSource::Session,
            observed_at: 1,
            error: None,
        };
        service.entries().insert(
            agent.0.clone(),
            Entry {
                usage: Some(usage.clone()),
                generation,
                probing: true,
                ..Entry::default()
            },
        );
        usage
    }

    fn probe_reading(used: f64) -> Reading {
        Reading {
            windows: vec![window(AgentPlanUsageWindowKind::FiveHour, "5h", used, 1)],
            limit_status: None,
        }
    }

    #[test]
    fn a_probe_older_than_a_live_update_keeps_its_account_windows() {
        let (service, announced) = service();
        let agent = AgentId::new("claude");
        let live = seeded(&service, &agent, 1);
        service.settle_probe(&agent, 0, Some(Ok(probe_reading(10.0))));
        let usage = service.entries()[&agent.0].usage.clone().unwrap();
        assert_eq!(usage.windows, live.windows, "the turn's 40% stands");
        assert_eq!(usage.source, AgentPlanUsageSource::Probe);
        assert_eq!(
            announced.lock().unwrap().len(),
            1,
            "a waiting refresh hears it settled"
        );
        assert!(!service.entries()[&agent.0].probing);
    }

    #[test]
    fn a_stale_probe_drops_a_model_it_no_longer_reports() {
        let (service, _announced) = service();
        let agent = AgentId::new("claude");
        let live = seeded(&service, &agent, 1);
        let fable = AgentPlanUsageWindow {
            model: Some("Fable".into()),
            ..window(AgentPlanUsageWindowKind::Weekly, "Fable", 2.0, 1)
        };
        service
            .entries()
            .get_mut(&agent.0)
            .unwrap()
            .usage
            .as_mut()
            .unwrap()
            .windows
            .push(fable);
        service.settle_probe(&agent, 0, Some(Ok(probe_reading(10.0))));
        let usage = service.entries()[&agent.0].usage.clone().unwrap();
        assert_eq!(usage.windows, live.windows);
    }

    #[test]
    fn a_forced_refresh_skips_freshness_and_backoff_but_not_a_running_probe() {
        let now = Instant::now();
        let fresh = Entry {
            probed_at: Some(now),
            ..Entry::default()
        };
        assert!(!fresh.admits_probe(now, false));
        assert!(fresh.admits_probe(now, true));
        let backing_off = Entry {
            retry_at: Some(now + MAX_RETRY),
            ..Entry::default()
        };
        assert!(!backing_off.admits_probe(now, false));
        assert!(backing_off.admits_probe(now, true));
        let running = Entry {
            probing: true,
            ..Entry::default()
        };
        assert!(!running.admits_probe(now, true));
        assert!(Entry::default().admits_probe(now, false));
    }

    #[test]
    fn a_stale_probe_still_lands_its_model_windows() {
        let (service, announced) = service();
        let agent = AgentId::new("claude");
        let live = seeded(&service, &agent, 1);
        let fable = AgentPlanUsageWindow {
            model: Some("Fable".into()),
            ..window(AgentPlanUsageWindowKind::Weekly, "Fable", 2.0, 1)
        };
        let mut reading = probe_reading(10.0);
        reading.windows.push(fable.clone());
        service.settle_probe(&agent, 0, Some(Ok(reading)));
        let usage = service.entries()[&agent.0].usage.clone().unwrap();
        assert_eq!(usage.windows, vec![live.windows[0].clone(), fable]);
        assert_eq!(announced.lock().unwrap().len(), 1);
    }

    #[test]
    fn windows_merge_by_kind_and_model_not_label() {
        let scoped_wk = AgentPlanUsageWindow {
            model: Some("wk".into()),
            ..window(AgentPlanUsageWindowKind::Weekly, "wk", 3.0, 1)
        };
        let merged = merge_windows(
            vec![scoped_wk.clone()],
            vec![window(AgentPlanUsageWindowKind::Weekly, "wk", 15.0, 1)],
        );
        assert_eq!(merged.len(), 2, "a model named wk is not the weekly window");
        assert_eq!(
            merged[1], scoped_wk,
            "per-model windows sort after account ones"
        );
    }

    #[test]
    fn duplicate_scoped_limits_keep_the_first() {
        let response = json!({"rate_limits_available": true, "rate_limits": {"limits": [
            {"kind": "weekly_scoped", "percent": 2, "scope": {"model": {"display_name": "Fable"}}},
            {"kind": "weekly_scoped", "percent": 9, "scope": {"model": {"display_name": "Fable"}}}]}});
        let reading = claude_probe_reading(&response).unwrap();
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].used_percent, 2.0);
    }

    #[test]
    fn a_current_probe_replaces_the_snapshot() {
        let (service, announced) = service();
        let agent = AgentId::new("claude");
        seeded(&service, &agent, 0);
        service.settle_probe(&agent, 0, Some(Ok(probe_reading(10.0))));
        let usage = announced.lock().unwrap()[0].usage.clone().unwrap();
        assert_eq!(usage.source, AgentPlanUsageSource::Probe);
        assert_eq!(usage.windows[0].used_percent, 10.0);
    }

    #[test]
    fn losing_plan_limits_announces_a_clear() {
        let (service, announced) = service();
        let agent = AgentId::new("claude");
        seeded(&service, &agent, 0);
        service.settle_probe(&agent, 0, Some(Ok(Reading::default())));
        assert_eq!(service.entries()[&agent.0].usage, None);
        let announced = announced.lock().unwrap();
        assert_eq!(announced.len(), 1);
        assert_eq!(announced[0].usage, None);
    }

    #[test]
    fn a_failure_keeps_the_windows_and_backs_off() {
        let (service, announced) = service();
        let agent = AgentId::new("claude");
        seeded(&service, &agent, 0);
        let before = Instant::now();
        service.settle_probe(&agent, 0, Some(Err("timed out".into())));
        service.entries().get_mut(&agent.0).unwrap().probing = true;
        service.settle_probe(&agent, 0, Some(Err("timed out".into())));
        let entries = service.entries();
        let entry = &entries[&agent.0];
        // Backoff alone gates the retry; a failure is not a fresh answer.
        assert_eq!(entry.probed_at, None);
        assert!(entry.retry_at.unwrap() >= before + FIRST_RETRY * 2);
        let announced = announced.lock().unwrap();
        assert_eq!(
            announced[1].usage.as_ref().unwrap().error.as_deref(),
            Some("timed out")
        );
        assert_eq!(announced[1].usage.as_ref().unwrap().windows.len(), 1);
    }

    fn fake_cli(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("cli");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn claude_probe_runs_the_control_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(
            dir.path(),
            r#"read init; read usage
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init","response":{}}}'
echo '{"type":"control_response","response":{"subtype":"success","request_id":"usage","response":{"rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":40,"resets_at":null}}}}}'
read eof"#,
        );
        let reading = run_probe(Provider::Claude, &cli, &[], &[]).await.unwrap();
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0].used_percent, 40.0);
        assert_eq!(reading.windows[0].resets_at, None);
    }

    #[tokio::test]
    async fn claude_probe_reports_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(
            dir.path(),
            r#"read init
echo '{"type":"control_response","response":{"subtype":"error","request_id":"init","error":"Not logged in"}}'"#,
        );
        assert_eq!(
            run_probe(Provider::Claude, &cli, &[], &[]).await,
            Err("Not logged in".to_string())
        );
    }

    #[tokio::test]
    async fn codex_probe_runs_the_app_server_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(
            dir.path(),
            r#"read initialize
echo '{"id":1,"result":{}}'
read initialized; read rate_limits
echo 'not json'
echo '{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":5}}}}'"#,
        );
        let reading = run_probe(Provider::Codex, &cli, &[], &[]).await.unwrap();
        assert_eq!(
            reading.windows,
            vec![window(AgentPlanUsageWindowKind::FiveHour, "5h", 7.0, 5)]
        );
    }

    #[tokio::test]
    async fn a_silent_probe_exits_without_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(dir.path(), "exit 0");
        assert_eq!(
            run_probe(Provider::Codex, &cli, &[], &[]).await,
            Err("The usage probe exited without an answer".to_string())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_probe_times_out_and_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fake_cli(dir.path(), "trap '' TERM; while :; do sleep 1; done");
        assert_eq!(
            run_probe(Provider::Claude, &cli, &[], &[]).await,
            Err("Usage refresh timed out".to_string())
        );
    }
}
