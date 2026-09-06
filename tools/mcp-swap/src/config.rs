//! Format-aware reads and byte-preserving server-entry edits.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toml_edit::{
    Array, DocumentMut, InlineTable, Item, Table, TableLike, Value as TomlValue, value,
};

use crate::jsonc;

/// The configuration layer selected for Claude.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The top-level user fallback.
    User,
    /// The current repository's project entry.
    Project,
}

/// One portable stdio MCP server definition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSpec {
    /// Executable name or path.
    pub command: String,
    /// Arguments passed after the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment overlay written into the client entry.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// The semantic result of an entry edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// A new server entry was added.
    Added,
    /// An existing server entry was replaced.
    Replaced,
    /// An existing server entry was removed.
    Removed,
    /// The requested state already existed.
    Unchanged,
}

/// Bytes and semantic outcome produced by a configuration edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit {
    /// Complete configuration bytes after the edit.
    pub bytes: Vec<u8>,
    /// Whether the entry was added, replaced, removed, or unchanged.
    pub action: Action,
}

/// An invalid or unsupported client configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConfigError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Json,
    Jsonc,
    Toml,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dialect {
    Standard,
    Claude,
    Opencode,
}

/// Static configuration syntax for one supported client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientConfig {
    name: &'static str,
    format: Format,
    dialect: Dialect,
    container: &'static [&'static str],
}

impl ClientConfig {
    /// Return the descriptor for one canonical client name.
    #[must_use]
    pub fn for_name(name: &str) -> Option<Self> {
        let config = match name {
            "claude" => Self {
                name: "claude",
                format: Format::Json,
                dialect: Dialect::Claude,
                container: &["mcpServers"],
            },
            "codex" | "grok" => Self {
                name: if name == "codex" { "codex" } else { "grok" },
                format: Format::Toml,
                dialect: Dialect::Standard,
                container: &["mcp_servers"],
            },
            "cursor" | "gemini" | "agy" => Self {
                name: match name {
                    "cursor" => "cursor",
                    "gemini" => "gemini",
                    _ => "agy",
                },
                format: Format::Json,
                dialect: Dialect::Standard,
                container: &["mcpServers"],
            },
            "opencode" => Self {
                name: "opencode",
                format: Format::Jsonc,
                dialect: Dialect::Opencode,
                container: &["mcp"],
            },
            "pi" => Self {
                name: "pi",
                format: Format::Jsonc,
                dialect: Dialect::Standard,
                container: &["mcpServers"],
            },
            _ => return None,
        };
        Some(config)
    }

    /// Canonical client name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn path(&self, repo: &Path, scope: Scope) -> Vec<String> {
        if self.name == "claude" && scope == Scope::Project {
            vec![
                "projects".into(),
                repo.to_string_lossy().into_owned(),
                "mcpServers".into(),
            ]
        } else {
            self.container.iter().map(|part| (*part).into()).collect()
        }
    }
}

/// Read one server entry from complete client configuration bytes.
///
/// # Errors
///
/// Returns [`ConfigError`] for malformed syntax, a non-object container, or
/// an invalid server entry.
pub fn read_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
    repo: &Path,
    scope: Scope,
) -> Result<Option<ServerSpec>, ConfigError> {
    match client.format {
        Format::Json | Format::Jsonc => {
            let document = parse_json(client, bytes)?;
            let Some(container) = get_json_container(&document, &client.path(repo, scope))? else {
                return Ok(None);
            };
            container
                .get(server)
                .map(|entry| spec_from_json(entry, client.dialect))
                .transpose()
        }
        Format::Toml => read_toml_server(client, bytes, server),
    }
}

