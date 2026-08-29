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
