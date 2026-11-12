//! A one-time import of server definitions from a detected Claude Code or
//! Codex setup, or from a repository's `.mcp.json`. Nothing is saved here:
//! the app shows each server for approval and adds the approved ones.
//!
//! Values come as the source wrote them. Core never reads a CLI's
//! credentials, so a server that signs in through the CLI arrives without
//! that sign-in.

use super::config::validate;
use crate::agent::setup_detection::{self, read_config, MAX_CONFIG_BYTES};
use crate::agent::HomeLayout;
use crate::core_error::CoreError;
use ottam_protocol::{
    McpConfigValue, McpImportCandidate, McpImportCandidatesResult, McpImportSkipped,
    McpImportSource, McpServerConfig, McpServerSummary, McpServerTransport,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MAX_NAME_LEN: usize = 64;

/// Which shape a file's servers are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `mcpServers` in Claude Code's `.claude.json` or a `.mcp.json`.
    Claude,
    /// `mcp_servers` in Codex's `config.toml`.
    Codex,
}

/// The servers `source` defines, as `mcp.add` would take them. Blocking: a
/// setup is found through the login shell, as agent setup finds it.
pub(crate) fn candidates(
    source: &McpImportSource,
    configured: &[McpServerSummary],
) -> Result<McpImportCandidatesResult, CoreError> {
    let (path, shape) = match source {
        McpImportSource::Setup { harness_id } => setup_file(harness_id)?,
        McpImportSource::File { path } => (mcp_json(Path::new(path))?, Shape::Claude),
    };
    read(&path, shape, configured)
}

fn setup_file(harness: &str) -> Result<(PathBuf, Shape), CoreError> {
    if !matches!(harness, "claude" | "codex") {
        return Err(CoreError::new(
            "invalidParams",
            "Import reads Claude Code and Codex setups",
        ));
    }
    let detected = setup_detection::detect(harness)
        .map_err(|error| CoreError::new("mcpImportUnavailable", error.message))?;
    let HomeLayout::Existing { path, environment } = detected.layout else {
        return Err(unreadable("The setup has no home to read"));
    };
    setup_path(
        harness,
        path,
        &environment,
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Where a setup keeps its servers: Codex's `config.toml` in its home, and
/// Claude Code's `.claude.json` in the home directory, or in its config
/// directory when `CLAUDE_CONFIG_DIR` moved it there.
fn setup_path(
    harness: &str,
    setup_home: PathBuf,
    environment: &BTreeMap<String, Option<String>>,
    home: Option<PathBuf>,
) -> Result<(PathBuf, Shape), CoreError> {
    if harness == "codex" {
        return Ok((setup_home.join("config.toml"), Shape::Codex));
    }
    if environment
        .get("CLAUDE_CONFIG_DIR")
        .is_some_and(Option::is_some)
    {
        return Ok((setup_home.join(".claude.json"), Shape::Claude));
    }
    let home = home.ok_or_else(|| unreadable("Cannot locate your home directory"))?;
    Ok((home.join(".claude.json"), Shape::Claude))
}

/// `path` itself, or the `.mcp.json` in it.
fn mcp_json(path: &Path) -> Result<PathBuf, CoreError> {
    let file = if path.is_dir() {
        path.join(".mcp.json")
    } else {
        path.to_path_buf()
    };
    if !file.is_file() {
        return Err(unreadable(format!(
            "There is no .mcp.json at {}",
            path.display()
        )));
    }
    Ok(file)
}

/// A server as read, before it is named and checked.
struct Parsed {
    transport: McpServerTransport,
    notes: Vec<String>,
    disabled_in_source: bool,
}

/// A missing file has no servers: a setup that never added one has no
/// config file yet.
fn read(
    path: &Path,
    shape: Shape,
    configured: &[McpServerSummary],
) -> Result<McpImportCandidatesResult, CoreError> {
    let config = read_config(path.to_path_buf(), shape == Shape::Codex)
        .ok_or_else(|| unreadable(unreadable_reason(path, shape)))?;
    let key = match shape {
        Shape::Claude => "mcpServers",
        Shape::Codex => "mcp_servers",
    };
    let mut servers: Vec<(&String, &Value)> = config
        .get(key)
        .and_then(Value::as_object)
        .map(|servers| servers.iter().collect())
        .unwrap_or_default();
    servers.sort_by(|left, right| left.0.cmp(right.0));

    // A name that is already valid keeps it; the others are rewritten
    // around those, so adding `my db` later never takes `my_db`'s name.
    let mut taken: BTreeSet<String> = servers
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| is_valid_name(name))
        .map(str::to_owned)
        .collect();
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    for (source_name, server) in servers {
        let skip = |reason: &str| McpImportSkipped {
            name: source_name.clone(),
            reason: reason.into(),
        };
        let Some(server) = server.as_object() else {
            skipped.push(skip("Its definition is not an object"));
            continue;
        };
        let name = if is_valid_name(source_name) {
            source_name.clone()
        } else {
            let Some(name) = valid_name(source_name, &taken) else {
                skipped.push(skip("Its name has no letters or digits"));
                continue;
            };
            taken.insert(name.clone());
            name
        };
        let parsed = match shape {
            Shape::Claude => claude_server(server),
            Shape::Codex => codex_server(server),
        };
        let Parsed {
            transport,
            mut notes,
            disabled_in_source,
        } = match parsed {
            Ok(parsed) => parsed,
            Err(reason) => {
                skipped.push(skip(&reason));
                continue;
            }
        };
        let unexpanded = unexpanded(&transport);
        let config = McpServerConfig {
            name: name.clone(),
            transport,
        };
        // What `mcp.add` would refuse is not offered.
        if let Err(error) = validate(&config) {
            let reason = if unexpanded.is_empty() {
                error.message
            } else {
                format!(
                    "{} (it uses ${{…}}, which Ottam does not expand)",
                    error.message
                )
            };
            skipped.push(skip(&reason));
            continue;
        }
        notes.extend(unexpanded);
        if let McpServerTransport::Http { url, .. } = &config.transport {
            notes.extend(url_credential_note(url));
        }
        let mut seen = BTreeSet::new();
        notes.retain(|note| seen.insert(note.clone()));
        candidates.push(McpImportCandidate {
            existing_server_id: configured
                .iter()
                .find(|server| server.config.name == name)
                .map(|server| server.server_id.clone()),
            source_name: (name != *source_name).then(|| source_name.clone()),
            config,
            notes,
            disabled_in_source,
        });
    }
    Ok(McpImportCandidatesResult {
        path: path.to_string_lossy().into_owned(),
        candidates,
        skipped,
    })
}

fn unreadable_reason(path: &Path, shape: Shape) -> String {
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => format!("{} is not a file", path.display()),
        Ok(metadata) if metadata.len() > MAX_CONFIG_BYTES => format!(
            "{} is larger than the {} MB import reads",
            path.display(),
            MAX_CONFIG_BYTES / (1024 * 1024)
        ),
        Ok(_) => format!(
            "{} is not valid {}",
            path.display(),
            if shape == Shape::Codex {
                "TOML"
            } else {
                "JSON"
            }
        ),
        Err(_) => format!("{} could not be read", path.display()),
    }
}

