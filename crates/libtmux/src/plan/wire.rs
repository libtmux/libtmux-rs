//! Serializing a plan without lying about what tmux arguments can hold.
//!
//! Two things resist a text wire format. A tmux ID is a struct but reads as
//! `%1`, and writing it as an object would make a hand-written plan tedious.
//! A command argument is operating-system bytes, which need not be UTF-8 at
//! all, so writing it as a string would either corrupt it or refuse it.
//!
//! IDs therefore serialize as their rendered text, and an argument serializes
//! as text when it is text and as an array of bytes when it is not. Both
//! round-trip exactly, and the common case stays readable.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serialize an ID as the text tmux prints for it.
pub(super) fn id<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: AsRef<str>,
{
    serializer.serialize_str(value.as_ref())
}

/// Read an ID back from that text.
pub(super) fn parse_id<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(D::Error::custom)
}

/// A command argument on the wire: text where it can be, bytes where it cannot.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub(super) enum Argument {
    Text(String),
    Bytes(Vec<u8>),
}

#[cfg(feature = "schema")]
fn id_schema(generator: &mut schemars::SchemaGenerator, sigil: &str) -> schemars::Schema {
    use schemars::JsonSchema as _;

    const U32: &str = concat!(
        "0*(?:[0-9]{1,9}|[0-3][0-9]{9}|4[0-1][0-9]{8}|42[0-8][0-9]{7}|",
        "429[0-3][0-9]{6}|4294[0-8][0-9]{5}|42949[0-5][0-9]{4}|",
        "429496[0-6][0-9]{3}|4294967[0-1][0-9]{2}|42949672[0-8][0-9]|",
        "429496729[0-5])"
    );
    let mut schema = String::json_schema(generator);
    schema.insert("pattern".into(), format!("^{sigil}{U32}$").into());
    schema
}

#[cfg(feature = "schema")]
pub(super) fn session_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, r"\$")
}

#[cfg(feature = "schema")]
pub(super) fn window_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, "@")
}

#[cfg(feature = "schema")]
pub(super) fn pane_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, "%")
}

/// Serialize one argument.
pub(super) fn argument<S: Serializer>(value: &OsString, serializer: S) -> Result<S::Ok, S::Error> {
    match value.to_str() {
        Some(text) => Argument::Text(text.to_owned()),
        None => Argument::Bytes(value.as_bytes().to_vec()),
    }
    .serialize(serializer)
}

/// Read one argument back.
pub(super) fn parse_argument<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<OsString, D::Error> {
    Ok(match Argument::deserialize(deserializer)? {
        Argument::Text(text) => OsString::from(text),
        Argument::Bytes(bytes) => OsString::from_vec(bytes),
    })
}

/// The same, for an argument a command may omit.
///
/// Takes `&Option<_>` rather than `Option<&_>` because that is the shape
/// serde's `serialize_with` hands a field of this type.
#[allow(
    clippy::ref_option,
    reason = "the signature serde's serialize_with requires"
)]
pub(super) fn optional_argument<S: Serializer>(
    value: &Option<OsString>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => argument(value, serializer),
        None => serializer.serialize_none(),
    }
}

/// Read back an argument a command may omit.
pub(super) fn parse_optional_argument<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<OsString>, D::Error> {
    Ok(
        Option::<Argument>::deserialize(deserializer)?.map(|argument| match argument {
            Argument::Text(text) => OsString::from(text),
            Argument::Bytes(bytes) => OsString::from_vec(bytes),
        }),
    )
}

/// The same, for the name/value pairs an operation carries.
pub(super) fn pairs<S: Serializer>(
    value: &[(OsString, OsString)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let wire: Vec<(Argument, Argument)> = value
        .iter()
        .map(|(name, item)| (owned(name), owned(item)))
        .collect();
    wire.serialize(serializer)
}

/// Read those pairs back.
pub(super) fn parse_pairs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(OsString, OsString)>, D::Error> {
    Ok(Vec::<(Argument, Argument)>::deserialize(deserializer)?
        .into_iter()
        .map(|(name, value)| (borrowed(name), borrowed(value)))
        .collect())
}

/// The same, for a list of key names an operation sends.
pub(super) fn list<S: Serializer>(value: &[OsString], serializer: S) -> Result<S::Ok, S::Error> {
    value
        .iter()
        .map(owned)
        .collect::<Vec<_>>()
        .serialize(serializer)
}

/// Read that list back.
pub(super) fn parse_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<OsString>, D::Error> {
    Ok(Vec::<Argument>::deserialize(deserializer)?
        .into_iter()
        .map(borrowed)
        .collect())
}

fn owned(value: &OsString) -> Argument {
    value.to_str().map_or_else(
        || Argument::Bytes(value.as_bytes().to_vec()),
        |text| Argument::Text(text.to_owned()),
    )
}

fn borrowed(argument: Argument) -> OsString {
    match argument {
        Argument::Text(text) => OsString::from(text),
        Argument::Bytes(bytes) => OsString::from_vec(bytes),
    }
}
