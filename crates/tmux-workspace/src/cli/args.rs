use std::fmt::Write as _;

use clap::{Arg, ArgAction, ArgGroup, Command, builder::styling::Styles};

/// A command line shown under a command's help. The tests parse every one,
/// so an example cannot outlive the flag it demonstrates.
pub(super) struct Example {
    pub line: &'static str,
    pub effect: &'static str,
}

const fn example(line: &'static str, effect: &'static str) -> Example {
    Example { line, effect }
}

const ROOT_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace load ./dev.yaml",
        "Build dev.yaml, then attach",
    ),
    example(
        "tmux-workspace load -d .",
        "Build ./.tmuxp.yaml in the background",
    ),
    example(
        "tmux-workspace freeze -o dev.yaml",
        "Save the session this pane is in",
    ),
    example("tmux-workspace ls", "List the workspaces it can find"),
];

const LOAD_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace load ./dev.yaml",
        "Build dev.yaml, then attach",
    ),
    example("tmux-workspace load .", "Build this project's .tmuxp.yaml"),
    example(
        "tmux-workspace load dev",
        "Build dev.yaml from ~/.config/tmuxp",
    ),
    example(
        "tmux-workspace load -d api web",
        "Build two sessions without attaching",
    ),
    example(
        "tmux-workspace load -s scratch dev",
        "Build dev under the session name scratch",
    ),
    example(
        "tmux-workspace load -a tools",
        "Add tools' windows to the current session",
    ),
    example(
        "tmux-workspace load -L work -d dev",
        "Build on the tmux server named work",
    ),
];

const FREEZE_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace freeze -o dev.yaml",
        "Save the session this pane is in",
    ),
    example(
        "tmux-workspace freeze api -o api.yaml",
        "Save the session named api",
    ),
    example(
        "tmux-workspace --json freeze api",
        "Print the capture instead of saving it",
    ),
];

const CONVERT_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace convert dev.yaml",
        "Write dev.json beside it, asking first",
    ),
    example(
        "tmux-workspace convert -y dev.json",
        "Write dev.yaml without asking",
    ),
];

const EDIT_EXAMPLES: &[Example] = &[example(
    "tmux-workspace edit dev",
    "Open dev.yaml in $EDITOR, or vi",
)];

const TMUXINATOR_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace import tmuxinator -y api --save-to api.yaml",
        "Convert ~/.tmuxinator/api.yml, save api.yaml",
    ),
    example(
        "tmux-workspace --json import tmuxinator api",
        "Print the conversion as JSON",
    ),
];

const TEAMOCIL_EXAMPLES: &[Example] = &[example(
    "tmux-workspace import teamocil -y dev --save-to dev.yaml",
    "Convert ~/.teamocil/dev.yml and save it",
)];

const LS_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace ls",
        "List workspaces here and in ~/.config/tmuxp",
    ),
    example("tmux-workspace ls --tree", "Group them by directory"),
    example("tmux-workspace --json ls", "Print them as JSON"),
];

const SEARCH_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace search api",
        "Find workspaces mentioning api",
    ),
    example(
        "tmux-workspace search window:logs",
        "Find a window named like logs",
    ),
    example(
        "tmux-workspace search -i --any api web",
        "Match either term, ignoring case",
    ),
];

const SHELL_EXAMPLES: &[Example] = &[
    example(
        "tmux-workspace shell",
        "Open Python with this session loaded",
    ),
    example(
        "tmux-workspace shell dev -c 'print(session.name)'",
        "Run one statement against session dev",
    ),
];

const DEBUG_INFO_EXAMPLES: &[Example] = &[example(
    "tmux-workspace debug-info",
    "Print what a bug report needs",
)];

