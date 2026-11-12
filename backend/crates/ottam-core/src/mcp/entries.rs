//! Which configured servers an existing setup already runs itself, so its
//! sessions are not also given the proxied copy.
//!
//! An Ottam server matches a native one when its name matches after
//! codex-acp's normalization (whitespace to `_`), or its signature matches
//! Claude Code's rule: `url:` the URL without fragment or trailing slash,
//! or `stdio:` the command and arguments. Only existing-setup instances of
//! Claude Code, Codex, and OpenCode are filtered; Ottam profiles and
//! Oh-My-Pi read no config of their own under ACP.
//!
//! Each CLI's own layers are read as it reads them, project ones included,
//! and a server it would not start (turned off, or a Claude project server
//! never approved) does not count.

use crate::agent::setup_detection::read_config;
use ottam_protocol::{McpServerConfig, McpServerTransport};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The MCP servers a CLI's own config defines.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NativeServers {
    names: BTreeSet<String>,
    signatures: BTreeSet<Signature>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Signature {
    Url(String),
    Stdio(Vec<String>),
}

impl NativeServers {
    /// Whether the setup already has `config`'s server.
    pub(crate) fn has(&self, config: &McpServerConfig) -> bool {
        self.names.contains(&config.name)
            || match &config.transport {
                McpServerTransport::Stdio { command, args, .. } => self.signatures.contains(
                    &Signature::Stdio(std::iter::once(command).chain(args).cloned().collect()),
                ),
                McpServerTransport::Http { url, .. } => normalized_url(url)
                    .is_some_and(|url| self.signatures.contains(&Signature::Url(url))),
            }
    }

    fn add(&mut self, name: &str, signature: Option<Signature>) {
        self.names
            .insert(name.split_whitespace().collect::<Vec<_>>().join("_"));
        self.signatures.extend(signature);
    }
}

/// Server definitions by name, later layers overriding earlier ones key by
/// key, as the CLIs merge them.
type Layered = BTreeMap<String, Map<String, Value>>;

/// The servers `harness`'s existing setup at `home` defines for a session
/// in `cwd`. `environment` is the instance's recorded login-shell setup;
/// `user_home` is where the CLI finds its defaults. An unreadable file adds
/// nothing, so the proxied copy is given rather than lost.
pub(crate) fn native_servers(
    harness: &str,
    home: &Path,
    environment: &BTreeMap<String, Option<String>>,
    user_home: &Path,
    cwd: &Path,
) -> NativeServers {
    let set = |variable: &str| {
        environment
            .get(variable)
            .cloned()
            .flatten()
            .map(PathBuf::from)
    };
    let servers = match harness {
        "claude" => claude_servers(home, set("CLAUDE_CONFIG_DIR").is_some(), user_home, cwd),
        "codex" => codex_servers(home, cwd),
        "opencode" => {
            let directory = set("OPENCODE_CONFIG_DIR").unwrap_or_else(|| {
                set("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| user_home.join(".config"))
                    .join("opencode")
            });
            opencode_servers(&directory, set("OPENCODE_CONFIG"), cwd)
        }
        _ => Vec::new(),
    };
    let mut native = NativeServers::default();
    for (name, signature) in servers {
        native.add(&name, signature);
    }
    native
}

/// Claude Code: user and local scope from `.claude.json`, and the
/// project's `.mcp.json` servers once approved, less any `/mcp` turned off
/// for the project.
fn claude_servers(
    home: &Path,
    custom_home: bool,
    user_home: &Path,
    cwd: &Path,
) -> Vec<(String, Option<Signature>)> {
    // A custom config directory holds its own metadata file.
    let metadata = if custom_home {
        home.join(".claude.json")
    } else {
        user_home.join(".claude.json")
    };
    let metadata = read_config(metadata, false).unwrap_or_default();
    let project = metadata
        .get("projects")
        .and_then(|projects| projects.get(cwd.to_string_lossy().as_ref()));
    let mut servers = command_servers(metadata.get("mcpServers"));
    servers.extend(command_servers(
        project.and_then(|project| project.get("mcpServers")),
    ));

    let settings: Vec<Value> = [
        home.join("settings.json"),
        cwd.join(".claude/settings.json"),
        cwd.join(".claude/settings.local.json"),
    ]
    .into_iter()
    .filter_map(|path| read_config(path, false))
    .chain(project.cloned())
    .collect();
    let approve_all = settings
        .iter()
        .any(|settings| settings.get("enableAllProjectMcpServers") == Some(&Value::Bool(true)));
    let approved = strings(&settings, "enabledMcpjsonServers");
    let rejected = strings(&settings, "disabledMcpjsonServers");
    if let Some(mcp_json) = read_config(cwd.join(".mcp.json"), false) {
        servers.extend(
            command_servers(mcp_json.get("mcpServers"))
                .into_iter()
                .filter(|(name, _)| {
                    (approve_all || approved.contains(name)) && !rejected.contains(name)
                }),
        );
    }
    let disabled = strings(project, "disabledMcpServers");
    servers.retain(|(name, _)| !disabled.contains(name));
    servers
}

/// Codex: `mcp_servers` in `config.toml`, with a trusted project's own
/// `.codex/config.toml` files layered over it from its root down to `cwd`.
fn codex_servers(home: &Path, cwd: &Path) -> Vec<(String, Option<Signature>)> {
    let Some(user) = read_config(home.join("config.toml"), true) else {
        return Vec::new();
    };
    let mut layered = Layered::new();
    layer(&mut layered, entries(user.get("mcp_servers")));
    if let Some(root) = trusted_root(&user, cwd) {
        let mut directories: Vec<&Path> = cwd
            .ancestors()
            .take_while(|directory| directory.starts_with(root))
            .collect();
        directories.reverse();
        for directory in directories {
            if let Some(project) = read_config(directory.join(".codex/config.toml"), true) {
                layer(&mut layered, entries(project.get("mcp_servers")));
            }
        }
    }
    layered
        .into_iter()
        .filter(|(_, server)| server.get("enabled") != Some(&Value::Bool(false)))
        .map(|(name, server)| (name, command_signature(&server)))
        .collect()
}

/// The nearest directory at or above `cwd` that Codex's config trusts.
fn trusted_root<'a>(config: &Value, cwd: &'a Path) -> Option<&'a Path> {
    let projects = config.get("projects")?.as_object()?;
    cwd.ancestors().find(|directory| {
        projects
            .get(directory.to_string_lossy().as_ref())
            .and_then(|project| project.get("trust_level"))
            .and_then(Value::as_str)
            == Some("trusted")
    })
}

