//! Integration tests for typed tmux IDs and targets.

use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::str::FromStr;

use libtmux::{
    IdParseError, PaneId, PaneTarget, ServerIdentity, SessionId, SessionTarget, WindowId,
    WindowTarget,
};
use static_assertions::assert_impl_all;

assert_impl_all!(SessionId: AsRef<str>, Clone, Debug, Display, Eq, FromStr, Hash, Ord, Send, Sync);
assert_impl_all!(WindowId: AsRef<str>, Clone, Debug, Display, Eq, FromStr, Hash, Ord, Send, Sync);
assert_impl_all!(PaneId: AsRef<str>, Clone, Debug, Display, Eq, FromStr, Hash, Ord, Send, Sync);
assert_impl_all!(SessionTarget: Clone, Debug, Eq, Hash, Ord, Send, Sync);
assert_impl_all!(WindowTarget: Clone, Debug, Eq, Hash, Ord, Send, Sync);
assert_impl_all!(PaneTarget: Clone, Debug, Eq, Hash, Ord, Send, Sync);
assert_impl_all!(ServerIdentity: Clone, Debug, Eq, Hash, Send, Sync);
assert_impl_all!(IdParseError: std::error::Error, Send, Sync);

#[test]
fn ids_accept_only_their_scope_sigil_and_ascii_digits() {
    let session: SessionId = "$0".parse().expect("fixture is a session ID");
    let window: WindowId = "@7".parse().expect("fixture is a window ID");
    let pane: PaneId = "%4294967295".parse().expect("fixture is a pane ID");

    assert_eq!(session.as_ref(), "$0");
    assert_eq!(window.as_ref(), "@7");
    assert_eq!(pane.as_ref(), "%4294967295");
    assert_eq!(session.to_string(), "$0");
    assert_eq!(window.to_string(), "@7");
    assert_eq!(pane.to_string(), "%4294967295");
}

#[test]
fn ids_reject_empty_wrong_scope_and_non_ascii_digit_shapes() {
    let invalid_session_ids = [
        "",
        "$",
        "@1",
        "%1",
        "1",
        "$+1",
        "$-1",
        "$ 1",
        "$1 ",
        "$1x",
        "$\u{0661}",
        "$4294967296",
    ];
    let invalid_window_ids = [
        "",
        "@",
        "$1",
        "%1",
        "1",
        "@+1",
        "@-1",
        "@ 1",
        "@1 ",
        "@1x",
        "@\u{0661}",
        "@4294967296",
    ];
    let invalid_pane_ids = [
        "",
        "%",
        "$1",
        "@1",
        "1",
        "%+1",
        "%-1",
        "% 1",
        "%1 ",
        "%1x",
        "%\u{0661}",
        "%4294967296",
    ];

    for invalid in invalid_session_ids {
        let error = invalid
            .parse::<SessionId>()
            .expect_err("fixture is not a session ID");
        assert_eq!(error.expected_sigil(), '$');
    }
    for invalid in invalid_window_ids {
        let error = invalid
            .parse::<WindowId>()
            .expect_err("fixture is not a window ID");
        assert_eq!(error.expected_sigil(), '@');
    }
    for invalid in invalid_pane_ids {
        let error = invalid
            .parse::<PaneId>()
            .expect_err("fixture is not a pane ID");
        assert_eq!(error.expected_sigil(), '%');
    }
}

#[test]
fn targets_preserve_scope_specific_identity() {
    let session_id: SessionId = "$12".parse().unwrap();
    let window_id: WindowId = "@34".parse().unwrap();
    let pane_id: PaneId = "%56".parse().unwrap();

    let session = SessionTarget::from(session_id.clone());
    let window = WindowTarget::from(window_id.clone());
    let pane = PaneTarget::from(pane_id.clone());

    assert_eq!(SessionTarget::from(session_id), session);
    assert_eq!(WindowTarget::from(window_id), window);
    assert_eq!(PaneTarget::from(pane_id), pane);
}

#[test]
fn ids_canonicalize_numeric_identity_before_ordering_and_hashing() {
    use std::collections::{BTreeSet, HashSet};

    let first: PaneId = "%1".parse().unwrap();
    let first_with_zeros: PaneId = "%0001".parse().unwrap();
    let second: PaneId = "%2".parse().unwrap();
    let tenth: PaneId = "%10".parse().unwrap();

    assert_eq!(first_with_zeros.as_ref(), "%1");
    assert_eq!(first, first_with_zeros);
    assert!(first < second);
    assert!(second < tenth);
    assert_eq!(
        BTreeSet::from([second.clone(), first.clone()]),
        BTreeSet::from([first.clone(), second.clone()]),
    );
    assert_eq!(
        HashSet::from([first.clone()]),
        HashSet::from([first_with_zeros]),
    );
}