/// Claude Code's shape: `type` is `stdio` (the default with a `command`),
/// `http`, or `sse`.
fn claude_server(server: &Map<String, Value>) -> Result<Parsed, String> {
    let kind =
        server
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(if server.contains_key("command") {
                "stdio"
            } else {
                "http"
            });
    let mut notes = Vec::new();
    let transport = match kind {
        "stdio" => McpServerTransport::Stdio {
            command: command(server)?,
            args: strings(server.get("args"))?,
            env: values(server.get("env"), environment_secret)?,
        },
        "http" | "streamable-http" => {
            if server.contains_key("headersHelper") {
                notes.push("Headers its headersHelper command adds are not imported".into());
            }
            McpServerTransport::Http {
                url: url(server)?,
                headers: values(server.get("headers"), |_, _| true)?,
            }
        }
        "sse" => return Err("It uses SSE, which Ottam does not support".into()),
        other => return Err(format!("Its type, {other}, is not one Ottam runs")),
    };
    Ok(Parsed {
        transport,
        notes,
        disabled_in_source: false,
    })
}

/// Codex's shape: a `command` or a `url`, with settings Ottam has no
/// place for called out.
fn codex_server(server: &Map<String, Value>) -> Result<Parsed, String> {
    let mut notes = Vec::new();
    let disabled_in_source = server.get("enabled") == Some(&Value::Bool(false));
    if disabled_in_source {
        notes.push("It is turned off in Codex".into());
    }
    if let Some(tools) = tool_names(server.get("enabled_tools")) {
        notes.push(format!(
            "Codex offers only {tools}; every one of its tools is offered here"
        ));
    }
    if let Some(tools) = tool_names(server.get("disabled_tools")) {
        notes.push(format!("Codex hides {tools}; they are offered here"));
    }
    let transport = if server.contains_key("command") {
        if let Some(cwd) = server.get("cwd").and_then(Value::as_str) {
            notes.push(format!("Its working directory, {cwd}, is not kept"));
        }
        for name in server
            .get("env_vars")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            notes.push(format!(
                "Codex passes {name} through from its environment; add it here to keep it"
            ));
        }
        McpServerTransport::Stdio {
            command: command(server)?,
            args: strings(server.get("args"))?,
            env: values(server.get("env"), environment_secret)?,
        }
    } else {
        if let Some(variable) = server.get("bearer_token_env_var").and_then(Value::as_str) {
            notes.push(format!(
                "Codex reads its bearer token from {variable}; add it as an Authorization header"
            ));
        }
        for (header, variable) in server
            .get("env_http_headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            notes.push(format!(
                "Codex sets {header} from {}; add its value here",
                variable.as_str().unwrap_or("an environment variable")
            ));
        }
        McpServerTransport::Http {
            url: url(server)?,
            headers: values(server.get("http_headers"), |_, _| true)?,
        }
    };
    Ok(Parsed {
        transport,
        notes,
        disabled_in_source,
    })
}

