use super::{
    Error, NO_CURRENT_TARGET, ObjectKind, OptionErrorKind, SENSITIVE_OUTPUT_WITHHELD,
    ServerGoneKind,
};

/// Say whether a target tmux echoed back names an object or a place.
///
/// tmux gives every object an id carrying a sigil -- `$` a session, `@` a
/// window, `%` a pane -- unique for the life of the server. Every other
/// spelling is scoped to something that can be renumbered or reused: an index
/// belongs to a session, a window name belongs to a session, and neither
/// survives as a way to name one particular object.
///
/// A session is the exception, because `-t` takes a session's name and tmux
/// keeps those unique, so a bare word there is still an identity. So is a
/// client, which tmux names by its terminal.
///
/// Measured against tmux 3.2a through 3.7b; see `docs/design.md`.
const fn is_identity(kind: ObjectKind, target: &str) -> bool {
    match kind {
        ObjectKind::Session | ObjectKind::Client => true,
        ObjectKind::Window => matches!(target.as_bytes().first(), Some(b'@')),
        ObjectKind::Pane => matches!(target.as_bytes().first(), Some(b'%')),
    }
}

impl Error {
    /// Classify a refused tmux command, recognizing a target that has gone.
    ///
    /// tmux reports a missing target as `can't find <kind>: <target>` and
    /// exits 1, the same status it uses for an argument it did not like, so
    /// the message is the only thing that separates them. It is not
    /// localized -- tmux has no message catalogue -- and the wording has been
    /// stable across every supported release.
    ///
    /// Anything that does not match stays a refusal, so a future rewording
    /// costs the distinction rather than correctness.
    /// `target` is the request's own `-t`, when it had one. tmux reports a
    /// server holding no sessions as `no current target` even for a target it
    /// was given, so the request is what recovers the name.
    pub(crate) fn refused(
        command: &'static str,
        exit_code: Option<i32>,
        stderr: String,
        target: Option<&std::ffi::OsStr>,
    ) -> Self {
        // The wording is tmux's own and is identical on every supported
        // release. None of these say the request was wrong, so they are read
        // before anything that does.
        const GONE: [(&str, ServerGoneKind); 4] = [
            ("no server running on", ServerGoneKind::NotRunning),
            ("error connecting to", ServerGoneKind::Unreachable),
            // Before the shorter one, which it starts with and does not mean.
            ("server exited unexpectedly", ServerGoneKind::Lost),
            ("server exited", ServerGoneKind::Stopped),
        ];

        // tmux has two vocabularies for the same fact and they come from
        // different files. `cmd-find.c` resolves a target and says "can't
        // find"; `options.c` and the environment commands resolve their own
        // and say "no such". A caller asking `is_object_gone` about one dead
        // session got `true` from `windows()` and `false` from `get_option`
        // until both were matched here.
        const MISSING: [(&str, ObjectKind); 7] = [
            ("can't find session:", ObjectKind::Session),
            ("can't find window:", ObjectKind::Window),
            ("can't find pane:", ObjectKind::Pane),
            ("can't find client:", ObjectKind::Client),
            ("no such session:", ObjectKind::Session),
            ("no such window:", ObjectKind::Window),
            ("no such pane:", ObjectKind::Pane),
        ];

        // tmux spells "no such option name" two ways. `set-option` and
        // `show-options` resolve the name with `options_match` first, which
        // says "invalid option"; the "unknown option" in `options_scope_from_name`
        // sits behind that call and so is unreachable from the CLI on every
        // supported release. Both mean the same thing, so both map to the same
        // kind rather than leaving a hole if tmux ever reorders the two.
        const OPTION: [(&str, OptionErrorKind); 5] = [
            ("invalid option:", OptionErrorKind::Unknown),
            ("unknown option:", OptionErrorKind::Unknown),
            ("ambiguous option:", OptionErrorKind::Ambiguous),
            ("bad value:", OptionErrorKind::BadValue),
            ("value is invalid:", OptionErrorKind::BadValue),
        ];

        for (prefix, kind) in GONE {
            if stderr.trim_end().starts_with(prefix) {
                return Self::ServerGone { command, kind };
            }
        }

        for (prefix, kind) in OPTION {
            if let Some(detail) = stderr.trim_end().strip_prefix(prefix) {
                return Self::OptionRejected {
                    kind,
                    detail: detail.trim().to_owned(),
                };
            }
        }

        if let Some(name) = stderr.trim_end().strip_prefix("duplicate session:") {
            return Self::SessionExists {
                name: name.trim().to_owned(),
            };
        }

        if let Some(target) = target.filter(|_| stderr.trim_end() == NO_CURRENT_TARGET) {
            return Self::object_gone(&target.to_string_lossy());
        }

        for (prefix, kind) in MISSING {
            let Some(echo) = stderr.trim_end().strip_prefix(prefix) else {
                continue;
            };
            let echo = echo.trim();

            // tmux echoes the part of the target it could not resolve, and
            // that echo says which fact it established. A coordinate -- an
            // index, or a window name -- is scoped to one session, so its
            // absence means that session holds nothing there and says nothing
            // about any object. Reporting it as an identity would name a
            // different object, and `is_object_gone` would tell the caller to
            // drop a handle that still works.
            if is_identity(kind, echo) {
                return Self::ObjectGone {
                    kind,
                    id: echo.to_owned(),
                };
            }

            // The echo drops the session, so the request is what still knows
            // the whole target. Without one, the echo alone is what is true.
            return Self::LinkGone {
                kind,
                target: target.map_or_else(
                    || echo.to_owned(),
                    |sent| sent.to_string_lossy().into_owned(),
                ),
            };
        }

        Self::CommandFailed {
            command,
            exit_code,
            stderr,
        }
    }

    /// Report a refusal without retaining tmux output.
    pub(crate) fn refused_withheld(command: &'static str, exit_code: Option<i32>) -> Self {
        Self::CommandFailed {
            command,
            exit_code,
            stderr: SENSITIVE_OUTPUT_WITHHELD.to_owned(),
        }
    }

    /// Classify a nonzero result, withholding output after sensitive input.
    pub(crate) fn from_refused_result(
        command: &'static str,
        result: &crate::CommandResult,
        target: Option<&std::ffi::OsStr>,
    ) -> Self {
        let stderr = result.stderr_lossy().into_owned();
        if result.command().sensitive_argument_count() > 0 {
            let classified = Self::refused(command, result.exit_code(), stderr, target);
            if matches!(
                &classified,
                Self::ObjectGone { id, .. }
                    if target.is_some_and(|target| id == &target.to_string_lossy())
            ) || matches!(classified, Self::ServerGone { .. })
            {
                return classified;
            }
            Self::refused_withheld(command, result.exit_code())
        } else {
            Self::refused(command, result.exit_code(), stderr, target)
        }
    }

    /// Report a tmux target that could not be resolved.
    ///
    /// The kind comes from the sigil, which is how tmux names its objects.
    /// A target that is a name rather than an ID is reported as a session,
    /// because a name is what `-t` accepts for one.
    fn object_gone(target: &str) -> Self {
        Self::ObjectGone {
            kind: match target.as_bytes().first() {
                Some(b'@') => ObjectKind::Window,
                Some(b'%') => ObjectKind::Pane,
                _ => ObjectKind::Session,
            },
            id: target.to_owned(),
        }
    }
}
