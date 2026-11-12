//! Detection reads only configuration, never auth.json or keychain data.
use super::{HarnessError, HarnessErrorKind, HomeLayout};
use ottam_protocol::AgentDetectedSetup;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub(crate) struct DetectedSetup {
    pub summary: AgentDetectedSetup,
    pub layout: HomeLayout,
}

pub(crate) fn summary(harness: &str) -> AgentDetectedSetup {
    match detect(harness) {
        Ok(setup) => setup.summary,
        Err(error) => {
            let setup = super::existing_setup(harness);
            AgentDetectedSetup {
                harness_id: harness.into(),
                display_name: setup.map_or(harness, |setup| setup.title).into(),
                path: "Unavailable".into(),
                available: false,
                detail: Some(error.message),
                cli_version: None,
                skills: None,
                hooks: None,
                mcp_servers: None,
                plugins: None,
            }
        }
    }
}

pub(crate) fn detect(harness: &str) -> Result<DetectedSetup, HarnessError> {
    let setup = super::existing_setup(harness).ok_or_else(|| {
        HarnessError::new(
            HarnessErrorKind::AgentNotFound,
            "Existing setups are available for Codex, Claude Code, and OpenCode.",
        )
    })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = super::registry::command_output_with_timeout(
        setup_command(&shell, setup.variables, setup.cli)?,
        Duration::from_secs(3),
    )
    .map_err(|_| {
        HarnessError::new(
            HarnessErrorKind::Internal,
            "Could not read your login-shell setup. Check your shell configuration and retry.",
        )
    })?;
    let marker = b"\x1eOTTAM_SETUP\0";
    let index = output
        .stdout
        .windows(marker.len())
        .rposition(|part| part == marker)
        .ok_or_else(|| {
            HarnessError::new(
                HarnessErrorKind::Internal,
                "Login shell did not report its setup.",
            )
        })?;
    let fields: Vec<_> = output.stdout[index + marker.len()..]
        .split(|byte| *byte == 0)
        .collect();
    if fields.len() < setup.variables.len() * 2 + 1 {
        return Err(HarnessError::new(
            HarnessErrorKind::Internal,
            "Incomplete login-shell setup",
        ));
    }
    let mut environment = BTreeMap::new();
    for (index, variable) in setup.variables.iter().enumerate() {
        let value = (fields[index * 2] == b"set")
            .then(|| String::from_utf8_lossy(fields[index * 2 + 1]).into_owned());
        match value.as_deref() {
            Some("") => return Err(HarnessError::new(
                HarnessErrorKind::Internal,
                format!(
                    "{variable} is empty. Unset it or use an absolute path in your login shell."
                ),
            )),
            // OpenCode resolves a relative database against its data home.
            Some(value) if !Path::new(value).is_absolute() && *variable != "OPENCODE_DB" => {
                return Err(HarnessError::new(
                    HarnessErrorKind::Internal,
                    format!("{variable} must be an absolute path."),
                ))
            }
            _ => {}
        }
        environment.insert(variable.to_string(), value);
    }
    let user_home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        HarnessError::new(
            HarnessErrorKind::Internal,
            "Cannot locate your home directory",
        )
    })?;
    let set = |variable: &str| {
        environment
            .get(variable)
            .cloned()
            .flatten()
            .map(PathBuf::from)
    };
    let cli =
        PathBuf::from(String::from_utf8_lossy(fields[setup.variables.len() * 2]).into_owned());
    let cli_version = if cli.is_absolute() {
        super::registry::command_output_args_with_timeout(
            &cli.to_string_lossy(),
            &["--version"],
            Duration::from_secs(3),
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .chars()
                .take(160)
                .collect()
        })
    } else {
        None
    };
    let (path, mut summary) = match harness {
        "opencode" => {
            // `join` keeps an absolute OPENCODE_DB as is.
            let database = set("XDG_DATA_HOME")
                .unwrap_or_else(|| user_home.join(".local/share"))
                .join("opencode")
                .join(set("OPENCODE_DB").unwrap_or_else(|| "opencode.db".into()));
            let config = set("OPENCODE_CONFIG_DIR").unwrap_or_else(|| {
                set("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| user_home.join(".config"))
                    .join("opencode")
            });
            let path = database
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| database.clone());
            let summary = inspect_opencode(setup, &path, &database, &config);
            (path, summary)
        }
        "omp" => {
            let path = set("PI_CODING_AGENT_DIR").unwrap_or_else(|| user_home.join(".omp/agent"));
            let summary = inspect_omp(setup, &path);
            (path, summary)
        }
        _ => {
            let variable = setup.variables[0];
            let custom = set(variable);
            let path = custom.clone().unwrap_or_else(|| {
                user_home.join(if harness == "codex" {
                    ".codex"
                } else {
                    ".claude"
                })
            });
            let metadata_path = if custom.is_some() {
                path.join(".claude.json")
            } else {
                user_home.join(".claude.json")
            };
            let summary = inspect_with_metadata(harness, setup.title, &path, &metadata_path);
            (path, summary)
        }
    };
    summary.cli_version = cli_version;
    let layout = HomeLayout::Existing { path, environment };
    Ok(DetectedSetup { summary, layout })
}

