//! Schema metadata for portable filter expressions.

#[cfg(feature = "schema")]
use super::FilterExpr;
use super::Filterable;

/// A filter target whose portable expression grammar can be described.
///
/// `#[derive(libtmux::Filterable)]` implements this and [`Filterable`] from
/// the same field declaration. Implement it manually only when a handwritten
/// [`Filterable`] target must also expose a `JsonSchema` for its
/// [`FilterExpr`].
///
/// The schema closes targets, fields, operators, relations, and JSON value
/// families. Deserialization remains authoritative for fixed-width integer
/// bounds, regex compilation, and whole-expression complexity limits.
///
/// # Examples
///
/// ```
/// # #[cfg(all(feature = "derive", feature = "schema"))]
/// # {
/// use libtmux::query::FilterExpr;
///
/// #[derive(libtmux::Filterable)]
/// #[filterable(target = "task", crate = "libtmux")]
/// struct Task {
///     name: String,
///     done: bool,
/// }
///
/// let schema = schemars::schema_for!(FilterExpr<Task>);
/// assert!(schema.as_object().is_some());
/// # }
/// ```
pub trait FilterSchema: Filterable {
    /// Return the closed field grammar for this target.
    #[doc(hidden)]
    fn __filter_schema() -> FilterSchemaDescriptor;
}

/// One target and every field its decoder accepts.
#[doc(hidden)]
#[derive(Debug)]
pub struct FilterSchemaDescriptor {
    target: &'static str,
    fields: Vec<FilterFieldSchema>,
}

impl FilterSchemaDescriptor {
    /// Build generated target metadata.
    #[must_use]
    pub fn new(target: &'static str, fields: Vec<FilterFieldSchema>) -> Self {
        Self { target, fields }
    }

    /// Reuse one scalar catalog under a composite target name.
    #[must_use]
    pub fn retarget(mut self, target: &'static str) -> Self {
        self.target = target;
        self
    }

    /// Add one generated relation to an existing scalar catalog.
    #[must_use]
    pub fn with_field(mut self, field: FilterFieldSchema) -> Self {
        self.fields.push(field);
        self
    }
}

/// One accepted field and the predicates valid for it.
#[doc(hidden)]
#[derive(Debug)]
pub struct FilterFieldSchema {
    #[cfg_attr(not(feature = "schema"), allow(dead_code))]
    name: &'static str,
    #[cfg_attr(not(feature = "schema"), allow(dead_code))]
    kind: FilterValueSchema,
}

impl FilterFieldSchema {
    /// Build generated field metadata.
    #[must_use]
    pub const fn new(name: &'static str, kind: FilterValueSchema) -> Self {
        Self { name, kind }
    }
}

/// The value or relation family behind one filter field.
#[doc(hidden)]
#[derive(Debug)]
pub enum FilterValueSchema {
    /// Strict UTF-8 text.
    Text,
    /// A boolean.
    Bool,
    /// A signed integer encoded as decimal text.
    Signed,
    /// An unsigned integer encoded as decimal text.
    Unsigned,
    /// A stable closed string vocabulary.
    Enum(&'static [&'static str]),
    /// A to-many relation.
    Many(fn() -> FilterSchemaDescriptor),
    /// A to-one relation.
    One(fn() -> FilterSchemaDescriptor),
}

/// Return generated metadata for one related target.
#[doc(hidden)]
#[must_use]
pub fn filter_schema<T: FilterSchema>() -> FilterSchemaDescriptor {
    T::__filter_schema()
}

#[cfg(feature = "schema")]
mod json {
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    use schemars::{JsonSchema, Schema, SchemaGenerator};
    use serde_json::{Map, Value, json};

    use crate::query::grammar::{
        MAX_EXPRESSION_NODES, MAX_SET_VALUES, RelationQuantifier, SetOperator, TextOperator,
        VERSION,
    };

    use super::{FilterExpr, FilterSchema, FilterSchemaDescriptor, FilterValueSchema};

    const INTEGER_PATTERN: &str = "^-?(0|[1-9][0-9]*)$";
    const UNSIGNED_PATTERN: &str = "^(0|[1-9][0-9]*)$";

    impl<T: FilterSchema> JsonSchema for FilterExpr<T> {
        fn schema_name() -> Cow<'static, str> {
            format!("FilterExpr_{}", T::FILTER_TARGET).into()
        }