/// OpenCode: its global config, the file `OPENCODE_CONFIG` names, then the
/// project's from its worktree root down to `cwd`, each overriding the last.
fn opencode_servers(
    directory: &Path,
    custom: Option<PathBuf>,
    cwd: &Path,
) -> Vec<(String, Option<Signature>)> {
    let mut files: Vec<PathBuf> = ["opencode.json", "opencode.jsonc"]
        .iter()
        .map(|file| directory.join(file))
        .collect();
    files.extend(custom);
    let root = cwd
        .ancestors()
        .find(|directory| directory.join(".git").exists());
    let mut directories: Vec<&Path> = match root {
        Some(root) => cwd
            .ancestors()
            .take_while(|directory| directory.starts_with(root))
            .collect(),
        None => vec![cwd],
    };
    directories.reverse();
    for directory in directories {
        for file in [
            "opencode.json",
            "opencode.jsonc",
            ".opencode/opencode.json",
            ".opencode/opencode.jsonc",
        ] {
            files.push(directory.join(file));
        }
    }
    let mut layered = Layered::new();
    for file in files {
        if let Some(config) = read_config(file, false) {
            layer(&mut layered, opencode_entries(config.get("mcp")));
        }
    }
    layered
        .into_iter()
        .filter(|(_, server)| server.get("enabled") != Some(&Value::Bool(false)))
        .map(|(name, server)| (name, opencode_signature(&server)))
        .collect()
}

