use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::{PaneId, ServerGeneration, ServerIdentity, SessionId, SessionName, TmuxArg, WindowId};

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn ids_keep_canonical_rendering_numeric_order_and_scope_errors() {
    let session: SessionId = "$0002".parse().unwrap();
    assert_eq!(session.as_ref(), "$2");
    assert_eq!(session.number(), 2);
    assert!("@2".parse::<WindowId>().unwrap() < "@10".parse().unwrap());
    assert_eq!("@1".parse::<PaneId>().unwrap_err().expected_sigil(), '%');
    for invalid in ["%", "%+1", "%-1", "% 1", "%4294967296", "%١"] {
        assert!(invalid.parse::<PaneId>().is_err());
    }
}

#[test]
fn name_errors_keep_the_released_wording() {
    assert_eq!(
        SessionName::new("").unwrap_err().to_string(),
        "a session name cannot be empty: tmux reads that as the current session",
    );
    assert_eq!(
        SessionName::new("a:b").unwrap_err().to_string(),
        "a session name cannot hold ':': tmux splits a target there, so the session would not be addressable by name",
    );
}

#[test]
fn literal_provenance_does_not_change_encoded_argument_traits() {
    let literal = TmuxArg::literal("#{session_id}");
    let format = TmuxArg::format("##{session_id}");
    let literal_value: for<'a> fn(&'a TmuxArg) -> Option<&'a OsStr> = TmuxArg::literal_value;

    assert_eq!(literal_value(&literal), Some(OsStr::new("#{session_id}")));
    assert_eq!(literal_value(&format), None);
    assert_eq!(literal, format);
    assert_eq!(literal.cmp(&format), std::cmp::Ordering::Equal);
    assert_eq!(hash(&literal), hash(&format));
    assert_eq!(format!("{literal:?}"), format!("{format:?}"));
    assert_eq!(literal.clone().into_os_string(), "##{session_id}");
}

#[cfg(unix)]
#[test]
fn literal_and_text_arguments_preserve_nul_and_non_utf8_bytes() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let bytes = [b'#', 0, 0xff, b'#'];
    let source = OsString::from_vec(bytes.to_vec());
    let literal = TmuxArg::literal(&source);
    assert_eq!(
        literal.as_os_str().as_bytes(),
        [b'#', b'#', 0, 0xff, b'#', b'#']
    );
    assert_eq!(literal.literal_value().unwrap().as_bytes(), bytes);
    let text = crate::TmuxText::from_bytes(bytes);
    assert_eq!(TmuxArg::from(&text), literal);
    assert_eq!(
        TmuxArg::from(&text).literal_value(),
        Some(source.as_os_str())
    );
}

#[cfg(windows)]
#[test]
fn literal_arguments_preserve_unpaired_surrogates() {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let source = OsString::from_wide(&[b'#' as u16, 0xd800]);
    let literal = TmuxArg::literal(&source);
    assert_eq!(
        literal.as_os_str().encode_wide().collect::<Vec<_>>(),
        [b'#' as u16, b'#' as u16, 0xd800],
    );
    assert_eq!(literal.literal_value(), Some(source.as_os_str()));
}

#[test]
fn endpoint_identity_keeps_spelling_and_redacts_debug_output() {
    let plain = ServerIdentity::from_socket_path(PathBuf::from("/private/socket"));
    let trailing = ServerIdentity::from_socket_path(PathBuf::from("/private/socket/"));
    let leading = ServerIdentity::from_socket_path(PathBuf::from("//private/socket"));
    assert_eq!(plain.socket_path(), Path::new("/private/socket"));
    assert_ne!(plain, trailing);
    assert_ne!(plain, leading);
    assert_ne!(hash(&plain), hash(&trailing));
    assert!(!format!("{plain:?}").contains("private"));
}

#[test]
fn generations_keep_the_raw_timestamp_and_public_time_view() {
    let generation = ServerGeneration::from_raw(42, -2);
    assert_eq!(generation.pid(), 42);
    assert_eq!(generation.unix_start_time(), -2);
    assert_eq!(generation.start_time(), UNIX_EPOCH - Duration::from_secs(2));
    assert_eq!(generation.to_string(), "pid 42 started -2");
}
