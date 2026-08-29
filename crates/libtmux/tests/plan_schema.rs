//! The plan operation schema and its public metadata describe one closed set.

#![cfg(all(feature = "plan", feature = "schema"))]
#![allow(clippy::expect_used)]

use libtmux::plan::{Op, OperationKind};

#[test]
fn every_generated_operation_name_has_core_metadata() {
    let schema = serde_json::to_value(schemars::schema_for!(Op)).expect("the schema serializes");
    let variants = schema["oneOf"].as_array().expect("Op is a tagged union");

    assert!(!variants.is_empty());
    for variant in variants {
        let required = variant["required"]
            .as_array()
            .expect("an operation requires its tag");
        assert_eq!(required.len(), 1, "an operation has one tag: {variant}");
        let name = required[0].as_str().expect("the operation tag is text");
        assert!(
            OperationKind::from_wire_name(name).is_some(),
            "missing metadata for {name}",
        );
    }
    assert_eq!(OperationKind::from_wire_name("Unknown"), None);
}