/// The examples for the command at `path`, from the root's name down.
pub(super) fn examples_for(path: &[&str]) -> &'static [Example] {
    match path {
        [_] => ROOT_EXAMPLES,
        [_, "load"] => LOAD_EXAMPLES,
        [_, "freeze"] => FREEZE_EXAMPLES,
        [_, "convert"] => CONVERT_EXAMPLES,
        [_, "edit"] => EDIT_EXAMPLES,
        [_, "import", "tmuxinator"] => TMUXINATOR_EXAMPLES,
        [_, "import", "teamocil"] => TEAMOCIL_EXAMPLES,
        [_, "ls"] => LS_EXAMPLES,
        [_, "search"] => SEARCH_EXAMPLES,
        [_, "shell"] => SHELL_EXAMPLES,
        [_, "debug-info"] => DEBUG_INFO_EXAMPLES,
        _ => &[],
    }
}

fn examples(list: &[Example]) -> String {
    let header = Styles::default().get_header().to_owned();
    let width = list.iter().map(|e| e.line.len()).max().unwrap_or(0);
    let mut text = format!("{header}Examples:{header:#}");
    for Example { line, effect } in list {
        let _ = write!(text, "\n  {line:width$}  {effect}");
    }
    text
}

pub(super) struct DeclaredArgument {
    pub command: &'static [&'static str],
    pub name: String,
    pub overrides: Vec<&'static str>,
    pub numeric_bounds: Option<(i32, i32)>,
}

#[derive(Default)]
struct Declarations {
    facts: Option<Vec<DeclaredArgument>>,
    /// Leave out arguments accepted only to be refused. `clap_complete`'s
    /// generators offer hidden arguments anyway, so the only way to keep one
    /// out of a completion script is to not build it.
    visible_only: bool,
}

impl Declarations {
    fn overriding(
        &mut self,
        command: &'static [&'static str],
        arg: Arg,
        other: &'static str,
    ) -> Arg {
        if let Some(facts) = &mut self.facts {
            facts.push(DeclaredArgument {
                command,
                name: arg.get_id().as_str().into(),
                overrides: vec![other],
                numeric_bounds: None,
            });
        }
        arg.overrides_with(other)
    }

    fn integer(&mut self, command: &'static [&'static str], arg: Arg, minimum: i32) -> Arg {
        if let Some(facts) = &mut self.facts {
            facts.push(DeclaredArgument {
                command,
                name: arg.get_id().as_str().into(),
                overrides: Vec::new(),
                numeric_bounds: Some((minimum, i32::MAX)),
            });
        }
        arg.value_parser(clap::value_parser!(i32).range(i64::from(minimum)..))
    }
}

