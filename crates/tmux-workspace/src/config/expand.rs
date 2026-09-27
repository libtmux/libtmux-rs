//! Expanding `~` and environment variables the way tmuxp's loader does, and
//! resolving a relative `start_directory` against the right base.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use yaml_rust2::Yaml;

use super::Problem;

/// Where a workspace's relative start directories are resolved from.
pub(super) struct Directories<'a> {
    /// The workspace file's directory, or `None` for the current directory.
    pub(super) base: Option<&'a Path>,
}

impl Directories<'_> {
    /// Read a `start_directory` and resolve it the way tmuxp's loader does.
    ///
    /// `~` and variables expand first. An absolute result stands. A result
    /// starting with `.` is relative to `parent`, else to the base. Any other
    /// relative result joins `parent` when `join` is set, which tmuxp does for
    /// a window under its session, and is otherwise relative to the current
    /// directory, where tmux would resolve it.
    ///
    /// Absence defaults; a wrong shape does not. `start_directory: 123` used
    /// to read as "no start directory", which builds a workspace that is valid
    /// and not the one the file describes.
    pub(super) fn resolve(
        &self,
        value: &Yaml,
        path: &str,
        parent: Option<&Path>,
        join: bool,
    ) -> Result<Option<PathBuf>, Problem> {
        let text = match value {
            Yaml::BadValue | Yaml::Null => return Ok(None),
            Yaml::String(text) => text,
            _ => return Err(Problem::new(path, "must be a string")),
        };
        if text.starts_with('~') && !(text == "~" || text.starts_with("~/")) {
            return Err(Problem::new(
                path,
                "starts with `~name`, which is not expanded here; write the directory out",
            ));
        }
        let expanded = PathBuf::from(expand(text, path)?);
        if expanded.is_absolute() {
            return Ok(Some(tidy(&expanded)));
        }
        let anchor = if text.starts_with('.') {
            parent.or(self.base)
        } else if join {
            parent
        } else {
            None
        };
        let anchor = match anchor {
            Some(anchor) => anchor.to_owned(),
            None => std::env::current_dir().map_err(|error| {
                Problem::new(
                    path,
                    format!("is relative, and the current directory cannot be read: {error}"),
                )
            })?,
        };
        Ok(Some(tidy(&anchor.join(expanded))))
    }
}

/// Drop `.` components and doubled separators. `..` is kept for the kernel
/// to resolve, since a lexical `..` is wrong across a symbolic link.
fn tidy(path: &Path) -> PathBuf {
    path.components().collect()
}

/// Expand `text` against this process's environment, as tmuxp's
/// `expandshell` does.
pub(super) fn expand(text: &str, path: &str) -> Result<String, Problem> {
    expand_with(text, |name| std::env::var_os(name)).map_err(|reason| Problem::new(path, reason))
}

/// Python's `os.path.expanduser` then `os.path.expandvars`, which is what
/// tmuxp applies.
///
/// A leading `~` or `~/` becomes `$HOME`. `$NAME` (ASCII letters, digits and
/// `_`) and `${NAME}` become the variable's value; an unset variable, `~name`
/// and a lone `$` stay as written. There is no escape, in tmuxp or here.
fn expand_with(text: &str, variable: impl Fn(&str) -> Option<OsString>) -> Result<String, String> {
    let text_of = |name: &str, value: OsString| {
        value
            .into_string()
            .map_err(|_| format!("names ${name}, whose value is not UTF-8"))
    };
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    if let Some(tail) = text.strip_prefix('~') {
        if tail.is_empty() || tail.starts_with('/') {
            let home = variable("HOME").ok_or("starts with `~`, and HOME is not set")?;
            expanded.push_str(text_of("HOME", home)?.trim_end_matches('/'));
            if expanded.is_empty() && tail.is_empty() {
                expanded.push('/');
            }
            rest = tail;
        }
    }
    while let Some(at) = rest.find('$') {
        expanded.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, length) = if let Some(braced) = after.strip_prefix('{') {
            braced
                .find('}')
                .map_or(("", 0), |end| (&braced[..end], end + 2))
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], end)
        };
        // A name no environment variable can have is never looked up.
        let value = if name.is_empty() || name.contains(['=', '\0']) {
            None
        } else {
            variable(name)
        };
        match value {
            Some(value) => expanded.push_str(&text_of(name, value)?),
            None => expanded.push_str(&rest[at..=at + length]),
        }
        rest = &after[length..];
    }
    expanded.push_str(rest);
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::expand_with;

    /// Each expected value is what Python's `os.path.expandvars(
    /// os.path.expanduser(text))` returns with the same two variables set.
    #[test]
    fn expansion_is_pythons_expanduser_then_expandvars() {
        let expand = |text| {
            expand_with(text, |name| match name {
                "HOME" => Some("/home/me/".into()),
                "PROJECT" => Some("tmux".into()),
                _ => None,
            })
        };
        for (text, expected) in [
            ("~", "/home/me"),
            ("~/src", "/home/me/src"),
            ("~nosuchuser/src", "~nosuchuser/src"),
            ("a~", "a~"),
            ("$PROJECT/x", "tmux/x"),
            ("${PROJECT}x", "tmuxx"),
            ("$PROJECTx", "$PROJECTx"),
            ("$UNSET and ${UNSET}", "$UNSET and ${UNSET}"),
            ("$ ${ ${} $-", "$ ${ ${} $-"),
            ("~/$PROJECT", "/home/me/tmux"),
            ("price: $5", "price: $5"),
        ] {
            assert_eq!(expand(text).as_deref(), Ok(expected), "{text}");
        }

        let root = |text| expand_with(text, |_| Some("/".into()));
        assert_eq!(root("~").as_deref(), Ok("/"));
        assert_eq!(root("~/x").as_deref(), Ok("/x"));
        assert!(expand_with("~", |_| None).is_err(), "no HOME, no `~`");
    }
}