/// A Codex tool list, as a sentence fragment.
fn tool_names(value: Option<&Value>) -> Option<String> {
    let names: Vec<&str> = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn command(server: &Map<String, Value>) -> Result<String, String> {
    server
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "It has no command".into())
}

fn url(server: &Map<String, Value>) -> Result<String, String> {
    server
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "It has no command or URL".into())
}

/// Arguments as text. Anything else would change the command, so the
/// server is skipped instead.
fn strings(value: Option<&Value>) -> Result<Vec<String>, String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|value| scalar(value).ok_or_else(|| "One of its arguments is not text".into()))
        .collect()
}

fn values(
    map: Option<&Value>,
    secret: impl Fn(&str, &str) -> bool,
) -> Result<Vec<McpConfigValue>, String> {
    map.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| {
            let value = scalar(value).ok_or_else(|| format!("{name} has no text value"))?;
            Ok(McpConfigValue {
                secret: secret(name, &value),
                name: name.clone(),
                value: Some(value),
            })
        })
        .collect()
}

/// Text, or a number or boolean written as text; `None` for null, arrays,
/// and objects.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// An environment value is secret when its name looks like a credential's
/// or it is a URL with a password, such as a database connection string.
fn environment_secret(name: &str, value: &str) -> bool {
    credential(name) || reqwest::Url::parse(value).is_ok_and(|url| url.password().is_some())
}

/// Whether a variable or parameter name looks like it holds a credential.
fn credential(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
        "DSN",
    ]
    .iter()
    .any(|word| name.contains(word))
        || name.ends_with("KEY")
        || name.contains("_KEY")
        || name == "PAT"
        || name.ends_with("_PAT")
}

/// A URL is stored as written, so one carrying a password or a key in its
/// query is called out.
fn url_credential_note(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let in_query = parsed.query_pairs().any(|(name, _)| credential(&name));
    (parsed.password().is_some() || in_query).then(|| {
        "Its URL carries a credential, which is stored as written; a secret header keeps it in the Keychain".into()
    })
}

/// Whether core takes `name` as it is.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// A valid name for `name`: letters, digits, `-` and `_`, other characters
/// becoming `_`, numbered past any already taken.
fn valid_name(name: &str, taken: &BTreeSet<String>) -> Option<String> {
    let base: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(MAX_NAME_LEN)
        .collect();
    if !base
        .chars()
        .any(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }
    (1..)
        .map(|index| {
            if index == 1 {
                base.clone()
            } else {
                let suffix = format!("_{index}");
                let keep = base.len().min(MAX_NAME_LEN - suffix.len());
                format!("{}{suffix}", &base[..keep])
            }
        })
        .find(|candidate| !taken.contains(candidate))
}

/// A note per part that names an environment variable core will not
/// expand, as Claude Code does for `${VAR}`.
fn unexpanded(transport: &McpServerTransport) -> Vec<String> {
    let mut parts = Vec::new();
    let values = match transport {
        McpServerTransport::Stdio { command, args, env } => {
            if command.contains("${") {
                parts.push("The command".to_owned());
            }
            for (index, arg) in args.iter().enumerate() {
                if arg.contains("${") {
                    parts.push(format!("Argument {}", index + 1));
                }
            }
            env
        }
        McpServerTransport::Http { url, headers } => {
            if url.contains("${") {
                parts.push("The URL".to_owned());
            }
            headers
        }
    };
    for value in values {
        if value
            .value
            .as_deref()
            .is_some_and(|text| text.contains("${"))
        {
            parts.push(value.name.clone());
        }
    }
    parts
        .into_iter()
        .map(|part| {
            format!(
                "{part} uses ${{…}}, which Ottam does not expand; replace it with the value"
            )
        })
        .collect()
}

