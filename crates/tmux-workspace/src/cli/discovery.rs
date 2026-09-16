use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CliError, Result, document};

const EXTENSIONS: [&str; 3] = ["yaml", "yml", "json"];

pub(super) fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub(super) fn expand(text: &str) -> String {
    let mut text = if text == "~" {
        home().to_string_lossy().into_owned()
    } else if let Some(tail) = text.strip_prefix("~/") {
        home().join(tail).to_string_lossy().into_owned()
    } else {
        text.to_owned()
    };
    let mut output = String::new();
    while let Some(index) = text.find('$') {
        output.push_str(&text[..index]);
        let tail = &text[index + 1..];
        let (name, consumed) = if let Some(tail) = tail.strip_prefix('{') {
            if let Some(end) = tail.find('}') {
                (&tail[..end], end + 2)
            } else {
                ("", 0)
            }
        } else {
            let end = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(tail.len());
            (&tail[..end], end)
        };
        if name.is_empty() {
            output.push('$');
            text = tail.to_owned();
            continue;
        }
        if let Ok(value) = std::env::var(name) {
            output.push_str(&value);
        } else {
            output.push_str(&text[index..index + 1 + consumed]);
        }
        text = tail[consumed..].to_owned();
    }
    output.push_str(&text);
    output
}

pub(super) fn masked(path: &Path) -> String {
    path.strip_prefix(home()).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |tail| {
            if tail.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", tail.display())
            }
        },
    )
}

pub(super) fn global_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("TMUXP_CONFIGDIR").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(expand(&path.to_string_lossy())));
    }
    dirs.push(
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home().join(".config"), PathBuf::from)
            .join("tmuxp"),
    );
    dirs.push(home().join(".tmuxp"));
    let mut seen = BTreeSet::new();
    dirs.retain(|path| seen.insert(path.clone()));
    dirs
}

pub(super) fn global_metadata() -> Result<Vec<Value>> {
    let mut active = false;
    global_dirs().into_iter().map(|path| {
        let exists = path.is_dir();
        let selected = exists && !active;
        active |= exists;
        let count = if exists { std::fs::read_dir(&path)?.filter_map(std::result::Result::ok).filter(|entry| {
            let name = entry.file_name();
            !name.to_string_lossy().starts_with('.') && entry.path().extension().is_some_and(|ext| EXTENSIONS.contains(&ext.to_string_lossy().to_ascii_lowercase().as_str()))
        }).count() } else { 0 };
        let source = if std::env::var_os("TMUXP_CONFIGDIR").is_some_and(|raw| Path::new(&expand(&raw.to_string_lossy())) == path) { "$TMUXP_CONFIGDIR" }
        else if path == home().join(".tmuxp") { "Legacy" }
        else if std::env::var_os("XDG_CONFIG_HOME").is_some() { "$XDG_CONFIG_HOME/tmuxp" } else { "XDG default" };
        Ok(json!({"path":masked(&path),"source":source,"exists":exists,"workspace_count":count,"active":selected}))
    }).collect()
}

fn project(directory: &Path) -> Option<PathBuf> {
    EXTENSIONS
        .into_iter()
        .map(|ext| directory.join(format!(".tmuxp.{ext}")))
        .find(|p| p.is_file())
}

fn candidate(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_owned());
    }
    if path.is_dir() {
        return project(path);
    }
    EXTENSIONS
        .into_iter()
        .map(|ext| path.with_extension(ext))
        .find(|p| p.is_file())
}

pub(super) fn resolve(name: &str, importer: Option<&str>) -> Result<PathBuf> {
    let path = PathBuf::from(expand(name));
    if let Some(found) = candidate(&path) {
        return Ok(found.canonicalize()?);
    }
    if path.components().count() == 1 {
        let directory = match importer {
            Some("teamocil") => home().join(".teamocil"),
            Some("tmuxinator") => std::env::var("TMUXINATOR_CONFIG").map_or_else(
                |_| home().join(".tmuxinator"),
                |value| PathBuf::from(expand(&value)),
            ),
            _ => global_dirs()
                .into_iter()
                .find(|p| p.is_dir())
                .unwrap_or_else(|| home().join(".tmuxp")),
        };
        if let Some(found) = candidate(&directory.join(path)) {
            return Ok(found.canonicalize()?);
        }
    }
    Err(CliError::new(
        "workspace_not_found",
        format!("workspace source {name:?} was not found"),
    ))
}

pub(super) fn paths() -> Result<Vec<(PathBuf, &'static str)>> {
    let mut paths = Vec::new();
    let cwd = std::env::current_dir()?;
    for directory in cwd.ancestors() {
        if let Some(path) = project(directory) {
            paths.push((path, "local"));
        }
        if directory == home() {
            break;
        }
    }
    if let Some(directory) = global_dirs().into_iter().find(|p| p.is_dir()) {
        let mut entries =
            std::fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| EXTENSIONS.contains(&s))
            {
                paths.push((path, "global"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    paths.retain(|(path, _)| seen.insert(path.clone()));
    Ok(paths)
}

pub(super) fn records(full: bool) -> Result<Vec<Value>> {
    paths()?.into_iter().map(|(path, source)| {
        let metadata = path.metadata()?;
        let parsed = document::read(&path);
        let mut record = json!({
            "name":path.file_stem().unwrap_or_default().to_string_lossy(),
            "path":masked(&path), "format": if path.extension().is_some_and(|ext| ext == "json") {"json"} else {"yaml"},
            "size":metadata.len(), "mtime":metadata.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs().to_string()),
            "session_name":parsed.as_ref().ok().and_then(|v| v.get("session_name")).cloned().unwrap_or(Value::Null), "source":source,
        });
        match parsed {
            Ok(value) if full => { record["config"] = value; }
            Err(error) => { record["error"] = json!(error.message); }
            _ => {}
        }
        Ok(record)
    }).collect()
}
