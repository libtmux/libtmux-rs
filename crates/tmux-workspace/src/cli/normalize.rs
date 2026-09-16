use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::{CliError, Result, discovery, document};

pub(super) struct Workspace {
    pub(super) source: PathBuf,
    pub(super) name: String,
    pub(super) directory: PathBuf,
    pub(super) environment: Vec<(String, String)>,
    pub(super) options: Vec<(String, String)>,
    pub(super) global_options: Vec<(String, String)>,
    pub(super) before_script: Option<String>,
    pub(super) script_directory: PathBuf,
    pub(super) bridge: bool,
    pub(super) readiness: Option<bool>,
    pub(super) windows: Vec<Window>,
}

pub(super) struct Window {
    pub(super) name: Option<String>,
    pub(super) index: Option<i32>,
    pub(super) layout: Option<String>,
    pub(super) focus: bool,
    pub(super) options: Vec<(String, String)>,
    pub(super) options_after: Vec<(String, String)>,
    pub(super) panes: Vec<Pane>,
}

pub(super) struct Pane {
    pub(super) directory: PathBuf,
    pub(super) environment: Vec<(String, String)>,
    pub(super) shell: Option<String>,
    pub(super) focus: bool,
    pub(super) commands: Vec<TypedCommand>,
}

pub(super) struct TypedCommand {
    pub(super) text: String,
    pub(super) enter: bool,
    pub(super) before: Duration,
    pub(super) after: Duration,
}

fn boolean(value: &Value, default: bool, name: &str) -> Result<bool> {
    match value {
        Value::Null => Ok(default),
        Value::Bool(value) => Ok(*value),
        Value::String(value) => match value.as_str() {
            "true" | "yes" | "on" | "1" => Ok(true),
            "false" | "no" | "off" | "0" => Ok(false),
            _ => Err(CliError::invalid(format!("{name} must be a boolean"))),
        },
        _ => Err(CliError::invalid(format!("{name} must be a boolean"))),
    }
}

fn text(value: &Value, name: &str) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|v| Some(discovery::expand(v)))
        .ok_or_else(|| CliError::invalid(format!("{name} must be a string")))
}

fn directory(value: &Value, parent: &Path) -> Result<PathBuf> {
    let path = text(value, "start_directory")?.map_or_else(|| parent.to_owned(), PathBuf::from);
    Ok(if path.is_absolute() {
        path
    } else {
        parent.join(path)
    })
}

pub(super) fn pairs(value: &Value, options: bool) -> Result<Vec<(String, String)>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    document::object(value)?
        .iter()
        .map(|(key, value)| {
            if value.is_array() || value.is_object() || value.is_null() {
                return Err(CliError::invalid(format!("{key} must have a scalar value")));
            }
            let value = if options && value.is_boolean() {
                if value == true {
                    "on".into()
                } else {
                    "off".into()
                }
            } else {
                discovery::expand(&document::scalar(value))
            };
            Ok((key.to_owned(), value))
        })
        .collect()
}

fn keys(value: &Value, names: &[&str], scope: &str) -> Result<()> {
    for key in document::object(value)?.keys() {
        if !names.contains(&key.as_str()) {
            return Err(CliError::invalid(format!(
                "unsupported execution key {scope}.{key}; conversion preserves this key"
            )));
        }
    }
    Ok(())
}

fn entries(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(values)
            if values.len() == 1
                && (values[0].is_null()
                    || matches!(values[0].as_str(), Some("blank" | "pane"))) =>
        {
            Vec::new()
        }
        Value::Array(values) => values.clone(),
        Value::String(value) if matches!(value.as_str(), "blank" | "pane") => Vec::new(),
        Value::Object(value) if value.contains_key("shell_command") => {
            entries(&value["shell_command"])
        }
        value => vec![value.clone()],
    }
}

fn delay(value: &Value, default: Duration, name: &str) -> Result<Duration> {
    if value.is_null() {
        return Ok(default);
    }
    let seconds = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        .ok_or_else(|| CliError::invalid(format!("{name} must be a nonnegative number")))?;
    Duration::try_from_secs_f64(seconds)
        .map_err(|_| CliError::invalid(format!("{name} must be a finite nonnegative number")))
}

