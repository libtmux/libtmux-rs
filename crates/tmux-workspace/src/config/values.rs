//! Readers for one YAML value: a string, number, boolean, duration, a
//! mapping of names to values, or a list of commands. The workspace, window
//! and pane parsers compose these, so each coercion tmuxp allows is written
//! once, with the path an error names.

use std::time::Duration;

use yaml_rust2::Yaml;

use super::expand::expand;
use super::{Problem, ShellCommand};

/// Read a mapping of names to values, as `environment` and `options` use.
pub(super) fn pairs(value: &Yaml, path: &str) -> Result<Vec<(String, String)>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(Vec::new()),
        Yaml::Hash(entries) => entries
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| Problem::new(path, "names must be strings"))?;
                let at = format!("{path}.{key}");
                // tmuxp writes option values as strings, numbers, or bools,
                // and expands only the strings.
                let value = match value {
                    Yaml::String(text) => expand(text, &at)?,
                    Yaml::Integer(number) => number.to_string(),
                    Yaml::Boolean(true) => "on".to_owned(),
                    Yaml::Boolean(false) => "off".to_owned(),
                    _ => {
                        return Err(Problem::new(at, "must be a string, a number, or a boolean"));
                    }
                };
                Ok((key.to_owned(), value))
            })
            .collect(),
        _ => Err(Problem::new(path, "must be a mapping of names to values")),
    }
}

/// Read `shell_command` or `shell_command_before`: one command, a list of
/// them, or nothing.
pub(super) fn commands(value: &Yaml, path: &str) -> Result<Vec<ShellCommand>, Problem> {
    let (entries, single) = match value {
        Yaml::BadValue | Yaml::Null => return Ok(Vec::new()),
        Yaml::Array(entries) => (entries.as_slice(), false),
        _ => (std::slice::from_ref(value), true),
    };
    // tmuxp reads a lone null, `pane` or `blank` as a pane with no command.
    // Among other commands the words are typed, and a null is refused.
    if let [only] = entries {
        if matches!(only, Yaml::Null) || matches!(only.as_str(), Some("pane" | "blank")) {
            return Ok(Vec::new());
        }
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let at = if single {
                path.to_owned()
            } else {
                format!("{path}[{index}]")
            };
            command(entry, &at)
        })
        .collect()
}

pub(super) fn command(value: &Yaml, path: &str) -> Result<ShellCommand, Problem> {
    match value {
        Yaml::String(text) => Ok(ShellCommand::new(text.as_str())),
        Yaml::Hash(_) => Ok(ShellCommand {
            cmd: value["cmd"]
                .as_str()
                .ok_or_else(|| Problem::new(format!("{path}.cmd"), "must be a string"))?
                .to_owned(),
            enter: optional_bool(&value["enter"], &format!("{path}.enter"))?,
            sleep_before: optional_seconds(
                &value["sleep_before"],
                &format!("{path}.sleep_before"),
            )?,
            sleep_after: optional_seconds(&value["sleep_after"], &format!("{path}.sleep_after"))?,
        }),
        Yaml::Null => Err(Problem::new(
            path,
            "is empty among other commands; remove it, or write \"\" to press Enter",
        )),
        _ => Err(Problem::new(
            path,
            "must be a command or a mapping with `cmd`; \
             quote a command YAML would read as a number or a boolean",
        )),
    }
}

/// Read a number of seconds, which tmuxp passes to `time.sleep`.
pub(super) fn optional_seconds(value: &Yaml, path: &str) -> Result<Option<Duration>, Problem> {
    let refused = || Problem::new(path, "must be a number of seconds, zero or more");
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Integer(seconds) => u64::try_from(*seconds)
            .map(|seconds| Some(Duration::from_secs(seconds)))
            .map_err(|_| refused()),
        Yaml::Real(_) => value
            .as_f64()
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
            .map(Some)
            .ok_or_else(refused),
        _ => Err(refused()),
    }
}

/// Read a window index, which tmuxp writes as an integer or a string.
pub(super) fn optional_index(value: &Yaml, path: &str) -> Result<Option<i32>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Integer(index) => i32::try_from(*index)
            .map(Some)
            .map_err(|_| Problem::new(path, "is out of range")),
        Yaml::String(index) => index
            .parse()
            .map(Some)
            .map_err(|_| Problem::new(path, "must be a number")),
        _ => Err(Problem::new(path, "must be a number")),
    }
}