fn unreadable(message: impl Into<String>) -> CoreError {
    CoreError::new("mcpImportUnreadable", message).recoverable(true)
}

#[cfg(test)]
mod tests {
    use super::{read, setup_path, Shape};
    use ottam_protocol::{
        McpConfigValue, McpServerConfig, McpServerID, McpServerState, McpServerStatus,
        McpServerSummary, McpServerTransport,
    };
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write(directory: &TempDir, file: &str, text: &str) -> std::path::PathBuf {
        let path = directory.path().join(file);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn value(name: &str, value: &str, secret: bool) -> McpConfigValue {
        McpConfigValue {
            name: name.into(),
            value: Some(value.into()),
            secret,
        }
    }

    #[test]
    fn claude_servers_import_with_credentials_marked_secret() {
        let home = TempDir::new().unwrap();
        let path = write(
            &home,
            ".claude.json",
            r#"{"mcpServers": {
                "docs search": {"command": "uvx", "args": ["docs-mcp", 3],
                                "env": {"DOCS_API_KEY": "abc", "MODE": "fast"}},
                "github": {"type": "http", "url": "https://api.example.com/mcp",
                           "headers": {"X-Team": "core"}},
                "legacy": {"type": "sse", "url": "https://old.example.com/sse"},
                "broken": {"type": "stdio"},
                "???": {"command": "x"}
            },
            "projects": {"/work": {"mcpServers": {"local": {"command": "y"}}}}}"#,
        );
        let configured = [McpServerSummary {
            server_id: McpServerID("existing".into()),
            config: McpServerConfig {
                name: "github".into(),
                transport: McpServerTransport::Http {
                    url: "https://elsewhere.example.com".into(),
                    headers: Vec::new(),
                },
            },
            disabled_workspace_ids: Vec::new(),
            state: McpServerState {
                status: McpServerStatus::NotStarted,
                error: None,
            },
            signed_in: false,
        }];
        let result = read(&path, Shape::Claude, &configured).unwrap();