fn commands(values: Vec<Value>, pane: &Value, suppress: bool) -> Result<Vec<TypedCommand>> {
    let mut enter = boolean(&pane["enter"], true, "enter")?;
    let mut before = delay(&pane["sleep_before"], Duration::ZERO, "sleep_before")?;
    let mut after = delay(&pane["sleep_after"], Duration::ZERO, "sleep_after")?;
    let mut result = Vec::new();
    for value in values {
        let value = if value.is_object() {
            keys(
                &value,
                &["cmd", "enter", "sleep_before", "sleep_after"],
                "command",
            )?;
            enter = boolean(&value["enter"], enter, "enter")?;
            if let Some(value) = value.get("sleep_before") {
                before = delay(value, Duration::ZERO, "sleep_before")?;
            }
            if let Some(value) = value.get("sleep_after") {
                after = delay(value, Duration::ZERO, "sleep_after")?;
            }
            value["cmd"].clone()
        } else {
            value
        };
        let Some(mut text) = text(&value, "command")? else {
            continue;
        };
        if suppress {
            text.insert(0, ' ');
        }
        result.push(TypedCommand {
            text,
            enter,
            before,
            after,
        });
    }
    Ok(result)
}

pub(super) fn workspace(value: &Value, path: &Path) -> Result<Workspace> {
    keys(
        value,
        &[
            "session_name",
            "start_directory",
            "environment",
            "options",
            "global_options",
            "shell_command_before",
            "suppress_history",
            "windows",
            "before_script",
            "plugins",
            "workspace_builder",
            "workspace_builder_options",
            "workspace_builder_paths",
            "config",
            "socket_name",
        ],
        "workspace",
    )?;
    let name = text(&value["session_name"], "session_name")?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| CliError::invalid("session_name is required"))?;
    // tmux stores the name verbatim, then uses `:` and `.` as the window and
    // pane separators in every `-t` target, so a name that contains either
    // can be created but never addressed again (H8).
    if let Some(separator) = name.chars().find(|c| matches!(c, ':' | '.')) {
        return Err(CliError::invalid(format!(
            "session_name must not contain {separator:?}; tmux reads it as a target separator and the session could not be addressed afterward"
        )));
    }
    let bridge = extension_bridge(value)?;
    if !bridge {
        for key in ["config", "socket_name"] {
            if value.get(key).is_some() {
                return Err(CliError::invalid(format!(
                    "unsupported execution key workspace.{key}; select the endpoint with CLI flags"
                )));
            }
        }
    }
    if !bridge && !value["workspace_builder_options"].is_null() {
        keys(
            &value["workspace_builder_options"],
            &["pane_readiness"],
            "workspace_builder_options",
        )?;
    }
    let readiness = readiness(&value["workspace_builder_options"])?;
    // With no start_directory at all, panes start in the invocation
    // directory, matching tmuxp; go, java and rs used to start them in the
    // workspace file's directory instead (H9). An explicit start_directory,
    // relative or not, still resolves against the document's directory —
    // that part was already correct and is unchanged here.
    let directory = if value.get("start_directory").is_some() {
        directory(
            &value["start_directory"],
            path.parent().unwrap_or_else(|| Path::new(".")),
        )?
    } else {
        std::env::current_dir()?
    };
    let suppress = boolean(&value["suppress_history"], true, "suppress_history")?;
    let source_windows = value["windows"]
        .as_array()
        .ok_or_else(|| CliError::invalid("windows must be a list"))?;
    if source_windows.is_empty() {
        return Err(CliError::invalid("workspace requires at least one window"));
    }
    let mut indexes = BTreeSet::new();
    let windows = source_windows
        .iter()
        .map(|source| window(source, value, &directory, suppress, &mut indexes))
        .collect::<Result<Vec<_>>>()?;
    Ok(Workspace {
        source: path.to_owned(),
        name,
        script_directory: directory.clone(),
        directory,
        environment: pairs(&value["environment"], false)?,
        options: pairs(&value["options"], true)?,
        global_options: pairs(&value["global_options"], true)?,
        before_script: text(&value["before_script"], "before_script")?,
        bridge,
        readiness,
        windows,
    })
}