/// Interactive login shells load the same startup files as a terminal window,
/// including .zshrc. Serialize only the selected home variables and CLI path:
/// each variable as whether it is set, then its value.
fn setup_command(shell: &str, variables: &[&str], cli: &str) -> Result<Command, HarnessError> {
    // All identifiers originate from the fixed `existing_setup` table.
    let name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let format = format!(
        "\\036OTTAM_SETUP\\000{}%s\\000",
        "%s\\000%s\\000".repeat(variables.len())
    );
    let script = match name {
        "zsh" | "bash" | "sh" | "dash" | "ksh" => {
            let values: String = variables
                .iter()
                .map(|variable| format!(" \"${{{variable}+set}}\" \"${{{variable}-}}\""))
                .collect();
            format!("printf '{format}'{values} \"$(command -v {cli})\"")
        }
        "fish" => {
            let mut script = format!("set -l cli_path (type -p {cli});");
            let mut values = String::new();
            for (index, variable) in variables.iter().enumerate() {
                script.push_str(&format!(" set -l present{index} ''; set -l value{index} ''; if set -q {variable}; set present{index} set; set value{index} \"${variable}\"; end;"));
                values.push_str(&format!(" \"$present{index}\" \"$value{index}\""));
            }
            format!("{script} printf '{format}'{values} \"$cli_path\"")
        }
        "csh" | "tcsh" => {
            let mut script = String::new();
            let mut values = String::new();
            for (index, variable) in variables.iter().enumerate() {
                script.push_str(&format!("set present{index} = ''\nset value{index} = ''\nif ( $?{variable} ) then\nset present{index} = set\nset value{index} = \"${variable}\"\nendif\n"));
                values.push_str(&format!(" \"$present{index}\" \"$value{index}\""));
            }
            format!("{script}printf '{format}'{values} \"`which {cli}`\"")
        }
        _ => return Err(HarnessError::new(HarnessErrorKind::Internal, "Your login shell is not supported for setup detection. Use an Ottam profile or choose a supported login shell.")),
    };
    let mut command = Command::new(shell);
    if matches!(name, "csh" | "tcsh") {
        // csh's -l must be its only argument. A login argv[0] supports -c.
        command.arg0(format!("-{name}")).args(["-i", "-c", &script]);
    } else {
        command.args(["-i", "-l", "-c", &script]);
    }
    Ok(command)
}

/// OpenCode's setup by file presence alone: its database holds sign-in, so
/// core never opens it.
fn inspect_opencode(
    setup: super::ExistingSetup,
    path: &Path,
    database: &Path,
    config: &Path,
) -> AgentDetectedSetup {
    let readable = std::fs::read_dir(path).is_ok() && database.is_file();
    let detail = if readable {
        "OpenCode 2 upgrades this database in place. OpenCode 1.x, and editor plugins that launch it, may not open it afterward."
    } else if path.exists() {
        "Your OpenCode data could not be read, or holds no database. Sign in with the OpenCode CLI first, or choose an Ottam profile."
    } else {
        "No existing setup was found. Set up the CLI first or choose a separate profile."
    };
    // OpenCode merges both files from its config directory.
    let settings: Option<Vec<serde_json::Value>> = ["opencode.json", "opencode.jsonc"]
        .iter()
        .map(|name| config.join(name))
        .filter(|path| path.is_file())
        .map(|path| read_config(path, false))
        .collect();
    let configured = std::fs::read_dir(config).is_ok();
    AgentDetectedSetup {
        harness_id: "opencode".into(),
        display_name: setup.title.into(),
        path: path.to_string_lossy().into_owned(),
        available: readable,
        detail: Some(detail.into()),
        cli_version: None,
        skills: if configured {
            count_skills(&config.join("skills"))
        } else {
            Some(0)
        },
        hooks: None,
        mcp_servers: settings
            .as_ref()
            .map(|settings| count_opencode_mcp(settings)),
        plugins: settings.as_ref().map(|settings| {
            // `plugins` is v2's key; `plugin` is v1's, which v2 still reads.
            settings
                .iter()
                .flat_map(|settings| ["plugin", "plugins"].map(|key| settings.get(key)))
                .flatten()
                .filter_map(serde_json::Value::as_array)
                .map(|plugins| plugins.len() as u64)
                .sum()
        }),
    }
}

