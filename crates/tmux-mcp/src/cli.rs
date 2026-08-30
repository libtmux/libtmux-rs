//! Reading the command line the binary was started with.
//!
//! Hand-written rather than reached for a parser crate. There are four
//! options, and a server an agent runs is a server whose dependency list gets
//! read: a tree pulled in to format one help text is a poor trade. The flags
//! mirror tmux's own `-S` and `-L` so that someone who knows tmux already
//! knows these.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::Safety;

/// What the binary was asked to do.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// The socket path to talk to, from `-S`.
    pub socket_path: Option<PathBuf>,
    /// The socket name to talk to, from `-L`.
    pub socket_name: Option<OsString>,
    /// Which route surface to offer, when the command line said.
    pub safety: Option<Safety>,
    /// Command-line override for asking before destructive operations.
    pub confirm: Option<bool>,
}

/// Why the binary is stopping before it serves anything.
#[derive(Debug, Eq, PartialEq)]
pub enum Stop {
    /// Print the help text and exit successfully.
    Help,
    /// Print the version and exit successfully.
    Version,
    /// Complain and exit unsuccessfully.
    Misuse(String),
}

/// The help text, which is also the documentation for the flags.
pub const HELP: &str = concat!(
    "tmux-mcp — serve tmux over the Model Context Protocol on stdio\n",
    "\n",
    "USAGE:\n",
    "    tmux-mcp [OPTIONS]\n",
    "\n",
    "OPTIONS:\n",
    "    -S, --socket <PATH>     Talk to the tmux server on this socket path\n",
    "    -L, --socket-name <NAME>\n",
    "                            Talk to the tmux server with this socket name\n",
    "        --safety <TIER>     Which tools to offer: readonly, mutating, or\n",
    "                            destructive. Defaults to mutating, which offers\n",
    "                            everything except the four dedicated kill tools.\n",
    "                            This filter is not a sandbox. Overrides\n",
    "                            TMUX_MCP_SAFETY.\n",
    "        --confirm           Ask before dedicated kill tools and destructive\n",
    "                            plan operations. Command text is not inspected.\n",
    "        --no-confirm        Do not ask before those operations. Either flag\n",
    "                            overrides TMUX_MCP_CONFIRM.\n",
    "    -h, --help              Print this help\n",
    "    -V, --version           Print the version\n",
    "\n",
    "Without -S or -L the server follows $TMUX when it was started inside tmux,\n",
    "and otherwise talks to tmux's default socket.\n",
    "\n",
    "The protocol runs on stdout, so anything this prints for a person goes to\n",
    "stderr, where an MCP client collects it as the server's log.\n",
);

impl Options {
    /// Read the options from arguments, excluding the program name.
    ///
    /// # Errors
    ///
    /// Returns the reason to stop before serving: the caller asked for help or
    /// the version, or passed something unusable.
    pub fn parse<I>(arguments: I) -> Result<Self, Stop>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter().peekable();

        while let Some(argument) = arguments.next() {
            let text = argument.to_string_lossy().into_owned();
            // `--flag=value` is split here so each flag below only has to
            // handle the separated form.
            let (flag, inline) = match text.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => {
                    (flag.to_owned(), Some(OsString::from(value)))
                }
                _ => (text, None),
            };

            let mut take = |flag: &str| -> Result<OsString, Stop> {
                inline.clone().map_or_else(
                    || {
                        arguments
                            .next()
                            .ok_or_else(|| Stop::Misuse(format!("{flag} needs a value")))
                    },
                    Ok,
                )
            };