        fn schema_id() -> Cow<'static, str> {
            format!("libtmux::query::FilterExpr<{}>", std::any::type_name::<T>()).into()
        }

        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let descriptor = T::__filter_schema();
            install_definitions(generator, &descriptor, &mut BTreeMap::new());
            object_schema(
                [
                    ("version", json!({"type": "integer", "const": VERSION})),
                    (
                        "target",
                        json!({"type": "string", "const": descriptor.target}),
                    ),
                    ("expr", reference(generator, descriptor.target)),
                ],
                &["version", "target", "expr"],
            )
        }
    }

    fn install_definitions(
        generator: &mut SchemaGenerator,
        descriptor: &FilterSchemaDescriptor,
        seen: &mut BTreeMap<&'static str, Value>,
    ) {
        let schema = expression_schema(generator, descriptor);
        if let Some(existing) = seen.get(descriptor.target) {
            assert!(
                existing == &schema,
                "filter target `{}` has conflicting schemas",
                descriptor.target,
            );
            return;
        }
        seen.insert(descriptor.target, schema.clone());

        let name = definition_name(descriptor.target);
        if let Some(existing) = generator.definitions().get(&name) {
            assert!(
                existing == &schema,
                "filter target `{}` collides with an existing schema",
                descriptor.target,
            );
        } else {
            generator.definitions_mut().insert(name, schema);
        }

        for field in &descriptor.fields {
            if let FilterValueSchema::Many(related) | FilterValueSchema::One(related) = field.kind {
                install_definitions(generator, &related(), seen);
            }
        }
    }

    fn expression_schema(
        generator: &SchemaGenerator,
        descriptor: &FilterSchemaDescriptor,
    ) -> Value {
        let recursive = reference(generator, descriptor.target);
        let mut variants = vec![
            object_value(
                [
                    ("op", string_enum(&["and", "or"])),
                    (
                        "args",
                        json!({
                            "type": "array",
                            "items": recursive,
                            "minItems": 2,
                            "maxItems": MAX_EXPRESSION_NODES,
                        }),
                    ),
                ],
                &["op", "args"],
            ),
            object_value(
                [("op", string_const("not")), ("expr", recursive)],
                &["op", "expr"],
            ),
        ];

        let text = field_names(descriptor, |kind| matches!(kind, FilterValueSchema::Text));
        let text_operators = text_operator_labels(&TextOperator::SCALAR_SCHEMA);
        let membership_operators = set_operator_labels(&SetOperator::MEMBERSHIP_SCHEMA);
        add_scalar_variants(
            &mut variants,
            &text,
            &text_operators,
            json!({"type": "string"}),
            &membership_operators,
            &json!({"type": "string"}),
        );

        let booleans = field_names(descriptor, |kind| matches!(kind, FilterValueSchema::Bool));
        let equality_operators = set_operator_labels(&SetOperator::EQ_SCHEMA);
        add_scalar_variants(
            &mut variants,
            &booleans,
            &equality_operators,
            json!({"type": "boolean"}),
            &membership_operators,
            &json!({"type": "boolean"}),
        );

        let signed = field_names(descriptor, |kind| matches!(kind, FilterValueSchema::Signed));
        let comparison_operators = set_operator_labels(&SetOperator::COMPARISON_SCHEMA);
        add_scalar_variants(
            &mut variants,
            &signed,
            &comparison_operators,
            json!({"type": "string", "pattern": INTEGER_PATTERN}),
            &membership_operators,
            &json!({"type": "string", "pattern": INTEGER_PATTERN}),
        );

        let unsigned = field_names(descriptor, |kind| {
            matches!(kind, FilterValueSchema::Unsigned)
        });
        add_scalar_variants(
            &mut variants,
            &unsigned,
            &comparison_operators,
            json!({"type": "string", "pattern": UNSIGNED_PATTERN}),
            &membership_operators,
            &json!({"type": "string", "pattern": UNSIGNED_PATTERN}),
        );

        let many_quantifiers = relation_labels(&RelationQuantifier::MANY_SCHEMA);
        let one_quantifiers = relation_labels(&RelationQuantifier::ONE_SCHEMA);
        for field in &descriptor.fields {
            match field.kind {
                FilterValueSchema::Enum(values) if !values.is_empty() => add_scalar_variants(
                    &mut variants,
                    &[field.name],
                    &equality_operators,
                    string_enum(values),
                    &membership_operators,
                    &string_enum(values),
                ),
                FilterValueSchema::Many(related) => variants.push(relation_variant(
                    generator,
                    field.name,
                    &many_quantifiers,
                    related().target,
                )),
                FilterValueSchema::One(related) => variants.push(relation_variant(
                    generator,
                    field.name,
                    &one_quantifiers,
                    related().target,
                )),
                FilterValueSchema::Enum(_)
                | FilterValueSchema::Text
                | FilterValueSchema::Bool
                | FilterValueSchema::Signed
                | FilterValueSchema::Unsigned => {}
            }
        }

        json!({"oneOf": variants})
    }

    fn field_names(
        descriptor: &FilterSchemaDescriptor,
        matches: impl Fn(&FilterValueSchema) -> bool,
    ) -> Vec<&'static str> {
        descriptor
            .fields
            .iter()
            .filter(|field| matches(&field.kind))
            .map(|field| field.name)
            .collect()
    }

    fn text_operator_labels(operators: &[TextOperator]) -> Vec<&'static str> {
        operators.iter().map(|operator| operator.label()).collect()
    }

    fn set_operator_labels(operators: &[SetOperator]) -> Vec<&'static str> {
        operators.iter().map(|operator| operator.label()).collect()
    }

    fn relation_labels(quantifiers: &[RelationQuantifier]) -> Vec<&'static str> {
        quantifiers
            .iter()
            .map(|quantifier| quantifier.label())
            .collect()
    }

    fn add_scalar_variants(
        variants: &mut Vec<Value>,
        fields: &[&str],
        scalar_ops: &[&str],
        scalar_value: Value,
        set_ops: &[&str],
        set_item: &Value,
    ) {
        if fields.is_empty() {
            return;
        }
        variants.push(object_value(
            [
                ("op", string_enum(scalar_ops)),
                ("field", string_enum(fields)),
                ("value", scalar_value),
            ],
            &["op", "field", "value"],
        ));
        variants.push(object_value(
            [
                ("op", string_enum(set_ops)),
                ("field", string_enum(fields)),
                (
                    "value",
                    json!({
                        "type": "array",
                        "items": set_item,
                        "maxItems": MAX_SET_VALUES,
                    }),
                ),
            ],
            &["op", "field", "value"],
        ));
    }

    fn relation_variant(
        generator: &SchemaGenerator,
        field: &str,
        quantifiers: &[&str],
        related: &str,
    ) -> Value {
        object_value(
            [
                ("op", string_const("relation")),
                ("field", string_const(field)),
                ("quantifier", string_enum(quantifiers)),
                ("expr", reference(generator, related)),
            ],
            &["op", "field", "quantifier", "expr"],
        )
    }

    fn object_schema<const N: usize>(properties: [(&str, Value); N], required: &[&str]) -> Schema {
        object_map(properties, required).into()
    }

    fn object_value<const N: usize>(properties: [(&str, Value); N], required: &[&str]) -> Value {
        Value::Object(object_map(properties, required))
    }

    fn object_map<const N: usize>(
        properties: [(&str, Value); N],
        required: &[&str],
    ) -> Map<String, Value> {
        let properties = properties
            .into_iter()
            .map(|(name, schema)| (name.to_owned(), schema))
            .collect::<Map<_, _>>();
        Map::from_iter([
            ("type".to_owned(), Value::String("object".to_owned())),
            ("properties".to_owned(), Value::Object(properties)),
            ("required".to_owned(), json!(required)),
            ("additionalProperties".to_owned(), Value::Bool(false)),
        ])
    }

    fn string_const(value: &str) -> Value {
        json!({"type": "string", "const": value})
    }

    fn string_enum(values: &[&str]) -> Value {
        let values = values
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        json!({"type": "string", "enum": values})
    }

    fn reference(generator: &SchemaGenerator, target: &str) -> Value {
        let path = generator.settings().definitions_path.trim_end_matches('/');
        json!({"$ref": format!("#{path}/{}", pointer_segment(&definition_name(target)))})
    }

    fn definition_name(target: &str) -> String {
        format!("libtmux_filter_expr_{target}")
    }

    fn pointer_segment(value: &str) -> String {
        value.replace('~', "~0").replace('/', "~1")
    }
}
