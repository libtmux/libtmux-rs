mod vocabulary;

pub(super) use vocabulary::{
    ChannelWaitOutcomeSchema, OptionScopeSchema, PlanAttributionSchema, PlanGroupingSchema,
    PlanOperationKindSchema, PlanOutcomeSchema, ResizeDirectionSchema, SelectPaneDirectionSchema,
    SelectWindowDirectionSchema, SplitDirectionSchema, WatchStopSchema,
};

/// Keep selected variants in one generated externally tagged enum.
///
/// A malformed schema returns `false` without changing any variants.
pub(super) fn retain_tagged_union_variants(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    definition: &str,
    mut retain: impl FnMut(&str) -> bool,
) -> bool {
    let Some(definitions) = schema.get("$defs").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let Some(variants) = definitions
        .get(definition)
        .and_then(serde_json::Value::as_object)
        .and_then(|definition| definition.get("oneOf"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let Some(names): Option<Vec<_>> = variants
        .iter()
        .map(|variant| tagged_variant_name(variant, definitions))
        .collect()
    else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    if names.is_empty() || names.iter().any(|name| !seen.insert(name.as_str())) {
        return false;
    }
    let keep: Vec<_> = names.iter().map(|name| retain(name)).collect();
    if !keep.iter().any(|keep| *keep) {
        return false;
    }

    let Some(variants) = schema
        .get_mut("$defs")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|definitions| definitions.get_mut(definition))
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|definition| definition.get_mut("oneOf"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut keep = keep.into_iter();
    variants.retain(|_| keep.next().unwrap_or(false));
    true
}

fn tagged_variant_name(
    variant: &serde_json::Value,
    definitions: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let object = variant.as_object()?;
    if object.get("type")?.as_str()? != "object"
        || !matches!(object.get("additionalProperties")?.as_bool(), Some(false))
    {
        return None;
    }
    let required = object.get("required")?.as_array()?;
    let properties = object.get("properties")?.as_object()?;
    if required.len() != 1 || properties.len() != 1 {
        return None;
    }
    let name = required.first()?.as_str()?;
    let reference = properties.get(name)?.as_object()?.get("$ref")?.as_str()?;
    (reference == format!("#/$defs/{name}") && definitions.contains_key(name))
        .then(|| name.to_owned())
}

/// Drop `format` keywords that are not part of JSON Schema.
///
/// Rust's unsigned integers are described by schemars as `format: "uint32"`
/// and friends, which no JSON Schema dialect defines. Clients that validate
/// schemas log a line per occurrence -- one real client emitted forty-four on
/// a single listing -- and a strict validator may reject outright.
///
/// Nothing is lost by removing them: schemars already writes `minimum: 0`
/// beside each one, so `type: integer` plus that bound says everything the
/// format was there to say.
pub(super) fn strip_unknown_formats(schema: &mut serde_json::Map<String, serde_json::Value>) {
    // The formats JSON Schema itself defines, plus the two integer widths
    // OpenAPI added that tooling widely understands.
    const KNOWN: &[&str] = &[
        "date-time",
        "date",
        "time",
        "duration",
        "email",
        "idn-email",
        "hostname",
        "idn-hostname",
        "ipv4",
        "ipv6",
        "uri",
        "uri-reference",
        "iri",
        "iri-reference",
        "uuid",
        "uri-template",
        "json-pointer",
        "relative-json-pointer",
        "regex",
        "int32",
        "int64",
        "float",
        "double",
    ];

    if schema
        .get("format")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|format| !KNOWN.contains(&format))
    {
        schema.remove("format");
    }
    for value in schema.values_mut() {
        strip_value(value);
    }
}

/// Walk into whatever shape a schema keyword holds.
fn strip_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => strip_unknown_formats(map),
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::retain_tagged_union_variants;

    #[test]
    fn tagged_union_filter_rejects_malformed_variants_without_mutating() {
        for variant in [
            json!({
                "type": "array",
                "properties": {"Known": {"$ref": "#/$defs/Known"}},
                "required": ["Known"],
                "additionalProperties": false,
            }),
            json!({
                "type": "object",
                "properties": {"Known": {"$ref": "#/$defs/Known"}},
                "required": ["Known"],
                "additionalProperties": true,
            }),
            json!({
                "type": "object",
                "properties": {"Known": {"type": "string"}},
                "required": ["Known"],
                "additionalProperties": false,
            }),
            json!({
                "type": "object",
                "properties": {"Known": {"$ref": "#/$defs/Other"}},
                "required": ["Known"],
                "additionalProperties": false,
            }),
        ] {
            let mut schema = json!({"$defs": {"Op": {"oneOf": [variant]}}})
                .as_object()
                .expect("the fixture is an object")
                .clone();
            let before = schema.clone();

            assert!(!retain_tagged_union_variants(&mut schema, "Op", |_| true));
            assert_eq!(schema, before);
        }
    }
}
