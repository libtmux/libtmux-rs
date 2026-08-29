//! Version 1 portable-filter schema and optional serde contract tests.

#![cfg(feature = "query")]

use std::collections::BTreeSet;
use std::error::Error as StdError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jsonschema::Validator;
use serde_json::{Value, json};

const SCHEMA_TEXT: &str = include_str!("../schema/filter-v1.schema.json");
const FIXTURE_NAMES: [&str; 3] = ["relations.json", "scalars.json", "text-operators.json"];

type TestResult = Result<(), Box<dyn StdError>>;

fn schema_value() -> Result<Value, serde_json::Error> {
    serde_json::from_str(SCHEMA_TEXT)
}

fn validate_schema_document(schema: &Value) -> Result<(), io::Error> {
    jsonschema::draft202012::meta::validate(schema).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid Draft 2020-12 schema: {error}"),
        )
    })
}

fn compiled_schema() -> Result<Validator, Box<dyn StdError>> {
    let schema = schema_value()?;
    validate_schema_document(&schema)?;
    Ok(jsonschema::draft202012::new(&schema)?)
}

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/filter-v1")
}

fn fixture_text(name: &str) -> Result<String, io::Error> {
    fs::read_to_string(fixture_directory().join(name))
}

fn fixture_names_on_disk() -> Result<BTreeSet<String>, io::Error> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(fixture_directory())? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 fixture name"))?;
        names.insert(name);
    }
    Ok(names)
}

fn assert_closed_object(schema: &Value, properties: &[&str], required: &[&str]) -> TestResult {
    let object = schema
        .as_object()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "schema object is not a map"))?;
    assert_eq!(object.get("type"), Some(&json!("object")));
    assert_eq!(object.get("additionalProperties"), Some(&json!(false)));

    let actual_properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing schema properties"))?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_properties = properties.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual_properties, expected_properties);

    let actual_required = object
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing required members"))?
        .iter()
        .map(|value| {
            value.as_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "required member is not a string",
                )
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected_required = required.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(actual_required, expected_required);
    Ok(())
}

fn assert_only_local_refs_and_no_all_of(value: &Value) {
    match value {
        Value::Object(object) => {
            assert!(!object.contains_key("allOf"));
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                assert!(reference.starts_with("#/$defs/"));
            }
            for nested in object.values() {
                assert_only_local_refs_and_no_all_of(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                assert_only_local_refs_and_no_all_of(nested);
            }
        }
        _ => {}
    }
}

fn definition<'a>(
    definitions: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a Value, io::Error> {
    definitions
        .get(name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing schema definition"))
}

fn assert_envelope_and_expression_shapes(
    schema: &Value,
    definitions: &serde_json::Map<String, Value>,
) {
    assert_eq!(
        definitions.get("name"),
        Some(&json!({"type": "string", "pattern": "^[a-z][a-z0-9_]*$"}))
    );
    assert_eq!(
        schema.get("properties"),
        Some(&json!({
            "version": {"const": 1},
            "target": {"$ref": "#/$defs/name"},
            "expr": {"$ref": "#/$defs/expression"}
        }))
    );
    assert_eq!(
        definitions.get("expression"),
        Some(&json!({
            "oneOf": [
                {"$ref": "#/$defs/and"},
                {"$ref": "#/$defs/or"},
                {"$ref": "#/$defs/not"},
                {"$ref": "#/$defs/stringScalar"},
                {"$ref": "#/$defs/booleanScalar"},
                {"$ref": "#/$defs/membershipScalar"},
                {"$ref": "#/$defs/relation"}
            ]
        }))
    );
}

fn assert_logical_shapes(definitions: &serde_json::Map<String, Value>) -> TestResult {
    for name in ["and", "or"] {
        let definition = definition(definitions, name)?;
        assert_eq!(
            definition
                .get("properties")
                .and_then(|value| value.get("op")),
            Some(&json!({"const": name}))
        );
        assert_eq!(
            definition
                .get("properties")
                .and_then(|value| value.get("args")),
            Some(&json!({
                "type": "array",
                "items": {"$ref": "#/$defs/expression"},
                "minItems": 2
            }))
        );
    }
    assert_eq!(
        definition(definitions, "not")?.get("properties"),
        Some(&json!({
            "op": {"const": "not"},
            "expr": {"$ref": "#/$defs/expression"}
        }))
    );
    Ok(())
}

fn assert_scalar_shapes(definitions: &serde_json::Map<String, Value>) -> TestResult {
    assert_eq!(
        definition(definitions, "stringScalar")?.get("properties"),
        Some(&json!({
            // Ordering lives here because integers ride the wire as text.
            // Which fields accept it is the crate's business, not the
            // schema's: a text field rejects a bound on decode.
            "op": {"enum": [
                "eq", "eq_ignore_case", "contains", "contains_ignore_case",
                "starts_with", "starts_with_ignore_case", "ends_with",
                "ends_with_ignore_case", "regex", "regex_ignore_case",
                "lt", "lte", "gt", "gte"
            ]},
            "field": {"$ref": "#/$defs/name"},
            "value": {"type": "string"}
        }))
    );
    assert_eq!(
        definition(definitions, "booleanScalar")?.get("properties"),
        Some(&json!({
            "op": {"const": "eq"},
            "field": {"$ref": "#/$defs/name"},
            "value": {"type": "boolean"}
        }))
    );
    assert_eq!(
        definition(definitions, "membershipScalar")?.get("properties"),
        Some(&json!({
            "op": {"enum": ["in", "not_in"]},
            "field": {"$ref": "#/$defs/name"},
            "value": {"anyOf": [
                {"type": "array", "items": {"type": "boolean"}},
                {"type": "array", "items": {"type": "string"}}
            ]}
        }))
    );
    Ok(())
}

fn assert_relation_shape(definitions: &serde_json::Map<String, Value>) -> TestResult {
    assert_eq!(
        definition(definitions, "relation")?.get("properties"),
        Some(&json!({
            "op": {"const": "relation"},
            "field": {"$ref": "#/$defs/name"},
            "quantifier": {"enum": ["any", "all", "none", "is"]},
            "expr": {"$ref": "#/$defs/expression"}
        }))
    );
    Ok(())
}

#[test]
fn schema_is_valid_draft_2020_12_and_compiles_offline() -> TestResult {
    let schema = schema_value()?;
    assert_eq!(
        schema.get("$schema"),
        Some(&json!("https://json-schema.org/draft/2020-12/schema"))
    );
    validate_schema_document(&schema)?;
    let _validator = jsonschema::draft202012::new(&schema)?;
    Ok(())
}

#[test]
fn every_wire_object_has_exact_required_members_and_is_closed() -> TestResult {
    let schema = schema_value()?;
    assert_only_local_refs_and_no_all_of(&schema);
    assert_closed_object(
        &schema,
        &["version", "target", "expr"],
        &["version", "target", "expr"],
    )?;
    let definitions = schema
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing schema definitions"))?;
    for (name, properties) in [
        ("and", &["op", "args"][..]),
        ("or", &["op", "args"][..]),
        ("not", &["op", "expr"][..]),
        ("stringScalar", &["op", "field", "value"][..]),
        ("booleanScalar", &["op", "field", "value"][..]),
        ("membershipScalar", &["op", "field", "value"][..]),
        ("relation", &["op", "field", "quantifier", "expr"][..]),
    ] {
        let definition = definitions.get(name).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing object definition")
        })?;
        assert_closed_object(definition, properties, properties)?;
    }

    assert_envelope_and_expression_shapes(&schema, definitions);
    assert_logical_shapes(definitions)?;
    assert_scalar_shapes(definitions)?;
    assert_relation_shape(definitions)?;
    Ok(())
}

#[test]
fn every_discovered_fixture_is_registered_compact_and_schema_valid() -> TestResult {
    let expected = FIXTURE_NAMES
        .into_iter()
        .map(String::from)
        .collect::<BTreeSet<_>>();
    assert_eq!(fixture_names_on_disk()?, expected);

    let validator = compiled_schema()?;
    for name in FIXTURE_NAMES {
        let text = fixture_text(name)?;
        assert!(text.ends_with('\n'), "{name} needs a trailing newline");
        assert_eq!(text.lines().count(), 1, "{name} must stay compact");
        let value: Value = serde_json::from_str(&text)?;
        assert!(validator.is_valid(&value), "{name} must match the schema");
    }
    Ok(())
}