/// Add or replace one server entry without rewriting unrelated JSON/JSONC.
///
/// # Errors
///
/// Returns [`ConfigError`] when the document or target container is invalid.
pub fn set_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
    spec: &ServerSpec,
    repo: &Path,
    scope: Scope,
) -> Result<Edit, ConfigError> {
    if read_server(client, bytes, server, repo, scope)?.as_ref() == Some(spec) {
        return Ok(Edit {
            bytes: bytes.to_vec(),
            action: Action::Unchanged,
        });
    }
    let existed = read_server(client, bytes, server, repo, scope)?.is_some();
    let edited = match client.format {
        Format::Json | Format::Jsonc => {
            let mut document = parse_json(client, bytes)?;
            let container = ensure_json_container(&mut document, &client.path(repo, scope))?;
            container.insert(server.into(), spec_to_json(spec, client.dialect));
            render_json(client, bytes, &document)?
        }
        Format::Toml => set_toml_server(client, bytes, server, spec)?,
    };
    Ok(Edit {
        bytes: edited,
        action: if existed {
            Action::Replaced
        } else {
            Action::Added
        },
    })
}

/// Remove one server entry while preserving unrelated configuration bytes.
///
/// # Errors
///
/// Returns [`ConfigError`] when the document or target container is invalid.
pub fn delete_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
    repo: &Path,
    scope: Scope,
) -> Result<Edit, ConfigError> {
    if read_server(client, bytes, server, repo, scope)?.is_none() {
        return Ok(Edit {
            bytes: bytes.to_vec(),
            action: Action::Unchanged,
        });
    }
    let edited = match client.format {
        Format::Json | Format::Jsonc => {
            let mut document = parse_json(client, bytes)?;
            if let Some(container) =
                get_json_container_mut(&mut document, &client.path(repo, scope))?
            {
                container.remove(server);
            }
            render_json(client, bytes, &document)?
        }
        Format::Toml => delete_toml_server(client, bytes, server)?,
    };
    Ok(Edit {
        bytes: edited,
        action: Action::Removed,
    })
}

fn parse_json(client: &ClientConfig, bytes: &[u8]) -> Result<Value, ConfigError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        let mut root = Map::new();
        if client.dialect == Dialect::Opencode {
            root.insert(
                "$schema".into(),
                Value::String("https://opencode.ai/config.json".into()),
            );
        }
        return Ok(Value::Object(root));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ConfigError::new(format!("{} config is not UTF-8: {error}", client.name))
    })?;
    let value = match client.format {
        Format::Json => jsonc::parse_json(text)
            .map_err(|error| ConfigError::new(format!("{} JSON: {error}", client.name)))?,
        Format::Jsonc => jsonc::parse(text)
            .map_err(|error| ConfigError::new(format!("{} JSONC: {error}", client.name)))?,
        Format::Toml => return Err(ConfigError::new("internal format mismatch")),
    };
    if !value.is_object() {
        return Err(ConfigError::new(format!(
            "{} config root must be an object",
            client.name
        )));
    }
    Ok(value)
}

fn render_json(
    client: &ClientConfig,
    original: &[u8],
    document: &Value,
) -> Result<Vec<u8>, ConfigError> {
    if original.iter().all(u8::is_ascii_whitespace) {
        let mut rendered = serde_json::to_vec_pretty(document)
            .map_err(|error| ConfigError::new(format!("render JSON: {error}")))?;
        rendered.push(b'\n');
        return Ok(rendered);
    }
    let source = std::str::from_utf8(original).map_err(|error| {
        ConfigError::new(format!("{} config is not UTF-8: {error}", client.name))
    })?;
    jsonc::merge(source, document)
        .map(String::into_bytes)
        .map_err(|error| ConfigError::new(format!("render {} config: {error}", client.name)))
}

fn get_json_container<'a>(
    root: &'a Value,
    path: &[String],
) -> Result<Option<&'a Map<String, Value>>, ConfigError> {
    let mut value = root;
    for part in path {
        let object = value
            .as_object()
            .ok_or_else(|| ConfigError::new(format!("{} must be an object", path.join("."))))?;
        let Some(next) = object.get(part) else {
            return Ok(None);
        };
        value = next;
    }
    value
        .as_object()
        .map(Some)
        .ok_or_else(|| ConfigError::new(format!("{} must be an object", path.join("."))))
}

