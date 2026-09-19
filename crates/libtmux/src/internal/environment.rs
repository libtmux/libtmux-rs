//! The implementation behind the environment methods on servers and sessions.
//!
//! The two differ only in which environment they address, so the reading,
//! which is the part with the awkward cases in it, is written once.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::error::ListingDecodeError;
use crate::formats::FormatCodecError;
use crate::internal::core::Core;
use crate::internal::listing;
use crate::{Command, EnvironmentEntry, Error, TmuxText};

/// Which environment a command addresses.
///
/// tmux keeps these genuinely apart rather than layering one over the other:
/// a name set with `-g` is not visible to `show-environment -t`, which
/// reports it as an unknown variable.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'target> {
    /// The server's own environment, tmux's `-g`, which new sessions copy.
    Global,
    /// One session's environment.
    Session(&'target str),
}

impl Scope<'_> {
    /// Point a command at this environment.
    fn apply(self, command: Command) -> Command {
        match self {
            Self::Global => command.arg("-g"),
            Self::Session(target) => command.arg("-t").arg(target),
        }
    }
}

/// Set a variable, so processes started later inherit it.
pub(crate) async fn set(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
    value: OsString,
) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg("--")
            .arg(OsString::from(name))
            // An environment carries tokens, so the value never reaches a log.
            .sensitive_arg(value),
    )
    .await
}

/// Mark a variable, so processes started later are handed it absent.
pub(crate) async fn hide(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg("-r")
            .arg("--")
            .arg(OsString::from(name)),
    )
    .await
}

/// Delete a variable, letting whatever tmux inherited show through again.
pub(crate) async fn unset(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg("-u")
            .arg("--")
            .arg(OsString::from(name)),
    )
    .await
}

/// Read one variable back, telling a removal from a value.
///
/// `None` means tmux does not hold the name at all, which is how a
/// continuation line from a multi-line value is discarded.
///
/// It does not mean the request failed. tmux refuses a name it does not hold
/// with "unknown variable", and refuses a target that is gone with "no such
/// session" -- two different facts on the same exit code. Reading every
/// refusal as the first told a caller their variable was unset when their
/// session had ended, which is the answer they would act on.
pub(crate) async fn get(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
) -> Result<Option<EnvironmentEntry>, Error> {
    let result = core
        .execute(
            scope
                .apply(Command::new("show-environment"))
                .arg("--")
                .arg(OsString::from(name)),
        )
        .await?;
    if !result.success() {
        // "unknown variable: NAME" is the answer `None` exists for. Anything
        // else -- a session that has ended, a server that has gone -- is a
        // failure and must not read as a variable nobody set.
        if result
            .stderr_lossy()
            .trim_start()
            .starts_with("unknown variable")
        {
            return Ok(None);
        }

        return Err(Error::from_refused_result(
            "show-environment",
            &result,
            None,
        ));
    }

    let stdout = result.stdout();
    let line = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    if line.first() == Some(&b'-') {
        return Ok(Some(EnvironmentEntry::Removed));
    }
    let Some(position) = line.iter().position(|byte| *byte == b'=') else {
        return Ok(None);
    };

    Ok(Some(EnvironmentEntry::Set(TmuxText::from(
        line[position + 1..].to_vec(),
    ))))
}

/// Read the whole environment in one command.
///
/// The plain listing cannot be framed: a value containing a newline occupies
/// more than one line, and a continuation line holding an `=` reads as the
/// next variable. `-s` prints each entry as a shell statement instead, with
/// every `"`, `\`, `$` and backtick in the value escaped, so the first
/// unescaped `"` ends the value. That escaping is undone and nothing else is,
/// so each value reads exactly as [`get`] reads it.
pub(crate) async fn all(
    core: &Core,
    scope: Scope<'_>,
) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
    let result = core
        .execute(scope.apply(Command::new("show-environment")).arg("-s"))
        .await?;
    if !result.success() {
        return Err(Error::from_refused_result(
            "show-environment",
            &result,
            None,
        ));
    }

    parse_shell_listing(result.stdout()).map_err(|(row, offset)| Error::DecodeListing {
        list_command: "show-environment",
        detail: ListingDecodeError::new(FormatCodecError::row_mismatch(
            row,
            None,
            None,
            Some(offset),
        )),
    })
}

/// Parse `show-environment -s`, reporting the entry and byte offset that
/// failed rather than guessing past it.
fn parse_shell_listing(
    stdout: &[u8],
) -> Result<BTreeMap<String, EnvironmentEntry>, (usize, usize)> {
    let mut environment = BTreeMap::new();
    let mut at = 0;
    let mut row = 0;
    while at < stdout.len() {
        let rest = &stdout[at..];
        let (name, entry, consumed) = parse_unset(rest)
            .or_else(|| parse_exported(rest))
            .ok_or((row, at))?;
        environment.insert(name, entry);
        at += consumed;
        row += 1;
    }
    Ok(environment)
}

