//! Key bindings read as fields through `list-keys -F`.

use super::Server;
use crate::formats::{
    FormatCodecError, FormatCodecErrorKind, FormatCodecPhase, TmuxText, TransportDialect,
};
use crate::version::since::LIST_KEYS_FORMAT;
use crate::{Command, Error, ListingDecodeError};

/// The fields [`TEMPLATE`] renders, in order.
const FIELDS: [&str; 5] = [
    "key_table",
    "key_string",
    "key_repeat",
    "key_note",
    "key_command",
];

/// `list-keys -F` template for [`FIELDS`], framed as a format plan's is.
const TEMPLATE: &str =
    "#{q:key_table}=#{q:key_string}=#{key_repeat}=#{q:key_note}=#{q:key_command}=";

/// One key binding, as tmux holds it.
///
/// `bind-key -n` is `-T root`, so [`Self::table`] answers it. The command is
/// the text a `bind-key` line in a configuration file carries, with `\;`
/// between commands, and is not parsed further. That is argument syntax: as
/// the single command string [`Server::bind_key`] takes, `\;` is a literal
/// semicolon rather than a separator.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
/// server.bind_key("prefix", "Y", "display-message hello").await?;
///
/// let version = server.capabilities().await?.tmux_version().clone();
/// if version.has_behavior(&libtmux::since::LIST_KEYS_FORMAT) {
///     let bindings = server.typed_key_bindings(Some("prefix")).await?;
///     let bound = bindings
///         .iter()
///         .find(|binding| binding.key() == "Y")
///         .ok_or("the binding is listed")?;
///     assert_eq!(bound.command(), "display-message hello");
///     assert!(!bound.repeats());
///     assert_eq!(bound.note(), None);
/// }
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyBinding {
    table: TmuxText,
    key: TmuxText,
    command: TmuxText,
    note: Option<TmuxText>,
    repeats: bool,
}

impl KeyBinding {
    /// Return the key table, such as `prefix`, `root` or `copy-mode-vi`.
    #[must_use]
    pub const fn table(&self) -> &TmuxText {
        &self.table
    }

    /// Return the key as tmux names it, such as `C-b` or `"`, unquoted.
    #[must_use]
    pub const fn key(&self) -> &TmuxText {
        &self.key
    }

    /// Return the bound command list, as a `bind-key` line renders it.
    #[must_use]
    pub const fn command(&self) -> &TmuxText {
        &self.command
    }

    /// Return the note `bind-key -N` attached, if any.
    ///
    /// An empty note reads as none: tmux reports both as an empty value.
    #[must_use]
    pub const fn note(&self) -> Option<&TmuxText> {
        self.note.as_ref()
    }

    /// Report whether the binding repeats, `bind-key -r`.
    #[must_use]
    pub const fn repeats(&self) -> bool {
        self.repeats
    }
}

/// Decode `list-keys -F` output rendered with [`TEMPLATE`].
///
/// A row that does not frame, or whose repeat flag is neither `0` nor `1`,
/// fails the whole listing rather than being skipped.
fn parse(stdout: &[u8]) -> Result<Vec<KeyBinding>, FormatCodecError> {
    // `list-keys -F` arrived in 3.7, after the `vis` releases.
    crate::formats::split_quoted_rows(stdout, FIELDS, TransportDialect::RawQ)?
        .into_iter()
        .enumerate()
        .map(|(row, [table, key, repeat, note, command])| {
            let repeats = match repeat.as_slice() {
                b"0" => false,
                b"1" => true,
                _ => {
                    return Err(FormatCodecError::uncatalogued(
                        FormatCodecErrorKind::InvalidValue,
                        FormatCodecPhase::Decode,
                        row,
                        2,
                        FIELDS[2],
                        None,
                    ));
                }
            };
            Ok(KeyBinding {
                table: TmuxText::from(table),
                key: TmuxText::from(key),
                command: TmuxText::from(command),
                note: (!note.is_empty()).then(|| TmuxText::from(note)),
                repeats,
            })
        })
        .collect()
}

