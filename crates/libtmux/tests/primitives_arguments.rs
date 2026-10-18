//! Byte and validation contracts for tmux arguments.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use libtmux::{SessionName, SessionNameError, TmuxArg, TmuxText, escape_format};

#[test]
fn literal_arguments_preserve_bytes_and_escape_every_format_marker() {
    let source = [b'#', b'(', b'i', b'd', b')', 0xff, b'#', b'#'];
    let expected = [
        b'#', b'#', b'(', b'i', b'd', b')', 0xff, b'#', b'#', b'#', b'#',
    ];
    let text = TmuxText::from_bytes(source);
    let argument = TmuxArg::from(&text);

    assert_eq!(argument.as_os_str().as_bytes(), expected);
    assert_eq!(argument.into_os_string().into_vec(), expected);
    assert_eq!(
        escape_format(OsStr::from_bytes(&source)).into_vec(),
        expected
    );
    assert_eq!(text.as_bytes(), source);
}

#[test]
fn explicit_formats_and_prepared_arguments_do_not_escape_again() {
    fn accept(argument: impl Into<TmuxArg>) -> TmuxArg {
        argument.into()
    }

    let source = OsString::from_vec(b"#{pane_title}\xff".to_vec());
    let argument = TmuxArg::format(source.clone());

    assert_eq!(accept(argument).into_os_string(), source);
    assert_eq!(TmuxArg::from("##").as_os_str(), "####");
}

#[test]
fn session_names_reject_only_empty_names_and_target_separators() {
    assert_eq!(SessionName::new(""), Err(SessionNameError::Empty));
    for separator in [':', '.'] {
        assert_eq!(
            SessionName::new(format!("build{separator}release")),
            Err(SessionNameError::Separator { separator }),
        );
    }
    for name in [
        "has space",
        "has,comma",
        "#(literal)",
        "\u{e9}",
        "nul\0byte",
    ] {
        let validated: SessionName = name.parse().expect("name has no target separator");
        assert_eq!(validated.as_str(), name);
        assert_eq!(validated.to_string(), name);
        assert_eq!(OsString::from(validated), OsString::from(name));
    }
}

#[test]
fn session_name_errors_keep_the_existing_diagnostic_contract() {
    assert_eq!(
        SessionNameError::Empty.to_string(),
        "a session name cannot be empty: tmux reads that as the current session",
    );
    let error = SessionNameError::Separator { separator: ':' };
    assert_eq!(
        error.to_string(),
        "a session name cannot hold ':': tmux splits a target there, so the session \
         would not be addressable by name",
    );
    assert!(std::error::Error::source(&error).is_none());
}