/// omp's setup by file presence alone: `agent.db` holds its credentials.
fn inspect_omp(setup: super::ExistingSetup, path: &Path) -> AgentDetectedSetup {
    let readable = std::fs::read_dir(path).is_ok() && path.join("agent.db").is_file();
    let detail = if readable {
        "Ottam uses this agent directory with omp's default profile. Named profiles stay as they are."
    } else if path.exists() {
        "Your Oh-My-Pi setup could not be read, or has no sign-in yet. Run omp login first, or choose an Ottam profile."
    } else {
        "No existing setup was found. Set up the CLI first or choose a separate profile."
    };
    AgentDetectedSetup {
        harness_id: "omp".into(),
        display_name: setup.title.into(),
        path: path.to_string_lossy().into_owned(),
        available: readable,
        detail: Some(detail.into()),
        cli_version: None,
        skills: None,
        hooks: None,
        mcp_servers: None,
        plugins: None,
    }
}

/// OpenCode's MCP servers by name: v2 nests them under `mcp.servers`; v1
/// listed them directly under `mcp`, beside v2's `timeout`.
fn count_opencode_mcp(settings: &[serde_json::Value]) -> u64 {
    let mut names = std::collections::BTreeSet::new();
    for mcp in settings
        .iter()
        .filter_map(|settings| settings.get("mcp")?.as_object())
    {
        for (name, value) in mcp {
            match (name.as_str(), value.as_object()) {
                ("servers", Some(servers)) => names.extend(servers.keys().cloned()),
                ("timeout", _) => {}
                _ => {
                    names.insert(name.clone());
                }
            }
        }
    }
    names.len() as u64
}

fn inspect_with_metadata(
    harness: &str,
    title: &str,
    path: &Path,
    metadata_path: &Path,
) -> AgentDetectedSetup {
    let readable = std::fs::read_dir(path).is_ok();
    let detail = (!readable).then(|| {
        if path.exists() {
            "Your CLI setup cannot be read. Check its permissions or choose a separate profile."
        } else {
            "No existing setup was found. Set up the CLI first or choose a separate profile."
        }
        .into()
    });
    let config = if harness == "codex" {
        read_config(path.join("config.toml"), true)
    } else {
        read_config(path.join("settings.json"), false)
    };
    let mcp_config = if harness == "claude" {
        // The default Claude metadata file is a non-secret configuration file.
        read_config(metadata_path.to_path_buf(), false)
    } else {
        config.clone()
    };
    AgentDetectedSetup {
        harness_id: harness.into(),
        display_name: title.into(),
        path: path.to_string_lossy().into_owned(),
        available: readable,
        detail,
        cli_version: None,
        skills: readable
            .then(|| count_skills(&path.join("skills")))
            .flatten(),
        hooks: readable
            .then(|| count_config_map(config.as_ref(), "hooks"))
            .flatten(),
        mcp_servers: readable
            .then(|| {
                count_config_map(
                    mcp_config.as_ref(),
                    if harness == "codex" {
                        "mcp_servers"
                    } else {
                        "mcpServers"
                    },
                )
            })
            .flatten(),
        plugins: readable
            .then(|| {
                config
                    .as_ref()
                    .and_then(|value| value.get("enabledPlugins"))
                    .and_then(|value| value.as_object())
                    .map(|plugins| {
                        plugins
                            .values()
                            .filter(|enabled| enabled.as_bool() == Some(true))
                            .count() as u64
                    })
            })
            .flatten(),
    }
}