impl Server {
    /// List key bindings as fields: table, key, command, note and repeat.
    ///
    /// `table` narrows the listing to one key table. A table with no bindings
    /// lists empty, since tmux keeps no empty table to refuse.
    ///
    /// Every table is fetched and the narrowing done here. tmux 3.7 through
    /// 3.7c send a listing of exactly one binding to the message log instead
    /// of printing it, so `list-keys -T` on a table holding one binding
    /// answers with nothing. Across every table, that happens only on a
    /// server with a single binding left, which lists none on those releases.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.7, which has no
    /// `list-keys -F`; [`Self::key_bindings`] reads the `bind-key` lines
    /// those releases print. Returns [`Error::DecodeListing`] when a row does
    /// not decode, and an error when tmux cannot be reached or refuses the
    /// listing.
    pub async fn typed_key_bindings(&self, table: Option<&str>) -> Result<Vec<KeyBinding>, Error> {
        self.require("list-keys formats", LIST_KEYS_FORMAT).await?;

        let result = self
            .cmd(Command::new("list-keys").arg("-F").arg(TEMPLATE))
            .await?;
        if !result.success() {
            return Err(Error::from_refused_result("list-keys", &result, None));
        }

        let bindings = parse(result.stdout()).map_err(|detail| Error::DecodeListing {
            list_command: "list-keys",
            detail: ListingDecodeError::new(detail),
        })?;
        Ok(match table {
            Some(table) => bindings
                .into_iter()
                .filter(|binding| binding.table == table)
                .collect(),
            None => bindings,
        })
    }
}

/// Decode `list-keys -F` output, for fuzzing only.
///
/// Not a supported API: it lets a fuzzer reach the decoder without making it
/// public, behind a feature no release turns on.
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub fn __fuzz_parse_key_bindings(stdout: &[u8]) {
    let _ = parse(stdout);
}

#[cfg(test)]
mod tests {
    use super::{KeyBinding, parse};
    use crate::TmuxText;
    use crate::formats::FormatCodecErrorKind;

    fn binding(table: &str, key: &str, command: &str) -> KeyBinding {
        KeyBinding {
            table: TmuxText::from(table),
            key: TmuxText::from(key),
            command: TmuxText::from(command),
            note: None,
            repeats: false,
        }
    }

    /// tmux 3.7c's output for two bindings; its `#{q:}` leaves LF bare.
    #[test]
    fn rows_decode_escapes_and_keep_bare_newlines() {
        let stdout = br#"solo=M-\'=0==display-message\ a\=b\ \\\;\ display-message\ c=
sp\ ace=\"=1=a
note=display-message\ \"two\ words\"=
"#;

        let solo = binding("solo", "M-'", r"display-message a=b \; display-message c");
        let mut spaced = binding("sp ace", "\"", "display-message \"two words\"");
        spaced.note = Some(TmuxText::from("a\nnote"));
        spaced.repeats = true;

        assert_eq!(parse(stdout), Ok(vec![solo, spaced]));
        assert_eq!(parse(b""), Ok(Vec::new()));
    }

    /// Each malformed shape fails the listing; none yields a shorter one.
    #[test]
    fn a_row_that_does_not_frame_fails_the_listing() {
        let good = b"root=x=0==send-keys=\n".as_slice();
        for (stdout, kind) in [
            (
                b"root=x=0==send-keys".as_slice(),
                FormatCodecErrorKind::MissingFieldTerminator,
            ),
            (b"root=x=0==send-keys=", FormatCodecErrorKind::MissingRowLf),
            (
                b"root=x=0==send-keys=x\n",
                FormatCodecErrorKind::UnexpectedRowTerminator,
            ),
            (
                b"root=\\x=0==send-keys=\n",
                FormatCodecErrorKind::InvalidEscape,
            ),
            (b"root=x\\", FormatCodecErrorKind::DanglingEscape),
            (
                b"root=x\0=0==send-keys=\n",
                FormatCodecErrorKind::EmbeddedNul,
            ),
            (
                b"root=x=2==send-keys=\n",
                FormatCodecErrorKind::InvalidValue,
            ),
        ] {
            let listing = [good, stdout].concat();
            let error = parse(&listing).expect_err("a malformed row fails");
            assert_eq!(error.kind(), kind, "{:?}", String::from_utf8_lossy(stdout));
            assert_eq!(error.row(), Some(1));
        }
    }
}