        let names: Vec<_> = result
            .candidates
            .iter()
            .map(|c| c.config.name.as_str())
            .collect();
        assert_eq!(names, ["docs_search", "github"], "user scope only");
        let docs = &result.candidates[0];
        assert_eq!(docs.source_name.as_deref(), Some("docs search"));
        assert_eq!(
            docs.config.transport,
            McpServerTransport::Stdio {
                command: "uvx".into(),
                args: vec!["docs-mcp".into(), "3".into()],
                env: vec![
                    value("DOCS_API_KEY", "abc", true),
                    value("MODE", "fast", false)
                ],
            }
        );
        let github = &result.candidates[1];
        assert_eq!(
            github.existing_server_id,
            Some(McpServerID("existing".into()))
        );
        assert_eq!(
            github.config.transport,
            McpServerTransport::Http {
                url: "https://api.example.com/mcp".into(),
                headers: vec![value("X-Team", "core", true)],
            },
            "every header is treated as a credential"
        );
        let skipped: Vec<_> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                ("???", "Its name has no letters or digits"),
                ("broken", "It has no command"),
                ("legacy", "It uses SSE, which Ottam does not support"),
            ]
        );
    }

    #[test]
    fn codex_servers_import_with_what_does_not_carry_over_noted() {
        let home = TempDir::new().unwrap();
        let path = write(
            &home,
            "config.toml",
            "[mcp_servers.fs]\ncommand = 'npx'\nargs = ['-y', 'fs-mcp', '${HOME}/notes']\n\
             cwd = '/work'\nenv_vars = ['GITHUB_TOKEN']\nenabled = false\n\
             [mcp_servers.fs.env]\nDATABASE_URL = 'postgres://app:pw@db/prod'\nLOG = 'debug'\n\
             [mcp_servers.search]\nurl = 'https://search.example.com/mcp'\n\
             bearer_token_env_var = 'SEARCH_TOKEN'\ndisabled_tools = ['purge', 'reindex']\n\
             [mcp_servers.search.http_headers]\nX-Region = 'eu'\n",
        );
        let result = read(&path, Shape::Codex, &[]).unwrap();
        assert!(result.skipped.is_empty());
        let fs = &result.candidates[0];
        assert_eq!(
            fs.config.transport,
            McpServerTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "fs-mcp".into(), "${HOME}/notes".into()],
                env: vec![
                    value("DATABASE_URL", "postgres://app:pw@db/prod", true),
                    value("LOG", "debug", false)
                ],
            },
            "a connection string with a password is a secret"
        );
        assert!(fs.disabled_in_source);
        assert_eq!(
            fs.notes,
            [
                "It is turned off in Codex",
                "Its working directory, /work, is not kept",
                "Codex passes GITHUB_TOKEN through from its environment; add it here to keep it",
                "Argument 3 uses ${…}, which Ottam does not expand; replace it with the value",
            ]
        );
        let search = &result.candidates[1];
        assert_eq!(
            search.config.transport,
            McpServerTransport::Http {
                url: "https://search.example.com/mcp".into(),
                headers: vec![value("X-Region", "eu", true)],
            }
        );
        assert!(!search.disabled_in_source);
        assert_eq!(
            search.notes,
            [
                "Codex hides purge, reindex; they are offered here",
                "Codex reads its bearer token from SEARCH_TOKEN; add it as an Authorization header",
            ]
        );
    }

    #[test]
    fn a_repositorys_mcp_json_is_read_from_its_directory() {
        let repo = TempDir::new().unwrap();
        write(
            &repo,
            ".mcp.json",
            r#"{"mcpServers": {
                "db": {"command": "db-mcp", "args": ["${A}", "--root", "${B}"],
                       "env": {"DB_URL": "${DATABASE_URL}"}},
                "my db": {"command": "a"}, "my_db": {"command": "b"},
                "api": {"type": "http", "url": "${API_URL}/mcp"},
                "keyed": {"type": "http", "url": "https://k.example.com/mcp?api_key=abc"},
                "nulls": {"command": "x", "args": [null]},
                "unset": {"command": "x", "env": {"OPT": null}}
            }}"#,
        );
        let path = super::mcp_json(repo.path()).unwrap();
        let result = read(&path, Shape::Claude, &[]).unwrap();
        let names: Vec<_> = result
            .candidates
            .iter()
            .map(|c| (c.config.name.as_str(), c.source_name.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                ("db", None),
                ("keyed", None),
                ("my_db_2", Some("my db")),
                ("my_db", None)
            ],
            "a valid name is kept, and a rewritten one goes around it"
        );
        assert_eq!(
            result.candidates[0].notes,
            [
                "Argument 1 uses ${…}, which Ottam does not expand; replace it with the value",
                "Argument 3 uses ${…}, which Ottam does not expand; replace it with the value",
                "DB_URL uses ${…}, which Ottam does not expand; replace it with the value",
            ]
        );
        assert_eq!(
            result.candidates[1].notes,
            ["Its URL carries a credential, which is stored as written; a secret header keeps it in the Keychain"]
        );
        let skipped: Vec<_> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                (
                    "api",
                    "A remote server needs a valid URL (it uses ${…}, which Ottam does not expand)"
                ),
                ("nulls", "One of its arguments is not text"),
                ("unset", "OPT has no text value"),
            ],
            "what mcp.add would refuse is not offered"
        );
        assert!(super::mcp_json(&repo.path().join("missing")).is_err());
        assert!(super::mcp_json(Path::new("/nonexistent/.mcp.json")).is_err());
    }

    #[test]
    fn a_setup_without_its_file_has_no_servers() {
        let home = TempDir::new().unwrap();
        let result = read(&home.path().join("config.toml"), Shape::Codex, &[]).unwrap();
        assert!(result.candidates.is_empty() && result.skipped.is_empty());

        let broken = write(&home, ".claude.json", "{not json");
        let error = read(&broken, Shape::Claude, &[]).unwrap_err();
        assert!(
            error.message.ends_with("is not valid JSON"),
            "{}",
            error.message
        );
    }

    #[test]
    fn each_setup_is_read_from_its_own_file() {
        let setup_home = PathBuf::from("/setup");
        let home = Some(PathBuf::from("/home/me"));
        let mut environment = BTreeMap::new();
        let (path, shape) = setup_path("codex", setup_home.clone(), &environment, None).unwrap();
        assert_eq!(
            (path, shape),
            (PathBuf::from("/setup/config.toml"), Shape::Codex)
        );
        let (path, _) =
            setup_path("claude", setup_home.clone(), &environment, home.clone()).unwrap();
        assert_eq!(path, PathBuf::from("/home/me/.claude.json"));
        environment.insert("CLAUDE_CONFIG_DIR".to_owned(), None);
        let (path, _) =
            setup_path("claude", setup_home.clone(), &environment, home.clone()).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/me/.claude.json"),
            "a removed variable moves nothing"
        );
        environment.insert("CLAUDE_CONFIG_DIR".to_owned(), Some("/setup".to_owned()));
        let (path, _) = setup_path("claude", setup_home, &environment, home).unwrap();
        assert_eq!(path, PathBuf::from("/setup/.claude.json"));
    }
}
