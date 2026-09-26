use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::{CliError, Result, discovery};

fn mapping<'a>(value: &'a Value, allowed: &[&str], path: &str) -> Result<&'a Map<String, Value>> {
    let value = value
        .as_object()
        .ok_or_else(|| CliError::invalid(format!("{path} must be a mapping")))?;
    for key in value.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(CliError::invalid(format!(
                "unsupported import field {path}.{key}; use the source tool for this behavior"
            )));
        }
    }
    Ok(value)
}

fn alias<'a>(value: &'a Map<String, Value>, names: &[&str], path: &str) -> Result<&'a Value> {
    let mut found = None;
    for name in names {
        if let Some(value) = value.get(*name) {
            if found.is_some() {
                return Err(CliError::invalid(format!(
                    "{path}: supply only one of {}",
                    names.join(", ")
                )));
            }
            found = Some(value);
        }
    }
    Ok(found.unwrap_or(&Value::Null))
}

fn commands(value: &Value, path: &str) -> Result<Vec<String>> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.is_null())
            .map(|(index, value)| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    CliError::invalid(format!("{path}[{index}] must be a string or null"))
                })
            })
            .collect(),
        _ => Err(CliError::invalid(format!(
            "{path} must be a string or list of strings"
        ))),
    }
}

fn sequence(commands: impl IntoIterator<Item = String>) -> Value {
    Value::Array(
        commands
            .into_iter()
            .map(|command| json!({"cmd":command}))
            .collect(),
    )
}

fn group(value: &Value, separator: &str, path: &str) -> Result<Value> {
    let commands = commands(value, path)?;
    Ok(if commands.is_empty() {
        json!([])
    } else {
        sequence([commands.join(separator)])
    })
}

fn focus(panes: &mut [Value]) {
    let selected = panes
        .iter()
        .position(|pane| pane["focus"] == true)
        .unwrap_or(0);
    for (index, pane) in panes.iter_mut().enumerate() {
        pane["focus"] = json!(index == selected);
    }
}

fn list<'a>(value: &'a Value, path: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| CliError::invalid(format!("{path} must be a list")))
}

fn copy(value: &Map<String, Value>, result: &mut Value, names: &[(&str, &str)]) {
    for (from, to) in names {
        if let Some(value) = value.get(*from) {
            result[*to] = value.clone();
        }
    }
}

// Tmuxinator expands ERB before it parses YAML, and nothing here does, so
// markup that reaches this point would otherwise become a literal command.
fn reject_template_text(text: &str) -> Result<()> {
    if text.contains("<%") {
        return Err(CliError::invalid(
            "tmuxinator ERB templates are unsupported; expand them before import",
        ));
    }
    Ok(())
}