fn flag(name: &'static str, short: Option<char>, help: &'static str) -> Arg {
    let arg = Arg::new(name)
        .long(name)
        .action(ArgAction::SetTrue)
        .help(help);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

fn value(name: &'static str, short: Option<char>, help: &'static str) -> Arg {
    let arg = Arg::new(name).long(name).help(help);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

fn sockets(command: Command) -> Command {
    command
        .arg(
            Arg::new("socket-path")
                .short('S')
                .help("Use this tmux socket path"),
        )
        .arg(
            Arg::new("socket-name")
                .short('L')
                .help("Use this tmux socket name"),
        )
}

fn file() -> Arg {
    Arg::new("workspace_file")
        .value_name("WORKSPACE")
        .required(true)
        .help("Workspace file, project directory, or configured name")
}

fn source(help: &'static str) -> Arg {
    Arg::new("workspace_file")
        .value_name("SOURCE")
        .required(true)
        .help(help)
}

fn saving(command: Command) -> Command {
    command
        .arg(value(
            "save-to",
            None,
            "Save atomically to this file; machine mode otherwise returns the document",
        ))
        .arg(
            value(
                "workspace-format",
                None,
                "Saved document encoding, independent of --json/--ndjson",
            )
            .value_parser(["yaml", "json"]),
        )
        .arg(flag("force", None, "Replace an existing destination file"))
}

fn shell(declarations: &mut Declarations) -> Command {
    let mut shell = sockets(
        Command::new("shell")
            .about("Run a version-checked tmuxp Python shell")
            .after_help(examples(SHELL_EXAMPLES)),
    )
    .arg(
        Arg::new("session_name")
            .value_name("SESSION")
            .help("Session to load; defaults to the attached one"),
    )
    .arg(
        Arg::new("window_name")
            .value_name("WINDOW")
            .help("Window to load; defaults to the session's active one"),
    )
    .arg(
        Arg::new("python-code")
            .short('c')
            .help("Execute Python code in the selected libtmux context"),
    )
    .arg(declarations.overriding(
        &["shell"],
        flag(
            "use-pythonrc",
            None,
            "Load PYTHONSTARTUP and ~/.pythonrc.py",
        ),
        "no-startup",
    ))
    .arg(declarations.overriding(
        &["shell"],
        flag("no-startup", None, "Disable Python startup files"),
        "use-pythonrc",
    ))
    .arg(declarations.overriding(
        &["shell"],
        flag("use-vi-mode", None, "Use vi editing in ptpython/ptipython"),
        "no-vi-mode",
    ))
    .arg(declarations.overriding(
        &["shell"],
        flag("no-vi-mode", None, "Disable vi editing"),
        "use-vi-mode",
    ));
    for name in [
        "best",
        "pdb",
        "code",
        "ptipython",
        "ptpython",
        "ipython",
        "bpython",
    ] {
        shell = shell.arg(flag(name, None, "Select this Python shell backend"));
    }
    shell = shell.group(ArgGroup::new("backend").args([
        "best",
        "pdb",
        "code",
        "ptipython",
        "ptpython",
        "ipython",
        "bpython",
    ]));

    shell
}

fn imports() -> Command {
    let mut imports = Command::new("import")
        .about("Convert another workspace manager's configuration")
        .subcommand_required(true);
    for (name, about, help, list) in [
        (
            "teamocil",
            "Convert a teamocil layout to a workspace",
            "teamocil file, or a name in ~/.teamocil",
            TEAMOCIL_EXAMPLES,
        ),
        (
            "tmuxinator",
            "Convert a tmuxinator project to a workspace",
            "tmuxinator file, or a name in $TMUXINATOR_CONFIG or ~/.tmuxinator",
            TMUXINATOR_EXAMPLES,
        ),
    ] {
        let command = Command::new(name)
            .about(about)
            .after_help(examples(list))
            .arg(source(help));
        imports = imports.subcommand(saving(command).arg(flag(
            "yes",
            Some('y'),
            "Accept the save confirmation",
        )));
    }

    imports
}

fn root() -> Command {
    Command::new("tmux-workspace")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Load, capture, discover and manage tmux workspaces")
        .after_help(examples(ROOT_EXAMPLES))
        .args_override_self(true)
        .disable_help_subcommand(true)
        .arg(
            flag(
                "json",
                None,
                "Write JSON; saved workspace encoding is selected separately",
            )
            .global(true),
        )
        .arg(
            flag(
                "ndjson",
                None,
                "Stream newline-delimited JSON; takes precedence over --json",
            )
            .global(true),
        )
        .arg(
            value(
                "color",
                None,
                "Human output color policy; machine output never contains ANSI",
            )
            .value_parser(["auto", "always", "never"])
            .default_value("auto")
            .global(true),
        )
        .arg(
            value("log-level", None, "Diagnostic verbosity")
                .value_parser(["debug", "info", "warning", "error", "critical"])
                .default_value("warning")
                .global(true),
        )
        .arg(
            value(
                "generate",
                None,
                "Generate command metadata, completion, or a manual without tmux",
            )
            .value_parser([
                "schema",
                "bash",
                "zsh",
                "fish",
                "powershell",
                "elvish",
                "man",
            ]),
        )
}

pub(super) fn command() -> Command {
    build(&mut Declarations::default())
}

pub(super) fn declared_command() -> (Command, Vec<DeclaredArgument>) {
    let mut declarations = Declarations {
        facts: Some(Vec::new()),
        visible_only: false,
    };
    let command = build(&mut declarations);
    (command, declarations.facts.unwrap_or_default())
}

/// The command a completion script is generated from: every argument a user
/// can pick, and none that exists only to be refused.
pub(super) fn completion_command() -> Command {
    build(&mut Declarations {
        facts: None,
        visible_only: true,
    })
}

fn build(declarations: &mut Declarations) -> Command {
    root()
        .subcommand(
            saving(
                Command::new("convert")
                    .about("Convert YAML and JSON without discarding configuration fields")
                    .after_help(examples(CONVERT_EXAMPLES))
                    .arg(file()),
            )
            .arg(flag("yes", Some('y'), "Accept the save confirmation")),
        )
        .subcommand(
            Command::new("debug-info")
                .about("Report runtime and tmux diagnostics")
                .after_help(examples(DEBUG_INFO_EXAMPLES)),
        )
        .subcommand(
            Command::new("edit")
                .about("Edit a workspace and return the editor's status")
                .after_help(examples(EDIT_EXAMPLES))
                .arg(file()),
        )
        .subcommand(freeze())
        .subcommand(imports())
        .subcommand(load(declarations))
        .subcommand(
            Command::new("ls")
                .about("List local and global workspace files")
                .after_help(examples(LS_EXAMPLES))
                .arg(flag("tree", None, "Group workspaces by their directory"))
                .arg(flag(
                    "full",
                    None,
                    "Include complete workspace configuration",
                )),
        )
        .subcommand(search())
        .subcommand(shell(declarations))
}

fn freeze() -> Command {
    sockets(
        Command::new("freeze")
            .about("Capture a live session; human mode requires --save-to")
            .after_help(examples(FREEZE_EXAMPLES)),
    )
    .arg(
        Arg::new("session_name")
            .value_name("SESSION")
            .help("Session to capture; defaults to this pane's, else the only one"),
    )
    .arg(
        value(
            "workspace-format",
            Some('f'),
            "Saved document encoding; machine stdout follows --json/--ndjson",
        )
        .value_parser(["yaml", "json"]),
    )
    .arg(value(
        "save-to",
        Some('o'),
        "Save the captured workspace to this file; required outside machine mode",
    ))
    .arg(flag(
        "yes",
        Some('y'),
        "Accept confirmations; --save-to never prompts",
    ))
    .arg(flag(
        "quiet",
        Some('q'),
        "Suppress explanatory and status text",
    ))
    .arg(flag("force", None, "Replace an existing destination"))
}

fn search() -> Command {
    Command::new("search")
        .about("Search workspace fields; queries combine with AND unless --any")
        .after_help(examples(SEARCH_EXAMPLES))
        .arg(
            Arg::new("query_terms")
                .value_name("PATTERN")
                .num_args(0..)
                .help("Regular expressions; FIELD:PATTERN limits one to a field"),
        )
        .arg(
            value(
                "field",
                Some('f'),
                "Search name, session/s, path/p, window/w, or pane fields",
            )
            .action(ArgAction::Append),
        )
        .arg(flag("ignore-case", Some('i'), "Ignore case"))
        .arg(flag(
            "smart-case",
            Some('S'),
            "Ignore case when the pattern has no uppercase",
        ))
        .arg(flag(
            "fixed-strings",
            Some('F'),
            "Treat patterns as literal text",
        ))
        .arg(flag("word-regexp", Some('w'), "Match whole words"))
        .arg(flag(
            "invert-match",
            Some('v'),
            "Select nonmatching workspaces",
        ))
        .arg(flag("any", None, "Combine query terms with OR"))
}

fn load(declarations: &mut Declarations) -> Command {
    let load = sockets(
        Command::new("load")
            .about("Create, reuse or append workspace sessions")
            .long_about(
                "Create, reuse or append workspace sessions, then attach to or switch to \
                 the last one.\n\n\
                 A WORKSPACE is a file, a project directory holding .tmuxp.yaml, \
                 .tmuxp.yml or .tmuxp.json, or a bare name looked up only in the \
                 first of $TMUXP_CONFIGDIR, $XDG_CONFIG_HOME/tmuxp (~/.config/tmuxp) \
                 and ~/.tmuxp that exists. Write ./dev for a file in this directory.",
            )
            .after_help(examples(LOAD_EXAMPLES)),
    )
    .arg(
        Arg::new("workspace_files")
            .value_name("WORKSPACE")
            .num_args(1..)
            .required(true)
            .help("Workspace files, project directories, or configured names"),
    )
        .arg(
            Arg::new("tmux-config")
                .short('f')
                .help("Read this tmux configuration file"),
        )
        .arg(
            Arg::new("session-name")
                .short('s')
                .help("Override the workspace's session name"),
        )
        .arg(
            Arg::new("detached")
                .short('d')
                .action(ArgAction::SetTrue)
                .help("Load without attaching; required by machine mode unless appending"),
        )
        .arg(flag(
            "yes",
            Some('y'),
            "Accept confirmations; load never prompts",
        ))
        .arg(flag(
            "append",
            Some('a'),
            "Append windows to the selected current session",
        ))
        .arg(
            Arg::new("colors256")
                .short('2')
                .action(ArgAction::SetTrue)
                .help("Assume 256 terminal colors"),
        )
        .arg(value("log-file", None, "Write diagnostics to this file"))
        .arg(
            value(
                "progress-format",
                None,
                "Progress preset (default/minimal/window/pane/verbose) or token template; defaults to TMUXP_PROGRESS_FORMAT or default",
            ),
        )
        .arg(declarations.integer(
            &["load"],
            value(
                "progress-lines",
                None,
                "Script panel lines: 0 preserves raw streams, -1 uses terminal height (up to 128); overrides TMUXP_PROGRESS_LINES",
            )
            .allow_negative_numbers(true)
            .default_value("3"),
            -1,
        ))
        .arg(flag(
            "no-progress",
            None,
            "Disable terminal progress; TMUXP_PROGRESS=0 also disables it",
        ));
    if declarations.visible_only {
        return load;
    }
    // Supported tmux releases have no 88-colour mode. tmuxp's -8 is still
    // parsed so a tmuxp command line is refused with that reason, rather than
    // as an unknown flag.
    load.mut_arg("colors256", |arg| arg.conflicts_with("colors88"))
        .arg(
        Arg::new("colors88")
            .short('8')
            .long("88-colors")
            .action(ArgAction::SetTrue)
            .conflicts_with("colors256")
            .hide(true)
            .help("Reject legacy 88-color mode; supported tmux versions require omitting it or using -2"),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Split a documented command line the way a POSIX shell would for the
    /// quoting the docs use: single and double quotes, nothing else.
    fn words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quote = None;
        let mut started = false;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => word.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    started = true;
                }
                (None, c) if c.is_whitespace() => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                (None, c) => {
                    word.push(c);
                    started = true;
                }
            }
        }
        assert!(quote.is_none(), "unterminated quote in {line:?}");
        if started {
            words.push(word);
        }
        words
    }

    /// Every `$ tmux-workspace ...` command in a Markdown document, with
    /// `\` continuations joined and leading `NAME=value` assignments and any
    /// trailing redirection or pipeline dropped.
    fn documented(markdown: &str) -> Vec<String> {
        let mut commands = Vec::new();
        let mut lines = markdown.lines();
        while let Some(line) = lines.next() {
            let Some(command) = line.trim_start().strip_prefix("$ ") else {
                continue;
            };
            let mut command = command.to_owned();
            while command.ends_with('\\') {
                command.pop();
                command.push(' ');
                command.push_str(lines.next().unwrap_or_default().trim());
            }
            let command = command
                .split(['|', '>', ';'])
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .skip_while(|word| word.contains('=') && !word.starts_with('-'))
                .collect::<Vec<_>>()
                .join(" ");
            if command.starts_with("tmux-workspace ") || command == "tmux-workspace" {
                commands.push(command);
            }
        }
        commands
    }

    fn parses(line: &str) -> Result<(), String> {
        command()
            .try_get_matches_from(words(line))
            .map(drop)
            .map_err(|error| format!("{line:?} does not parse:\n{error}"))
    }

    /// Every command's path, root first.
    fn paths(command: &Command, parent: &[String], found: &mut Vec<Vec<String>>) {
        let mut path = parent.to_vec();
        path.push(command.get_name().to_owned());
        for child in command.get_subcommands() {
            paths(child, &path, found);
        }
        found.push(path);
    }

    #[test]
    fn every_command_has_examples_and_every_example_parses() {
        let mut found = Vec::new();
        paths(&command(), &[], &mut found);
        for path in found {
            let path: Vec<&str> = path.iter().map(String::as_str).collect();
            let examples = examples_for(&path);
            // `import` only chooses an importer; its two commands have examples.
            if path != ["tmux-workspace", "import"] {
                assert!(!examples.is_empty(), "{path:?} has no examples");
            }
            for example in examples {
                let command = path.join(" ");
                let shown = example.line.replace("--json ", "");
                assert!(
                    path.len() == 1
                        || shown == command
                        || shown.starts_with(&format!("{command} ")),
                    "{:?} is listed under {path:?}",
                    example.line
                );
                parses(example.line).unwrap();
            }
        }
    }

    #[test]
    fn every_documented_command_parses() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut documents = vec![
            ("README.md", include_str!("../../README.md").to_owned()),
            ("docs/cli.md", include_str!("../../docs/cli.md").to_owned()),
        ];
        // The repository's own README ships in no crate, so it is read only
        // where the repository is: inside it, it has to be there.
        let root = manifest.join("../../README.md");
        if manifest.join("../../Cargo.toml").is_file() {
            documents.push(("../../README.md", std::fs::read_to_string(root).unwrap()));
        }
        let mut failures = Vec::new();
        for (name, markdown) in &documents {
            let commands = documented(markdown);
            assert!(!commands.is_empty(), "{name} documents no command");
            failures.extend(
                commands
                    .iter()
                    .filter_map(|line| parses(line).err())
                    .map(|error| format!("{name}: {error}")),
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    #[test]
    fn documented_commands_are_found_as_a_shell_reads_them() {
        let markdown = "```console\n$ tmux-workspace load \\\n    -d \\\n    dev\n```\n\
                        $ TMUXP_CONFIGDIR=/x tmux-workspace ls --tree | head\n\
                        $ cargo install tmux-workspace\n";
        assert_eq!(
            documented(markdown),
            ["tmux-workspace load -d dev", "tmux-workspace ls --tree"]
        );
        assert_eq!(words("a -c 'b c' \"d\" ''"), ["a", "-c", "b c", "d", ""]);
        assert!(parses("tmux-workspace load --no-such-flag dev").is_err());
    }

    #[test]
    fn declarations_drive_bounds_and_keep_native_override_order() {
        let (command, facts) = declared_command();
        let bounds = facts.iter().find_map(|fact| fact.numeric_bounds).unwrap();
        assert_eq!(bounds, (-1, i32::MAX));
        for (number, accepted) in [
            ("-2", false),
            ("-1", true),
            ("0", true),
            ("2147483647", true),
            ("2147483648", false),
        ] {
            let argv = [
                "tmux-workspace",
                "load",
                "fixture",
                "--progress-lines",
                number,
            ];
            assert_eq!(command.clone().try_get_matches_from(argv).is_ok(), accepted);
            assert_eq!(
                super::command().try_get_matches_from(argv).is_ok(),
                accepted
            );
        }
        for (first, second) in [
            ("use-pythonrc", "no-startup"),
            ("no-startup", "use-pythonrc"),
            ("use-vi-mode", "no-vi-mode"),
            ("no-vi-mode", "use-vi-mode"),
        ] {
            let flags = [format!("--{first}"), format!("--{second}")];
            let matches = command
                .clone()
                .try_get_matches_from(["tmux-workspace", "shell", &flags[0], &flags[1]])
                .unwrap();
            let shell = matches.subcommand_matches("shell").unwrap();
            assert!(!shell.get_flag(first));
            assert!(shell.get_flag(second));
        }
        assert!(
            command
                .clone()
                .try_get_matches_from(["tmux-workspace", "shell", "--pdb", "--code"])
                .is_err()
        );
        assert!(
            command
                .try_get_matches_from(["tmux-workspace", "load", "fixture", "-2", "-8"])
                .is_err()
        );
    }
}