fn get_json_container_mut<'a>(
    root: &'a mut Value,
    path: &[String],
) -> Result<Option<&'a mut Map<String, Value>>, ConfigError> {
    let mut value = root;
    for part in path {
        let object = value
            .as_object_mut()
            .ok_or_else(|| ConfigError::new(format!("{} must be an object", path.join("."))))?;
        let Some(next) = object.get_mut(part) else {
            return Ok(None);
        };
        value = next;
    }
    value
        .as_object_mut()
        .map(Some)
        .ok_or_else(|| ConfigError::new(format!("{} must be an object", path.join("."))))
}

fn ensure_json_container<'a>(
    root: &'a mut Value,
    path: &[String],
) -> Result<&'a mut Map<String, Value>, ConfigError> {
    let mut value = root;
    for part in path {
        let object = value
            .as_object_mut()
            .ok_or_else(|| ConfigError::new(format!("{} must be an object", path.join("."))))?;
        value = object
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !value.is_object() {
            return Err(ConfigError::new(format!(
                "{} must be an object",
                path.join(".")
            )));
        }
    }
    value
        .as_object_mut()
        .ok_or_else(|| ConfigError::new("target container must be an object"))
}

fn spec_to_json(spec: &ServerSpec, dialect: Dialect) -> Value {
    let mut entry = Map::new();
    match dialect {
        Dialect::Claude => {
            entry.insert("type".into(), Value::String("stdio".into()));
            entry.insert("command".into(), Value::String(spec.command.clone()));
            entry.insert("args".into(), strings_to_json(&spec.args));
            entry.insert("env".into(), environment_to_json(&spec.env));
        }
        Dialect::Opencode => {
            let command = std::iter::once(&spec.command)
                .chain(spec.args.iter())
                .cloned()
                .map(Value::String)
                .collect();
            entry.insert("type".into(), Value::String("local".into()));
            entry.insert("command".into(), Value::Array(command));
            if !spec.env.is_empty() {
                entry.insert("environment".into(), environment_to_json(&spec.env));
            }
        }
        Dialect::Standard => {
            entry.insert("command".into(), Value::String(spec.command.clone()));
            entry.insert("args".into(), strings_to_json(&spec.args));
            if !spec.env.is_empty() {
                entry.insert("env".into(), environment_to_json(&spec.env));
            }
        }
    }
    Value::Object(entry)
}

fn strings_to_json(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn environment_to_json(values: &BTreeMap<String, String>) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

fn spec_from_json(value: &Value, dialect: Dialect) -> Result<ServerSpec, ConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| ConfigError::new("server entry must be an object"))?;
    let (command, args, environment_key) = if dialect == Dialect::Opencode {
        let command = object
            .get("command")
            .and_then(Value::as_array)
            .ok_or_else(|| ConfigError::new("opencode command must be a non-empty string array"))?;
        let mut parts = command
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ConfigError::new("opencode command must contain only strings"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if parts.is_empty() {
            return Err(ConfigError::new(
                "opencode command must be a non-empty string array",
            ));
        }
        let command = parts.remove(0);
        (command, parts, "environment")
    } else {
        let command = object
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ConfigError::new("server command must be a string"))?
            .to_owned();
        let args = json_strings(object.get("args"), "server args")?;
        (command, args, "env")
    };
    let env = json_environment(object.get(environment_key))?;
    Ok(ServerSpec { command, args, env })
}

fn json_strings(value: Option<&Value>, label: &str) -> Result<Vec<String>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| ConfigError::new(format!("{label} must be an array")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ConfigError::new(format!("{label} must contain only strings")))
        })
        .collect()
}

fn json_environment(value: Option<&Value>) -> Result<BTreeMap<String, String>, ConfigError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| ConfigError::new("server environment must be an object"))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| ConfigError::new("server environment values must be strings"))
        })
        .collect()
}

fn parse_toml(bytes: &[u8], client: &ClientConfig) -> Result<DocumentMut, ConfigError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ConfigError::new(format!("{} config is not UTF-8: {error}", client.name))
    })?;
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse()
        .map_err(|error| ConfigError::new(format!("{} TOML: {error}", client.name)))
}