fn extension_bridge(value: &Value) -> Result<bool> {
    let plugins = match value.get("plugins") {
        None => false,
        Some(Value::Array(plugins)) => {
            for (index, plugin) in plugins.iter().enumerate() {
                if !plugin.is_string() {
                    return Err(CliError::invalid(format!(
                        "plugins[{index}] must be a string"
                    )));
                }
            }
            !plugins.is_empty()
        }
        Some(_) => return Err(CliError::invalid("plugins must be a list of strings")),
    };
    let builder = match value.get("workspace_builder") {
        None | Some(Value::Null) => false,
        Some(Value::String(builder)) => !builder.is_empty(),
        Some(_) => {
            return Err(CliError::invalid(
                "workspace_builder must be a string or null",
            ));
        }
    };
    Ok(plugins || builder)
}

fn readiness(options: &Value) -> Result<Option<bool>> {
    if options.is_null() {
        return Ok(None);
    }
    let value = document::object(options)?
        .get("pane_readiness")
        .unwrap_or(&Value::Null);
    if value.is_null() {
        return Ok(None);
    }
    match document::scalar(value).trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(None),
        "always" | "true" | "yes" | "on" | "1" => Ok(Some(true)),
        "never" | "false" | "no" | "off" | "0" => Ok(Some(false)),
        _ => Err(CliError::invalid(
            "pane_readiness must be auto, always/true/on/yes/1, or never/false/off/no/0",
        )),
    }
}

