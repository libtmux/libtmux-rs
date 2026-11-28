//! Option metadata and validation without a runtime.

#![allow(clippy::unwrap_used)]

use libtmux_core::options::{
    OptionKind, OptionScope, OptionValue, OptionValueRefusal, option_schema,
};
use libtmux_core::{OptionScope as PlacementScope, TmuxText, since};

#[test]
fn catalog_resolves_aliases_indices_and_unambiguous_prefixes() {
    assert_eq!(option_schema("mous"), option_schema("mouse"));
    assert_eq!(
        option_schema("clock-mode-color"),
        option_schema("clock-mode-colour")
    );
    assert_eq!(
        option_schema("after-new-window[3]").unwrap().kind(),
        OptionKind::Command
    );
    assert!(option_schema("status-").is_none());
    assert!(option_schema("@application").is_none());
}

#[test]
fn catalog_preserves_declared_scopes_and_value_kinds() {
    let mouse = option_schema("mouse").unwrap();
    assert_eq!(mouse.kind(), OptionKind::Flag);
    assert!(mouse.accepts(OptionScope::Session));
    assert!(!mouse.accepts(OptionScope::Window));
    assert!(!mouse.accepts(OptionScope::Pane));

    let retained = option_schema("remain-on-exit").unwrap();
    assert!(retained.accepts(OptionScope::Window));
    assert!(retained.accepts(OptionScope::Pane));
    assert!(!retained.accepts(OptionScope::Session));
    assert_eq!(option_schema("status").unwrap().kind(), OptionKind::Choice);
}

#[test]
fn value_checks_refuse_wrong_kinds_choices_and_integer_bounds() {
    assert!(option_schema("mouse").unwrap().check(&true.into()).is_ok());
    assert!(matches!(
        option_schema("mouse").unwrap().check(&"on".into()),
        Err(OptionValueRefusal::WrongKind {
            expected: OptionKind::Flag
        })
    ));
    assert!(
        option_schema("mode-keys")
            .unwrap()
            .check(&"vi".into())
            .is_ok()
    );
    let refused = option_schema("mode-keys")
        .unwrap()
        .check(&"private-value".into())
        .unwrap_err();
    assert!(matches!(refused, OptionValueRefusal::NotAChoice { .. }));
    assert!(!format!("{refused:?}").contains("private-value"));
    assert!(matches!(
        option_schema("buffer-limit").unwrap().check(&0_i64.into()),
        Err(OptionValueRefusal::OutOfRange { .. })
    ));
}

#[test]
fn decoding_preserves_text_bytes_and_uses_the_declared_kind() {
    assert_eq!(
        OptionValue::decode("mouse", TmuxText::from("on")),
        OptionValue::Flag(true)
    );
    assert!(matches!(
        OptionValue::decode("status", TmuxText::from("on")),
        OptionValue::Text(_)
    ));
    for name in ["@application", "history-limit", "mouse"] {
        let bytes = vec![0xff, b'#', b'{', b'}'];
        let decoded = OptionValue::decode(name, TmuxText::from(bytes.clone()));
        assert_eq!(TmuxText::from(decoded).as_bytes(), bytes);
    }
}

#[test]
fn later_scopes_keep_their_version_requirement() {
    for name in ["pane-border-style", "pane-active-border-style"] {
        let schema = option_schema(name).unwrap();
        assert_eq!(
            schema.required_version(OptionScope::Pane),
            Some(since::PANE_BORDER_STYLE_PER_PANE)
        );
        assert_eq!(schema.required_version(OptionScope::Window), None);
    }
    assert_eq!(
        option_schema("mouse")
            .unwrap()
            .required_version(OptionScope::Session),
        None
    );
}

#[test]
fn option_scope_is_the_shared_placement_value() {
    let scope: PlacementScope = OptionScope::Pane;
    assert_eq!(scope, PlacementScope::Pane);
}
