//! The text dialect wire schema version 1 implies.
//!
//! `filter_serde.rs` locks the shape of an expression: which operators exist
//! and how they serialise. It says nothing about what any of them *match*, so
//! two implementations could agree on every byte of the wire format and still
//! disagree about whether `Straße` equals `STRASSE`.
//!
//! This suite locks the answers. The cases live in a fixture rather than in
//! the assertions so a port in another language can read the same file and be
//! held to the same behaviour.
//!
//! The dialect is Unicode default case folding without normalization for the
//! scalar text operators, and the Rust `regex` grammar for the regex ones.
//! Those are two different things, and the fixture says where they part
//! company: a case-insensitive regex does not fold one character into two.

// The derive implies `query`, and using it here keeps the subject the same
// shape a caller would write.
#![cfg(feature = "derive")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and this file has them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use libtmux::Filterable;
use libtmux::query::Filterable as _;
use serde_json::Value;

/// A single text field, which is all these cases need.
#[derive(Filterable)]
#[filterable(target = "subject", crate = "libtmux")]
struct Subject {
    text: String,
}

fn fixture() -> Value {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dialect-v1/text.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is JSON: {error}", path.display()))
}

/// Read one array of cases, refusing an empty one.
///
/// An empty array would make every assertion below vacuous while the suite
/// still reported success, which is the failure this whole file exists to
/// prevent elsewhere.
fn cases(fixture: &Value, section: &str) -> Vec<Value> {
    let cases = fixture[section]
        .as_array()
        .unwrap_or_else(|| panic!("the fixture has a {section} array"))
        .clone();
    assert!(!cases.is_empty(), "{section} has cases to check");
    cases
}

fn text_of(case: &Value, key: &str) -> String {
    case[key]
        .as_str()
        .unwrap_or_else(|| panic!("case {case} has a string {key}"))
        .to_owned()
}

fn flag_of(case: &Value, key: &str) -> bool {
    case[key]
        .as_bool()
        .unwrap_or_else(|| panic!("case {case} has a boolean {key}"))
}

#[test]
fn case_insensitive_text_operators_use_full_folding_without_normalization() {
    let fixture = fixture();
    let fields = Subject::filter_fields();

    for case in cases(&fixture, "case_folding") {
        let why = text_of(&case, "why");
        let left = text_of(&case, "left");
        let right = text_of(&case, "right");
        let expected = flag_of(&case, "equal");

        let subject = Subject { text: left.clone() };
        let expression = fields.text.eq_ignore_case(right.clone());
        assert_eq!(
            expression.matches(&subject),
            expected,
            "eq_ignore_case({left:?}, {right:?}): {why}",
        );

        // Folding is symmetric, and a port that folds only the pattern would
        // pass the check above on half these cases.
        let flipped = Subject {
            text: right.clone(),
        };
        let expression = fields.text.eq_ignore_case(left.clone());
        assert_eq!(
            expression.matches(&flipped),
            expected,
            "eq_ignore_case({right:?}, {left:?}) the other way round: {why}",
        );

        // `contains` folds the same way `eq` does; a port that folded only for
        // equality would answer differently here.
        let subject = Subject { text: left.clone() };
        let expression = fields.text.contains_ignore_case(right.clone());
        assert_eq!(
            expression.matches(&subject),
            expected,
            "contains_ignore_case({left:?}, {right:?}): {why}",
        );
    }
}

#[test]
fn the_regex_grammar_is_the_one_that_rejects_backreferences() {
    let fixture = fixture();
    let fields = Subject::filter_fields();

    for case in cases(&fixture, "regex_grammar") {
        let why = text_of(&case, "why");
        let pattern = text_of(&case, "pattern");
        let accepted = flag_of(&case, "accepted");

        // A pattern outside the grammar is refused when the expression is
        // built, not silently treated as literal text.
        let built = fields.text.regex(pattern.clone());
        assert_eq!(
            built.is_ok(),
            accepted,
            "regex({pattern:?}) is {}: {why}",
            if accepted {
                "in the grammar"
            } else {
                "outside it"
            },
        );
    }
}

#[test]
fn a_case_insensitive_regex_folds_differently_from_a_text_operator() {
    let fixture = fixture();
    let fields = Subject::filter_fields();

    for case in cases(&fixture, "regex_ignore_case") {
        let why = text_of(&case, "why");
        let pattern = text_of(&case, "pattern");
        let subject = Subject {
            text: text_of(&case, "subject"),
        };
        let expected = flag_of(&case, "matches");

        let expression = fields
            .text
            .regex_ignore_case(pattern.clone())
            .unwrap_or_else(|error| panic!("regex_ignore_case({pattern:?}) builds: {error}"));
        assert_eq!(
            expression.matches(&subject),
            expected,
            "regex_ignore_case({pattern:?}) against {:?}: {why}",
            subject.text,
        );
    }
}

#[test]
fn the_two_case_insensitive_families_disagree_where_the_fixture_says_they_do() {
    // The point of keeping both families: `Straße` equals `STRASSE` as text
    // and does not match `^strasse$` as a regex. A port that implements one
    // family in terms of the other passes every other test in this file.
    let fields = Subject::filter_fields();
    let subject = Subject {
        text: String::from("Straße"),
    };

    let as_text = fields.text.eq_ignore_case(String::from("STRASSE"));
    assert!(as_text.matches(&subject), "text folding is full folding");

    let as_regex = fields
        .text
        .regex_ignore_case(String::from("^strasse$"))
        .expect("the pattern builds");
    assert!(
        !as_regex.matches(&subject),
        "regex case folding is not full folding",
    );
}