fn reject_templates(value: &Value) -> Result<()> {
    match value {
        Value::String(text) => reject_template_text(text),
        Value::Array(values) => values.iter().try_for_each(reject_templates),
        Value::Object(values) => {
            for (key, value) in values {
                reject_template_text(key)?;
                reject_templates(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn workspace(kind: &str, source: &Value, path: &Path) -> Result<Value> {
    if kind == "tmuxinator" {
        reject_templates(source)?;
    }
    let source = if kind == "teamocil" && source.get("session").is_some() {
        mapping(source, &["session"], "teamocil")?;
        &source["session"]
    } else {
        source
    };
    let allowed = if kind == "tmuxinator" {
        &[
            "name",
            "project_name",
            "root",
            "project_root",
            "windows",
            "tabs",
            "pre_window",
            "pre_tab",
        ][..]
    } else {
        &["name", "root", "windows"][..]
    };
    let map = mapping(source, allowed, kind)?;
    let name = alias(map, &["name", "project_name"], kind)?.clone();
    // teamocil's current format has no session name; the document starts
    // at `windows:`, so fall back to the file's own name.
    let name = if kind == "teamocil" && name.is_null() {
        let stem = path.file_stem().and_then(|stem| stem.to_str()).ok_or_else(|| {
            CliError::invalid(
                "teamocil: session_name is required and could not be derived from the import path",
            )
        })?;
        json!(stem)
    } else {
        name
    };
    let root = alias(map, &["root", "project_root"], kind)?;
    let cwd = std::env::current_dir()?;
    let root = match root {
        Value::Null => cwd,
        Value::String(root) => cwd.join(PathBuf::from(discovery::expand(root))),
        _ => return Err(CliError::invalid(format!("{kind}.root must be a string"))),
    };
    let root = root
        .to_str()
        .ok_or_else(|| CliError::invalid("import directory must be UTF-8"))?;
    let entries = list(alias(map, &["windows", "tabs"], kind)?, "windows")?;
    let mut windows = entries
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("{kind}.windows[{index}]");
            if kind == "tmuxinator" {
                tmuxinator_window(value, &path)
            } else {
                teamocil_window(value, &path)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    focus(&mut windows);
    let mut result = json!({"session_name":name,"start_directory":root,"windows":windows});
    if kind == "tmuxinator" {
        result["shell_command_before"] = group(
            alias(map, &["pre_window", "pre_tab"], kind)?,
            "; ",
            "tmuxinator.pre_window",
        )?;
    }
    Ok(result)
}

fn tmuxinator_window(value: &Value, path: &str) -> Result<Value> {
    let map = value
        .as_object()
        .filter(|map| map.len() == 1)
        .ok_or_else(|| CliError::invalid(format!("{path} must contain exactly one window name")))?;
    let (name, value) = map
        .iter()
        .next()
        .ok_or_else(|| CliError::invalid("window name is required"))?;
    let mut result = json!({"window_name":name});
    let mut panes = if value.is_object() {
        let map = mapping(
            value,
            &["root", "layout", "pre", "panes", "synchronize"],
            path,
        )?;
        copy(
            map,
            &mut result,
            &[("root", "start_directory"), ("layout", "layout")],
        );
        let values = if value["panes"].is_null() {
            &[][..]
        } else {
            list(&value["panes"], &format!("{path}.panes"))?
        };
        if map.contains_key("pre") && values.is_empty() {
            return Err(CliError::invalid(format!(
                "{path}.pre requires explicit panes"
            )));
        }
        result["shell_command_before"] = group(&value["pre"], " && ", &format!("{path}.pre"))?;
        match &value["synchronize"] {
            Value::Null | Value::Bool(false) => {}
            Value::String(mode) if mode == "after" => {
                result["options_after"] = json!({"synchronize-panes":true});
            }
            _ => {
                return Err(CliError::invalid(format!(
                    "{path}.synchronize supports only false or after; before requires sequential pane creation"
                )));
            }
        }
        values.iter().enumerate().map(|(index, value)| {
            Ok(json!({"shell_command":sequence(commands(value, &format!("{path}.panes[{index}]"))?)}))
        }).collect::<Result<Vec<_>>>()?
    } else {
        vec![json!({"shell_command":sequence(commands(value, path)?)})]
    };
    if panes.is_empty() {
        panes.push(json!({"shell_command":[]}));
    }
    focus(&mut panes);
    result["panes"] = json!(panes);
    Ok(result)
}

fn teamocil_window(value: &Value, path: &str) -> Result<Value> {
    let map = mapping(
        value,
        &[
            "name", "root", "layout", "panes", "splits", "focus", "options",
        ],
        path,
    )?;
    let mut result = json!({});
    copy(
        map,
        &mut result,
        &[
            ("name", "window_name"),
            ("root", "start_directory"),
            ("layout", "layout"),
            ("focus", "focus"),
            ("options", "options"),
        ],
    );
    let sync = &value["options"]["synchronize-panes"];
    if !sync.is_null() && sync != false && !matches!(sync.as_str(), Some("off" | "false" | "0")) {
        return Err(CliError::invalid(format!(
            "{path}.options.synchronize-panes requires sequential pane creation"
        )));
    }
    let values = alias(map, &["panes", "splits"], path)?;
    let values = if values.is_null() {
        &[][..]
    } else {
        list(values, &format!("{path}.panes"))?
    };
    let mut panes = values.iter().enumerate().map(|(index, value)| {
        let path = format!("{path}.panes[{index}]");
        if value.is_object() {
            let map = mapping(value, &["commands", "cmd", "focus"], &path)?;
            if map.get("commands").is_some_and(|v| !v.is_null() && !v.is_array()) {
                return Err(CliError::invalid(format!("{path}.commands must be a list")));
            }
            let mut pane = json!({"shell_command":group(alias(map, &["commands", "cmd"], &path)?, "; ", &format!("{path}.commands"))?});
            copy(map, &mut pane, &[("focus", "focus")]);
            if pane.get("focus").is_some_and(|v| !v.is_boolean()) {
                return Err(CliError::invalid(format!("{path}.focus must be a boolean")));
            }
            Ok(pane)
        } else {
            Ok(json!({"shell_command":group(value, "; ", &path)?}))
        }
    }).collect::<Result<Vec<_>>>()?;
    if panes.is_empty() {
        panes.push(json!({"shell_command":[]}));
    }
    focus(&mut panes);
    result["panes"] = json!(panes);
    if result.get("focus").is_some_and(|v| !v.is_boolean()) {
        return Err(CliError::invalid(format!("{path}.focus must be a boolean")));
    }
    Ok(result)
}