fn toml_container<'a>(
    document: &'a DocumentMut,
    client: &ClientConfig,
) -> Result<Option<&'a dyn TableLike>, ConfigError> {
    let Some(item) = document.get(client.container[0]) else {
        return Ok(None);
    };
    item.as_table_like()
        .map(Some)
        .ok_or_else(|| ConfigError::new("mcp_servers must be a TOML table"))
}

fn read_toml_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
) -> Result<Option<ServerSpec>, ConfigError> {
    let document = parse_toml(bytes, client)?;
    let Some(container) = toml_container(&document, client)? else {
        return Ok(None);
    };
    let Some(entry) = container.get(server) else {
        return Ok(None);
    };
    let table = entry
        .as_table_like()
        .ok_or_else(|| ConfigError::new("server entry must be a TOML table"))?;
    let command = table
        .get("command")
        .and_then(Item::as_str)
        .ok_or_else(|| ConfigError::new("server command must be a string"))?
        .to_owned();
    let args = match table.get("args") {
        None => Vec::new(),
        Some(item) => item
            .as_array()
            .ok_or_else(|| ConfigError::new("server args must be an array"))?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ConfigError::new("server args must contain only strings"))
            })
            .collect::<Result<_, _>>()?,
    };
    let env = match table.get("env") {
        None => BTreeMap::new(),
        Some(item) => item
            .as_table_like()
            .ok_or_else(|| ConfigError::new("server env must be a TOML table"))?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.to_owned(), value.to_owned()))
                    .ok_or_else(|| ConfigError::new("server env values must be strings"))
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(Some(ServerSpec { command, args, env }))
}

fn ensure_toml_container<'a>(
    document: &'a mut DocumentMut,
    client: &ClientConfig,
) -> Result<&'a mut dyn TableLike, ConfigError> {
    if document.get(client.container[0]).is_none() {
        document.insert(client.container[0], Item::Table(Table::new()));
    }
    document
        .get_mut(client.container[0])
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| ConfigError::new("mcp_servers must be a TOML table"))
}

fn set_toml_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
    spec: &ServerSpec,
) -> Result<Vec<u8>, ConfigError> {
    let mut document = parse_toml(bytes, client)?;
    let container_is_inline = document
        .get(client.container[0])
        .is_some_and(Item::is_inline_table);
    let container = ensure_toml_container(&mut document, client)?;
    if container.get(server).is_none() {
        container.insert(server, empty_toml_table(container_is_inline));
    }
    let entry_is_inline = container.get(server).is_some_and(Item::is_inline_table);
    let table = container
        .get_mut(server)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| ConfigError::new("server entry must be a TOML table"))?;
    let stale: Vec<_> = table
        .iter()
        .filter(|(key, _)| !matches!(*key, "command" | "args" | "env"))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in stale {
        table.remove(&key);
    }
    table.insert("command", value(spec.command.clone()));
    let mut args = Array::new();
    for argument in &spec.args {
        args.push(argument.as_str());
    }
    table.insert("args", value(args));
    if spec.env.is_empty() {
        table.remove("env");
    } else {
        if table.get("env").is_none() {
            table.insert("env", empty_toml_table(entry_is_inline));
        }
        let env = table
            .get_mut("env")
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| ConfigError::new("server env must be a TOML table"))?;
        let stale: Vec<_> = env
            .iter()
            .filter(|(key, _)| !spec.env.contains_key(*key))
            .map(|(key, _)| key.to_owned())
            .collect();
        for key in stale {
            env.remove(&key);
        }
        for (key, value) in &spec.env {
            env.insert(key, toml_edit::value(value.clone()));
        }
    }
    Ok(document.to_string().into_bytes())
}

fn delete_toml_server(
    client: &ClientConfig,
    bytes: &[u8],
    server: &str,
) -> Result<Vec<u8>, ConfigError> {
    let mut document = parse_toml(bytes, client)?;
    if let Some(container) = document
        .get_mut(client.container[0])
        .and_then(Item::as_table_like_mut)
    {
        container.remove(server);
    }
    Ok(document.to_string().into_bytes())
}

fn empty_toml_table(inline: bool) -> Item {
    if inline {
        Item::Value(TomlValue::InlineTable(InlineTable::new()))
    } else {
        Item::Table(Table::new())
    }
}
