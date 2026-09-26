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
        Yaml::Hash(values) => Value::Object(merge_hash(values)?),
        _ => return Err(CliError::invalid("unsupported YAML value")),
    })
}

/// Resolves `<<: *anchor` and `<<: [*a, *b]` at this mapping level. Merged
/// keys are folded in first-source-wins order, then every key the mapping
/// wrote itself overrides whatever the merge produced, regardless of where
/// `<<` appeared among the document's own keys.
fn merge_hash(values: &yaml_rust2::yaml::Hash) -> Result<Map<String, Value>> {
    let mut own = Vec::new();
    let mut sources = Vec::new();
    for (key, value) in values {
        if key.as_str() == Some("<<") {
            match value {
                Yaml::Array(items) => sources.extend(items.iter()),
                other => sources.push(other),
            }
            continue;
        }
        let key = key
            .as_str()
            .ok_or_else(|| CliError::invalid("document mapping keys must be strings"))?;
        own.push((key.to_owned(), from_yaml(value)?));
    }
    let mut merged = Map::new();
    for source in sources {
        let Value::Object(fields) = from_yaml(source)? else {
            return Err(CliError::invalid("merge key << must reference a mapping"));
        };
        for (key, value) in fields {
            merged.entry(key).or_insert(value);
        }
    }
    for (key, value) in own {
        merged.insert(key, value);
    }
    Ok(merged)
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
        file.persist_noclobber(path).map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                CliError::new(
                    "destination_exists",
                    format!(
                        "{} already exists; pass --force to replace it",
                        path.display()
                    ),
                )
            } else {
                CliError::from(e.error)
            }
        })?;
    }
    Ok(())
}

mod importers;

pub(super) fn import(kind: &str, source: &Value, path: &Path) -> Result<Value> {
    importers::workspace(kind, source, path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_sequence_merge_keys_resolve_with_explicit_keys_winning() -> Result<()> {
        let single = parse(
            "windows:\n  - &base\n    window_name: a\n    panes: [echo a]\n  - <<: *base\n    window_name: b\n",
        )?;
        assert_eq!(single["windows"][1]["window_name"], "b");
        assert_eq!(single["windows"][1]["panes"], json!(["echo a"]));

        // Two sources disagree on `shared`; the first in the sequence wins,
        // and the mapping's own `shared` still overrides both.
        let sequence = parse(
            "a: &a\n  shared: from-a\n  only_a: 1\nb: &b\n  shared: from-b\n  only_b: 2\nc:\n  <<: [*a, *b]\n  shared: explicit\n",
        )?;
        assert_eq!(sequence["c"]["shared"], "explicit");
        assert_eq!(sequence["c"]["only_a"], 1);
        assert_eq!(sequence["c"]["only_b"], 2);
        Ok(())
    }
}