fn layer<'a>(layered: &mut Layered, servers: impl Iterator<Item = (&'a String, &'a Value)>) {
    for (name, server) in servers {
        if let Some(server) = server.as_object() {
            let entry = layered.entry(name.clone()).or_default();
            entry.extend(
                server
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }
}

fn entries(servers: Option<&Value>) -> impl Iterator<Item = (&String, &Value)> {
    servers.and_then(Value::as_object).into_iter().flatten()
}

/// OpenCode's `mcp`: v2 nests servers under `servers`, v1 listed them
/// directly beside v2's `timeout`.
fn opencode_entries(mcp: Option<&Value>) -> impl Iterator<Item = (&String, &Value)> {
    let mcp = mcp.and_then(Value::as_object);
    let v1 = mcp
        .into_iter()
        .flatten()
        .filter(|(name, _)| !matches!(name.as_str(), "servers" | "timeout"));
    let v2 = entries(mcp.and_then(|mcp| mcp.get("servers")));
    v1.chain(v2)
}

/// Claude Code's `mcpServers`: a map of name to `{command, args}` or
/// `{url}`.
fn command_servers(servers: Option<&Value>) -> Vec<(String, Option<Signature>)> {
    entries(servers)
        .filter_map(|(name, server)| Some((name.clone(), command_signature(server.as_object()?))))
        .collect()
}

fn command_signature(server: &Map<String, Value>) -> Option<Signature> {
    match (server.get("url"), server.get("command")) {
        (Some(url), _) => url.as_str().and_then(normalized_url).map(Signature::Url),
        (None, Some(command)) => command.as_str().map(|command| {
            let args = server.get("args").and_then(Value::as_array);
            let args = args.into_iter().flatten().filter_map(Value::as_str);
            Signature::Stdio(
                std::iter::once(command)
                    .chain(args)
                    .map(str::to_owned)
                    .collect(),
            )
        }),
        (None, None) => None,
    }
}

/// A local OpenCode server's `command` is an array holding the arguments;
/// a remote one has a `url`.
fn opencode_signature(server: &Map<String, Value>) -> Option<Signature> {
    match server.get("type").and_then(Value::as_str) {
        Some("remote") => server
            .get("url")
            .and_then(Value::as_str)
            .and_then(normalized_url)
            .map(Signature::Url),
        Some("local") => server
            .get("command")
            .and_then(Value::as_array)
            .map(|command| {
                Signature::Stdio(
                    command
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                )
            }),
        _ => None,
    }
}

/// Every string in `key`'s list across `layers`.
fn strings<'a>(layers: impl IntoIterator<Item = &'a Value>, key: &str) -> BTreeSet<String> {
    layers
        .into_iter()
        .filter_map(|layer| layer.get(key)?.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn normalized_url(url: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(url).ok()?;
    url.set_fragment(None);
    Some(url.as_str().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::native_servers;
    use ottam_protocol::{McpConfigValue, McpServerConfig, McpServerTransport};
    use std::collections::BTreeMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn local(name: &str, command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: McpServerTransport::Stdio {
                command: command.into(),
                args: args.iter().map(|arg| arg.to_string()).collect(),
                env: Vec::<McpConfigValue>::new(),
            },
        }
    }

    fn remote(name: &str, url: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: McpServerTransport::Http {
                url: url.into(),
                headers: Vec::new(),
            },
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn claude_matches_user_and_project_servers_by_name_or_signature() {
        let user = TempDir::new().unwrap();
        let home = user.path().join(".claude");
        let project = Path::new("/work/app");
        write(
            &user.path().join(".claude.json"),
            r#"{"mcpServers":{"my docs":{"type":"http","url":"https://docs.example.com/mcp/#x"}},
                "projects":{"/work/app":{"mcpServers":{"git":{"command":"uvx","args":["mcp-git"]}}},
                            "/elsewhere":{"mcpServers":{"other":{"command":"other"}}}}}"#,
        );
        let native = native_servers("claude", &home, &BTreeMap::new(), user.path(), project);
        assert!(native.has(&remote("my_docs", "https://unrelated.example.com")));
        assert!(native.has(&remote("docs", "https://docs.example.com/mcp")));
        assert!(native.has(&local("repo", "uvx", &["mcp-git"])));
        assert!(!native.has(&local("repo", "uvx", &["mcp-git", "--verbose"])));
        assert!(!native.has(&local("other", "something", &[])));
    }

    #[test]
    fn a_custom_claude_config_dir_holds_its_own_metadata() {
        let user = TempDir::new().unwrap();
        let home = user.path().join("custom");
        write(
            &home.join(".claude.json"),
            r#"{"mcpServers":{"mine":{"command":"x"}}}"#,
        );
        let environment = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            Some(home.display().to_string()),
        )]);
        let native = native_servers("claude", &home, &environment, user.path(), Path::new("/w"));
        assert!(native.has(&local("mine", "y", &[])));
    }

    #[test]
    fn codex_and_opencode_read_their_own_config() {
        let user = TempDir::new().unwrap();
        let codex = user.path().join(".codex");
        write(
            &codex.join("config.toml"),
            "[mcp_servers.search]\nurl = 'https://search.example.com/'\n",
        );
        let native = native_servers(
            "codex",
            &codex,
            &BTreeMap::new(),
            user.path(),
            Path::new("/w"),
        );
        assert!(native.has(&remote("web", "https://search.example.com")));

        write(
            &user.path().join(".config/opencode/opencode.jsonc"),
            r#"{ // v2
                "mcp": {"timeout": 5, "servers": {"fs": {"type": "local", "command": ["npx", "fs-mcp"]}},
                        "legacy": {"type": "remote", "url": "https://legacy.example.com"}}}"#,
        );
        let native = native_servers(
            "opencode",
            &user.path().join(".local/share/opencode"),
            &BTreeMap::new(),
            user.path(),
            Path::new("/w"),
        );
        assert!(native.has(&local("files", "npx", &["fs-mcp"])));
        assert!(native.has(&remote("legacy", "https://elsewhere.example.com")));
        assert!(!native.has(&remote("timeout", "https://t.example.com")));
    }

    #[test]
    fn claude_counts_only_project_servers_it_would_start() {
        let user = TempDir::new().unwrap();
        let home = user.path().join(".claude");
        let project = user.path().join("app");
        let key = project.display().to_string();
        write(
            &user.path().join(".claude.json"),
            &serde_json::json!({
                "mcpServers": {"off": {"command": "off"}},
                "projects": {key: {
                    "enabledMcpjsonServers": ["approved"],
                    "disabledMcpServers": ["off"],
                }},
            })
            .to_string(),
        );
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers":{"approved":{"command":"a"},"pending":{"command":"p"},
                "rejected":{"command":"r"}}}"#,
        );
        write(
            &project.join(".claude/settings.local.json"),
            r#"{"enabledMcpjsonServers":["rejected"],"disabledMcpjsonServers":["rejected"]}"#,
        );
        let native = native_servers("claude", &home, &BTreeMap::new(), user.path(), &project);
        assert!(native.has(&local("approved", "x", &[])));
        assert!(!native.has(&local("pending", "p", &[])), "never approved");
        assert!(!native.has(&local("rejected", "r", &[])), "rejected wins");
        assert!(!native.has(&local("off", "off", &[])), "turned off here");

        write(
            &home.join("settings.json"),
            r#"{"enableAllProjectMcpServers":true}"#,
        );
        let native = native_servers("claude", &home, &BTreeMap::new(), user.path(), &project);
        assert!(native.has(&local("pending", "p", &[])));
        assert!(!native.has(&local("rejected", "r", &[])));
    }

    #[test]
    fn codex_layers_a_trusted_projects_config_and_skips_disabled_servers() {
        let user = TempDir::new().unwrap();
        let codex = user.path().join(".codex");
        let (trusted, untrusted) = (user.path().join("trusted"), user.path().join("other"));
        write(
            &codex.join("config.toml"),
            &format!(
                "[mcp_servers.kept]\ncommand = 'k'\n\
                 [mcp_servers.dropped]\ncommand = 'd'\n\
                 [mcp_servers.idle]\ncommand = 'i'\nenabled = false\n\
                 [projects.'{}']\ntrust_level = 'trusted'\n",
                trusted.display()
            ),
        );
        let project = "[mcp_servers.dropped]\nenabled = false\n\
                       [mcp_servers.local]\ncommand = 'l'\n";
        let cwd = trusted.join("crate");
        write(&trusted.join(".codex/config.toml"), project);
        write(
            &cwd.join(".codex/config.toml"),
            "[mcp_servers.nested]\ncommand = 'n'\n",
        );
        write(&untrusted.join(".codex/config.toml"), project);

        let native = native_servers("codex", &codex, &BTreeMap::new(), user.path(), &cwd);
        assert!(native.has(&local("kept", "k", &[])));
        assert!(native.has(&local("local", "l", &[])));
        assert!(native.has(&local("nested", "n", &[])));
        assert!(
            !native.has(&local("dropped", "d", &[])),
            "turned off by the project"
        );
        assert!(!native.has(&local("idle", "i", &[])));

        let native = native_servers("codex", &codex, &BTreeMap::new(), user.path(), &untrusted);
        assert!(
            native.has(&local("dropped", "d", &[])),
            "an untrusted project counts for nothing"
        );
        assert!(!native.has(&local("local", "l", &[])));
    }

    #[test]
    fn opencode_layers_its_project_config_and_skips_disabled_servers() {
        let user = TempDir::new().unwrap();
        let root = user.path().join("repo");
        let cwd = root.join("pkg");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &user.path().join(".config/opencode/opencode.json"),
            r#"{"mcp":{"kept":{"type":"local","command":["k"]},
                       "dropped":{"type":"local","command":["d"]}}}"#,
        );
        write(
            &root.join("opencode.json"),
            r#"{"mcp":{"dropped":{"enabled":false},"root":{"type":"local","command":["r"]}}}"#,
        );
        write(
            &cwd.join(".opencode/opencode.jsonc"),
            r#"{"mcp":{"nested":{"type":"remote","url":"https://nested.example.com"}}}"#,
        );
        let native = native_servers("opencode", user.path(), &BTreeMap::new(), user.path(), &cwd);
        assert!(native.has(&local("kept", "k", &[])));
        assert!(native.has(&local("root", "r", &[])));
        assert!(native.has(&remote("nested", "https://elsewhere.example.com")));
        assert!(!native.has(&local("dropped", "d", &[])));
    }

    #[test]
    fn other_harnesses_and_missing_files_filter_nothing() {
        let user = TempDir::new().unwrap();
        for harness in ["omp", "claude", "codex", "opencode"] {
            let native = native_servers(
                harness,
                user.path(),
                &BTreeMap::new(),
                user.path(),
                Path::new("/w"),
            );
            assert_eq!(native, Default::default(), "{harness}");
        }
    }
}