/// Read an optional string, refusing a value that is present and not one.
pub(super) fn optional_text(value: &Yaml, path: &str) -> Result<Option<String>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::String(text) => Ok(Some(text.clone())),
        _ => Err(Problem::new(path, "must be a string")),
    }
}

/// tmuxp writes booleans as bools in some files and strings in others.
///
/// Both spellings are accepted; a third thing is refused. `focus: "tru"` used
/// to read as `false`, which is a different workspace rather than an error.
pub(super) fn optional_bool(value: &Yaml, path: &str) -> Result<Option<bool>, Problem> {
    match value {
        Yaml::BadValue | Yaml::Null => Ok(None),
        Yaml::Boolean(flag) => Ok(Some(*flag)),
        Yaml::String(text) => match text.as_str() {
            "true" | "yes" | "on" => Ok(Some(true)),
            "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(Problem::new(
                path,
                format!("must be a boolean, found {text:?}"),
            )),
        },
        _ => Err(Problem::new(path, "must be a boolean")),
    }
}

/// Read a boolean that defaults to false when absent, and fails when wrong.
pub(super) fn is_true(value: &Yaml, path: &str) -> Result<bool, Problem> {
    Ok(optional_bool(value, path)?.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::{ShellCommand, commands, optional_bool, optional_index, pairs};
    use yaml_rust2::YamlLoader;

    /// The value a single-document YAML fragment parses to, for feeding
    /// these functions the same shapes tmuxp's parser hands them.
    fn value(source: &str) -> yaml_rust2::Yaml {
        YamlLoader::load_from_str(source).unwrap().remove(0)
    }

    #[test]
    fn pairs_reads_scalars_of_every_kind_tmuxp_writes() {
        let read = pairs(
            &value("EDITOR: vim\nRETRIES: 3\nDEBUG: true\nQUIET: false"),
            "environment",
        )
        .unwrap();
        assert_eq!(
            read,
            vec![
                ("EDITOR".into(), "vim".into()),
                ("RETRIES".into(), "3".into()),
                ("DEBUG".into(), "on".into()),
                ("QUIET".into(), "off".into()),
            ]
        );
    }

    #[test]
    fn pairs_is_empty_for_an_absent_mapping() {
        assert_eq!(pairs(&value("~"), "environment").unwrap(), Vec::new());
    }

    #[test]
    fn pairs_refuses_a_non_scalar_value() {
        assert!(pairs(&value("KEY:\n  - nested"), "environment").is_err());
    }

    #[test]
    fn commands_accepts_a_bare_string_or_a_list() {
        assert_eq!(
            commands(&value("echo hi"), "shell_command").unwrap(),
            vec![ShellCommand::new("echo hi")]
        );
        assert_eq!(
            commands(&value("[echo one, echo two]"), "shell_command").unwrap(),
            vec![ShellCommand::new("echo one"), ShellCommand::new("echo two")]
        );
        assert_eq!(
            commands(&value("~"), "shell_command").unwrap(),
            Vec::<ShellCommand>::new()
        );
    }

    #[test]
    fn commands_refuses_a_list_entry_that_is_not_a_string() {
        assert!(commands(&value("[echo hi, 7]"), "shell_command").is_err());
    }

    #[test]
    fn optional_index_accepts_an_integer_or_a_numeric_string() {
        assert_eq!(
            optional_index(&value("2"), "window_index").unwrap(),
            Some(2)
        );
        assert_eq!(
            optional_index(&value("\"3\""), "window_index").unwrap(),
            Some(3)
        );
        assert_eq!(optional_index(&value("~"), "window_index").unwrap(), None);
    }

    #[test]
    fn optional_index_refuses_a_non_numeric_string() {
        assert!(optional_index(&value("\"first\""), "window_index").is_err());
    }

    #[test]
    fn optional_bool_accepts_a_bool_and_tmuxps_string_spellings() {
        for (source, expected) in [
            ("true", true),
            ("\"yes\"", true),
            ("\"on\"", true),
            ("false", false),
            ("\"no\"", false),
            ("\"off\"", false),
        ] {
            assert_eq!(
                optional_bool(&value(source), "focus").unwrap(),
                Some(expected),
                "{source}"
            );
        }
        assert_eq!(optional_bool(&value("~"), "focus").unwrap(), None);
    }

    /// `focus: "tru"` is a typo, not a workspace with focus false: this must
    /// refuse rather than silently pick a boolean.
    #[test]
    fn optional_bool_refuses_a_string_that_is_not_one_of_the_spellings() {
        assert!(optional_bool(&value("\"tru\""), "focus").is_err());
    }
}