/// `unset NAME;` and its newline.
///
/// tmux refuses a name containing `=`, which is how an exported entry whose
/// name merely begins `unset ` is told from this.
fn parse_unset(rest: &[u8]) -> Option<(String, EnvironmentEntry, usize)> {
    let body = rest.strip_prefix(b"unset ")?;
    let end = body.windows(2).position(|pair| pair == b";\n")?;
    let name = &body[..end];
    if name.is_empty() || name.contains(&b'=') {
        return None;
    }
    Some((
        String::from_utf8_lossy(name).into_owned(),
        EnvironmentEntry::Removed,
        b"unset ".len() + end + 2,
    ))
}

/// `NAME="VALUE"; export NAME;` and its newline.
fn parse_exported(rest: &[u8]) -> Option<(String, EnvironmentEntry, usize)> {
    // tmux refuses a name containing `=`, so the first one ends the name.
    let equals = rest.iter().position(|byte| *byte == b'=')?;
    let name = &rest[..equals];
    if name.is_empty() || rest.get(equals + 1) != Some(&b'"') {
        return None;
    }
    let mut at = equals + 2;
    let mut value = Vec::new();
    loop {
        match *rest.get(at)? {
            b'"' => break,
            b'\\' => {
                let next = *rest.get(at + 1)?;
                if matches!(next, b'"' | b'\\' | b'$' | b'`') {
                    value.push(next);
                    at += 2;
                } else {
                    // tmux's own rendering of a byte, such as `\033`, which
                    // the plain listing carries too.
                    value.push(b'\\');
                    at += 1;
                }
            }
            byte => {
                value.push(byte);
                at += 1;
            }
        }
    }
    at += 1;
    let suffix = [b"; export ".as_slice(), name, b";\n"].concat();
    if !rest[at..].starts_with(&suffix) {
        return None;
    }
    Some((
        String::from_utf8_lossy(name).into_owned(),
        EnvironmentEntry::Set(TmuxText::from(value)),
        at + suffix.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_shell_listing;
    use crate::{EnvironmentEntry, TmuxText};

    fn set(value: &[u8]) -> EnvironmentEntry {
        EnvironmentEntry::Set(TmuxText::from(value.to_vec()))
    }

    /// Bytes tmux 3.7d printed. `a$b` arrives as `a\\$b`: `-s` escapes the
    /// `$` for a shell, and tmux escapes it again for display, as the plain
    /// listing that `get` reads does too.
    #[test]
    fn shell_listing_undoes_only_the_shell_escaping() {
        let listing = b"V_BS=\"a\\\\b\"; export V_BS;\n\
                        V_DOLLAR=\"a\\\\$b\"; export V_DOLLAR;\n\
                        V_DQ=\"a\\\"b\"; export V_DQ;\n\
                        V_ESC=\"a\\033b\"; export V_ESC;\n\
                        V_TAB=\"a\tb\"; export V_TAB;\n";
        let parsed = parse_shell_listing(listing).expect("listing parses");

        assert_eq!(parsed["V_BS"], set(b"a\\b"));
        assert_eq!(parsed["V_DOLLAR"], set(b"a\\$b"));
        assert_eq!(parsed["V_DQ"], set(b"a\"b"));
        assert_eq!(parsed["V_ESC"], set(b"a\\033b"));
        assert_eq!(parsed["V_TAB"], set(b"a\tb"));
    }

    #[test]
    fn a_value_shaped_like_framing_stays_one_value() {
        let listing = b"MULTI=\"first\nDECOY=x\n\\\"; export X;\nY=\\\"z\"; export MULTI;\n\
                        unset GONE;\n\
                        unset A=\"v\"; export unset A;\n";
        let parsed = parse_shell_listing(listing).expect("listing parses");

        assert_eq!(parsed.len(), 3, "{parsed:?}");
        assert_eq!(
            parsed["MULTI"],
            set(b"first\nDECOY=x\n\"; export X;\nY=\"z")
        );
        assert_eq!(parsed["GONE"], EnvironmentEntry::Removed);
        assert_eq!(parsed["unset A"], set(b"v"));
    }

    #[test]
    fn unframed_output_is_refused_not_guessed() {
        for listing in [
            b"PLAIN=value\n".as_slice(),
            b"OPEN=\"never closed; export OPEN;\n",
            b"MISMATCH=\"v\"; export OTHER;\n",
            b"unset ;\n",
            b"TRUNCATED=\"v\"; export TRUNCATED;",
        ] {
            assert!(
                parse_shell_listing(listing).is_err(),
                "accepted {:?}",
                String::from_utf8_lossy(listing)
            );
        }
    }
}
