use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value, json};
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

use super::{CliError, Result};

pub(super) fn read(path: &Path) -> Result<Value> {
    parse(&fs::read_to_string(path)?)
}

pub(super) fn parse(source: &str) -> Result<Value> {
    let documents =
        YamlLoader::load_from_str(source).map_err(|e| CliError::invalid(e.to_string()))?;
    let [document] = documents.as_slice() else {
        return Err(CliError::invalid(
            "expected exactly one YAML or JSON document",
        ));
    };
    let value = from_yaml(document)?;
    if !value.is_object() {
        return Err(CliError::invalid("workspace document must be a mapping"));
    }
    Ok(value)
}

fn from_yaml(value: &Yaml) -> Result<Value> {
    Ok(match value {
        Yaml::Null => Value::Null,
        Yaml::Boolean(v) => Value::Bool(*v),
        Yaml::Integer(v) => json!(v),
        Yaml::Real(v) => serde_json::from_str(v).map_err(|_| {
            CliError::invalid("non-finite YAML numbers cannot be represented in JSON")
        })?,
        Yaml::String(v) => json!(v),
        Yaml::Array(values) => Value::Array(values.iter().map(from_yaml).collect::<Result<_>>()?),
        Yaml::Hash(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let key = key.as_str().ok_or_else(|| {
                        CliError::invalid("document mapping keys must be strings")
                    })?;
                    Ok((key.to_owned(), from_yaml(value)?))
                })
                .collect::<Result<_>>()?,
        ),
        _ => return Err(CliError::invalid("unsupported YAML value")),
    })
}

pub(super) fn encode(value: &Value, format: &str) -> Result<String> {
    if format == "json" {
        return Ok(serde_json::to_string_pretty(value)? + "\n");
    }
    let json = serde_json::to_string(value)?;
    let documents =
        YamlLoader::load_from_str(&json).map_err(|e| CliError::invalid(e.to_string()))?;
    let mut output = String::new();
    YamlEmitter::new(&mut output)
        .dump(&documents[0])
        .map_err(|e| CliError::invalid(e.to_string()))?;
    output.push('\n');
    Ok(output)
}

pub(super) fn save(path: &Path, value: &Value, format: &str, force: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(encode(value, format)?.as_bytes())?;
    file.as_file().sync_all()?;
    if force {
        file.persist(path).map_err(|e| CliError::from(e.error))?;
    } else {
        file.persist_noclobber(path)
            .map_err(|e| CliError::from(e.error))?;
    }
    Ok(())
}

pub(super) fn import(kind: &str, source: &Value) -> Result<Value> {
    let source = if kind == "teamocil" {
        source.get("session").unwrap_or(source)
    } else {
        source
    };
    let name = source
        .get("project_name")
        .or_else(|| source.get("name"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut result = json!({"session_name": name, "windows": []});
    if let Some(root) = source.get("project_root").or_else(|| source.get("root")) {
        result["start_directory"] = root.clone();
    }
    let entries = source
        .get("tabs")
        .or_else(|| source.get("windows"))
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::invalid("import source requires a windows list"))?;
    let mut windows = Vec::new();
    if kind == "tmuxinator" {
        import_tmuxinator(source, entries, &mut result, &mut windows)?;
    } else {
        import_teamocil(entries, &mut windows)?;
    }
    result["windows"] = json!(windows);
    Ok(result)
}

fn import_tmuxinator(
    source: &Value,
    entries: &[Value],
    result: &mut Value,
    windows: &mut Vec<Value>,
) -> Result<()> {
    if let Some(args) = source
        .get("cli_args")
        .or_else(|| source.get("tmux_options"))
        .and_then(Value::as_str)
    {
        result["config"] = json!(args.replace("-f", "").trim());
    }
    if let Some(socket) = source.get("socket_name") {
        result["socket_name"] = socket.clone();
    }
    if let Some(pre) = source.get("pre") {
        let before = source.get("pre_window").unwrap_or(pre);
        result["shell_command_before"] = if before.is_array() {
            before.clone()
        } else {
            json!([before])
        };
        if source.get("pre_window").is_some() {
            result["shell_command"] = pre.clone();
        }
    }
    if let Some(rbenv) = source.get("rbenv") {
        if result.get("shell_command_before").is_none() {
            result["shell_command_before"] = json!([]);
        }
        if let Some(before) = result["shell_command_before"].as_array_mut() {
            before.push(json!(format!("rbenv shell {}", scalar(rbenv))));
        }
    }
    for entry in entries {
        let mapping = entry
            .as_object()
            .ok_or_else(|| CliError::invalid("tmuxinator windows must be mappings"))?;
        for (name, value) in mapping {
            let mut window = json!({"window_name":name});
            if value.is_string() || value.is_null() {
                window["panes"] = json!([value]);
            } else if value.is_array() {
                window["panes"] = value.clone();
            } else if value.is_object() {
                for (from, to) in [
                    ("pre", "shell_command_before"),
                    ("panes", "panes"),
                    ("root", "start_directory"),
                    ("layout", "layout"),
                ] {
                    if let Some(v) = value.get(from) {
                        window[to] = v.clone();
                    }
                }
            } else {
                return Err(CliError::invalid("invalid tmuxinator window"));
            }
            windows.push(window);
        }
    }
    Ok(())
}

fn import_teamocil(entries: &[Value], windows: &mut Vec<Value>) -> Result<()> {
    for value in entries {
        let mut window = json!({"window_name":value.get("name").cloned().unwrap_or(Value::Null)});
        for (from, to) in [
            ("root", "start_directory"),
            ("layout", "layout"),
            ("clear", "clear"),
        ] {
            if let Some(v) = value.get(from) {
                window[to] = v.clone();
            }
        }
        for (from, to) in [
            ("before", "shell_command_before"),
            ("after", "shell_command_after"),
        ] {
            if let Some(v) = value.get("filters").and_then(|v| v.get(from)) {
                window[to] = v.clone();
            }
        }
        if let Some(panes) = value.get("splits").or_else(|| value.get("panes")) {
            let mut panes = panes
                .as_array()
                .ok_or_else(|| CliError::invalid("teamocil panes must be a list"))?
                .clone();
            for pane in &mut panes {
                if let Some(map) = pane.as_object_mut() {
                    if let Some(cmd) = map.remove("cmd") {
                        map.insert("shell_command".into(), cmd);
                    }
                    map.remove("width");
                }
            }
            window["panes"] = json!(panes);
        }
        windows.push(window);
    }
    Ok(())
}

pub(super) fn scalar(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

pub(super) fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| CliError::invalid("expected a mapping"))
}
