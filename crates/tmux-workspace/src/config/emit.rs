//! Rendering a [`Workspace`] back to tmuxp-style YAML. [`Workspace::to_yaml`]
//! writes only the keys this crate acts on, so a round trip can come back
//! shorter than the document that produced it.

use std::fmt::Write as _;
use std::path::Path;

use super::{PaneConfig, ShellCommand, WindowConfig, Workspace};

impl Workspace {
    /// Render this workspace as tmuxp-style YAML.
    ///
    /// Emits the keys this crate acts on and nothing else, so a document that
    /// came from [`Self::from_yaml`] and back may be shorter than it started:
    /// what is dropped is what `unsupported_keys` already named.
    ///
    /// # Examples
    ///
    /// ```
    /// use tmux_workspace::Workspace;
    ///
    /// let workspace = Workspace::from_yaml(
    ///     "
    /// session_name: demo
    /// windows:
    ///   - window_name: editor
    ///     panes: [htop]
    /// ",
    /// )?;
    ///
    /// // What it writes, it can read.
    /// assert_eq!(Workspace::from_yaml(&workspace.to_yaml())?, workspace);
    /// # Ok::<(), tmux_workspace::ConfigError>(())
    /// ```
    #[must_use]
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "session_name: {}", quoted(&self.session_name));
        if let Some(directory) = &self.start_directory {
            let _ = writeln!(out, "start_directory: {}", path(directory));
        }
        if !self.suppress_history {
            out.push_str("suppress_history: false\n");
        }
        write_pairs(&mut out, Some("environment"), &self.environment, 2);
        write_pairs(&mut out, Some("options"), &self.options, 2);
        write_pairs(&mut out, Some("global_options"), &self.global_options, 2);
        write_commands(
            &mut out,
            Some("shell_command_before"),
            &self.shell_command_before,
            2,
        );

        if self.windows.is_empty() {
            // A bare `windows:` reads back as null, which this crate
            // refuses; an explicit empty list is what round-trips.
            out.push_str("windows: []\n");
        } else {
            out.push_str("windows:\n");
            for window in &self.windows {
                window.write_yaml(&mut out);
            }
        }

        out
    }
}

/// Writes the keys of one sequence entry, indenting all but the first.
///
/// A YAML sequence entry marks only its first line with `-`, so which line
/// that is has to be tracked rather than decided per key.
struct Entry {
    marker: &'static str,
    indent: &'static str,
    first: bool,
}

impl Entry {
    const fn new(marker: &'static str, indent: &'static str) -> Self {
        Self {
            marker,
            indent,
            first: true,
        }
    }

    fn key(&mut self, out: &mut String, line: &str) {
        out.push_str(if self.first { self.marker } else { self.indent });
        self.first = false;
        out.push_str(line);
        out.push('\n');
    }
}

impl WindowConfig {
    /// Write this window as one entry of a `windows:` sequence.
    fn write_yaml(&self, out: &mut String) {
        let mut entry = Entry::new("  - ", "    ");

        if let Some(name) = &self.window_name {
            entry.key(out, &format!("window_name: {}", quoted(name)));
        }
        if let Some(index) = self.window_index {
            entry.key(out, &format!("window_index: {index}"));
        }
        if let Some(shell) = &self.window_shell {
            entry.key(out, &format!("window_shell: {}", quoted(shell)));
        }
        if let Some(layout) = &self.layout {
            entry.key(out, &format!("layout: {}", quoted(layout)));
        }
        if let Some(directory) = &self.start_directory {
            entry.key(out, &format!("start_directory: {}", path(directory)));
        }
        if self.focus {
            entry.key(out, "focus: true");
        }
        if let Some(suppress) = self.suppress_history {
            entry.key(out, &format!("suppress_history: {suppress}"));
        }
        if !self.environment.is_empty() {
            entry.key(out, "environment:");
            write_pairs(out, None, &self.environment, 6);
        }
        if !self.options.is_empty() {
            entry.key(out, "options:");
            write_pairs(out, None, &self.options, 6);
        }
        if !self.shell_command_before.is_empty() {
            entry.key(out, "shell_command_before:");
            write_commands(out, None, &self.shell_command_before, 6);
        }

        // Always written, even when empty: an entry with no keys at all is
        // not a mapping, and `panes` is the one key every window has.
        entry.key(out, "panes:");
        for pane in &self.panes {
            pane.write_yaml(out);
        }
    }
}