fn window(
    window: &Value,
    value: &Value,
    directory: &Path,
    suppress: bool,
    indexes: &mut BTreeSet<i32>,
) -> Result<Window> {
    keys(
        window,
        &[
            "window_name",
            "window_index",
            "window_shell",
            "start_directory",
            "environment",
            "options",
            "options_after",
            "layout",
            "focus",
            "shell_command_before",
            "suppress_history",
            "panes",
        ],
        "window",
    )?;
    let window_directory = self::directory(&window["start_directory"], directory)?;
    let environment = pairs(&window["environment"], false)?;
    let window_suppress = boolean(&window["suppress_history"], suppress, "suppress_history")?;
    let index = if window["window_index"].is_null() {
        None
    } else {
        let index = document::scalar(&window["window_index"])
            .parse::<i32>()
            .map_err(|_| CliError::invalid("window_index must be a nonnegative integer"))?;
        if index < 0 || !indexes.insert(index) {
            return Err(CliError::invalid(
                "window_index must be nonnegative and unique",
            ));
        }
        Some(index)
    };
    let mut source_panes = if window["panes"].is_null() {
        vec![Value::Null]
    } else {
        window["panes"]
            .as_array()
            .ok_or_else(|| CliError::invalid("panes must be a list"))?
            .clone()
    };
    if source_panes.is_empty() {
        source_panes.push(Value::Null);
    }
    let panes = source_panes
        .into_iter()
        .map(|source| {
            pane(
                source,
                value,
                window,
                &window_directory,
                &environment,
                window_suppress,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Window {
        name: text(&window["window_name"], "window_name")?,
        index,
        layout: text(&window["layout"], "layout")?,
        focus: boolean(&window["focus"], false, "focus")?,
        options: pairs(&window["options"], true)?,
        options_after: pairs(&window["options_after"], true)?,
        panes,
    })
}

fn pane(
    source: Value,
    value: &Value,
    window: &Value,
    window_directory: &Path,
    environment: &[(String, String)],
    window_suppress: bool,
) -> Result<Pane> {
    let pane = if source.is_object() {
        source
    } else {
        json!({"shell_command":source})
    };
    keys(
        &pane,
        &[
            "shell_command",
            "shell_command_before",
            "environment",
            "start_directory",
            "focus",
            "shell",
            "enter",
            "suppress_history",
            "sleep_before",
            "sleep_after",
        ],
        "pane",
    )?;
    let mut sequence = entries(&value["shell_command_before"]);
    sequence.extend(entries(&window["shell_command_before"]));
    sequence.extend(entries(&pane["shell_command_before"]));
    sequence.extend(entries(&pane["shell_command"]));
    let suppress = boolean(
        &pane["suppress_history"],
        window_suppress,
        "suppress_history",
    )?;
    Ok(Pane {
        directory: directory(&pane["start_directory"], window_directory)?,
        environment: if pane.get("environment").is_some() {
            pairs(&pane["environment"], false)?
        } else {
            environment.to_owned()
        },
        shell: text(
            pane.get("shell").unwrap_or(&window["window_shell"]),
            "shell",
        )?,
        focus: boolean(&pane["focus"], false, "focus")?,
        commands: commands(sequence, &pane, suppress)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_rejects_tmux_target_separators() {
        // tmux stores the name verbatim, then `:` and `.` are the window and
        // pane separators in every `-t` target, so a name that contains
        // either can be created but never addressed again (H8).
        for (name, separator) in [("a:b", ':'), ("a.b", '.')] {
            let source = json!({"session_name":name,"windows":[{"panes":["blank"]}]});
            let Err(error) = workspace(&source, Path::new("/tmp/workspace.yaml")) else {
                panic!("{name} should have been refused");
            };
            assert!(
                error.message.contains(separator),
                "expected the message to name {separator:?}: {}",
                error.message
            );
        }
    }

    #[test]
    fn endpoint_fields_require_the_explicit_extension_route() -> Result<()> {
        for key in ["config", "socket_name"] {
            let mut value = json!({"session_name":"demo","windows":[{}]});
            value[key] = json!("endpoint");
            assert!(workspace(&value, Path::new("/tmp/workspace.yaml")).is_err());
            value["plugins"] = json!(["custom.Extension"]);
            assert!(workspace(&value, Path::new("/tmp/workspace.yaml"))?.bridge);
        }
        Ok(())
    }

    #[test]
    fn blank_shorthand_does_not_erase_explicit_commands_or_empty_enter() -> Result<()> {
        let source = json!({"session_name":"demo","suppress_history":false,"windows":[{"panes":["blank", ["blank","echo yes"], {"shell_command":[{"cmd":"blank","sleep_after":0.25},{"cmd":"","sleep_after":null},"echo done"]}]}]});
        let workspace = workspace(&source, Path::new("/tmp/workspace.yaml"))?;
        let panes = &workspace.windows[0].panes;
        assert!(panes[0].commands.is_empty());
        assert_eq!(panes[1].commands[0].text, "blank");
        assert_eq!(panes[2].commands.len(), 3);
        assert_eq!(panes[2].commands[0].text, "blank");
        assert_eq!(panes[2].commands[1].text, "");
        assert_eq!(panes[2].commands[1].after, Duration::ZERO);
        assert_eq!(panes[2].commands[2].after, Duration::ZERO);
        Ok(())
    }

    #[test]
    fn invalid_readiness_is_rejected_before_any_backend() {
        for options in [
            json!({"pane_readiness":"sometimes"}),
            json!(["always"]),
            json!({"pane_readines":"never"}),
            json!({"pane_readiness":"never", "timeout":1}),
        ] {
            let source = json!({"session_name":"demo","workspace_builder_options":options,"windows":[{"panes":["blank"]}]});
            assert!(workspace(&source, Path::new("/tmp/workspace.yaml")).is_err());
        }
    }

    #[test]
    fn readiness_preserves_native_values_and_extension_fields() -> Result<()> {
        for (value, expected) in [
            (Value::Null, None),
            (json!({}), None),
            (json!({"pane_readiness":"always"}), Some(true)),
            (json!({"pane_readiness":false}), Some(false)),
        ] {
            let source = json!({"session_name":"demo","workspace_builder_options":value,"windows":[{"panes":["blank"]}]});
            assert_eq!(
                workspace(&source, Path::new("workspace.yaml"))?.readiness,
                expected
            );
        }
        for (plugins, builder) in [
            (json!(["example.Plugin"]), Value::Null),
            (json!([]), json!("example:Builder")),
        ] {
            let source = json!({"session_name":"demo","plugins":plugins,"workspace_builder":builder,"workspace_builder_options":{"extension_setting":true},"windows":[{"panes":["blank"]}]});
            assert!(workspace(&source, Path::new("workspace.yaml"))?.bridge);
        }
        Ok(())
    }
}