fn count_config_map(config: Option<&serde_json::Value>, key: &str) -> Option<u64> {
    match config?.as_object()?.get(key) {
        None => Some(0),
        Some(value) => value.as_object().map(|entries| entries.len() as u64),
    }
}

/// The largest config file `read_config` reads.
pub(crate) const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

/// A config file as JSON: `{}` when it is missing, `None` when it cannot be
/// read or parsed, or is over 2 MiB. JSONC is accepted.
pub(crate) fn read_config(path: PathBuf, is_toml: bool) -> Option<serde_json::Value> {
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.len() <= MAX_CONFIG_BYTES => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(serde_json::json!({}))
        }
        _ => return None,
    };
    if !metadata.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    if is_toml {
        serde_json::to_value(toml::from_str::<toml::Value>(&text).ok()?).ok()
    } else {
        serde_json::from_str(&strip_jsonc(&text)).ok()
    }
}

/// JSON with comments and trailing commas, as OpenCode's `.jsonc` allows,
/// reduced to plain JSON. Plain JSON passes through unchanged.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push(c);
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            (']' | '}', _) => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn count_skills(path: &Path) -> Option<u64> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry.ok()?;
        if entry.path().join("SKILL.md").is_file() {
            count += 1;
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interactive_zsh_reads_custom_and_unset_home_from_zshrc() {
        if !Path::new("/bin/zsh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let rc = root.path().join(".zshrc");
        std::fs::write(&rc, "export CODEX_HOME='/tmp/ottam custom home'\n").unwrap();
        let mut command = setup_command("/bin/zsh", &["CODEX_HOME"], "codex").unwrap();
        command.env("ZDOTDIR", root.path());
        let output =
            super::super::registry::command_output_with_timeout(command, Duration::from_secs(3))
                .unwrap();
        assert!(output.status.success());
        assert!(output
            .stdout
            .windows(b"set\0/tmp/ottam custom home\0".len())
            .any(|part| part == b"set\0/tmp/ottam custom home\0"));
        std::fs::write(&rc, "unset CODEX_HOME\n").unwrap();
        let mut command = setup_command("/bin/zsh", &["CODEX_HOME"], "codex").unwrap();
        command
            .env("ZDOTDIR", root.path())
            .env("CODEX_HOME", "/tmp/inherited");
        let output =
            super::super::registry::command_output_with_timeout(command, Duration::from_secs(3))
                .unwrap();
        assert!(output
            .stdout
            .windows(b"OTTAM_SETUP\0\0\0".len())
            .any(|part| part == b"OTTAM_SETUP\0\0\0"));
    }

    #[test]
    fn interactive_zsh_reports_each_opencode_variable_in_order() {
        if !Path::new("/bin/zsh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".zshrc"),
            "export XDG_DATA_HOME=/tmp/data\nunset XDG_CONFIG_HOME OPENCODE_DB\nexport OPENCODE_CONFIG_DIR=/tmp/config\n",
        )
        .unwrap();
        let setup = crate::agent::existing_setup("opencode").unwrap();
        let mut command = setup_command("/bin/zsh", setup.variables, setup.cli).unwrap();
        command.env("ZDOTDIR", root.path());
        let output =
            super::super::registry::command_output_with_timeout(command, Duration::from_secs(3))
                .unwrap();
        let expected = b"OTTAM_SETUP\0set\0/tmp/data\0\0\0\0\0set\0/tmp/config\0";
        assert!(
            output
                .stdout
                .windows(expected.len())
                .any(|part| part == expected),
            "{:?}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn opencode_setup_is_judged_by_file_presence() {
        let setup = crate::agent::existing_setup("opencode").unwrap();
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data/opencode");
        let config = root.path().join("config/opencode");
        let database = data.join("opencode.db");
        let missing = inspect_opencode(setup, &data, &database, &config);
        assert!(!missing.available);
        std::fs::create_dir_all(&data).unwrap();
        assert!(!inspect_opencode(setup, &data, &database, &config).available);
        // Core never opens the database; any file there is a setup.
        std::fs::write(&database, b"not sqlite").unwrap();
        let found = inspect_opencode(setup, &data, &database, &config);
        assert!(found.available);
        assert!(found.detail.unwrap().contains("upgrades this database"));
        assert_eq!(
            (found.skills, found.mcp_servers, found.plugins),
            (Some(0), Some(0), Some(0))
        );
        std::fs::create_dir_all(config.join("skills/review")).unwrap();
        std::fs::write(config.join("skills/review/SKILL.md"), "").unwrap();
        std::fs::write(
            config.join("opencode.json"),
            r#"{"mcp":{"docs":{"type":"remote"}},"plugin":["one","two"]}"#,
        )
        .unwrap();
        let configured = inspect_opencode(setup, &data, &database, &config);
        assert_eq!(
            (
                configured.skills,
                configured.mcp_servers,
                configured.plugins
            ),
            (Some(1), Some(1), Some(2))
        );
        // v2's `.jsonc`, merged with the `.json`: comments, trailing commas,
        // `plugins`, and servers nested under `mcp.servers`.
        std::fs::write(
            config.join("opencode.jsonc"),
            "{\n  // mine\n  \"mcp\": {\"servers\": {\"docs\": {}, \"git\": {},}, \"timeout\": {}},\n  /* v2 */ \"plugins\": [\"three\", \"http://x//y\"],\n}\n",
        )
        .unwrap();
        let merged = inspect_opencode(setup, &data, &database, &config);
        assert_eq!((merged.mcp_servers, merged.plugins), (Some(2), Some(4)));
    }

    #[test]
    fn shell_scripts_are_specific_and_unknown_shells_fail_explicitly() {
        let fish = setup_command("/opt/bin/fish", &["CODEX_HOME"], "codex").unwrap();
        assert!(fish
            .get_args()
            .last()
            .unwrap()
            .to_string_lossy()
            .contains("set -q CODEX_HOME"));
        let tcsh = setup_command("/bin/tcsh", &["CLAUDE_CONFIG_DIR"], "claude").unwrap();
        assert!(tcsh
            .get_args()
            .last()
            .unwrap()
            .to_string_lossy()
            .contains("$?CLAUDE_CONFIG_DIR"));
        assert!(setup_command("/bin/unknown", &["CODEX_HOME"], "codex").is_err());
    }

    #[test]
    fn tcsh_login_reads_terminal_home_configuration() {
        if !Path::new("/bin/tcsh").exists() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".tcshrc"),
            "setenv CLAUDE_CONFIG_DIR '/tmp/ottam tcsh home'\n",
        )
        .unwrap();
        let mut command = setup_command("/bin/tcsh", &["CLAUDE_CONFIG_DIR"], "claude").unwrap();
        command.env("HOME", root.path());
        let output =
            super::super::registry::command_output_with_timeout(command, Duration::from_secs(3))
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output
                .stdout
                .windows(b"set\0/tmp/ottam tcsh home\0".len())
                .any(|part| part == b"set\0/tmp/ottam tcsh home\0"),
            "{:?}",
            output.stdout
        );
    }
    fn inspect(harness: &str, title: &str, path: &Path) -> AgentDetectedSetup {
        inspect_with_metadata(harness, title, path, &path.join(".claude.json"))
    }
    #[test]
    fn claude_metadata_path_is_independent_of_settings_home() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".claude");
        std::fs::create_dir(&home).unwrap();
        let metadata = root.path().join(".claude.json");
        std::fs::write(
            &metadata,
            r#"{"mcpServers":{"shared":{"command":"example"}}}"#,
        )
        .unwrap();
        let default = inspect_with_metadata("claude", "Claude Code", &home, &metadata);
        assert_eq!(default.mcp_servers, Some(1));
        let custom =
            inspect_with_metadata("claude", "Claude Code", &home, &home.join(".claude.json"));
        assert_eq!(custom.mcp_servers, Some(0));
    }
    #[test]
    fn setup_detection_distinguishes_missing_and_unknown_from_zero() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".codex");
        assert!(!inspect("codex", "Codex", &path).available);
        std::fs::create_dir(&path).unwrap();
        let empty = inspect("codex", "Codex", &path);
        assert_eq!(empty.skills, Some(0));
        assert_eq!(empty.mcp_servers, Some(0));
        assert_eq!(empty.plugins, None);
        std::fs::write(
            path.join("config.toml"),
            "[mcp_servers.example]\ncommand = 'example'\n",
        )
        .unwrap();
        assert_eq!(inspect("codex", "Codex", &path).mcp_servers, Some(1));
        std::fs::write(path.join("config.toml"), "[[broken").unwrap();
        assert_eq!(inspect("codex", "Codex", &path).mcp_servers, None);
    }
}
