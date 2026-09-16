use clap::{Arg, ArgAction, ArgGroup, Command};

pub(super) struct DeclaredArgument {
    pub command: &'static [&'static str],
    pub name: String,
    pub overrides: Vec<&'static str>,
    pub numeric_bounds: Option<(i32, i32)>,
}

#[derive(Default)]
struct Declarations(Option<Vec<DeclaredArgument>>);

impl Declarations {
    fn overriding(
        &mut self,
        command: &'static [&'static str],
        arg: Arg,
        other: &'static str,
    ) -> Arg {
        if let Some(facts) = &mut self.0 {
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
        if let Some(facts) = &mut self.0 {
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
        .required(true)
        .help("Workspace file, project directory, or configured name")
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
    let mut shell =
        sockets(Command::new("shell").about("Run a version-checked tmuxp Python shell"))
            .arg(Arg::new("session_name"))
            .arg(Arg::new("window_name"))
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
    for name in ["teamocil", "tmuxinator"] {
        imports = imports.subcommand(saving(Command::new(name).arg(file())).arg(flag(
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
    let mut declarations = Declarations(Some(Vec::new()));
    let command = build(&mut declarations);
    (command, declarations.0.unwrap_or_default())
}

fn build(declarations: &mut Declarations) -> Command {
    root()
        .subcommand(
            saving(
                Command::new("convert")
                    .about("Convert YAML and JSON without discarding configuration fields")
                    .arg(file()),
            )
            .arg(flag("yes", Some('y'), "Accept the save confirmation")),
        )
        .subcommand(Command::new("debug-info").about("Report runtime and tmux diagnostics"))
        .subcommand(
            Command::new("edit")
                .about("Edit a workspace and return the editor's status")
                .arg(file()),
        )
        .subcommand(
            sockets(
                Command::new("freeze")
                    .about("Capture a live session; human mode requires --save-to"),
            )
            .arg(Arg::new("session_name"))
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
            .arg(flag("yes", Some('y'), "Accept yes/no confirmations"))
            .arg(flag(
                "quiet",
                Some('q'),
                "Suppress explanatory and status text",
            ))
            .arg(flag("force", None, "Replace an existing destination")),
        )
        .subcommand(imports())
        .subcommand(load(declarations))
        .subcommand(
            Command::new("ls")
                .about("List local and global workspace files")
                .arg(flag("tree", None, "Group workspaces by their directory"))
                .arg(flag(
                    "full",
                    None,
                    "Include complete workspace configuration",
                )),
        )
        .subcommand(
            Command::new("search")
                .about("Search workspace fields; queries combine with AND unless --any"),
        )
        .mut_subcommand("search", |search| {
            search
                .arg(Arg::new("query_terms").num_args(0..))
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
        })
        .subcommand(shell(declarations))
}

fn load(declarations: &mut Declarations) -> Command {
    sockets(Command::new("load").about("Create, reuse or append workspace sessions"))
        .arg(Arg::new("workspace_files").num_args(1..).required(true))
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
                .conflicts_with("colors88")
                .help("Assume 256 terminal colors"),
        )
        .arg(
            Arg::new("colors88")
                .short('8')
                .long("88-colors")
                .action(ArgAction::SetTrue)
                .conflicts_with("colors256")
                .help("Reject legacy 88-color mode; supported tmux versions require omitting it or using -2"),
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
        ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

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