#[test]
fn schema_rejects_closed_shape_name_operator_and_type_defects() -> TestResult {
    let validator = compiled_schema()?;
    for operator in ["in", "not_in"] {
        let empty = json!({
            "version": 1,
            "target": "record",
            "expr": {"op": operator, "field": "name", "value": []}
        });
        assert!(validator.is_valid(&empty));
    }
    let invalid = [
        json!({"version": 1, "expr": {"op": "eq", "field": "name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "name", "value": "x"}, "extra": false}),
        json!({"version": 1, "target": "Record", "expr": {"op": "eq", "field": "name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "Name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "unknown", "field": "name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "relation", "field": "children", "quantifier": "unknown", "expr": {"op": "eq", "field": "name", "value": "x"}}}),
        json!({"version": 1, "target": "record", "expr": {"op": "and", "args": [{"op": "eq", "field": "name", "value": "x"}]}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "name", "value": 1}}),
        json!({"version": 1, "target": "record", "expr": {"op": "in", "field": "name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "in", "field": "name", "value": ["x", true]}}),
        json!({"version": 1, "target": "record", "expr": {"op": "in", "field": "name", "value": 1}}),
        json!({"version": 1, "target": "record", "expr": {"op": "in", "field": "name", "value": {}}}),
        json!({"version": 1, "target": "record", "expr": {"op": "in", "field": "name", "value": null}}),
    ];

    for document in invalid {
        assert!(!validator.is_valid(&document), "schema accepted {document}");
    }
    Ok(())
}

#[test]
fn schema_uses_the_same_host_decoded_version_boundary() -> TestResult {
    let validator = compiled_schema()?;
    for (source, accepted) in [
        ("1", true),
        ("1.0", true),
        ("1e0", true),
        ("1.0000000000000001", true),
        ("0.9999999999999999", false),
        ("1.0000000000000002", false),
        ("0", false),
        ("2", false),
        ("2.0", false),
        ("2e0", false),
        ("1.5", false),
        ("\"1\"", false),
    ] {
        let version: Value = serde_json::from_str(source)?;
        let document = json!({
            "version": version,
            "target": "record",
            "expr": {"op": "eq", "field": "name", "value": "x"}
        });
        assert_eq!(validator.is_valid(&document), accepted, "version {source}");
    }
    Ok(())
}

#[test]
fn schema_enforces_the_exact_ascii_name_grammar_for_targets_and_fields() -> TestResult {
    let validator = compiled_schema()?;
    for name in ["a", "a0", "a_", "a__", "record_10"] {
        let target = json!({
            "version": 1,
            "target": name,
            "expr": {"op": "eq", "field": "name", "value": "x"}
        });
        let field = json!({
            "version": 1,
            "target": "record",
            "expr": {"op": "eq", "field": name, "value": "x"}
        });
        assert!(validator.is_valid(&target), "valid target {name}");
        assert!(validator.is_valid(&field), "valid field {name}");
    }

    for name in ["", "0a", "_a", "a-b", "é", "a\n"] {
        let target = json!({
            "version": 1,
            "target": name,
            "expr": {"op": "eq", "field": "name", "value": "x"}
        });
        let field = json!({
            "version": 1,
            "target": "record",
            "expr": {"op": "eq", "field": name, "value": "x"}
        });
        assert!(!validator.is_valid(&target), "invalid target {name:?}");
        assert!(!validator.is_valid(&field), "invalid field {name:?}");
    }
    Ok(())
}

#[test]
fn generic_schema_leaves_application_typing_to_deserialization() -> TestResult {
    let validator = compiled_schema()?;
    let documents = [
        json!({"version": 1, "target": "other", "expr": {"op": "eq", "field": "name", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "unknown", "value": "x"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "phase", "value": "unknown"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "i8_value", "value": "01"}}),
        json!({"version": 1, "target": "record", "expr": {"op": "regex", "field": "name", "value": "["}}),
        json!({"version": 1, "target": "record", "expr": {"op": "eq", "field": "flag", "value": "true"}}),
    ];
    for document in documents {
        assert!(validator.is_valid(&document));
    }
    Ok(())
}

#[cfg(feature = "serde")]
mod serde_contract {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use libtmux::query::{
        __private::{self, IntegerKind, Predicate},
        BoolField, EnumField, FilterEnum, FilterExpr, FilterExpressionError,
        FilterExpressionErrorKind, Filterable, IntegerField, ManyRelation, OneRelation, TextField,
    };
    use serde::de::{DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, Visitor};
    use serde_json::Value;
    use static_assertions::assert_not_impl_any;

    use super::{FIXTURE_NAMES, TestResult, compiled_schema, fixture_text};

    #[allow(dead_code)]
    struct NotFilterable;

    assert_not_impl_any!(FilterExpr<NotFilterable>: serde::Serialize, serde::de::DeserializeOwned);

    #[derive(Clone, Copy)]
    enum Phase {
        Ready,
        Blocked,
        Numeric,
    }

    impl FilterEnum for Phase {
        const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked", "1"];

        fn filter_name(&self) -> &'static str {
            match self {
                Self::Ready => "ready",
                Self::Blocked => "blocked",
                Self::Numeric => "1",
            }
        }
    }

    struct LeafFields {
        label: TextField<Leaf>,
    }

    struct Leaf {
        label: &'static [u8],
    }

    impl Filterable for Leaf {
        type Fields = LeafFields;

        const FILTER_TARGET: &'static str = "leaf";

        fn filter_fields() -> Self::Fields {
            LeafFields {
                label: __private::text_field(Self::FILTER_TARGET, "label"),
            }
        }

        fn __filter_matches(&self, predicate: &Predicate) -> bool {
            match predicate.field() {
                "label" => predicate.matches_text(self.label),
                _ => false,
            }
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            match predicate.field() {
                "label" => predicate.validate_text(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    struct ChildFields {
        label: TextField<Child>,
        active: BoolField<Child>,
        leaves: ManyRelation<Child, Leaf>,
    }

    struct Child {
        label: &'static [u8],
        active: bool,
        leaves: Vec<Leaf>,
    }

    impl Filterable for Child {
        type Fields = ChildFields;

        const FILTER_TARGET: &'static str = "child";

        fn filter_fields() -> Self::Fields {
            ChildFields {
                label: __private::text_field(Self::FILTER_TARGET, "label"),
                active: __private::bool_field(Self::FILTER_TARGET, "active"),
                leaves: __private::many_relation(Self::FILTER_TARGET, "leaves"),
            }
        }

        fn __filter_matches(&self, predicate: &Predicate) -> bool {
            match predicate.field() {
                "label" => predicate.matches_text(self.label),
                "active" => predicate.matches_bool(self.active),
                "leaves" => predicate.matches_many(&self.leaves),
                _ => false,
            }
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            match predicate.field() {
                "label" => predicate.validate_text(),
                "active" => predicate.validate_bool(),
                "leaves" => predicate.validate_many::<Leaf>(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    struct RecordFields {
        name: TextField<Record>,
        flag: BoolField<Record>,
        phase: EnumField<Record, Phase>,
        i8_value: IntegerField<Record, i8>,
        i16_value: IntegerField<Record, i16>,
        i32_value: IntegerField<Record, i32>,
        i64_value: IntegerField<Record, i64>,
        i128_value: IntegerField<Record, i128>,
        u8_value: IntegerField<Record, u8>,
        u16_value: IntegerField<Record, u16>,
        u32_value: IntegerField<Record, u32>,
        u64_value: IntegerField<Record, u64>,
        u128_value: IntegerField<Record, u128>,
        children: ManyRelation<Record, Child>,
        owner: OneRelation<Record, Child>,
    }

    struct Record {
        name: &'static [u8],
        flag: bool,
        phase: Phase,
        i8_value: i8,
        i16_value: i16,
        i32_value: i32,
        i64_value: i64,
        i128_value: i128,
        u8_value: u8,
        u16_value: u16,
        u32_value: u32,
        u64_value: u64,
        u128_value: u128,
        children: Vec<Child>,
        owner: Option<Child>,
    }

    impl Filterable for Record {
        type Fields = RecordFields;

        const FILTER_TARGET: &'static str = "record";

        fn filter_fields() -> Self::Fields {
            RecordFields {
                name: __private::text_field(Self::FILTER_TARGET, "name"),
                flag: __private::bool_field(Self::FILTER_TARGET, "flag"),
                phase: __private::enum_field(Self::FILTER_TARGET, "phase"),
                i8_value: __private::integer_field(Self::FILTER_TARGET, "i8_value"),
                i16_value: __private::integer_field(Self::FILTER_TARGET, "i16_value"),
                i32_value: __private::integer_field(Self::FILTER_TARGET, "i32_value"),
                i64_value: __private::integer_field(Self::FILTER_TARGET, "i64_value"),
                i128_value: __private::integer_field(Self::FILTER_TARGET, "i128_value"),
                u8_value: __private::integer_field(Self::FILTER_TARGET, "u8_value"),
                u16_value: __private::integer_field(Self::FILTER_TARGET, "u16_value"),
                u32_value: __private::integer_field(Self::FILTER_TARGET, "u32_value"),
                u64_value: __private::integer_field(Self::FILTER_TARGET, "u64_value"),
                u128_value: __private::integer_field(Self::FILTER_TARGET, "u128_value"),
                children: __private::many_relation(Self::FILTER_TARGET, "children"),
                owner: __private::one_relation(Self::FILTER_TARGET, "owner"),
            }
        }

        fn __filter_matches(&self, predicate: &Predicate) -> bool {
            match predicate.field() {
                "name" => predicate.matches_text(self.name),
                "flag" => predicate.matches_bool(self.flag),
                "phase" => predicate.matches_enum(self.phase.filter_name()),
                "i8_value" => predicate.matches_signed(i128::from(self.i8_value)),
                "i16_value" => predicate.matches_signed(i128::from(self.i16_value)),
                "i32_value" => predicate.matches_signed(i128::from(self.i32_value)),
                "i64_value" => predicate.matches_signed(i128::from(self.i64_value)),
                "i128_value" => predicate.matches_signed(self.i128_value),
                "u8_value" => predicate.matches_unsigned(u128::from(self.u8_value)),
                "u16_value" => predicate.matches_unsigned(u128::from(self.u16_value)),
                "u32_value" => predicate.matches_unsigned(u128::from(self.u32_value)),
                "u64_value" => predicate.matches_unsigned(u128::from(self.u64_value)),
                "u128_value" => predicate.matches_unsigned(self.u128_value),
                "children" => predicate.matches_many(&self.children),
                "owner" => predicate.matches_one(self.owner.as_ref()),
                _ => false,
            }
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            match predicate.field() {
                "name" => predicate.validate_text(),
                "flag" => predicate.validate_bool(),
                "phase" => predicate.validate_enum(Phase::FILTER_VARIANTS),
                "i8_value" => predicate.validate_integer(IntegerKind::I8),
                "i16_value" => predicate.validate_integer(IntegerKind::I16),
                "i32_value" => predicate.validate_integer(IntegerKind::I32),
                "i64_value" => predicate.validate_integer(IntegerKind::I64),
                "i128_value" => predicate.validate_integer(IntegerKind::I128),
                "u8_value" => predicate.validate_integer(IntegerKind::U8),
                "u16_value" => predicate.validate_integer(IntegerKind::U16),
                "u32_value" => predicate.validate_integer(IntegerKind::U32),
                "u64_value" => predicate.validate_integer(IntegerKind::U64),
                "u128_value" => predicate.validate_integer(IntegerKind::U128),
                "children" => predicate.validate_many::<Child>(),
                "owner" => predicate.validate_one::<Child>(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    fn fixture_record() -> Record {
        let child = || Child {
            label: b"clear",
            active: true,
            leaves: vec![Leaf { label: b"leaf" }],
        };
        Record {
            name: "Straße".as_bytes(),
            flag: true,
            phase: Phase::Ready,
            i8_value: i8::MIN,
            i16_value: i16::MAX,
            i32_value: i32::MIN,
            i64_value: i64::MAX,
            i128_value: i128::MIN,
            u8_value: u8::MAX,
            u16_value: u16::MAX,
            u32_value: u32::MAX,
            u64_value: u64::MAX,
            u128_value: u128::MAX,
            children: vec![child()],
            owner: Some(child()),
        }
    }

    fn text_expression() -> Result<FilterExpr<Record>, FilterExpressionError> {
        let fields = Record::filter_fields();
        Ok(fields
            .name
            .eq("Straße")
            .and(fields.name.eq_ignore_case("STRASSE"))
            .and(fields.name.contains("tra"))
            .and(fields.name.contains_ignore_case("STR"))
            .and(fields.name.starts_with("Str"))
            .and(fields.name.starts_with_ignore_case("STR"))
            .and(fields.name.ends_with("ße"))
            .and(fields.name.ends_with_ignore_case("SSE"))
            .and(fields.name.is_in(["other", "Straße"]))
            .and(fields.name.not_in(["other", "third"]))
            .and(fields.name.regex("^Straße$")?)
            .and(
                fields
                    .name
                    .contains_ignore_case("STRASSE")
                    .or(fields.name.regex_ignore_case("^STRASSE$")?),
            )
            .and(fields.name.eq("other").not()))
    }

    fn scalar_expression() -> FilterExpr<Record> {
        let fields = Record::filter_fields();
        fields
            .flag
            .eq(true)
            .and(fields.flag.is_in([false, true]))
            .and(fields.flag.not_in([false]))
            .and(fields.phase.eq(Phase::Ready))
            .and(fields.phase.is_in([Phase::Blocked, Phase::Ready]))
            .and(fields.phase.not_in([Phase::Blocked]))
            .and(fields.i8_value.eq(i8::MIN))
            .and(fields.i16_value.eq(i16::MAX))
            .and(fields.i32_value.eq(i32::MIN))
            .and(fields.i64_value.eq(i64::MAX))
            .and(fields.i128_value.eq(i128::MIN))
            .and(fields.u8_value.eq(u8::MAX))
            .and(fields.u16_value.eq(u16::MAX))
            .and(fields.u32_value.eq(u32::MAX))
            .and(fields.u64_value.eq(u64::MAX))
            .and(fields.u128_value.eq(u128::MAX))
    }

    fn relation_expression() -> FilterExpr<Record> {
        let record = Record::filter_fields();
        let child = Child::filter_fields();
        let leaf = Leaf::filter_fields();
        record
            .children
            .any(child.leaves.any(leaf.label.eq("leaf")))
            .and(record.children.all(child.active.eq(true)))
            .and(record.children.none(child.label.eq("blocked")))
            .and(record.owner.is(child.leaves.any(leaf.label.eq("leaf"))))
    }

    fn invalid_test_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into()))
    }

    #[derive(Clone, Copy)]
    enum VersionToken {
        I8(i8),
        I16(i16),
        I32(i32),
        I64(i64),
        I128(i128),
        U8(u8),
        U16(u16),
        U32(u32),
        U64(u64),
        U128(u128),
        F64(f64),
    }

    struct VersionTokenDeserializer(VersionToken);

    impl<'de> Deserializer<'de> for VersionTokenDeserializer {
        type Error = serde_json::Error;

        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            match self.0 {
                VersionToken::I8(value) => visitor.visit_i8(value),
                VersionToken::I16(value) => visitor.visit_i16(value),
                VersionToken::I32(value) => visitor.visit_i32(value),
                VersionToken::I64(value) => visitor.visit_i64(value),
                VersionToken::I128(value) => visitor.visit_i128(value),
                VersionToken::U8(value) => visitor.visit_u8(value),
                VersionToken::U16(value) => visitor.visit_u16(value),
                VersionToken::U32(value) => visitor.visit_u32(value),
                VersionToken::U64(value) => visitor.visit_u64(value),
                VersionToken::U128(value) => visitor.visit_u128(value),
                VersionToken::F64(value) => visitor.visit_f64(value),
            }
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
            struct enum identifier ignored_any
        }
    }

    struct VersionEnvelopeMap {
        index: u8,
        token: VersionToken,
    }

    impl<'de> MapAccess<'de> for VersionEnvelopeMap {
        type Error = serde_json::Error;

        fn next_key_seed<K: DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Self::Error> {
            let key = match self.index {
                0 => "version",
                1 => "target",
                2 => "expr",
                _ => return Ok(None),
            };
            seed.deserialize(serde::de::value::StrDeserializer::<serde_json::Error>::new(
                key,
            ))
            .map(Some)
        }

        fn next_value_seed<V: DeserializeSeed<'de>>(
            &mut self,
            seed: V,
        ) -> Result<V::Value, Self::Error> {
            let index = self.index;
            self.index += 1;
            match index {
                0 => seed.deserialize(VersionTokenDeserializer(self.token)),
                1 => seed.deserialize(serde::de::value::StrDeserializer::<serde_json::Error>::new(
                    "record",
                )),
                2 => seed.deserialize(
                    serde_json::json!({"op": "eq", "field": "name", "value": "x"})
                        .into_deserializer(),
                ),
                _ => Err(serde::de::Error::custom("unexpected envelope value")),
            }
        }

        fn size_hint(&self) -> Option<usize> {
            Some(usize::from(3_u8.saturating_sub(self.index)))
        }
    }

    struct VersionEnvelopeDeserializer(VersionToken);

    impl<'de> Deserializer<'de> for VersionEnvelopeDeserializer {
        type Error = serde_json::Error;

        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            visitor.visit_map(VersionEnvelopeMap {
                index: 0,
                token: self.0,
            })
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map
            struct enum identifier ignored_any
        }
    }

    fn encode<T: Filterable>(expression: &FilterExpr<T>) -> Result<String, serde_json::Error> {
        serde_json::to_string(expression)
    }

    fn decode_from<'de, T, D>(deserializer: D) -> Result<FilterExpr<T>, D::Error>
    where
        T: Filterable,
        D: Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer)
    }

    fn decode<T: Filterable>(source: &str) -> Result<FilterExpr<T>, serde_json::Error> {
        let mut deserializer = serde_json::Deserializer::from_str(source);
        let expression = decode_from::<T, _>(&mut deserializer)?;
        deserializer.end()?;
        Ok(expression)
    }

    const fn category_text(kind: FilterExpressionErrorKind) -> &'static str {
        match kind {
            FilterExpressionErrorKind::InvalidRegex => "invalid regular expression",
            FilterExpressionErrorKind::UnsupportedVersion => "unsupported expression version",
            FilterExpressionErrorKind::InvalidTarget => "invalid expression target",
            FilterExpressionErrorKind::UnknownField => "unknown expression field",
            FilterExpressionErrorKind::UnknownOperator => "unknown expression operator",
            FilterExpressionErrorKind::UnknownQuantifier => "unknown relation quantifier",
            FilterExpressionErrorKind::InvalidLiteral => "invalid expression literal",
            FilterExpressionErrorKind::InvalidStructure => "invalid expression structure",
            _ => "unknown future expression error",
        }
    }

    fn assert_decode_error<T: Filterable>(
        source: &str,
        expected: FilterExpressionErrorKind,
        sentinels: &[&str],
    ) -> TestResult {
        let Err(error) = decode::<T>(source) else {
            return Err(invalid_test_error("invalid document decoded successfully"));
        };
        let display = error.to_string();
        let debug = format!("{error:?}");
        let library_message = display
            .split_once(" at line ")
            .map_or(display.as_str(), |(message, _)| message);
        assert_eq!(library_message, category_text(expected));
        for sentinel in sentinels {
            assert!(!display.contains(sentinel));
            assert!(!debug.contains(sentinel));
        }
        Ok(())
    }

    fn assert_encode_error_without_output<T: Filterable>(
        expression: &FilterExpr<T>,
        expected: FilterExpressionErrorKind,
        sentinels: &[&str],
    ) -> TestResult {
        let mut output = Vec::new();
        let result = {
            let mut serializer = serde_json::Serializer::new(&mut output);
            serde::Serialize::serialize(expression, &mut serializer)
        };
        let Err(error) = result else {
            return Err(invalid_test_error("invalid expression serialized"));
        };
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert_eq!(display, category_text(expected));
        assert!(output.is_empty());
        for sentinel in sentinels {
            assert!(!display.contains(sentinel));
            assert!(!debug.contains(sentinel));
        }
        Ok(())
    }

    fn scalar_document(field: &str, operator: &str, value: &str) -> String {
        format!(
            "{{\"version\":1,\"target\":\"record\",\"expr\":{{\"op\":\"{operator}\",\"field\":\"{field}\",\"value\":{value}}}}}"
        )
    }

    #[test]
    fn serde_traits_are_available_for_filter_expressions() -> TestResult {
        let expression = Record::filter_fields().name.eq("build");
        let encoded = encode(&expression)?;
        let decoded = decode::<Record>(&encoded)?;
        assert_eq!(decoded, expression);
        Ok(())
    }

    #[test]
    fn golden_fixtures_round_trip_equal_authored_expressions_and_match() -> TestResult {
        let record = fixture_record();
        let expressions = [
            (FIXTURE_NAMES[0], relation_expression()),
            (FIXTURE_NAMES[1], scalar_expression()),
            (FIXTURE_NAMES[2], text_expression()?),
        ];
        let schema = compiled_schema()?;

        for (name, authored) in expressions {
            let fixture = fixture_text(name)?;
            let encoded = encode(&authored)?;
            assert_eq!(format!("{encoded}\n"), fixture);
            let value: Value = serde_json::from_str(&fixture)?;
            assert!(schema.is_valid(&value));
            let decoded = decode::<Record>(&fixture)?;
            assert_eq!(decoded, authored);
            assert!(decoded.matches(&record));
            assert_eq!(encode(&decoded)?, encoded);
            assert_eq!(encode(&decoded.clone())?, encoded);
        }
        Ok(())
    }

    #[test]
    fn decoded_versions_follow_host_numeric_values_and_serialize_canonically() -> TestResult {
        for source in ["1", "1.0", "1e0", "1.0000000000000001"] {
            let document = format!(
                "{{\"version\":{source},\"target\":\"record\",\"expr\":{{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}}"
            );
            let expression = decode::<Record>(&document)?;
            assert_eq!(
                encode(&expression)?,
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}"
            );
        }

        for source in ["-1", "0", "2", "2.0", "2e0"] {
            let document = format!(
                "{{\"version\":{source},\"target\":\"record\",\"expr\":{{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}}"
            );
            assert_decode_error::<Record>(
                &document,
                FilterExpressionErrorKind::UnsupportedVersion,
                &[],
            )?;
        }
        for source in [
            "0.9999999999999999",
            "1.0000000000000002",
            "1.5",
            "\"1\"",
            "true",
            "null",
            "{}",
            "[]",
        ] {
            let document = format!(
                "{{\"version\":{source},\"target\":\"record\",\"expr\":{{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}}"
            );
            assert_decode_error::<Record>(
                &document,
                FilterExpressionErrorKind::InvalidStructure,
                &[],
            )?;
        }
        Ok(())
    }

    #[test]
    fn string_versions_are_invalid_without_disclosure() -> TestResult {
        assert_decode_error::<Record>(
            "{\"version\":\"secret_version\",\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            FilterExpressionErrorKind::InvalidStructure,
            &["secret_version"],
        )
    }

    #[test]
    fn version_visitor_accepts_every_integer_width_and_rejects_non_finite_floats() -> TestResult {
        for token in [
            VersionToken::I8(1),
            VersionToken::I16(1),
            VersionToken::I32(1),
            VersionToken::I64(1),
            VersionToken::I128(1),
            VersionToken::U8(1),
            VersionToken::U16(1),
            VersionToken::U32(1),
            VersionToken::U64(1),
            VersionToken::U128(1),
        ] {
            let expression = decode_from::<Record, _>(VersionEnvelopeDeserializer(token))?;
            assert_eq!(
                encode(&expression)?,
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}"
            );
        }
        for token in [
            VersionToken::I8(-1),
            VersionToken::I16(-1),
            VersionToken::I32(-1),
            VersionToken::I64(-1),
            VersionToken::I128(-1),
            VersionToken::U8(2),
            VersionToken::U16(2),
            VersionToken::U32(2),
            VersionToken::U64(2),
            VersionToken::U128(2),
        ] {
            let Err(error) = decode_from::<Record, _>(VersionEnvelopeDeserializer(token)) else {
                return Err(invalid_test_error("unsupported version decoded"));
            };
            assert_eq!(
                error.to_string(),
                category_text(FilterExpressionErrorKind::UnsupportedVersion)
            );
        }
        for token in [
            VersionToken::F64(f64::NAN),
            VersionToken::F64(f64::INFINITY),
            VersionToken::F64(f64::NEG_INFINITY),
        ] {
            let Err(error) = decode_from::<Record, _>(VersionEnvelopeDeserializer(token)) else {
                return Err(invalid_test_error("non-finite version decoded"));
            };
            assert_eq!(
                error.to_string(),
                category_text(FilterExpressionErrorKind::InvalidStructure)
            );
        }
        Ok(())
    }

    const STRUCTURAL_DEFECTS: &[(&str, &[&str])] = &[
        (
            "{\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            &[],
        ),
        ("{\"version\":1,\"target\":\"record\"}", &[]),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}]}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"field\":\"name\",\"value\":\"x\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"value\":\"x\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\"}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"extra\":false}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\",\"extra\":false}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}],\"extra\":false}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"extra\":false}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"},\"extra\":false}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":{}}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":\"secret_args_scalar\"}}",
            &["secret_args_scalar"],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":7}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":true}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":null}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}]}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":1}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":null}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":[\"secret_eq_array\"]}}",
            &["secret_eq_array"],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":{\"secret_key\":\"secret_object\"}}}",
            &["secret_key", "secret_object"],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":[\"secret_mixed\",true]}}",
            &["secret_mixed"],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":[1]}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":\"secret_rhs\"}}",
            &["secret_rhs"],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":1}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":null}}",
            &[],
        ),
        (
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":{\"secret_key\":\"secret_membership\"}}}",
            &["secret_key", "secret_membership"],
        ),
        ("\"secret_envelope_scalar\"", &["secret_envelope_scalar"]),
        ("7", &[]),
        ("true", &[]),
        ("null", &[]),
        ("[]", &[]),
    ];

    #[test]
    fn schema_and_serde_reject_representative_structural_defects() -> TestResult {
        let schema = compiled_schema()?;
        for (document, sentinels) in STRUCTURAL_DEFECTS {
            let value: Value = serde_json::from_str(document)?;
            assert!(!schema.is_valid(&value));
            assert_decode_error::<Record>(
                document,
                FilterExpressionErrorKind::InvalidStructure,
                sentinels,
            )?;
        }
        Ok(())
    }

    #[test]
    fn duplicate_keys_at_every_object_shape_are_rejected_without_disclosure() -> TestResult {
        let duplicates = [
            "{\"version\":1,\"version\":2,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            "{\"version\":1,\"target\":\"record\",\"target\":\"secret_target\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_expr\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"op\":\"secret_op\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}]}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}],\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_args\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"c\"}]}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"or\",\"op\":\"secret_or_op\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}]}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}],\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_or_args\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"c\"}]}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"op\":\"secret_not\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_not_expr\"}}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"op\":\"secret_scalar_op\",\"field\":\"name\",\"value\":\"x\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"field\":\"secret_field\",\"value\":\"x\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\",\"value\":\"secret_value\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"op\":\"secret_relation_op\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"field\":\"secret_relation_field\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"quantifier\":\"secret_quantifier\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"}}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"},\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret_relation_expr\"}}}",
        ];
        for document in duplicates {
            assert_decode_error::<Record>(
                document,
                FilterExpressionErrorKind::InvalidStructure,
                &["secret"],
            )?;
        }
        Ok(())
    }

    #[test]
    fn unknown_members_at_every_object_shape_are_rejected_without_disclosure() -> TestResult {
        let documents = [
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"secret_unknown\":\"secret_value\"}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}],\"secret_unknown\":\"secret_value\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"}],\"secret_unknown\":\"secret_value\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},\"secret_unknown\":\"secret_value\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\",\"secret_unknown\":\"secret_value\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"x\"},\"secret_unknown\":\"secret_value\"}}",
        ];
        for document in documents {
            assert_decode_error::<Record>(
                document,
                FilterExpressionErrorKind::InvalidStructure,
                &["secret_unknown", "secret_value"],
            )?;
        }
        Ok(())
    }

    #[test]
    fn duplicate_operator_detection_does_not_depend_on_value_or_position() -> TestResult {
        let documents = [
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_same\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_first\",\"op\":\"eq\"}}",
            "{\"version\":1,\"target\":\"record\",\"expr\":{\"field\":\"name\",\"value\":\"secret_last\",\"op\":\"eq\",\"op\":\"eq\"}}",
        ];
        for document in documents {
            assert_decode_error::<Record>(
                document,
                FilterExpressionErrorKind::InvalidStructure,
                &["secret"],
            )?;
        }
        Ok(())
    }

    #[test]
    fn semantic_failure_precedence_is_independent_of_member_order() -> TestResult {
        let cases: [(&str, &str, FilterExpressionErrorKind, &[&str]); 6] = [
            (
                "{\"version\":2,\"extra\":\"secret_extra\",\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
                "{\"expr\":{\"value\":\"x\",\"field\":\"name\",\"op\":\"eq\"},\"target\":\"record\",\"extra\":\"secret_extra\",\"version\":2}",
                FilterExpressionErrorKind::InvalidStructure,
                &["secret_extra"],
            ),
            (
                "{\"version\":2,\"target\":\"BadTarget\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
                "{\"expr\":{\"value\":\"x\",\"field\":\"name\",\"op\":\"eq\"},\"target\":\"BadTarget\",\"version\":2}",
                FilterExpressionErrorKind::UnsupportedVersion,
                &["BadTarget"],
            ),
            (
                "{\"version\":1,\"target\":\"other_target\",\"expr\":{\"op\":\"secret_operator\",\"field\":\"name\",\"value\":\"x\"}}",
                "{\"expr\":{\"value\":\"x\",\"field\":\"name\",\"op\":\"secret_operator\"},\"target\":\"other_target\",\"version\":1}",
                FilterExpressionErrorKind::InvalidTarget,
                &["other_target", "secret_operator"],
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"secret_operator\",\"field\":\"Bad-Field\",\"value\":\"secret_literal\"}}",
                "{\"expr\":{\"value\":\"secret_literal\",\"field\":\"Bad-Field\",\"op\":\"secret_operator\"},\"target\":\"record\",\"version\":1}",
                FilterExpressionErrorKind::UnknownOperator,
                &["secret_operator", "Bad-Field", "secret_literal"],
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"regex\",\"field\":\"secret_field\",\"value\":\"[secret_regex\"}}",
                "{\"expr\":{\"value\":\"[secret_regex\",\"field\":\"secret_field\",\"op\":\"regex\"},\"target\":\"record\",\"version\":1}",
                FilterExpressionErrorKind::UnknownField,
                &["secret_field", "secret_regex"],
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"is\",\"expr\":{\"op\":\"regex\",\"field\":\"label\",\"value\":\"[secret_nested\"}}}",
                "{\"expr\":{\"expr\":{\"value\":\"[secret_nested\",\"field\":\"label\",\"op\":\"regex\"},\"quantifier\":\"is\",\"field\":\"children\",\"op\":\"relation\"},\"target\":\"record\",\"version\":1}",
                FilterExpressionErrorKind::UnknownQuantifier,
                &["secret_nested"],
            ),
        ];

        for (forward, reverse, category, sentinels) in cases {
            assert_decode_error::<Record>(forward, category, sentinels)?;
            assert_decode_error::<Record>(reverse, category, sentinels)?;
        }
        Ok(())
    }

    #[test]
    fn relation_cardinality_precedes_nested_semantic_errors() -> TestResult {
        let cases = [
            (
                "{\"op\":\"eq\",\"field\":\"Bad-Field\",\"value\":\"secret_nested_literal\"}",
                &["Bad-Field", "secret_nested_literal"][..],
            ),
            (
                "{\"op\":\"secret_nested_operator\",\"field\":\"label\",\"value\":\"secret_nested_literal\"}",
                &["secret_nested_operator", "secret_nested_literal"][..],
            ),
        ];
        for (nested, sentinels) in cases {
            let document = format!(
                "{{\"version\":1,\"target\":\"record\",\"expr\":{{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"is\",\"expr\":{nested}}}}}"
            );
            assert_decode_error::<Record>(
                &document,
                FilterExpressionErrorKind::UnknownQuantifier,
                sentinels,
            )?;
        }
        Ok(())
    }

    #[test]
    fn every_fixed_width_integer_accepts_extrema_and_rejects_one_step_overflow() -> TestResult {
        let boundaries = [
            ("i8_value", "-128", "127", "-129", "128"),
            ("i16_value", "-32768", "32767", "-32769", "32768"),
            (
                "i32_value",
                "-2147483648",
                "2147483647",
                "-2147483649",
                "2147483648",
            ),
            (
                "i64_value",
                "-9223372036854775808",
                "9223372036854775807",
                "-9223372036854775809",
                "9223372036854775808",
            ),
            (
                "i128_value",
                "-170141183460469231731687303715884105728",
                "170141183460469231731687303715884105727",
                "-170141183460469231731687303715884105729",
                "170141183460469231731687303715884105728",
            ),
            ("u8_value", "0", "255", "-1", "256"),
            ("u16_value", "0", "65535", "-1", "65536"),
            ("u32_value", "0", "4294967295", "-1", "4294967296"),
            (
                "u64_value",
                "0",
                "18446744073709551615",
                "-1",
                "18446744073709551616",
            ),
            (
                "u128_value",
                "0",
                "340282366920938463463374607431768211455",
                "-1",
                "340282366920938463463374607431768211456",
            ),
        ];

        for (field, minimum, maximum, below, above) in boundaries {
            for accepted in [minimum, maximum] {
                let value = format!("\"{accepted}\"");
                let expression = decode::<Record>(&scalar_document(field, "eq", &value))?;
                assert_eq!(encode(&expression)?, scalar_document(field, "eq", &value));
            }
            for rejected in [below, above] {
                let value = format!("\"{rejected}\"");
                assert_decode_error::<Record>(
                    &scalar_document(field, "eq", &value),
                    FilterExpressionErrorKind::InvalidLiteral,
                    &[],
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn integer_strings_are_canonical_ascii_and_field_typed() -> TestResult {
        let signed_zero = decode::<Record>(&scalar_document("i8_value", "eq", "\"0\""))?;
        assert_eq!(signed_zero, Record::filter_fields().i8_value.eq(0));
        for rejected in ["+1", "01", "-0", " 1", "1 ", "١", "--1"] {
            for field in ["i64_value", "u64_value"] {
                assert_decode_error::<Record>(
                    &scalar_document(field, "eq", &format!("\"{rejected}\"")),
                    FilterExpressionErrorKind::InvalidLiteral,
                    &[],
                )?;
            }
        }
        assert_decode_error::<Record>(
            &scalar_document("u64_value", "eq", "\"-1\""),
            FilterExpressionErrorKind::InvalidLiteral,
            &[],
        )?;
        Ok(())
    }

    fn empty_membership(field: &str, excluded: bool) -> FilterExpr<Record> {
        let fields = Record::filter_fields();
        match (field, excluded) {
            ("name", false) => fields.name.is_in(Vec::<String>::new()),
            ("name", true) => fields.name.not_in(Vec::<String>::new()),
            ("flag", false) => fields.flag.is_in(Vec::<bool>::new()),
            ("flag", true) => fields.flag.not_in(Vec::<bool>::new()),
            ("phase", false) => fields.phase.is_in(Vec::<Phase>::new()),
            ("phase", true) => fields.phase.not_in(Vec::<Phase>::new()),
            ("i8_value", false) => fields.i8_value.is_in(Vec::<i8>::new()),
            ("i8_value", true) => fields.i8_value.not_in(Vec::<i8>::new()),
            ("i16_value", false) => fields.i16_value.is_in(Vec::<i16>::new()),
            ("i16_value", true) => fields.i16_value.not_in(Vec::<i16>::new()),
            ("i32_value", false) => fields.i32_value.is_in(Vec::<i32>::new()),
            ("i32_value", true) => fields.i32_value.not_in(Vec::<i32>::new()),
            ("i64_value", false) => fields.i64_value.is_in(Vec::<i64>::new()),
            ("i64_value", true) => fields.i64_value.not_in(Vec::<i64>::new()),
            ("i128_value", false) => fields.i128_value.is_in(Vec::<i128>::new()),
            ("i128_value", true) => fields.i128_value.not_in(Vec::<i128>::new()),
            ("u8_value", false) => fields.u8_value.is_in(Vec::<u8>::new()),
            ("u8_value", true) => fields.u8_value.not_in(Vec::<u8>::new()),
            ("u16_value", false) => fields.u16_value.is_in(Vec::<u16>::new()),
            ("u16_value", true) => fields.u16_value.not_in(Vec::<u16>::new()),
            ("u32_value", false) => fields.u32_value.is_in(Vec::<u32>::new()),
            ("u32_value", true) => fields.u32_value.not_in(Vec::<u32>::new()),
            ("u64_value", false) => fields.u64_value.is_in(Vec::<u64>::new()),
            ("u64_value", true) => fields.u64_value.not_in(Vec::<u64>::new()),
            ("u128_value", false) => fields.u128_value.is_in(Vec::<u128>::new()),
            ("u128_value", true) => fields.u128_value.not_in(Vec::<u128>::new()),
            _ => fields.name.eq("unreachable field"),
        }
    }

    #[test]
    fn empty_membership_resolves_to_every_scalar_family() -> TestResult {
        let record = fixture_record();
        for field in [
            "name",
            "flag",
            "phase",
            "i8_value",
            "i16_value",
            "i32_value",
            "i64_value",
            "i128_value",
            "u8_value",
            "u16_value",
            "u32_value",
            "u64_value",
            "u128_value",
        ] {
            for (operator, excluded) in [("in", false), ("not_in", true)] {
                let document = scalar_document(field, operator, "[]");
                let decoded = decode::<Record>(&document)?;
                let authored = empty_membership(field, excluded);
                assert_eq!(decoded, authored);
                assert_eq!(decoded.matches(&record), excluded);
                assert_eq!(encode(&decoded)?, document);
            }
        }
        Ok(())
    }

    #[test]
    fn neutral_scalar_families_and_enum_variants_are_resolved_by_field() -> TestResult {
        let cases = [
            (
                scalar_document("name", "eq", "true"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("flag", "eq", "\"true\""),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("name", "in", "[true]"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("flag", "in", "[\"true\"]"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("flag", "contains", "\"true\""),
                FilterExpressionErrorKind::UnknownOperator,
            ),
            (
                scalar_document("phase", "eq", "\"secret_variant\""),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("i8_value", "eq", "true"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("i8_value", "in", "[true]"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("phase", "eq", "true"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("phase", "in", "[true]"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                scalar_document("i8_value", "contains", "\"secret_integer_contains\""),
                FilterExpressionErrorKind::UnknownOperator,
            ),
            (
                scalar_document("phase", "contains", "\"secret_enum_contains\""),
                FilterExpressionErrorKind::UnknownOperator,
            ),
        ];
        for (document, kind) in cases {
            assert_decode_error::<Record>(&document, kind, &["secret"])?;
        }

        let phase = decode::<Record>(&scalar_document("phase", "eq", "\"ready\""))?;
        assert_eq!(phase, Record::filter_fields().phase.eq(Phase::Ready));
        Ok(())
    }

    #[test]
    fn ordering_operators_are_integer_only() -> TestResult {
        for operator in ["lt", "lte", "gt", "gte"] {
            for (field, value) in [("flag", "true"), ("phase", "\"ready\"")] {
                assert_decode_error::<Record>(
                    &scalar_document(field, operator, value),
                    FilterExpressionErrorKind::UnknownOperator,
                    &[],
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn empty_ordering_operands_do_not_escape_field_validation() -> TestResult {
        for operator in ["lt", "lte", "gt", "gte"] {
            for field in ["name", "flag", "phase"] {
                assert_decode_error::<Record>(
                    &scalar_document(field, operator, "[]"),
                    FilterExpressionErrorKind::UnknownOperator,
                    &[],
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn resolved_wire_families_drive_equality_and_survive_clone_before_serialization() -> TestResult
    {
        let record = Record {
            name: b"1",
            ..fixture_record()
        };
        let text = decode::<Record>(&scalar_document("name", "eq", "\"1\""))?;
        let text_clone = text.clone();
        assert_eq!(text_clone, text);
        assert!(text_clone.matches(&record));
        assert_ne!(
            text,
            __private::text_field::<Record>("other_target", "name").eq("1")
        );
        assert_ne!(
            text,
            __private::text_field::<Record>("record", "other_field").eq("1")
        );
        assert_ne!(
            text,
            __private::text_field::<Record>("record", "name").eq_ignore_case("1")
        );
        assert_ne!(
            text,
            __private::enum_field::<Record, Phase>("record", "name").eq(Phase::Numeric)
        );
        assert_ne!(
            text,
            __private::integer_field::<Record, i8>("record", "name").eq(1)
        );
        assert_ne!(
            text,
            __private::integer_field::<Record, u8>("record", "name").eq(1)
        );

        let enumeration = decode::<Record>(&scalar_document("phase", "eq", "\"1\""))?;
        assert_ne!(
            enumeration,
            __private::text_field::<Record>("record", "phase").eq("1")
        );
        assert_ne!(
            enumeration,
            __private::integer_field::<Record, i8>("record", "phase").eq(1)
        );
        assert_ne!(
            enumeration,
            __private::integer_field::<Record, u8>("record", "phase").eq(1)
        );
        let signed = decode::<Record>(&scalar_document("i8_value", "eq", "\"1\""))?;
        assert_ne!(
            signed,
            __private::text_field::<Record>("record", "i8_value").eq("1")
        );
        assert_ne!(
            signed,
            __private::enum_field::<Record, Phase>("record", "i8_value").eq(Phase::Numeric)
        );
        assert_ne!(
            signed,
            __private::integer_field::<Record, u8>("record", "i8_value").eq(1)
        );
        let unsigned = decode::<Record>(&scalar_document("u8_value", "eq", "\"1\""))?;
        assert_ne!(
            unsigned,
            __private::text_field::<Record>("record", "u8_value").eq("1")
        );
        assert_ne!(
            unsigned,
            __private::enum_field::<Record, Phase>("record", "u8_value").eq(Phase::Numeric)
        );
        assert_ne!(
            unsigned,
            __private::integer_field::<Record, i8>("record", "u8_value").eq(1)
        );

        let empty = decode::<Record>(&scalar_document("name", "in", "[]"))?;
        assert_eq!(empty.clone(), empty);
        assert!(!empty.clone().matches(&record));
        assert_ne!(
            empty,
            __private::bool_field::<Record>("record", "name").is_in([])
        );
        assert_ne!(
            empty,
            __private::enum_field::<Record, Phase>("record", "name").is_in(Vec::<Phase>::new())
        );
        assert_ne!(
            empty,
            __private::integer_field::<Record, i8>("record", "name").is_in([])
        );
        assert_ne!(
            empty,
            __private::integer_field::<Record, u8>("record", "name").is_in([])
        );
        Ok(())
    }

    #[test]
    fn membership_array_order_is_semantic_and_preserved() -> TestResult {
        let document = scalar_document("name", "in", "[\"second\",\"first\"]");
        let decoded = decode::<Record>(&document)?;
        let fields = Record::filter_fields();
        assert_eq!(decoded, fields.name.is_in(["second", "first"]));
        assert_ne!(decoded, fields.name.is_in(["first", "second"]));
        assert_eq!(encode(&decoded)?, document);
        Ok(())
    }

    #[test]
    fn integer_membership_preserves_order_and_duplicate_values() -> TestResult {
        let document = scalar_document("i8_value", "in", "[\"1\",\"1\",\"2\"]");
        let decoded = decode::<Record>(&document)?;
        let field = Record::filter_fields().i8_value;
        assert_eq!(decoded, field.is_in([1, 1, 2]));
        assert_ne!(decoded, field.is_in([1, 2, 1]));
        assert_eq!(encode(&decoded)?, document);
        Ok(())
    }

    #[test]
    fn signed_unsigned_and_enum_sets_validate_every_member_for_both_operators() -> TestResult {
        let fields = Record::filter_fields();
        for (document, authored) in [
            (
                scalar_document("i16_value", "in", "[\"-1\",\"0\",\"32767\"]"),
                fields.i16_value.is_in([-1, 0, i16::MAX]),
            ),
            (
                scalar_document("i16_value", "not_in", "[\"-1\",\"0\",\"32767\"]"),
                fields.i16_value.not_in([-1, 0, i16::MAX]),
            ),
            (
                scalar_document("u16_value", "in", "[\"0\",\"65535\"]"),
                fields.u16_value.is_in([0, u16::MAX]),
            ),
            (
                scalar_document("u16_value", "not_in", "[\"0\",\"65535\"]"),
                fields.u16_value.not_in([0, u16::MAX]),
            ),
        ] {
            let decoded = decode::<Record>(&document)?;
            assert_eq!(decoded, authored);
            assert_eq!(encode(&decoded)?, document);
        }

        for (field, operator, values, sentinel) in [
            ("i16_value", "in", "[\"0\",\"01\"]", "01"),
            ("i16_value", "not_in", "[\"0\",\"32768\"]", "32768"),
            ("u16_value", "in", "[\"0\",\"-1\"]", "-1"),
            ("u16_value", "not_in", "[\"0\",\"65536\"]", "65536"),
            (
                "phase",
                "in",
                "[\"ready\",\"secret_enum_set\"]",
                "secret_enum_set",
            ),
            (
                "phase",
                "not_in",
                "[\"blocked\",\"secret_enum_set\"]",
                "secret_enum_set",
            ),
        ] {
            let sentinels = if sentinel.starts_with("secret") {
                std::slice::from_ref(&sentinel)
            } else {
                &[]
            };
            assert_decode_error::<Record>(
                &scalar_document(field, operator, values),
                FilterExpressionErrorKind::InvalidLiteral,
                sentinels,
            )?;
        }
        Ok(())
    }

    #[test]
    fn numeric_looking_strings_remain_valid_text_and_enum_literals() -> TestResult {
        let text = decode::<Record>(&scalar_document("name", "eq", "\"01\""))?;
        let record = Record {
            name: b"01",
            ..fixture_record()
        };
        assert!(text.matches(&record));

        let enumeration = decode::<Record>(&scalar_document("phase", "eq", "\"1\""))?;
        assert_eq!(
            enumeration,
            Record::filter_fields().phase.eq(Phase::Numeric)
        );
        Ok(())
    }

    #[test]
    fn nested_same_operator_nodes_flatten_without_other_simplification() -> TestResult {
        let source = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"flag\",\"value\":true},{\"op\":\"eq\",\"field\":\"phase\",\"value\":\"ready\"}]}]}}";
        let canonical = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},{\"op\":\"eq\",\"field\":\"flag\",\"value\":true},{\"op\":\"eq\",\"field\":\"phase\",\"value\":\"ready\"}]}}";
        let fields = Record::filter_fields();
        let authored = fields
            .name
            .eq("x")
            .and(fields.flag.eq(true))
            .and(fields.phase.eq(Phase::Ready));
        let decoded = decode::<Record>(source)?;
        assert_eq!(decoded, authored);
        assert_eq!(encode(&decoded)?, canonical);
        Ok(())
    }

    #[test]
    fn nested_or_nodes_flatten_but_nested_unary_junctions_stay_invalid() -> TestResult {
        let source = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"c\"}]}]}}";
        let canonical = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"a\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"b\"},{\"op\":\"eq\",\"field\":\"name\",\"value\":\"c\"}]}}";
        let fields = Record::filter_fields();
        let decoded = decode::<Record>(source)?;
        assert_eq!(
            decoded,
            fields
                .name
                .eq("a")
                .or(fields.name.eq("b"))
                .or(fields.name.eq("c"))
        );
        assert_eq!(encode(&decoded)?, canonical);

        let unary_nested = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"or\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret\"}]}}}";
        assert_decode_error::<Record>(
            unary_nested,
            FilterExpressionErrorKind::InvalidStructure,
            &["secret"],
        )
    }

    #[test]
    fn double_not_is_preserved_exactly() -> TestResult {
        let source = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}}";
        let decoded = decode::<Record>(source)?;
        assert_eq!(decoded, Record::filter_fields().name.eq("x").not().not());
        assert_eq!(encode(&decoded)?, source);

        let not_and = "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"not\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"},{\"op\":\"eq\",\"field\":\"flag\",\"value\":true}]}}}";
        let fields = Record::filter_fields();
        let decoded = decode::<Record>(not_and)?;
        assert_eq!(decoded, fields.name.eq("x").and(fields.flag.eq(true)).not());
        assert_eq!(encode(&decoded)?, not_and);
        Ok(())
    }

    #[test]
    fn unicode_folding_is_not_normalization_or_regex_full_folding() -> TestResult {
        let fields = Record::filter_fields();
        let record = fixture_record();
        let folded = decode::<Record>(&scalar_document(
            "name",
            "contains_ignore_case",
            "\"STRASSE\"",
        ))?;
        let regex = decode::<Record>(&scalar_document(
            "name",
            "regex_ignore_case",
            "\"^STRASSE$\"",
        ))?;
        assert!(folded.matches(&record));
        assert!(!regex.matches(&record));

        let composed = Record {
            name: "é".as_bytes(),
            ..fixture_record()
        };
        assert!(!fields.name.eq_ignore_case("e\u{301}").matches(&composed));
        Ok(())
    }

    #[test]
    fn invalid_regex_is_rejected_before_an_expression_escapes() -> TestResult {
        assert_decode_error::<Record>(
            &scalar_document("name", "regex", "\"[secret_regex\""),
            FilterExpressionErrorKind::InvalidRegex,
            &["secret_regex"],
        )
    }

    #[test]
    fn rust_regex_rejects_python_lookaround_and_backreferences() -> TestResult {
        for pattern in ["a(?=b)", r"(a)\1"] {
            let value = serde_json::to_string(pattern)?;
            for operator in ["regex", "regex_ignore_case"] {
                assert_decode_error::<Record>(
                    &scalar_document("name", operator, &value),
                    FilterExpressionErrorKind::InvalidRegex,
                    &[pattern],
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn typed_target_field_operator_and_relation_errors_are_stable() -> TestResult {
        for target in ["", "Record", "0record", "_record", "record-name", "réc"] {
            let source = format!(
                "{{\"version\":1,\"target\":\"{target}\",\"expr\":{{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}}}"
            );
            let sentinels: &[&str] = if target.is_empty() {
                &[]
            } else {
                std::slice::from_ref(&target)
            };
            assert_decode_error::<Record>(
                &source,
                FilterExpressionErrorKind::InvalidTarget,
                sentinels,
            )?;
        }
        assert_decode_error::<Record>(
            "{\"version\":1,\"target\":\"other\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"x\"}}",
            FilterExpressionErrorKind::InvalidTarget,
            &["other"],
        )?;

        for field in [
            "",
            "Name",
            "0name",
            "_name",
            "name-here",
            "nämé",
            "secret_field",
        ] {
            let document = scalar_document(field, "eq", "\"secret_literal\"");
            let sentinels: &[&str] = if field.is_empty() {
                &["secret_literal"]
            } else {
                &[field, "secret_literal"]
            };
            assert_decode_error::<Record>(
                &document,
                FilterExpressionErrorKind::UnknownField,
                sentinels,
            )?;
        }

        let cases = [
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"unknown\",\"field\":\"name\",\"value\":\"secret\"}}",
                FilterExpressionErrorKind::UnknownOperator,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"secret_quantifier\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownQuantifier,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"is\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownQuantifier,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"owner\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownQuantifier,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"owner\",\"quantifier\":\"all\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownQuantifier,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"owner\",\"quantifier\":\"none\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownQuantifier,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"eq\",\"field\":\"children\",\"value\":\"secret\"}}",
                FilterExpressionErrorKind::UnknownOperator,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"name\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownOperator,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"secret_nested_field\",\"value\":\"secret\"}}}",
                FilterExpressionErrorKind::UnknownField,
            ),
            (
                "{\"version\":1,\"target\":\"record\",\"expr\":{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"secret\",\"target\":\"child\"}}}",
                FilterExpressionErrorKind::InvalidStructure,
            ),
        ];
        for (source, kind) in cases {
            assert_decode_error::<Record>(source, kind, &["secret"])?;
        }
        Ok(())
    }

    #[test]
    fn authored_and_decoded_debug_reveals_only_stable_structure() -> TestResult {
        let authored = Record::filter_fields().name.eq("secret_literal");
        let decoded = decode::<Record>(&scalar_document("name", "eq", "\"secret_literal\""))?;
        for debug in [format!("{authored:?}"), format!("{decoded:?}")] {
            assert!(debug.contains("record"));
            assert!(debug.contains("name"));
            assert!(debug.contains("eq"));
            assert!(!debug.contains("secret_literal"));
            assert!(!debug.contains("14"));
        }

        let nested_document = |literal: &str| {
            format!(
                "{{\"version\":1,\"target\":\"record\",\"expr\":{{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{{\"op\":\"relation\",\"field\":\"leaves\",\"quantifier\":\"any\",\"expr\":{{\"op\":\"eq\",\"field\":\"label\",\"value\":\"{literal}\"}}}}}}}}"
            )
        };
        let short = format!("{:?}", decode::<Record>(&nested_document("x"))?);
        let long_literal = "secret_nested_literal".repeat(128);
        let long = format!("{:?}", decode::<Record>(&nested_document(&long_literal))?);
        assert_eq!(short, long);
        for schema_name in ["record", "child", "leaf", "children", "leaves", "label"] {
            assert!(short.contains(schema_name));
        }
        assert!(!long.contains(&long_literal));
        Ok(())
    }

    #[test]
    fn short_and_long_rejected_literals_share_one_redacted_category() -> TestResult {
        let short = "secret_short";
        let long = "secret_long".repeat(512);
        let mut library_messages = Vec::new();
        for sentinel in [short, long.as_str()] {
            let document = scalar_document("phase", "eq", &format!("\"{sentinel}\""));
            assert_decode_error::<Record>(
                &document,
                FilterExpressionErrorKind::InvalidLiteral,
                &[sentinel],
            )?;
            let Err(error) = decode::<Record>(&document) else {
                return Err(invalid_test_error("invalid enum literal decoded"));
            };
            let display = error.to_string();
            library_messages.push(
                display
                    .split_once(" at line ")
                    .map_or(display.as_str(), |(message, _)| message)
                    .to_owned(),
            );
        }
        assert_eq!(library_messages[0], library_messages[1]);
        Ok(())
    }

    #[allow(dead_code)]
    struct PermissiveFields {
        name: TextField<PermissiveRecord>,
    }

    struct PermissiveRecord;

    impl Filterable for PermissiveRecord {
        type Fields = PermissiveFields;
        const FILTER_TARGET: &'static str = "permissive_record";

        fn filter_fields() -> Self::Fields {
            PermissiveFields {
                name: __private::text_field(Self::FILTER_TARGET, "name"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
            Ok(())
        }
    }

    #[test]
    fn unresolved_wire_values_cannot_escape_a_permissive_handwritten_validator() -> TestResult {
        let sources = [
            "{\"version\":1,\"target\":\"permissive_record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":\"secret_unresolved\"}}",
            "{\"version\":1,\"target\":\"permissive_record\",\"expr\":{\"op\":\"eq\",\"field\":\"name\",\"value\":true}}",
            "{\"version\":1,\"target\":\"permissive_record\",\"expr\":{\"op\":\"in\",\"field\":\"name\",\"value\":[]}}",
        ];
        for source in sources {
            assert_decode_error::<PermissiveRecord>(
                source,
                FilterExpressionErrorKind::InvalidStructure,
                &["secret_unresolved"],
            )?;
        }
        Ok(())
    }

    static FALLBACK_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[allow(dead_code)]
    struct FallbackFields {
        name: TextField<FallbackRecord>,
    }

    struct FallbackRecord;

    impl Filterable for FallbackRecord {
        type Fields = FallbackFields;
        const FILTER_TARGET: &'static str = "fallback_record";

        fn filter_fields() -> Self::Fields {
            FallbackFields {
                name: __private::text_field(Self::FILTER_TARGET, "name"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            if predicate.field() == "name" {
                predicate.validate_text()
            } else {
                FALLBACK_CALLS.fetch_add(1, Ordering::Relaxed);
                Err(__private::unknown_field_error())
            }
        }
    }

    #[test]
    fn unknown_field_reaches_the_handwritten_fallback_once() -> TestResult {
        FALLBACK_CALLS.store(0, Ordering::Relaxed);
        let source = "{\"version\":1,\"target\":\"fallback_record\",\"expr\":{\"op\":\"eq\",\"field\":\"secret_fallback_field\",\"value\":\"secret_literal\"}}";
        assert_decode_error::<FallbackRecord>(
            source,
            FilterExpressionErrorKind::UnknownField,
            &["secret_fallback_field", "secret_literal"],
        )?;
        assert_eq!(FALLBACK_CALLS.load(Ordering::Relaxed), 1);
        Ok(())
    }

    static ORDERED_FIRST_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ORDERED_SECOND_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CONVERSION_VALID_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CONVERSION_ERROR_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct ConversionOrderRecord;

    impl Filterable for ConversionOrderRecord {
        type Fields = ();
        const FILTER_TARGET: &'static str = "conversion_order";

        fn filter_fields() {}

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            match predicate.field() {
                "valid" => {
                    CONVERSION_VALID_CALLS.fetch_add(1, Ordering::Relaxed);
                    predicate.validate_text()
                }
                "typed_error" => {
                    CONVERSION_ERROR_CALLS.fetch_add(1, Ordering::Relaxed);
                    Err(__private::unknown_field_error())
                }
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    #[allow(dead_code)]
    struct OrderedFields {
        first: TextField<OrderedRecord>,
        second: TextField<OrderedRecord>,
    }

    struct OrderedRecord;

    impl Filterable for OrderedRecord {
        type Fields = OrderedFields;
        const FILTER_TARGET: &'static str = "ordered_record";

        fn filter_fields() -> Self::Fields {
            OrderedFields {
                first: __private::text_field(Self::FILTER_TARGET, "first"),
                second: __private::text_field(Self::FILTER_TARGET, "second"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            match predicate.field() {
                "first" => {
                    ORDERED_FIRST_CALLS.fetch_add(1, Ordering::Relaxed);
                    predicate.validate_bool()
                }
                "second" => {
                    ORDERED_SECOND_CALLS.fetch_add(1, Ordering::Relaxed);
                    Err(__private::unknown_field_error())
                }
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    #[test]
    fn logical_validation_is_exhaustive_and_returns_the_leftmost_error() -> TestResult {
        let predicate = |field: &str, value: &str| {
            format!("{{\"op\":\"eq\",\"field\":\"{field}\",\"value\":\"{value}\"}}")
        };
        for (left, right, expected) in [
            (
                predicate("first", "secret_first"),
                predicate("second", "secret_second"),
                FilterExpressionErrorKind::InvalidLiteral,
            ),
            (
                predicate("second", "secret_second"),
                predicate("first", "secret_first"),
                FilterExpressionErrorKind::UnknownField,
            ),
        ] {
            ORDERED_FIRST_CALLS.store(0, Ordering::Relaxed);
            ORDERED_SECOND_CALLS.store(0, Ordering::Relaxed);
            let source = format!(
                "{{\"version\":1,\"target\":\"ordered_record\",\"expr\":{{\"op\":\"and\",\"args\":[{left},{right}]}}}}"
            );
            assert_decode_error::<OrderedRecord>(
                &source,
                expected,
                &["secret_first", "secret_second"],
            )?;
            assert_eq!(ORDERED_FIRST_CALLS.load(Ordering::Relaxed), 1);
            assert_eq!(ORDERED_SECOND_CALLS.load(Ordering::Relaxed), 1);
        }
        Ok(())
    }

    #[test]
    fn junction_conversion_error_still_validates_a_sibling() -> TestResult {
        let unknown_operator = "{\"op\":\"secret_operator\",\"field\":\"ignored\",\"value\":\"secret_unknown_literal\"}";
        let valid = "{\"op\":\"eq\",\"field\":\"valid\",\"value\":\"secret_valid_literal\"}";
        CONVERSION_VALID_CALLS.store(0, Ordering::Relaxed);
        let source = format!(
            "{{\"version\":1,\"target\":\"conversion_order\",\"expr\":{{\"op\":\"and\",\"args\":[{unknown_operator},{valid}]}}}}"
        );
        assert_decode_error::<ConversionOrderRecord>(
            &source,
            FilterExpressionErrorKind::UnknownOperator,
            &[
                "secret_operator",
                "secret_unknown_literal",
                "secret_valid_literal",
            ],
        )?;
        assert_eq!(CONVERSION_VALID_CALLS.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn junction_conversion_error_preserves_an_earlier_typed_error() -> TestResult {
        let typed_error =
            "{\"op\":\"eq\",\"field\":\"typed_error\",\"value\":\"secret_typed_literal\"}";
        let unknown_operator = "{\"op\":\"secret_operator\",\"field\":\"ignored\",\"value\":\"secret_unknown_literal\"}";
        CONVERSION_ERROR_CALLS.store(0, Ordering::Relaxed);
        let source = format!(
            "{{\"version\":1,\"target\":\"conversion_order\",\"expr\":{{\"op\":\"and\",\"args\":[{typed_error},{unknown_operator}]}}}}"
        );
        assert_decode_error::<ConversionOrderRecord>(
            &source,
            FilterExpressionErrorKind::UnknownField,
            &[
                "secret_typed_literal",
                "secret_operator",
                "secret_unknown_literal",
            ],
        )?;
        assert_eq!(CONVERSION_ERROR_CALLS.load(Ordering::Relaxed), 1);
        Ok(())
    }

    struct RejectingFields {
        name: TextField<RejectingRecord>,
    }

    struct RejectingRecord;

    impl Filterable for RejectingRecord {
        type Fields = RejectingFields;

        const FILTER_TARGET: &'static str = "rejecting_record";

        fn filter_fields() -> Self::Fields {
            RejectingFields {
                name: __private::text_field(Self::FILTER_TARGET, "name"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
            Err(__private::unknown_field_error())
        }
    }

    struct InvalidAuthoredTarget;

    impl Filterable for InvalidAuthoredTarget {
        type Fields = ();

        const FILTER_TARGET: &'static str = "Bad-Target";

        fn filter_fields() {}

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            predicate.validate_text()
        }
    }

    struct AuthoredNameRecord;

    impl Filterable for AuthoredNameRecord {
        type Fields = ();

        const FILTER_TARGET: &'static str = "record";

        fn filter_fields() {}

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            predicate.validate_text()
        }
    }

    struct LaxAuthoredParent;
    struct LaxAuthoredChild;

    impl Filterable for LaxAuthoredParent {
        type Fields = ();

        const FILTER_TARGET: &'static str = "parent";

        fn filter_fields() {}

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
            Ok(())
        }
    }

    #[test]
    fn authored_names_are_preflighted_before_serialization() -> TestResult {
        let invalid_target = __private::text_field::<InvalidAuthoredTarget>(
            InvalidAuthoredTarget::FILTER_TARGET,
            "Bad-Field",
        )
        .eq("secret_target_literal");
        assert_encode_error_without_output(
            &invalid_target,
            FilterExpressionErrorKind::InvalidTarget,
            &["Bad-Target", "Bad-Field", "secret_target_literal"],
        )?;

        let invalid_field = __private::text_field::<AuthoredNameRecord>("record", "Bad-Field")
            .eq("secret_field_literal");
        assert_encode_error_without_output(
            &invalid_field,
            FilterExpressionErrorKind::UnknownField,
            &["Bad-Field", "secret_field_literal"],
        )?;
        Ok(())
    }

    #[test]
    fn nested_authored_identities_are_preflighted_without_validator_recursion() -> TestResult {
        let relation =
            __private::many_relation::<LaxAuthoredParent, LaxAuthoredChild>("parent", "children");
        let invalid_target = relation.any(
            __private::text_field::<LaxAuthoredChild>("Bad-Target", "name")
                .eq("secret_nested_target_literal"),
        );
        assert_encode_error_without_output(
            &invalid_target,
            FilterExpressionErrorKind::InvalidTarget,
            &["Bad-Target", "secret_nested_target_literal"],
        )?;

        let invalid_field = relation.any(
            __private::text_field::<LaxAuthoredChild>("child", "Bad-Field")
                .eq("secret_nested_field_literal"),
        );
        assert_encode_error_without_output(
            &invalid_field,
            FilterExpressionErrorKind::UnknownField,
            &["Bad-Field", "secret_nested_field_literal"],
        )?;
        Ok(())
    }

    #[test]
    fn serialization_runs_typed_validation_and_redacts_rejected_values() -> TestResult {
        let expression = RejectingRecord::filter_fields().name.eq("secret_authored");
        let Err(error) = encode(&expression) else {
            return Err(invalid_test_error("invalid authored expression serialized"));
        };
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert_eq!(
            display,
            category_text(FilterExpressionErrorKind::UnknownField)
        );
        assert!(!display.contains("secret_authored"));
        assert!(!debug.contains("secret_authored"));

        let wrong_target =
            __private::text_field::<Record>("wrong_target", "name").eq("secret_wrong_target");
        let Err(error) = encode(&wrong_target) else {
            return Err(invalid_test_error("wrong target serialized"));
        };
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert_eq!(
            display,
            category_text(FilterExpressionErrorKind::InvalidTarget)
        );
        for sentinel in ["wrong_target", "secret_wrong_target"] {
            assert!(!display.contains(sentinel));
            assert!(!debug.contains(sentinel));
        }
        Ok(())
    }

    static COUNTER_ROOT_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);
    static COUNTER_CHILD_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);
    static COUNTER_LEAF_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);

    #[allow(dead_code)]
    struct CounterLeafFields {
        label: TextField<CounterLeaf>,
    }

    struct CounterLeaf;

    impl Filterable for CounterLeaf {
        type Fields = CounterLeafFields;
        const FILTER_TARGET: &'static str = "counter_leaf";

        fn filter_fields() -> Self::Fields {
            CounterLeafFields {
                label: __private::text_field(Self::FILTER_TARGET, "label"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            COUNTER_LEAF_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
            match predicate.field() {
                "label" => predicate.validate_text(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    #[allow(dead_code)]
    struct CounterChildFields {
        label: TextField<CounterChild>,
        active: BoolField<CounterChild>,
        leaves: ManyRelation<CounterChild, CounterLeaf>,
    }

    struct CounterChild;

    impl Filterable for CounterChild {
        type Fields = CounterChildFields;
        const FILTER_TARGET: &'static str = "counter_child";

        fn filter_fields() -> Self::Fields {
            CounterChildFields {
                label: __private::text_field(Self::FILTER_TARGET, "label"),
                active: __private::bool_field(Self::FILTER_TARGET, "active"),
                leaves: __private::many_relation(Self::FILTER_TARGET, "leaves"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            COUNTER_CHILD_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
            match predicate.field() {
                "label" => predicate.validate_text(),
                "active" => predicate.validate_bool(),
                "leaves" => predicate.validate_many::<CounterLeaf>(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    #[allow(dead_code)]
    struct CounterRootFields {
        name: TextField<CounterRoot>,
        children: ManyRelation<CounterRoot, CounterChild>,
        owner: OneRelation<CounterRoot, CounterChild>,
    }

    struct CounterRoot;

    impl Filterable for CounterRoot {
        type Fields = CounterRootFields;
        const FILTER_TARGET: &'static str = "counter_root";

        fn filter_fields() -> Self::Fields {
            CounterRootFields {
                name: __private::text_field(Self::FILTER_TARGET, "name"),
                children: __private::many_relation(Self::FILTER_TARGET, "children"),
                owner: __private::one_relation(Self::FILTER_TARGET, "owner"),
            }
        }

        fn __filter_matches(&self, _: &Predicate) -> bool {
            false
        }

        fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
            COUNTER_ROOT_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
            match predicate.field() {
                "name" => predicate.validate_text(),
                "children" => predicate.validate_many::<CounterChild>(),
                "owner" => predicate.validate_one::<CounterChild>(),
                _ => Err(__private::unknown_field_error()),
            }
        }
    }

    fn reset_validation_counters() {
        COUNTER_ROOT_VALIDATIONS.store(0, Ordering::Relaxed);
        COUNTER_CHILD_VALIDATIONS.store(0, Ordering::Relaxed);
        COUNTER_LEAF_VALIDATIONS.store(0, Ordering::Relaxed);
    }

    fn assert_validation_counts() {
        assert_eq!(COUNTER_ROOT_VALIDATIONS.load(Ordering::Relaxed), 3);
        assert_eq!(COUNTER_CHILD_VALIDATIONS.load(Ordering::Relaxed), 3);
        assert_eq!(COUNTER_LEAF_VALIDATIONS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn validation_dispatch_is_linear_for_decode_and_encode() -> TestResult {
        let source = "{\"version\":1,\"target\":\"counter_root\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"name\",\"value\":\"root\"},{\"op\":\"relation\",\"field\":\"children\",\"quantifier\":\"any\",\"expr\":{\"op\":\"and\",\"args\":[{\"op\":\"eq\",\"field\":\"active\",\"value\":true},{\"op\":\"relation\",\"field\":\"leaves\",\"quantifier\":\"any\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"leaf\"}}]}},{\"op\":\"relation\",\"field\":\"owner\",\"quantifier\":\"is\",\"expr\":{\"op\":\"eq\",\"field\":\"label\",\"value\":\"owner\"}}]}}";

        reset_validation_counters();
        let expression = decode::<CounterRoot>(source)?;
        assert_validation_counts();

        reset_validation_counters();
        assert_eq!(encode(&expression)?, source);
        assert_validation_counts();
        Ok(())
    }
}