impl PaneConfig {
    /// Write this pane as one entry of a `panes:` sequence.
    fn write_yaml(&self, out: &mut String) {
        let mut entry = Entry::new("      - ", "        ");

        if !self.shell_command_before.is_empty() {
            entry.key(out, "shell_command_before:");
            write_commands(out, None, &self.shell_command_before, 10);
        }
        if !self.shell_commands.is_empty() {
            entry.key(out, "shell_command:");
            write_commands(out, None, &self.shell_commands, 10);
        }
        if let Some(directory) = &self.start_directory {
            entry.key(out, &format!("start_directory: {}", path(directory)));
        }
        if self.focus {
            entry.key(out, "focus: true");
        }
        if let Some(shell) = &self.shell {
            entry.key(out, &format!("shell: {}", quoted(shell)));
        }
        if !self.enter {
            entry.key(out, "enter: false");
        }
        if let Some(sleep) = self.sleep_before {
            entry.key(out, &format!("sleep_before: {}", sleep.as_secs_f64()));
        }
        if let Some(sleep) = self.sleep_after {
            entry.key(out, &format!("sleep_after: {}", sleep.as_secs_f64()));
        }
        if let Some(suppress) = self.suppress_history {
            entry.key(out, &format!("suppress_history: {suppress}"));
        }
        if let Some(environment) = &self.environment {
            entry.key(out, "environment:");
            write_pairs(out, None, environment, 10);
        }
        if entry.first {
            // Nothing distinguished this pane, so it is the empty mapping a
            // reader turns back into a default pane.
            out.push_str("      - {}\n");
        }
    }
}

/// Write a mapping of name to value, indented.
fn write_pairs(out: &mut String, name: Option<&str>, values: &[(String, String)], indent: usize) {
    if values.is_empty() {
        return;
    }
    if let Some(name) = name {
        let _ = writeln!(out, "{name}:");
    }
    for (key, value) in values {
        let _ = writeln!(out, "{:indent$}{}: {}", "", quoted(key), quoted(value));
    }
}

/// Write a sequence of commands, indented.
fn write_commands(out: &mut String, name: Option<&str>, commands: &[ShellCommand], indent: usize) {
    if commands.is_empty() {
        return;
    }
    if let Some(name) = name {
        let _ = writeln!(out, "{name}:");
    }
    for command in commands {
        let _ = writeln!(out, "{:indent$}- {}", "", command_yaml(command));
    }
}

/// A command as a string, or as a flow mapping when it has settings of its own.
fn command_yaml(command: &ShellCommand) -> String {
    if command.is_plain() {
        return quoted(&command.cmd);
    }
    let mut fields = vec![format!("cmd: {}", quoted(&command.cmd))];
    if let Some(enter) = command.enter {
        fields.push(format!("enter: {enter}"));
    }
    if let Some(sleep) = command.sleep_before {
        fields.push(format!("sleep_before: {}", sleep.as_secs_f64()));
    }
    if let Some(sleep) = command.sleep_after {
        fields.push(format!("sleep_after: {}", sleep.as_secs_f64()));
    }
    format!("{{{}}}", fields.join(", "))
}

/// Quote a path the way a scalar is quoted.
fn path(value: &Path) -> String {
    quoted(&value.display().to_string())
}

/// Quote a scalar so YAML reads it back as the string it started as.
///
/// Always quoted rather than only when necessary: a command is arbitrary
/// shell, and deciding which of YAML's bare-scalar rules it trips is a larger
/// job than quoting everything.
fn quoted(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str(r#"\""#),
            '\\' => escaped.push_str(r"\\"),
            character if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') => {
                let code = u32::from(character);
                let _ = write!(escaped, r"\u{code:04x}");
            }
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::quoted;

    #[test]
    fn quoted_escapes_the_characters_yaml_or_a_shell_would_read_specially() {
        assert_eq!(quoted("plain"), "\"plain\"");
        assert_eq!(quoted(r#"a "quote""#), r#""a \"quote\"""#);
        assert_eq!(quoted(r"back\slash"), r#""back\\slash""#);
        assert_eq!(quoted("tab\there"), "\"tab\\u0009here\"");
    }
}