            match flag.as_str() {
                "-h" | "--help" => return Err(Stop::Help),
                "-V" | "--version" => return Err(Stop::Version),
                "-S" | "--socket" => {
                    options.socket_path = Some(PathBuf::from(take("--socket")?));
                }
                "-L" | "--socket-name" => {
                    options.socket_name = Some(take("--socket-name")?);
                }
                "--confirm" | "--no-confirm" => {
                    if inline.is_some() {
                        return Err(Stop::Misuse(format!("{flag} does not take a value")));
                    }
                    let confirm = flag == "--confirm";
                    if options.confirm.is_some_and(|current| current != confirm) {
                        return Err(Stop::Misuse(
                            "give either --confirm or --no-confirm, not both".to_owned(),
                        ));
                    }
                    options.confirm = Some(confirm);
                }
                "--safety" => {
                    let value = take("--safety")?;
                    let value = value.to_string_lossy();
                    // Rejected rather than ignored: a tier is a safety
                    // decision, and quietly falling back to the default on a
                    // typo would offer more than the operator asked for.
                    options.safety = Some(Safety::parse(&value).ok_or_else(|| {
                        Stop::Misuse(format!(
                            "--safety must be readonly, mutating, or destructive, not {value}"
                        ))
                    })?);
                }
                other => {
                    return Err(Stop::Misuse(format!("unrecognised argument {other}")));
                }
            }
        }

        if options.socket_path.is_some() && options.socket_name.is_some() {
            return Err(Stop::Misuse(
                "give either --socket or --socket-name, not both".to_owned(),
            ));
        }

        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Options, Stop> {
        Options::parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_is_no_opinion() {
        assert_eq!(parse(&[]), Ok(Options::default()));
    }

    #[test]
    fn a_socket_path_is_read_either_way_round() {
        let separated = parse(&["-S", "/tmp/sock"]).expect("valid");
        let inline = parse(&["--socket=/tmp/sock"]).expect("valid");

        assert_eq!(
            separated.socket_path.as_deref(),
            Some(Path::new("/tmp/sock"))
        );
        assert_eq!(inline.socket_path, separated.socket_path);
    }

    #[test]
    fn a_socket_name_mirrors_tmuxs_own_flag() {
        let parsed = parse(&["-L", "work"]).expect("valid");

        assert_eq!(parsed.socket_name.as_deref(), Some(OsStr::new("work")));
    }

    #[test]
    fn the_two_socket_flags_are_exclusive() {
        let refused = parse(&["-S", "/tmp/sock", "-L", "work"]).expect_err("refused");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("not both")));
    }

    #[test]
    fn a_tier_is_read_by_name() {
        for (given, expected) in [
            ("readonly", Safety::ReadOnly),
            ("mutating", Safety::Mutating),
            ("destructive", Safety::Destructive),
        ] {
            let parsed = parse(&["--safety", given]).expect("valid");
            assert_eq!(parsed.safety, Some(expected));
        }
    }

    #[test]
    fn confirmation_flags_state_an_explicit_opinion() {
        assert_eq!(parse(&["--confirm"]).expect("valid").confirm, Some(true));
        assert_eq!(
            parse(&["--no-confirm"]).expect("valid").confirm,
            Some(false)
        );
        assert_eq!(
            parse(&["--no-confirm", "--no-confirm"])
                .expect("repeating one opinion is harmless")
                .confirm,
            Some(false)
        );
    }

    #[test]
    fn contradictory_confirmation_flags_are_refused() {
        let refused =
            parse(&["--confirm", "--no-confirm"]).expect_err("opposite flags are ambiguous");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("not both")));
    }

    #[test]
    fn confirmation_flags_do_not_accept_inline_values() {
        let refused = parse(&["--confirm=false"]).expect_err("a boolean flag has no value");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("does not take")));
    }

    #[test]
    fn an_unknown_tier_stops_rather_than_widening() {
        // The environment narrows to read-only on nonsense. A flag is typed
        // on purpose, so a wrong one is a mistake worth reporting.
        let refused = parse(&["--safety", "yolo"]).expect_err("refused");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("yolo")));
    }

    #[test]
    fn a_flag_without_its_value_says_so() {
        let refused = parse(&["--socket"]).expect_err("refused");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("needs a value")));
    }

    #[test]
    fn help_and_version_stop_before_anything_else() {
        assert_eq!(parse(&["--help"]), Err(Stop::Help));
        assert_eq!(parse(&["-h"]), Err(Stop::Help));
        assert_eq!(parse(&["--version"]), Err(Stop::Version));
        assert_eq!(parse(&["-V"]), Err(Stop::Version));
        // Even alongside something that would otherwise be rejected.
        assert_eq!(parse(&["--help", "--nonsense"]), Err(Stop::Help));
    }

    #[test]
    fn an_unknown_argument_is_named_back() {
        let refused = parse(&["--colour=blue"]).expect_err("refused");

        assert!(matches!(refused, Stop::Misuse(reason) if reason.contains("--colour")));
    }

    #[test]
    fn the_help_text_names_every_flag() {
        for flag in [
            "-S",
            "--socket",
            "-L",
            "--socket-name",
            "--safety",
            "--confirm",
            "--no-confirm",
            "-h",
            "--help",
            "-V",
            "--version",
        ] {
            assert!(HELP.contains(flag), "the help text should mention {flag}");
        }
    }

    use std::ffi::OsStr;
    use std::path::Path;
}
